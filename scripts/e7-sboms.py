"""E7-2 part 1d (release.yml's build and assemble jobs; the SBOM half of PLAN's E8-6): one CycloneDX SBOM per shipped
binary, as one release asset.

    python -I scripts/e7-sboms.py collect --target <triple> --out <dir> <raw dir>...
    python -I scripts/e7-sboms.py pack --version <v> --timestamp <seconds> --x64 <dir> --arm64 <dir> --out <dir>
    python -I scripts/e7-sboms.py check <zip>
    python -I scripts/e7-sboms.py --self-test

`collect`: every `*.cdx.json` under the raw dirs (what `cargo cyclonedx --describe binaries --format json` wrote,
moved out of the source tree by scripts/e7-release-build.sh) is named `<binary>_bin.cdx.json` or
`<binary>_cdylib.cdx.json` by cargo-cyclonedx. The six shipped binaries' (SHIPPED: lpk, lpk-check, lpk-worker,
lpk-worker-libarchive, lpk_shell, lpk-gui) are copied unchanged to `<out>/<binary>.cdx.json`; the SBOMs of the
workspace's binaries that are not shipped (NOT_SHIPPED: the bench, the test helpers) are left out by name. It fails,
writing nothing, on a shipped SBOM missing or found twice, on an SBOM of any other binary (an extra one: a new binary
target must be classed here first), on `<out>` not empty, and on an SBOM that is not valid (below).

Valid: JSON with `bomFormat` "CycloneDX" and `specVersion` SPEC_VERSION (the `--spec-version` the build passes),
`metadata.component.name` the binary's name, and the `cdx:rustc:sbom:target:triple` property of `metadata` the
target triple it was made for.

`pack`: writes `<out>/litepack-<v>-sbom.zip` (e7-assets.py's `sbom_name`) with the folders `x64/` and `arm64/`, each
holding the six `<binary>.cdx.json` of that architecture's `collect` (each checked again). Deterministic as
scripts/make-zips.py's zips: sorted entries, the one timestamp (the source commit's time), fixed attributes, Deflate.

`check`: the zip holds exactly the two folders and the 12 SBOMs, six per architecture, each valid for its triple.

Each command exits 1 naming every failure. The standard library only.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import re
import shutil
import sys
import tempfile
import zipfile
from pathlib import Path, PurePosixPath

HERE = Path(__file__).resolve().parent

SHIPPED = ("lpk", "lpk-check", "lpk-worker", "lpk-worker-libarchive", "lpk_shell", "lpk-gui")
# The workspace binaries cargo-cyclonedx describes that are not shipped (repro-build.sh's list of what ships).
NOT_SHIPPED = ("lpk-bench", "lpk-fake-archiver", "lpk-decode", "lpk-procstat-probe", "lpk-sandbox-testworker",
               "lpk-test-worker")
TRIPLES = {"x64": "x86_64-pc-windows-msvc", "arm64": "aarch64-pc-windows-msvc"}
SPEC_VERSION = "1.5"
TRIPLE_PROPERTY = "cdx:rustc:sbom:target:triple"
SUFFIX = ".cdx.json"
RAW_RE = re.compile(r"^(?P<bin>.+)_(?:bin|cdylib)\.cdx\.json$")
ZIP_PRODUCT = "litepack"
VERSION_RE = re.compile(r"[0-9A-Za-z][0-9A-Za-z.+-]*")


class Fail(Exception):
    pass


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, HERE / file)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def zip_name(version):
    if not VERSION_RE.fullmatch(version):
        raise Fail(f"unusable version {version!r}")
    return f"{ZIP_PRODUCT}-{version}-sbom.zip"


def problems(data, binary, triple):
    """What is wrong with the SBOM bytes `data` as the SBOM of `binary` for `triple`: [] when valid."""
    try:
        doc = json.loads(data.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as e:
        return [f"not JSON ({e})"]
    if not isinstance(doc, dict):
        return ["not a JSON object"]
    out = []
    if doc.get("bomFormat") != "CycloneDX":
        out.append(f"bomFormat is {doc.get('bomFormat')!r}, not 'CycloneDX'")
    if doc.get("specVersion") != SPEC_VERSION:
        out.append(f"specVersion is {doc.get('specVersion')!r}, not {SPEC_VERSION!r}")
    meta = doc.get("metadata") if isinstance(doc.get("metadata"), dict) else {}
    comp = meta.get("component") if isinstance(meta.get("component"), dict) else {}
    if comp.get("name") != binary:
        out.append(f"metadata.component names {comp.get('name')!r}, not {binary!r}")
    props = meta.get("properties") if isinstance(meta.get("properties"), list) else []
    got = [p.get("value") for p in props if isinstance(p, dict) and p.get("name") == TRIPLE_PROPERTY]
    if got != [triple]:
        out.append(f"metadata's {TRIPLE_PROPERTY} is {got}, not [{triple!r}]")
    return out


def collect(target, out, raws):
    """Copy the six shipped SBOMs to `out`; the lines to print. Raises Fail naming every failure."""
    if target not in TRIPLES.values():
        raise Fail(f"{target} is not a shipped target")
    out = Path(out)
    if out.exists() and (not out.is_dir() or any(out.iterdir())):
        raise Fail(f"{out} exists and is not an empty folder")
    found, errors, lines = {}, [], []
    for raw in raws:
        raw = Path(raw)
        if not raw.is_dir():
            errors.append(f"no folder {raw}")
            continue
        for f in sorted(raw.rglob("*" + SUFFIX)):
            m = RAW_RE.match(f.name)
            if not m:
                errors.append(f"{f}: not a name cargo-cyclonedx gives (<binary>_bin or _cdylib{SUFFIX})")
                continue
            b = m["bin"]
            if b in NOT_SHIPPED:
                lines.append(f"not shipped, left out: {f.relative_to(raw).as_posix()}")
            elif b in SHIPPED:
                found.setdefault(b, []).append(f)
            else:
                errors.append(f"an extra SBOM, of a binary neither shipped nor known as not shipped: {f}")
    for b in SHIPPED:
        fs = found.get(b, [])
        if not fs:
            errors.append(f"missing: the SBOM of {b}")
        elif len(fs) > 1:
            errors.append(f"an extra SBOM: {b}'s {len(fs)} times ({', '.join(map(str, fs))})")
        else:
            errors += [f"{fs[0]}: {p}" for p in problems(fs[0].read_bytes(), b, target)]
    if errors:
        raise Fail("; ".join(errors))
    out.mkdir(parents=True, exist_ok=True)
    for b in SHIPPED:
        data = found[b][0].read_bytes()
        (out / (b + SUFFIX)).write_bytes(data)
        lines.append(f"{b}{SUFFIX}: {hashlib.sha256(data).hexdigest()} (from {found[b][0].name}, {target})")
    return lines


def read_folder(folder, triple):
    """{name: bytes} of a collected SBOM folder: exactly the six, each valid for `triple`."""
    folder = Path(folder)
    if not folder.is_dir():
        raise Fail(f"no folder {folder}")
    want = sorted(b + SUFFIX for b in SHIPPED)
    present = sorted(p.name for p in folder.iterdir())
    errors = [f"{folder}: missing {n}" for n in want if n not in present]
    errors += [f"{folder}: an extra file {n}" for n in present if n not in want]
    data = {}
    for n in want:
        if n in present:
            data[n] = (folder / n).read_bytes()
            errors += [f"{folder / n}: {p}" for p in problems(data[n], n[: -len(SUFFIX)], triple)]
    if errors:
        raise Fail("; ".join(errors))
    return data


def pack(version, timestamp, folders, out):
    """Write the SBOM zip from {arch: collected folder}; its path."""
    mz = _load("make_zips", "make-zips.py")
    name = zip_name(version)
    if sorted(folders) != sorted(TRIPLES):
        raise Fail(f"pack needs the folders of {sorted(TRIPLES)}")
    files = {}
    for arch, folder in folders.items():
        for n, data in read_folder(folder, TRIPLES[arch]).items():
            files[f"{arch}/{n}"] = data
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    when = mz.zip_time(timestamp)

    def info(n, attr, kind):
        i = zipfile.ZipInfo(n, date_time=when)
        i.create_system = 0
        i.external_attr = attr
        i.compress_type = kind
        return i

    fd, tmp = tempfile.mkstemp(prefix=name + ".", suffix=".tmp", dir=out)
    os.close(fd)
    try:
        entries = {f"{a}/": None for a in TRIPLES}
        entries.update(files)
        with zipfile.ZipFile(tmp, "w", compresslevel=9) as z:
            for n in sorted(entries):
                if entries[n] is None:
                    z.writestr(info(n, mz.DIR_ATTR, zipfile.ZIP_STORED), b"")
                else:
                    z.writestr(info(n, mz.FILE_ATTR, zipfile.ZIP_DEFLATED), entries[n])
        os.replace(tmp, out / name)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)
    return out / name


def check(path):
    """The lines to print; raises Fail naming every failure."""
    path = Path(path)
    if not path.is_file():
        raise Fail(f"no {path}")
    try:
        z = zipfile.ZipFile(path)
    except zipfile.BadZipFile as e:
        raise Fail(f"{path}: not a zip ({e})")
    errors, lines = [], []
    with z:
        infos = z.infolist()
        names = [i.filename for i in infos]
        want_dirs = sorted(f"{a}/" for a in TRIPLES)
        want_files = sorted(f"{a}/{b}{SUFFIX}" for a in TRIPLES for b in SHIPPED)
        if len(set(names)) != len(names):
            errors.append("a member twice")
        errors += [f"missing: {n}" for n in want_dirs + want_files if n not in names]
        errors += [f"an extra member: {n}" for n in names if n not in want_dirs + want_files]
        if names != sorted(names):
            errors.append("the members are not sorted")
        for n in want_files:
            if n in names:
                arch, file = PurePosixPath(n).parts
                ps = problems(z.read(n), file[: -len(SUFFIX)], TRIPLES[arch])
                errors += [f"{n}: {p}" for p in ps]
                if not ps:
                    lines.append(f"{n}: CycloneDX {SPEC_VERSION}, {file[: -len(SUFFIX)]}, {TRIPLES[arch]}")
    if errors:
        raise Fail(f"{path.name}: " + "; ".join(errors))
    return lines + [f"{path.name}: the {len(want_files)} SBOMs, six per architecture, each valid"]


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    ap = argparse.ArgumentParser(prog="e7-sboms.py", description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("collect")
    c.add_argument("--target", required=True)
    c.add_argument("--out", required=True)
    c.add_argument("raw", nargs="+")
    p = sub.add_parser("pack")
    p.add_argument("--version", required=True)
    p.add_argument("--timestamp", required=True, type=int)
    p.add_argument("--x64", required=True)
    p.add_argument("--arm64", required=True)
    p.add_argument("--out", required=True)
    k = sub.add_parser("check")
    k.add_argument("zip")
    a = ap.parse_args(argv)
    try:
        if a.cmd == "collect":
            print("\n".join(collect(a.target, a.out, a.raw)))
        elif a.cmd == "pack":
            print(f"wrote {pack(a.version, a.timestamp, {'x64': a.x64, 'arm64': a.arm64}, a.out)}")
        else:
            print("\n".join(check(a.zip)))
    except Fail as e:
        print(f"e7-sboms: {e}", file=sys.stderr)
        return 1
    return 0


def fake(binary, triple, fmt="CycloneDX", spec=SPEC_VERSION):
    """A minimal SBOM as cargo-cyclonedx writes it (the fields checked here)."""
    return json.dumps({
        "bomFormat": fmt, "specVersion": spec, "version": 1,
        "metadata": {"component": {"type": "application", "name": binary, "version": "0.0.1"},
                     "properties": [{"name": TRIPLE_PROPERTY, "value": triple}]},
        "components": [],
    }, indent=2).encode()


def self_test():
    import subprocess

    failures = []

    def ok(name, cond):
        print(f"{'ok  ' if cond else 'FAIL'} {name}")
        if not cond:
            failures.append(name)

    def raises(fn, *args, contains):
        try:
            fn(*args)
        except Fail as e:
            if contains in str(e):
                return True
            print(f"     got: {e}")
            return False
        return False

    ok("the zip's name", zip_name("0.1.0") == "litepack-0.1.0-sbom.zip")
    ok("a bad version is refused", raises(zip_name, "../x", contains="unusable version"))
    ok("valid: no problems", problems(fake("lpk", TRIPLES["x64"]), "lpk", TRIPLES["x64"]) == [])
    ok("not JSON fails", problems(b"<bom/>", "lpk", TRIPLES["x64"])[0].startswith("not JSON"))
    ok("a wrong spec version fails", problems(fake("lpk", TRIPLES["x64"], spec="1.3"), "lpk", TRIPLES["x64"]) ==
       ["specVersion is '1.3', not '1.5'"])
    ok("the other triple fails", "cdx:rustc:sbom:target:triple" in
       problems(fake("lpk", TRIPLES["arm64"]), "lpk", TRIPLES["x64"])[0])

    with tempfile.TemporaryDirectory() as d:
        d = Path(d)

        def raw(name, arch, change=None):
            """A raw folder laid out as the build leaves it: the workspace's binaries, the GUI's own run, workers/."""
            t = TRIPLES[arch]
            r = d / name / arch
            files = {}
            for b in SHIPPED + NOT_SHIPPED:
                kind = "cdylib" if b == "lpk_shell" else "bin"
                sub = {"lpk-worker": "workers/lpk-worker", "lpk": "crates/lpk-cli"}.get(b, f"crates/{b}")
                files[f"{sub}/{b}_{kind}{SUFFIX}"] = fake(b, t)
            if change:
                files = change(files)
            for n, data in files.items():
                (r / n).parent.mkdir(parents=True, exist_ok=True)
                (r / n).write_bytes(data)
            return r

        def collected(name, arch, change=None):
            out = d / name / f"sboms-{arch}"
            lines = collect(TRIPLES[arch], out, [raw(name, arch, change)])
            return out, lines

        x64, lines = collected("good", "x64")
        arm64, _ = collected("good", "arm64")
        ok("collect: the six, named <binary>.cdx.json", sorted(p.name for p in x64.iterdir()) ==
           sorted(b + SUFFIX for b in SHIPPED))
        ok("collect: the bytes unchanged", (x64 / "lpk.cdx.json").read_bytes() == fake("lpk", TRIPLES["x64"]))
        ok("collect: the unshipped ones named as left out", sum("left out" in l for l in lines) == len(NOT_SHIPPED))
        ok("collect: again into the same folder fails", raises(collect, TRIPLES["x64"], x64,
                                                               [d / "good" / "x64"], contains="not an empty folder"))

        def collect_fails(label, change, contains, arch="x64"):
            name = label.replace(" ", "-").replace(":", "")
            try:
                collected(name, arch, change)
                ok(label, False)
            except Fail as e:
                ok(label, contains in str(e))
                if contains not in str(e):
                    print(f"     got: {e}")
            ok(label + " (nothing written)", not (d / name / f"sboms-{arch}").exists())

        def drop(name):
            return lambda fs: {n: v for n, v in fs.items() if not n.endswith(name)}

        def put(name, data):
            return lambda fs: {**fs, name: data}

        collect_fails("collect: a missing SBOM fails", drop(f"lpk-check_bin{SUFFIX}"), "missing: the SBOM of lpk-check")
        collect_fails("collect: an extra SBOM fails", put(f"crates/lpk-new/lpk-new_bin{SUFFIX}",
                                                          fake("lpk-new", TRIPLES["x64"])),
                      "an extra SBOM, of a binary neither shipped nor known as not shipped")
        collect_fails("collect: a shipped SBOM twice fails", put(f"gui/crates/lpk-gui/lpk-gui_bin{SUFFIX}",
                                                                 fake("lpk-gui", TRIPLES["x64"])),
                      "an extra SBOM: lpk-gui's 2 times")
        collect_fails("collect: a wrong bomFormat fails", put(f"crates/lpk-cli/lpk_bin{SUFFIX}",
                                                              fake("lpk", TRIPLES["x64"], fmt="SPDX")),
                      "bomFormat is 'SPDX', not 'CycloneDX'")
        collect_fails("collect: a wrong component name fails", put(f"crates/lpk-cli/lpk_bin{SUFFIX}",
                                                                   fake("lpk-cli", TRIPLES["x64"])),
                      "metadata.component names 'lpk-cli', not 'lpk'")
        collect_fails("collect: an SBOM of the other target fails", put(f"crates/lpk-cli/lpk_bin{SUFFIX}",
                                                                        fake("lpk", TRIPLES["arm64"])),
                      TRIPLE_PROPERTY)
        collect_fails("collect: not JSON fails", put(f"crates/lpk-cli/lpk_bin{SUFFIX}", b"<bom/>"), "not JSON")
        collect_fails("collect: a name cargo-cyclonedx does not give fails", put(f"crates/x/lpk{SUFFIX}",
                                                                                 fake("lpk", TRIPLES["x64"])),
                      "not a name cargo-cyclonedx gives")

        # pack: deterministic, the two folders, sorted; check reads it back.
        z1 = pack("0.1.0", 1_700_000_000, {"x64": x64, "arm64": arm64}, d / "out1")
        z2 = pack("0.1.0", 1_700_000_000, {"x64": x64, "arm64": arm64}, d / "out2")
        ok("pack: the name", z1.name == "litepack-0.1.0-sbom.zip")
        ok("pack: two packs are byte-identical", z1.read_bytes() == z2.read_bytes())
        ok("pack: no temporary file left", sorted(p.name for p in (d / "out1").iterdir()) == [z1.name])
        with zipfile.ZipFile(z1) as z:
            names = z.namelist()
            infos = z.infolist()
        ok("pack: the two folders and twelve SBOMs, sorted", names == sorted(names) and len(names) == 14 and
           names[:2] == ["arm64/", "arm64/lpk-check.cdx.json"] and "x64/lpk_shell.cdx.json" in names)
        mz = _load("make_zips", "make-zips.py")
        ok("pack: one timestamp and fixed attributes", all(i.date_time == mz.zip_time(1_700_000_000) and
                                                           i.create_system == 0 for i in infos))
        ok("pack: another timestamp, other bytes", pack("0.1.0", 1_800_000_000, {"x64": x64, "arm64": arm64},
                                                         d / "out3").read_bytes() != z1.read_bytes())
        ok("check: the packed zip passes", check(z1)[-1] ==
           "litepack-0.1.0-sbom.zip: the 12 SBOMs, six per architecture, each valid")
        ok("pack: the folders swapped fail (triples)", raises(pack, "0.1.0", 1, {"x64": arm64, "arm64": x64},
                                                              d / "out4", contains=TRIPLE_PROPERTY))
        extra = d / "extra"
        shutil.copytree(x64, extra)
        (extra / "notes.txt").write_bytes(b"x")
        ok("pack: an extra file in a folder fails", raises(pack, "0.1.0", 1, {"x64": extra, "arm64": arm64},
                                                           d / "out5", contains="an extra file notes.txt"))
        ok("pack: nothing written on failure", not (d / "out5" / z1.name).exists())

        def planted(label, change, contains):
            p = d / "planted" / label.replace(" ", "-").replace(":", "") / z1.name
            p.parent.mkdir(parents=True)
            with zipfile.ZipFile(z1) as src:
                members = [(i.filename, src.read(i)) for i in src.infolist()]
            with zipfile.ZipFile(p, "w", zipfile.ZIP_DEFLATED) as z:
                for n, data in change(members):
                    z.writestr(n, data)
            ok(label, raises(check, p, contains=contains))

        planted("check: a missing SBOM fails", lambda ms: [m for m in ms if m[0] != "x64/lpk-gui.cdx.json"],
                "missing: x64/lpk-gui.cdx.json")
        planted("check: an extra SBOM fails", lambda ms: ms + [("x64/lpk-bench.cdx.json", fake("lpk-bench",
                                                                                                TRIPLES["x64"]))],
                "an extra member: x64/lpk-bench.cdx.json")
        planted("check: a wrong bomFormat fails",
                lambda ms: [(n, fake("lpk", TRIPLES["arm64"], fmt="SPDX") if n == "arm64/lpk.cdx.json" else v)
                            for n, v in ms], "arm64/lpk.cdx.json: bomFormat is 'SPDX'")
        planted("check: a wrong component name fails",
                lambda ms: [(n, fake("lpk", TRIPLES["x64"]) if n == "x64/lpk-check.cdx.json" else v)
                            for n, v in ms], "x64/lpk-check.cdx.json: metadata.component names 'lpk'")
        planted("check: an SBOM under the other architecture fails",
                lambda ms: [(n, fake("lpk", TRIPLES["arm64"]) if n == "x64/lpk.cdx.json" else v)
                            for n, v in ms], TRIPLE_PROPERTY)
        (d / "bad.zip").write_bytes(b"not a zip")
        ok("check: not a zip fails", raises(check, d / "bad.zip", contains="not a zip"))

        # The command line: exit codes and messages.
        run = [sys.executable, "-I", __file__]
        r = subprocess.run(run + ["check", str(z1)], capture_output=True, text=True)
        ok("check: exit 0 and twelve lines", r.returncode == 0 and r.stdout.count(f"CycloneDX {SPEC_VERSION}") == 12)
        r = subprocess.run(run + ["check", str(d / "bad.zip")], capture_output=True, text=True)
        ok("check: exit 1 with the message", r.returncode == 1 and "not a zip" in r.stderr and
           "Traceback" not in r.stderr)
        bad = raw("cmd", "x64", drop(f"lpk_shell_cdylib{SUFFIX}"))
        r = subprocess.run(run + ["collect", "--target", TRIPLES["x64"], "--out", str(d / "cmd" / "out"), str(bad)],
                           capture_output=True, text=True)
        ok("collect: exit 1 naming the missing SBOM", r.returncode == 1 and "missing: the SBOM of lpk_shell"
           in r.stderr)
        r = subprocess.run(run + ["pack", "--version", "0.1.0", "--timestamp", "1700000000", "--x64", str(x64),
                                  "--arm64", str(arm64), "--out", str(d / "cmd" / "zip")], capture_output=True,
                           text=True)
        ok("pack: exit 0, the same bytes", r.returncode == 0 and (d / "cmd" / "zip" / z1.name).read_bytes()
           == z1.read_bytes())

    if failures:
        print(f"e7-sboms: self-test FAILED: {', '.join(failures)}")
        return 1
    print("e7-sboms: self-test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
