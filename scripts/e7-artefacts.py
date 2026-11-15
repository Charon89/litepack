"""E7-2 part 1c (release.yml's assemble job): what is inside the six release artefacts, the two NSIS installers and
the portable and CLI zip of each architecture (names as scripts/e7-assets.py gives them).

    python -I scripts/e7-artefacts.py notices <dir> --repo <root> [--sevenzip <7z.exe>]
    python -I scripts/e7-artefacts.py pes <dir> --unsigned-x64 <dir> --unsigned-arm64 <dir> [--sevenzip <7z.exe>]
    python -I scripts/e7-artefacts.py --self-test [--sevenzip <7z.exe>]

`notices` (E7-8 part 2, the notices finder): each artefact holds THIRD-PARTY-NOTICES, LICENSE-APACHE and LICENSE-MIT
exactly once each (by file name, in any folder), byte-equal to the repository's. One line per artefact and file.

`pes`: every PE inside each artefact (a file starting `MZ` whose `e_lfanew` points at a `PE\\0\\0` signature) is
classified, and anything that fits no class fails:
- `shipped`: one of the six shipped names (lpk.exe, lpk-check.exe, lpk-worker.exe, lpk-worker-libarchive.exe,
  lpk_shell.dll, lpk-gui.exe), at the artefact's machine (0x8664 x64, 0xAA64 arm64), carrying cargo-auditable's
  `.dep-v0` section, and byte-equal to that architecture's build (`unsigned.sha256` of the build job's artifact).
  Each artefact holds each shipped name it should exactly once (an installer all six, a portable zip all but
  lpk_shell.dll, a CLI zip the four command-line tools; scripts/make-zips.py).
- `nsis-plugin`: NSIS's own x86 parts (machine 0x14C) under `$PLUGINSDIR` in an installer.
- `nsis-uninstaller`: the uninstaller that NSIS writes at install time (WriteUninstaller), which 7-Zip shows as a file
  `uninstall.exe` at the installer's root: NSIS's own x86 stub (machine 0x14C), at most one per installer.
It prints each PE with its class, machine and SHA-256, a count per class, and 7-Zip's full listing of each installer
once.

The zips are read with zipfile; the installers through 7-Zip (`7z l -slt` for the member list, `7z x -aou` to unpack,
so that a duplicated member survives under a new name and fails as an unknown one). `--sevenzip` names 7z.exe;
without it, 7z on PATH or `%ProgramFiles%\\7-Zip\\7z.exe`. The PE reading is scripts/e7-pe-sections.py's. Each
command exits 1 naming every failure. The standard library only.
"""

import argparse
import hashlib
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import tempfile
import warnings
import zipfile
from collections import Counter
from pathlib import Path, PurePosixPath

HERE = Path(__file__).resolve().parent


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, HERE / file)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


pesec = _load("e7_pe_sections", "e7-pe-sections.py")

NOTICE_FILES = ("THIRD-PARTY-NOTICES", "LICENSE-APACHE", "LICENSE-MIT")
SHIPPED = ("lpk.exe", "lpk-check.exe", "lpk-worker.exe", "lpk-worker-libarchive.exe", "lpk_shell.dll", "lpk-gui.exe")
CLI_EXES = SHIPPED[:4]
# The shipped names each kind of artefact holds (scripts/make-zips.py; the NSIS overlay's resources and the GUI).
HOLDS = {
    "installer": set(SHIPPED),
    "portable": set(CLI_EXES) | {"lpk-gui.exe"},
    "cli": set(CLI_EXES),
}
MACHINE = {"x64": 0x8664, "arm64": 0xAA64}
X86 = 0x14C
DEP = ".dep-v0"
# As 7-Zip may show it (with or without the $INSTDIR prefix), compared in lower case.
UNINSTALLER = ("uninstall.exe", "$instdir/uninstall.exe")

INSTALLER_RE = re.compile(r"^.+_(?P<v>[^_]+)_(?P<arch>x64|arm64)-setup\.exe$")
ZIP_RE = re.compile(r"^litepack-(?P<v>.+)-windows-(?P<arch>x64|arm64)-(?P<kind>portable|cli)\.zip$")
SUM_RE = re.compile(r"^([0-9a-f]{64}) [ *](.+)$")


class Fail(Exception):
    pass


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


class Artefact:
    """One artefact: its name, architecture, kind, and members as (listed path, file on disk)."""

    def __init__(self, path, arch, kind):
        self.path, self.arch, self.kind = Path(path), arch, kind
        self.name = self.path.name
        self.listed = []  # every listed file member, '/'-separated, in listing order
        self.files = []  # (path relative to the unpacked root or listed path, file on disk)
        self.listing = ""


def find_artefacts(folder):
    """The six artefacts in `folder` as (path, arch, kind); other files are ignored. Fails unless each of the six
    slots is there exactly once, all of one version."""
    found, versions = {}, set()
    for p in sorted(Path(folder).iterdir()):
        m = INSTALLER_RE.match(p.name)
        if m:
            slot = (m["arch"], "installer")
        else:
            m = ZIP_RE.match(p.name)
            if not m:
                continue
            slot = (m["arch"], m["kind"])
        versions.add(m["v"])
        found.setdefault(slot, []).append(p)
    errors = []
    for arch in MACHINE:
        for kind in HOLDS:
            got = found.get((arch, kind), [])
            if len(got) != 1:
                errors.append(f"{len(got)} {arch} {kind} artefacts in {folder} ({', '.join(p.name for p in got)})")
    if len(versions) > 1:
        errors.append(f"artefacts of more than one version: {sorted(versions)}")
    if errors:
        raise Fail("; ".join(errors))
    return [(found[(a, k)][0], a, k) for a in MACHINE for k in HOLDS]


def find_sevenzip(given):
    if given:
        if not Path(given).is_file():
            raise Fail(f"no 7-Zip at {given}")
        return str(given)
    for c in (shutil.which("7z"), os.path.join(os.environ.get("ProgramFiles", r"C:\Program Files"), "7-Zip", "7z.exe")):
        if c and Path(c).is_file():
            return c
    raise Fail("no 7-Zip found (pass --sevenzip)")


def parse_slt(text):
    """The file members of a `7z l -slt` listing, '/'-separated (folders left out)."""
    items, cur, started = [], {}, False
    for line in text.splitlines():
        if line.startswith("----------"):
            started = True
            continue
        if not started:
            continue
        if not line.strip():
            if cur:
                items.append(cur)
            cur = {}
            continue
        k, sep, v = line.partition(" = ")
        if sep:
            cur[k.strip()] = v
    if cur:
        items.append(cur)
    out = []
    for i in items:
        if "Path" not in i or i.get("Folder") == "+" or i.get("Attributes", "").startswith("D"):
            continue
        out.append(i["Path"].replace("\\", "/"))
    return out


def run_7z(sz, args):
    r = subprocess.run([sz, *args], capture_output=True)
    text = r.stdout.decode("utf-8", "replace")
    if r.returncode != 0:
        raise Fail(f"7-Zip {' '.join(args[:1])} failed ({r.returncode}): {r.stderr.decode('utf-8', 'replace').strip()}")
    return text


def unpack(path, arch, kind, dest, sevenzip=None, through_7z=None):
    """Read the artefact's members into `dest`. A zip goes through zipfile (each member to its own numbered folder, so
    that duplicates stay apart); an installer (or, in the self-test, any archive with through_7z) through 7-Zip."""
    a = Artefact(path, arch, kind)
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    if through_7z if through_7z is not None else kind == "installer":
        sz = find_sevenzip(sevenzip)
        a.listing = run_7z(sz, ["l", str(path)])
        a.listed = parse_slt(run_7z(sz, ["l", "-slt", "-sccUTF-8", str(path)]))
        run_7z(sz, ["x", "-y", "-aou", f"-o{dest}", str(path)])
        on_disk = sorted(p for p in dest.rglob("*") if p.is_file())
        a.files = [(p.relative_to(dest).as_posix(), p) for p in on_disk]
        if len(a.files) != len(a.listed):
            raise Fail(f"{a.name}: 7-Zip lists {len(a.listed)} files and unpacked {len(a.files)}")
    else:
        with zipfile.ZipFile(path) as z:
            for n, info in enumerate(i for i in z.infolist() if not i.is_dir()):
                a.listed.append(info.filename)
                f = dest / f"{n:05d}" / (PurePosixPath(info.filename).name or "unnamed")
                f.parent.mkdir()
                f.write_bytes(z.read(info))
                a.files.append((info.filename, f))
    return a


def notices(artefacts, repo):
    """(lines, errors) of the notices check over unpacked artefacts."""
    lines, errors = [], []
    want = {n: (Path(repo) / n).read_bytes() for n in NOTICE_FILES}
    for a in artefacts:
        counts = Counter(PurePosixPath(p).name for p in a.listed)
        for n in NOTICE_FILES:
            hits = [f for rel, f in a.files if PurePosixPath(rel).name == n]
            if counts[n] == 0:
                errors.append(f"{a.name}: no {n}")
            elif counts[n] > 1 or len(hits) > 1:
                errors.append(f"{a.name}: {n} {max(counts[n], len(hits))} times")
            elif len(hits) != 1:
                errors.append(f"{a.name}: {n} listed but not unpacked")
            elif hits[0].read_bytes() != want[n]:
                errors.append(f"{a.name}: {n} differs from the repository's")
            else:
                where = [p for p in a.listed if PurePosixPath(p).name == n][0]
                lines.append(f"{a.name}: {where}: once, byte-equal to the repository's {n}")
    return lines, errors


def read_unsigned(folder):
    """{name: sha256} of a build job's unsigned.sha256 (the six shipped names exactly)."""
    path = Path(folder) / "unsigned.sha256"
    out = {}
    for line in path.read_text(encoding="ascii").splitlines():
        m = SUM_RE.match(line.strip())
        if not m:
            raise Fail(f"{path}: not a sha256sum line: {line!r}")
        out[PurePosixPath(m[2].replace("\\", "/")).name] = m[1]
    if sorted(out) != sorted(SHIPPED):
        raise Fail(f"{path} lists {sorted(out)}, expected {sorted(SHIPPED)}")
    return out


def is_pe(path):
    with open(path, "rb") as f:
        head = f.read(0x40)
        if len(head) < 0x40 or head[:2] != b"MZ":
            return False
        off = int.from_bytes(head[0x3C:0x40], "little")
        f.seek(off)
        return f.read(4) == b"PE\0\0"


def classify(rel, path, arch, kind, unsigned):
    """(class, machine, sha256, problems) of the PE at `path`, listed as `rel` in an artefact of `arch` and `kind`."""
    data = Path(path).read_bytes()
    h = sha256_bytes(data)
    m = pesec.machine(path)
    base = PurePosixPath(rel).name
    problems = []
    if base in SHIPPED and base in HOLDS[kind]:
        cls = "shipped"
        if m != MACHINE[arch]:
            problems.append(f"machine {m:#06x}, not {MACHINE[arch]:#06x}")
        if DEP not in pesec.sections(path):
            problems.append(f"no {DEP} section")
        if h != unsigned[arch].get(base):
            problems.append(f"not the {arch} build's {base} ({unsigned[arch].get(base)})")
    elif kind == "installer" and rel.startswith("$PLUGINSDIR/"):
        cls = "nsis-plugin"
        if m != X86:
            problems.append(f"machine {m:#06x}, not {X86:#06x}")
    elif kind == "installer" and rel.lower() in UNINSTALLER:
        cls = "nsis-uninstaller"
        if m != X86:
            problems.append(f"machine {m:#06x}, not {X86:#06x}")
    else:
        cls = "unclassified"
        problems.append("fits no class")
    return cls, m, h, problems


def pes(artefacts, unsigned):
    """(lines, errors) of the PE classification over unpacked artefacts."""
    lines, errors, total = [], [], Counter()
    for a in artefacts:
        if a.listing:
            lines.append(f"--- 7-Zip's listing of {a.name} ---")
            lines += a.listing.rstrip().splitlines()
            lines.append(f"--- end of {a.name} ---")
        per = Counter()
        seen = Counter()
        for rel, f in a.files:
            if not is_pe(f):
                continue
            try:
                cls, m, h, problems = classify(rel, f, a.arch, a.kind, unsigned)
            except SystemExit as e:
                errors.append(f"{a.name}: {rel}: an unreadable PE: {e}")
                continue
            per[cls] += 1
            if cls == "shipped":
                seen[PurePosixPath(rel).name] += 1
            lines.append(f"{a.name}: {rel}: {cls}, machine {m:#06x}, sha256 {h}")
            errors += [f"{a.name}: {rel}: {p}" for p in problems]
        for n in sorted(HOLDS[a.kind]):
            if seen[n] != 1:
                errors.append(f"{a.name}: {n} {seen[n]} times, expected once")
        if per["nsis-uninstaller"] > 1:
            errors.append(f"{a.name}: {per['nsis-uninstaller']} uninstallers")
        lines.append(f"{a.name}: " + ", ".join(f"{c} {per[c]}" for c in sorted(per)) if per else f"{a.name}: no PE")
        total += per
    lines.append("all artefacts: " + ", ".join(f"{c} {total[c]}" for c in sorted(total)))
    return lines, errors


def report(lines, errors, what):
    for l in lines:
        print(l)
    for e in errors:
        print(f"error: {e}", file=sys.stderr)
    if errors:
        print(f"e7-artefacts {what}: {len(errors)} failures", file=sys.stderr)
        return 1
    print(f"e7-artefacts {what}: passed")
    return 0


def unpack_all(folder, work, sevenzip):
    return [unpack(p, arch, kind, Path(work) / f"{arch}-{kind}", sevenzip) for p, arch, kind in find_artefacts(folder)]


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--sevenzip")
    sub = ap.add_subparsers(dest="cmd")
    n = sub.add_parser("notices")
    n.add_argument("dir")
    n.add_argument("--repo", required=True)
    n.add_argument("--sevenzip", dest="sevenzip_sub")
    p = sub.add_parser("pes")
    p.add_argument("dir")
    p.add_argument("--unsigned-x64", required=True)
    p.add_argument("--unsigned-arm64", required=True)
    p.add_argument("--sevenzip", dest="sevenzip_sub")
    a = ap.parse_args(argv)
    sz = getattr(a, "sevenzip_sub", None) or a.sevenzip
    if a.self_test:
        return self_test(sz)
    if a.cmd is None:
        ap.error("a command (notices or pes) or --self-test")
    try:
        with tempfile.TemporaryDirectory() as work:
            arts = unpack_all(a.dir, work, sz)
            if a.cmd == "notices":
                return report(*notices(arts, a.repo), "notices")
            unsigned = {"x64": read_unsigned(a.unsigned_x64), "arm64": read_unsigned(a.unsigned_arm64)}
            return report(*pes(arts, unsigned), "pes")
    except (Fail, OSError) as e:
        print(f"e7-artefacts: {e}", file=sys.stderr)
        return 1


def self_test(sevenzip=None):
    mz = _load("make_zips", "make-zips.py")
    failures = []

    def ok(name, cond):
        print(f"{'ok  ' if cond else 'FAIL'} {name}")
        if not cond:
            failures.append(name)

    with tempfile.TemporaryDirectory() as d:
        d = Path(d)
        repo = d / "repo"
        repo.mkdir()
        for n in NOTICE_FILES:
            (repo / n).write_bytes(f"the text of {n}\n".encode())
        bins, unsigned = {}, {}
        for arch, mach in MACHINE.items():
            b = d / f"unsigned-{arch}"
            b.mkdir()
            lines = []
            for i, n in enumerate(SHIPPED):
                data = pesec.fake_pe([".text", ".rdata", DEP], arch=mach) + f"{arch} {n} {i}".encode()
                (b / n).write_bytes(data)
                lines.append(f"{sha256_bytes(data)} *{n}\n")
            (b / "unsigned.sha256").write_text("".join(lines), encoding="ascii", newline="\n")
            bins[arch] = b
            unsigned[arch] = read_unsigned(b)
        ok("unsigned.sha256 read", sorted(unsigned["x64"]) == sorted(SHIPPED))

        def zips(name):
            out = d / name
            for arch in MACHINE:
                mz.make(bins[arch], arch, "0.1.0", 1_700_000_000, out, repo)
            return out

        def rewrite(zpath, change):
            """Rewrite a zip with its members passed through change({name: bytes}) -> [(name, bytes)]."""
            with zipfile.ZipFile(zpath) as z:
                members = [(i.filename, z.read(i)) for i in z.infolist() if not i.is_dir()]
            with zipfile.ZipFile(zpath, "w", zipfile.ZIP_DEFLATED) as z, warnings.catch_warnings():
                warnings.simplefilter("ignore")  # the planted duplicates
                for name, data in change(members):
                    z.writestr(name, data)

        def zip_arts(folder):
            arts = []
            for p in sorted(folder.glob("*.zip")):
                m = ZIP_RE.match(p.name)
                arts.append(unpack(p, m["arch"], m["kind"], d / "work" / folder.name / p.stem))
            return arts

        good = zips("good")
        arts = zip_arts(good)
        lines, errors = notices(arts, repo)
        ok("notices: four zips x three files pass", errors == [] and len(lines) == 12)
        lines, errors = pes(arts, unsigned)
        ok("pes: the zips' PEs are all shipped", errors == [] and lines[-1] == "all artefacts: shipped 18")

        def planted(label, zipname, change, check, contains):
            folder = zips(label)
            rewrite(folder / zipname, change)
            lines, errors = check(zip_arts(folder))
            hit = [e for e in errors if contains in e]
            ok(f"{label}: fails ({contains})", bool(hit))
            return errors

        cli = "litepack-0.1.0-windows-x64-cli.zip"
        port = "litepack-0.1.0-windows-arm64-portable.zip"
        n_check = lambda arts: notices(arts, repo)
        p_check = lambda arts: pes(arts, unsigned)
        planted("notice-missing", cli, lambda ms: [m for m in ms if not m[0].endswith("LICENSE-MIT")], n_check,
                f"{cli}: no LICENSE-MIT")
        planted("notice-twice", cli, lambda ms: ms + [("LitePack/sub/LICENSE-APACHE", (repo / "LICENSE-APACHE")
                .read_bytes())], n_check, f"{cli}: LICENSE-APACHE 2 times")
        planted("notice-same-name-twice", port, lambda ms: ms + [m for m in ms if m[0].endswith("THIRD-PARTY-NOTICES")],
                n_check, f"{port}: THIRD-PARTY-NOTICES 2 times")

        def flip(ms, suffix):
            out = []
            for name, data in ms:
                if name.endswith(suffix):
                    data = data[:-1] + bytes([data[-1] ^ 1])
                out.append((name, data))
            return out

        planted("notice-changed", port, lambda ms: flip(ms, "THIRD-PARTY-NOTICES"), n_check,
                f"{port}: THIRD-PARTY-NOTICES differs")

        def swap(ms, suffix, data):
            return [(n, data if n.endswith(suffix) else b) for n, b in ms]

        x64_worker = (bins["x64"] / "lpk-worker.exe").read_bytes()
        errs = planted("pe-wrong-machine", port, lambda ms: swap(ms, "lpk-worker.exe", x64_worker), p_check,
                       "lpk-worker.exe: machine 0x8664, not 0xaa64")
        nodep = pesec.fake_pe([".text", ".rdata"], arch=0xAA64)
        planted("pe-no-dep", port, lambda ms: swap(ms, "lpk.exe", nodep), p_check, "lpk.exe: no .dep-v0 section")
        errs = planted("pe-changed-byte", cli, lambda ms: flip(ms, "lpk-check.exe"), p_check,
                       "lpk-check.exe: not the x64 build's lpk-check.exe")
        ok("pe-changed-byte: only that failure", len(errs) == 1)
        helper = pesec.fake_pe([".text", DEP], arch=0x8664)
        planted("pe-unknown-name", cli, lambda ms: ms + [("LitePack/helper.exe", helper)], p_check,
                "LitePack/helper.exe: fits no class")
        planted("pe-dll-in-portable", port, lambda ms: ms + [("LitePack/lpk_shell.dll",
                (bins["arm64"] / "lpk_shell.dll").read_bytes())], p_check, "lpk_shell.dll: fits no class")
        planted("pe-shipped-twice", cli, lambda ms: ms + [("LitePack/bin/lpk.exe", (bins["x64"] / "lpk.exe")
                .read_bytes())], p_check, "lpk.exe 2 times")
        planted("pe-shipped-missing", cli, lambda ms: [m for m in ms if not m[0].endswith("lpk-worker.exe")], p_check,
                "lpk-worker.exe 0 times")
        planted("pe-nsis-in-zip", cli, lambda ms: ms + [("$PLUGINSDIR/System.dll", pesec.fake_pe([".text"], arch=X86))],
                p_check, "$PLUGINSDIR/System.dll: fits no class")
        # An MZ file without a PE header is no PE (a README that happens to start with MZ, say).
        folder = zips("not-a-pe")
        rewrite(folder / cli, lambda ms: ms + [("LitePack/MZ.txt", b"MZ" + bytes(200))])
        ok("an MZ file without a PE header is not a PE", pes(zip_arts(folder), unsigned)[1] == [])

        # The installer's classes, on synthetic members (no installer is built here).
        f = d / "member"

        def cls(rel, data, kind="installer", arch="x64"):
            f.write_bytes(data)
            return classify(rel, f, arch, kind, unsigned)

        plugin = pesec.fake_pe([".text"], arch=X86)
        ok("installer: $PLUGINSDIR x86 DLL is an nsis-plugin", cls("$PLUGINSDIR/nsDialogs.dll", plugin)[::3] ==
           ("nsis-plugin", []))
        ok("installer: an x64 DLL under $PLUGINSDIR fails", cls("$PLUGINSDIR/x.dll", helper)[3] ==
           ["machine 0x8664, not 0x014c"])
        ok("installer: uninstall.exe at the root is the nsis-uninstaller", cls("uninstall.exe", plugin)[::3] ==
           ("nsis-uninstaller", []))
        ok("installer: $INSTDIR/Uninstall.exe is the nsis-uninstaller", cls("$INSTDIR/Uninstall.exe", plugin)[0] ==
           "nsis-uninstaller")
        ok("installer: an x64 uninstall.exe fails", cls("uninstall.exe", helper)[3] == ["machine 0x8664, not 0x014c"])
        ok("installer: uninstall.exe elsewhere fits no class", cls("bin/uninstall.exe", plugin)[0] == "unclassified")
        ok("installer: an x86 PE outside $PLUGINSDIR fits no class", cls("helper.exe", plugin)[0] == "unclassified")
        ok("installer: lpk_shell.dll under shell/<v>/ is shipped",
           cls("shell/0.1.0/lpk_shell.dll", (bins["x64"] / "lpk_shell.dll").read_bytes())[::3] == ("shipped", []))
        ok("portable: lpk_shell.dll fits no class",
           cls("LitePack/lpk_shell.dll", (bins["x64"] / "lpk_shell.dll").read_bytes(), "portable")[0] == "unclassified")

        # An installer-shaped artefact, its members written by hand: the six shipped PEs, the NSIS parts.
        def installer(extra):
            a = Artefact(d / "LitePack_0.1.0_x64-setup.exe", "x64", "installer")
            root = d / "work" / f"inst-{len(extra)}"
            members = [("lpk-gui.exe", "lpk-gui.exe")] + [(f"bin/{n}", n) for n in CLI_EXES]
            members.append(("shell/0.1.0/lpk_shell.dll", "lpk_shell.dll"))
            for rel, n in members:
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_bytes((bins["x64"] / n).read_bytes())
            for rel, data in [("$PLUGINSDIR/System.dll", plugin), ("uninstall.exe", plugin)] + extra:
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_bytes(data)
            a.files = [(q.relative_to(root).as_posix(), q) for q in sorted(root.rglob("*")) if q.is_file()]
            a.listed = [rel for rel, _ in a.files]
            a.listing = "a listing\n"
            return a

        lines, errors = pes([installer([])], unsigned)
        ok("installer: six shipped, one plugin, one uninstaller", errors == [] and lines[-1] ==
           "all artefacts: nsis-plugin 1, nsis-uninstaller 1, shipped 6")
        ok("installer: the listing is printed", "a listing" in lines)
        errors = pes([installer([("$INSTDIR/uninstall.exe", plugin)])], unsigned)[1]
        ok("installer: two uninstallers fail", any("2 uninstallers" in e for e in errors))

        # 7-Zip's -slt listing.
        slt = ("7-Zip 26.03\n\nListing archive: s.exe\n\n--\nPath = s.exe\nType = Nsis\n\n----------\n"
               "Path = $PLUGINSDIR\\System.dll\nSize = 1\n\nPath = bin\nFolder = +\n\n"
               "Path = bin\\lpk.exe\nSize = 2\nAttributes = A\n\nPath = d\nAttributes = D\n\n"
               "Path = uninstall.exe\nSize = 3\n")
        ok("parse_slt: files only, '/'-separated", parse_slt(slt) == ["$PLUGINSDIR/System.dll", "bin/lpk.exe",
                                                                     "uninstall.exe"])

        # The artefact finder.
        try:
            find_artefacts(good)
            ok("find_artefacts: no installers fails", False)
        except Fail as e:
            ok("find_artefacts: no installers fails", "0 x64 installer artefacts" in str(e))
        for arch in MACHINE:
            (good / f"LitePack_0.1.0_{arch}-setup.exe").write_bytes(b"MZ")
        (good / "SHA256SUMS").write_bytes(b"")
        ok("find_artefacts: the six", len(find_artefacts(good)) == 6)
        (good / "LitePack_0.2.0_x64-setup.exe").write_bytes(b"MZ")
        try:
            find_artefacts(good)
            ok("find_artefacts: two x64 installers fail", False)
        except Fail as e:
            ok("find_artefacts: two x64 installers fail", "2 x64 installer" in str(e))

        # The command line, on the zips alone: it names the missing installers and exits 1.
        r = subprocess.run([sys.executable, "-I", __file__, "notices", str(zips("cmd")), "--repo", str(repo)],
                           capture_output=True, text=True)
        ok("notices: exit 1 without the installers", r.returncode == 1 and "installer" in r.stderr
           and "Traceback" not in r.stderr)

        # The 7-Zip path, where a 7-Zip is at hand: it reads a zip as it reads an installer, so a zip read both ways
        # gives the same members and the same notices.
        try:
            sz = find_sevenzip(sevenzip)
        except Fail:
            sz = None
            print("skip the 7-Zip reader (no 7-Zip)")
        if sz:
            z = good / cli
            via = unpack(z, "x64", "cli", d / "work" / "7z", sz, through_7z=True)
            direct = unpack(z, "x64", "cli", d / "work" / "zf", through_7z=False)
            ok("7-Zip lists the zip's members as zipfile does", sorted(via.listed) == sorted(direct.listed))
            ok("7-Zip's listing is kept for the log", "LitePack" in via.listing)
            ok("notices through 7-Zip pass", notices([via], repo)[1] == [])
            ok("pes through 7-Zip pass", pes([via], unsigned)[1] == [])
            folder = zips("7z-twice")
            rewrite(folder / cli, lambda ms: ms + [m for m in ms if m[0].endswith("LICENSE-MIT")])
            twice = unpack(folder / cli, "x64", "cli", d / "work" / "7z2", sz, through_7z=True)
            ok("7-Zip: a member twice fails the notices", any("LICENSE-MIT 2 times" in e for e in
                                                               notices([twice], repo)[1]))

    if failures:
        print(f"e7-artefacts: self-test FAILED: {', '.join(failures)}")
        return 1
    print("e7-artefacts: self-test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
