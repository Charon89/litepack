"""E7-2 part 1c (release.yml's assemble and smoke jobs): the release's asset names and their SHA256SUMS.

    python -I scripts/e7-assets.py expected --version <v>
    python -I scripts/e7-assets.py sums --version <v> <dir>
    python -I scripts/e7-assets.py check --version <v> <dir>
    python -I scripts/e7-assets.py --self-test

`expected`: the asset names, one per line, sorted: the two NSIS installers as the Tauri bundler names them
(`<productName>_<v>_<arch>-setup.exe`, productName from crates/lpk-gui/tauri.conf.json) and the portable and CLI zip
of each architecture as scripts/make-zips.py names them (`litepack-<v>-windows-<arch>-{portable,cli}.zip`), and
the SBOM zip as scripts/e7-sboms.py names it (`litepack-<v>-sbom.zip`, E7-2 part 1d): seven names. The first six are
the artefacts (`artefacts()`, what scripts/e7-artefacts.py looks inside); the SBOM zip holds no PE and no notices.

`sums`: writes `<dir>/SHA256SUMS` in `sha256sum`'s text format (`<hex>  <name>`, LF line ends, sorted by name) over
exactly the expected names. It fails, writing nothing, when an expected file is missing or `<dir>` holds any other
file than those and SHA256SUMS.

`check`: `<dir>/SHA256SUMS` lists exactly the expected names, in that format, and each file's SHA-256 matches.

Each command exits 1 naming every failure. The standard library only.
"""

import hashlib
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ARCHES = ("x64", "arm64")
# The installer's name prefix is the bundler's: tauri.conf.json's productName (E7-1 may change it, with make-zips.py's
# PRODUCT; the self-test checks both against the repository).
INSTALLER_PRODUCT = "LitePack"
ZIP_PRODUCT = "litepack"
SUMS = "SHA256SUMS"
VERSION_RE = re.compile(r"[0-9A-Za-z][0-9A-Za-z.+-]*")
LINE_RE = re.compile(r"^([0-9a-f]{64})  ([^/\\\r\n]+)$")


class AssetError(Exception):
    pass


def artefacts(version):
    """The six artefacts of version `version` (the installers and the zips that ship the binaries), sorted."""
    if not VERSION_RE.fullmatch(version):
        raise AssetError(f"unusable version {version!r}")
    names = []
    for arch in ARCHES:
        names.append(f"{INSTALLER_PRODUCT}_{version}_{arch}-setup.exe")
        stem = f"{ZIP_PRODUCT}-{version}-windows-{arch}"
        names += [f"{stem}-portable.zip", f"{stem}-cli.zip"]
    return sorted(names)


def sbom_name(version):
    """The SBOM zip's name (scripts/e7-sboms.py pack writes it)."""
    if not VERSION_RE.fullmatch(version):
        raise AssetError(f"unusable version {version!r}")
    return f"{ZIP_PRODUCT}-{version}-sbom.zip"


def expected(version):
    """The asset names of version `version`, sorted: the six artefacts and the SBOM zip."""
    return sorted(artefacts(version) + [sbom_name(version)])


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sums(version, folder):
    """Write folder/SHA256SUMS; its text."""
    folder = Path(folder)
    names = expected(version)
    present = sorted(p.name for p in folder.iterdir()) if folder.is_dir() else []
    errors = [f"missing: {n}" for n in names if n not in present]
    errors += [f"not an asset: {n}" for n in present if n not in names and n != SUMS]
    errors += [f"not a file: {n}" for n in names if n in present and not (folder / n).is_file()]
    if errors:
        raise AssetError("; ".join(errors))
    text = "".join(f"{sha256(folder / n)}  {n}\n" for n in names)
    (folder / SUMS).write_bytes(text.encode("ascii"))
    return text


def check(version, folder):
    """The lines to print; raises AssetError naming every failure."""
    folder = Path(folder)
    names = expected(version)
    path = folder / SUMS
    if not path.is_file():
        raise AssetError(f"no {path}")
    raw = path.read_bytes()
    errors, listed, lines = [], [], []
    if b"\r" in raw:
        errors.append(f"{SUMS} has CR line ends")
    if raw and not raw.endswith(b"\n"):
        errors.append(f"{SUMS} does not end with a line end")
    for n, line in enumerate(raw.decode("ascii", "replace").replace("\r", "").split("\n")[:-1] if raw else [], 1):
        m = LINE_RE.match(line)
        if not m:
            errors.append(f"{SUMS}:{n}: not `<sha256>  <name>`: {line!r}")
            continue
        want, name = m.groups()
        listed.append(name)
        if name not in names:
            errors.append(f"{SUMS}:{n}: not an asset: {name}")
            continue
        f = folder / name
        if not f.is_file():
            errors.append(f"{SUMS}:{n}: {name} is missing")
            continue
        got = sha256(f)
        if got != want:
            errors.append(f"{SUMS}:{n}: {name} is {got}, the list says {want}")
        else:
            lines.append(f"{name}: OK")
    if listed != sorted(listed):
        errors.append(f"{SUMS} is not sorted by name")
    if len(set(listed)) != len(listed):
        errors.append(f"{SUMS} lists a name twice")
    errors += [f"{SUMS} does not list {n}" for n in names if n not in listed]
    if errors:
        raise AssetError("; ".join(errors))
    return lines + [f"{SUMS}: the {len(names)} assets of {version}, every hash matches"]


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    try:
        if len(argv) == 3 and argv[0] == "expected" and argv[1] == "--version":
            print("\n".join(expected(argv[2])))
            return 0
        if len(argv) == 4 and argv[0] == "sums" and argv[1] == "--version":
            sys.stdout.write(sums(argv[2], argv[3]))
            return 0
        if len(argv) == 4 and argv[0] == "check" and argv[1] == "--version":
            print("\n".join(check(argv[2], argv[3])))
            return 0
    except AssetError as e:
        print(f"e7-assets: {e}", file=sys.stderr)
        return 1
    print(__doc__, file=sys.stderr)
    return 2


def self_test():
    import json
    import importlib.util

    failures = []

    def ok(name, cond):
        print(f"{'ok  ' if cond else 'FAIL'} {name}")
        if not cond:
            failures.append(name)

    def raises(fn, *args, contains):
        try:
            fn(*args)
        except AssetError as e:
            return contains in str(e)
        return False

    names = expected("0.1.0")
    ok("seven names, sorted", names == sorted(names) and len(names) == 7)
    ok("the exact names", names == [
        "LitePack_0.1.0_arm64-setup.exe", "LitePack_0.1.0_x64-setup.exe", "litepack-0.1.0-sbom.zip",
        "litepack-0.1.0-windows-arm64-cli.zip", "litepack-0.1.0-windows-arm64-portable.zip",
        "litepack-0.1.0-windows-x64-cli.zip", "litepack-0.1.0-windows-x64-portable.zip"])
    ok("the six artefacts: all but the SBOM zip", artefacts("0.1.0") == [n for n in names if not n.endswith("-sbom.zip")]
       and len(artefacts("0.1.0")) == 6)
    ok("a bad version is refused by sbom_name", raises(sbom_name, "a b", contains="unusable version"))
    ok("a bad version is refused", raises(expected, "../x", contains="unusable version"))

    # The names agree with the code that writes the files: make-zips.py's zip_names and tauri.conf.json's productName.
    repo = Path(__file__).resolve().parent.parent
    spec = importlib.util.spec_from_file_location("make_zips", repo / "scripts" / "make-zips.py")
    mz = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mz)
    for arch in ARCHES:
        ok(f"make-zips.py names the {arch} zips the same", all(n in names for n in mz.zip_names("0.1.0", arch)))
    spec = importlib.util.spec_from_file_location("e7_sboms", repo / "scripts" / "e7-sboms.py")
    sb = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(sb)
    ok("e7-sboms.py names the SBOM zip the same", sb.zip_name("0.1.0") == sbom_name("0.1.0"))
    conf = json.loads((repo / "crates" / "lpk-gui" / "tauri.conf.json").read_text(encoding="utf-8"))
    ok("the installers' prefix is tauri.conf.json's productName", conf["productName"] == INSTALLER_PRODUCT)

    with tempfile.TemporaryDirectory() as d:
        d = Path(d)

        def fill(folder):
            folder.mkdir()
            for n in names:
                (folder / n).write_bytes(b"asset " + n.encode())
            return folder

        a = fill(d / "a")
        text = sums("0.1.0", a)
        ok("SHA256SUMS: seven LF lines, sorted, two spaces", text.count("\n") == 7 and "\r" not in text
           and [l.split("  ")[1] for l in text.splitlines()] == names)
        ok("SHA256SUMS: the real hashes", text.splitlines()[0].split("  ")[0] == hashlib.sha256(
            b"asset " + names[0].encode()).hexdigest())
        ok("check passes", check("0.1.0", a)[-1].startswith("SHA256SUMS: the 7 assets"))
        ok("sums again over its own SHA256SUMS", sums("0.1.0", a) == text)

        b = fill(d / "b")
        (b / names[2]).unlink()
        ok("sums: a missing asset fails", raises(sums, "0.1.0", b, contains=f"missing: {names[2]}"))
        ok("sums: nothing written on failure", not (b / SUMS).exists())
        c = fill(d / "c")
        (c / "extra.txt").write_bytes(b"x")
        ok("sums: another file fails", raises(sums, "0.1.0", c, contains="not an asset: extra.txt"))
        ok("sums: another version's names fail", raises(sums, "0.2.0", a, contains="missing"))

        e = fill(d / "e")
        sums("0.1.0", e)
        (e / names[3]).write_bytes(b"changed")
        ok("check: a changed asset fails", raises(check, "0.1.0", e, contains=f"{names[3]} is "))
        f = fill(d / "f")
        good = sums("0.1.0", f)
        (f / SUMS).write_text("".join(good.splitlines(True)[1:]), encoding="ascii", newline="\n")
        ok("check: an unlisted asset fails", raises(check, "0.1.0", f, contains=f"does not list {names[0]}"))
        (f / SUMS).write_bytes(good.replace("\n", "\r\n").encode())
        ok("check: CRLF fails", raises(check, "0.1.0", f, contains="CR line ends"))
        lines = good.splitlines(True)
        (f / SUMS).write_bytes("".join([lines[1], lines[0]] + lines[2:]).encode())
        ok("check: an unsorted list fails", raises(check, "0.1.0", f, contains="not sorted"))
        (f / SUMS).write_bytes((good + lines[0]).encode())
        ok("check: a name twice fails", raises(check, "0.1.0", f, contains="twice"))
        (f / SUMS).write_bytes((good + f"{'0' * 64}  other.zip\n").encode())
        ok("check: an extra name fails", raises(check, "0.1.0", f, contains="not an asset: other.zip"))
        (f / SUMS).write_bytes(good.replace("  ", " *", 1).encode())
        ok("check: the binary-mode marker is not this format", raises(check, "0.1.0", f, contains="not `<sha256>"))
        (f / SUMS).unlink()
        ok("check: no SHA256SUMS fails", raises(check, "0.1.0", f, contains="no "))

        # The command line: exit codes, and SHA256SUMS readable by sha256sum -c where it exists.
        run = [sys.executable, "-I", __file__]
        r = subprocess.run(run + ["expected", "--version", "0.1.0"], capture_output=True, text=True)
        ok("expected: exit 0 and the names", r.returncode == 0 and r.stdout.split() == names)
        r = subprocess.run(run + ["check", "--version", "0.1.0", str(e)], capture_output=True, text=True)
        ok("check: exit 1 with the message", r.returncode == 1 and names[3] in r.stderr and "Traceback" not in r.stderr)
        r = subprocess.run(run + ["sums", "--version", "0.1.0", str(b)], capture_output=True, text=True)
        ok("sums: exit 1 with the message", r.returncode == 1 and "missing" in r.stderr)
        try:
            r = subprocess.run(["sha256sum", "-c", SUMS], cwd=a, capture_output=True, text=True)
            ok("sha256sum -c reads SHA256SUMS", r.returncode == 0 and r.stdout.count(": OK") == 7)
        except FileNotFoundError:
            print("skip sha256sum -c (no sha256sum on PATH)")

    if failures:
        print(f"e7-assets: self-test FAILED: {', '.join(failures)}")
        return 1
    print("e7-assets: self-test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
