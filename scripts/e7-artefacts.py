"""E7-2 part 1c (release.yml's package and assemble jobs): what is inside the six release artefacts, the two NSIS
installers and the portable and CLI zip of each architecture (names from scripts/e7-assets.py's `expected`).

    python -I scripts/e7-artefacts.py notices <dir> --version <v> --repo <root> [--sevenzip <7z.exe>]
    python -I scripts/e7-artefacts.py pes <dir> --version <v> --unsigned-x64 <dir> --unsigned-arm64 <dir> \\
        --nsis-parts <file> [--sevenzip <7z.exe>]
    python -I scripts/e7-artefacts.py nsis-parts <NSIS folder>
    python -I scripts/e7-artefacts.py --self-test [--sevenzip <7z.exe>]

`notices` and `pes` first require `<dir>` to hold exactly e7-assets.py's expected names for `<v>` and SHA256SUMS,
nothing else.

`notices` (E7-8 part 2, the notices finder): each artefact holds THIRD-PARTY-NOTICES, LICENSE-APACHE and LICENSE-MIT
exactly once each, at its top folder (`LitePack/` in a zip, the root as 7-Zip lists it in an installer), byte-equal to
the repository's. Names are compared without regard to case or separator, so a second copy elsewhere, or under
another case, fails as a duplicate. One line per artefact and file.

`pes`: every PE inside each artefact (a file starting `MZ` whose `e_lfanew` points at a `PE\\0\\0` signature) is
classified, and anything that fits no class fails:
- `shipped`: one of the six shipped names (lpk.exe, lpk-check.exe, lpk-worker.exe, lpk-worker-libarchive.exe,
  lpk_shell.dll, lpk-gui.exe), at the artefact's machine (0x8664 x64, 0xAA64 arm64), carrying cargo-auditable's
  `.dep-v0` section, and byte-equal to that architecture's build (`unsigned.sha256` of the build job's artifact).
  Each artefact holds each shipped name it should exactly once (an installer all six, a portable zip all but
  lpk_shell.dll, a CLI zip the four command-line tools; scripts/make-zips.py).
- `nsis-plugin`: NSIS's own x86 parts (machine 0x14C) directly under `$PLUGINSDIR` in an installer, each name at most
  once, each byte-equal to a file of the same name (case aside) in `--nsis-parts`: the package job's list of every PE
  of the NSIS installation the bundler used (this script's `nsis-parts` command). Anything else under `$PLUGINSDIR`
  fits no class.
- `nsis-uninstaller`: the uninstaller NSIS writes at install time (WriteUninstaller), which 7-Zip shows as a file
  `uninstall.exe` at the installer's root and rebuilds from the installer's own stub: machine 0x14C, exactly one per
  installer. S is the offset of the installer's first header (the first 512-byte boundary holding the flags and then
  `EF BE AD DE` `NullsoftInst`), at least 0x200, and the installer's [0, S) must be an MZ PE. The uninstaller carries
  its own first header at S with FH_FLAGS_UNINSTALL (1) set in its flags (clear in the installer's), the installer's
  section table, and the installer's bytes [0, S) everywhere but inside `.rsrc` (its icon) and the PE CheckSum field
  (R391). Both flags words are printed, and per part of the stub (a section, the headers, the CheckSum) one line
  with its count of differing ranges and bytes and its first and last range; every range outside .rsrc and the
  CheckSum (each one a failure) is also printed in full (R393).
It prints each PE with its class, machine and SHA-256, a count per class, and 7-Zip's full listing of each installer
once.

`nsis-parts`: the `sha256sum` lines (`<hex>  <path>`) of every PE file under a folder, found by content, with its
path relative to the folder ('/'-separated), sorted by path; every line is kept, the same base name twice included.

The zips are read with zipfile (member names normalised, `\\` to `/`); the installers through 7-Zip (`7z l -slt` for
the member list, `7z x -aou` to unpack, so that a duplicated member survives under a new name and fails as an unknown
one). `--sevenzip` names 7z.exe; without it, 7z on PATH or `%ProgramFiles%\\7-Zip\\7z.exe`. The PE reading is
scripts/e7-pe-sections.py's. Each command exits 1 naming every failure. The standard library only.
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
assets = _load("e7_assets", "e7-assets.py")
mz = _load("make_zips", "make-zips.py")

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
PLUGINSDIR = "$PLUGINSDIR/"
# As 7-Zip may show it (with or without the $INSTDIR prefix), compared in lower case.
UNINSTALLER = ("uninstall.exe", "$instdir/uninstall.exe")
# NSIS's first header: int flags, then int siginfo 0xDEADBEEF (little-endian) and "NullsoftInst".
NSIS_MAGIC = b"\xef\xbe\xad\xdeNullsoftInst"
# The first header's flag of an uninstaller (NSIS's fileform.h).
FH_FLAGS_UNINSTALL = 1

INSTALLER_RE = re.compile(r"^.+_(?P<arch>x64|arm64)-setup\.exe$")
ZIP_RE = re.compile(r"^.+-windows-(?P<arch>x64|arm64)-(?P<kind>portable|cli)\.zip$")
SUM_RE = re.compile(r"^([0-9a-f]{64}) [ *](.+)$")


class Fail(Exception):
    pass


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def base(rel):
    """A member's base name, as compared: lower case."""
    return PurePosixPath(rel).name.lower()


class Artefact:
    """One artefact: its name, architecture, kind, and members as (listed path, file on disk)."""

    def __init__(self, path, arch, kind):
        self.path, self.arch, self.kind = Path(path), arch, kind
        self.name = self.path.name
        self.listed = []  # every listed file member, '/'-separated, in listing order
        self.files = []  # (path relative to the unpacked root or listed path, file on disk)
        self.listing = ""


def find_artefacts(folder, version):
    """The six artefacts in `folder` as (path, arch, kind). Fails unless `folder` holds exactly e7-assets.py's expected
    names for `version` and, optionally, SHA256SUMS."""
    try:
        names = assets.expected(version)
    except assets.AssetError as e:
        raise Fail(str(e))
    present = sorted(p.name for p in Path(folder).iterdir())
    errors = [f"missing: {n}" for n in names if n not in present]
    errors += [f"not an expected asset: {n}" for n in present if n not in names and n != assets.SUMS]
    if errors:
        raise Fail(f"{folder}: " + "; ".join(errors))
    out = []
    for n in names:
        m = INSTALLER_RE.match(n)
        if m:
            out.append((Path(folder) / n, m["arch"], "installer"))
        else:
            m = ZIP_RE.match(n)
            out.append((Path(folder) / n, m["arch"], m["kind"]))
    if sorted((a, k) for _, a, k in out) != sorted((a, k) for a in MACHINE for k in HOLDS):
        raise Fail(f"the expected names do not give one artefact of each kind and architecture: {names}")
    return out


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
    that duplicates stay apart; names with `\\` turned to `/`); an installer (or, in the self-test, any archive with
    through_7z) through 7-Zip."""
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
                rel = info.filename.replace("\\", "/")
                a.listed.append(rel)
                # A fixed name of its own: never a path built from the member's name (`C:evil`, `../x`).
                f = dest / f"{n:05d}" / "member"
                f.parent.mkdir()
                f.write_bytes(z.read(info))
                a.files.append((rel, f))
    return a


def top_of(a, name):
    """Where a notice must sit in artefact `a`: the zip's top folder, or the installer's root."""
    return name if a.kind == "installer" else f"{mz.PRODUCT}/{name}"


def notices(artefacts, repo):
    """(lines, errors) of the notices check over unpacked artefacts."""
    lines, errors = [], []
    want = {n: (Path(repo) / n).read_bytes() for n in NOTICE_FILES}
    for a in artefacts:
        counts = Counter(base(p) for p in a.listed)
        for n in NOTICE_FILES:
            hits = [(rel, f) for rel, f in a.files if base(rel) == n.lower()]
            count = max(counts[n.lower()], len(hits))
            if count == 0:
                errors.append(f"{a.name}: no {n}")
            elif count > 1:
                errors.append(f"{a.name}: {n} {count} times ({', '.join(rel for rel, _ in hits)})")
            elif not hits:
                errors.append(f"{a.name}: {n} is listed but no unpacked file matches it")
            elif hits[0][0] != top_of(a, n):
                errors.append(f"{a.name}: {n} is at {hits[0][0]}, not at the top ({top_of(a, n)})")
            elif hits[0][1].read_bytes() != want[n]:
                errors.append(f"{a.name}: {n} differs from the repository's")
            else:
                lines.append(f"{a.name}: {hits[0][0]}: once, at the top, byte-equal to the repository's {n}")
    return lines, errors


def read_sums(path):
    """[(sha256, '/'-separated path)] of a sha256sum file."""
    out = []
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        m = SUM_RE.match(line.rstrip("\r"))
        if not m:
            raise Fail(f"{path}: not a sha256sum line: {line!r}")
        out.append((m[1], m[2].replace("\\", "/")))
    return out


def read_unsigned(folder):
    """{name: sha256} of a build job's unsigned.sha256 (the six shipped names exactly)."""
    path = Path(folder) / "unsigned.sha256"
    out = {PurePosixPath(p).name: h for h, p in read_sums(path)}
    if sorted(out) != sorted(SHIPPED):
        raise Fail(f"{path} lists {sorted(out)}, expected {sorted(SHIPPED)}")
    return out


def read_nsis_parts(path):
    """{lower-case base name: {sha256}} of the package job's nsis-parts.sha256."""
    out = {}
    for h, p in read_sums(path):
        out.setdefault(base(p), set()).add(h)
    if not out:
        raise Fail(f"{path} lists no file")
    return out


def is_pe(path):
    with open(path, "rb") as f:
        head = f.read(0x40)
        if len(head) < 0x40 or head[:2] != b"MZ":
            return False
        off = int.from_bytes(head[0x3C:0x40], "little")
        f.seek(off)
        return f.read(4) == b"PE\0\0"


def nsis_parts_lines(folder):
    """The sha256sum lines of every PE under `folder`, by content, sorted by relative path."""
    folder = Path(folder)
    if not folder.is_dir():
        raise Fail(f"no folder {folder}")
    lines = []
    for p in sorted(folder.rglob("*"), key=lambda q: q.relative_to(folder).as_posix()):
        if p.is_file() and is_pe(p):
            lines.append(f"{sha256_bytes(p.read_bytes())}  {p.relative_to(folder).as_posix()}")
    if not lines:
        raise Fail(f"no PE under {folder}")
    return lines


def nsis_first_header(data):
    """S, the offset of the first 512-byte boundary holding NSIS's first header (flags, then the magic); None if
    there is none."""
    i = data.find(NSIS_MAGIC)
    while i != -1:
        if (i - 4) % 512 == 0 and i >= 4:
            return i - 4
        i = data.find(NSIS_MAGIC, i + 1)
    return None


def u16(data, off):
    return int.from_bytes(data[off : off + 2], "little")


def u32(data, off):
    return int.from_bytes(data[off : off + 4], "little")


def pe_layout(data):
    """(CheckSum offset, [(name, VirtualSize, VirtualAddress, SizeOfRawData, PointerToRawData)]) of the PE image at
    the start of `data`. Raises Fail when it is not one."""
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise Fail("no MZ header")
    pe = u32(data, 0x3C)
    if pe + 24 > len(data) or data[pe : pe + 4] != b"PE\0\0":
        raise Fail(f"no PE signature at {pe:#x}")
    count, opt_size = u16(data, pe + 6), u16(data, pe + 20)
    if opt_size < 68:
        raise Fail(f"an optional header of {opt_size} bytes, too short for its CheckSum")
    table = pe + 24 + opt_size
    if table + 40 * count > len(data):
        raise Fail("the section table runs past the stub")
    secs = []
    for i in range(count):
        e = table + 40 * i
        name = data[e : e + 8].rstrip(b"\0").decode("ascii", "replace")
        secs.append((name, u32(data, e + 8), u32(data, e + 12), u32(data, e + 16), u32(data, e + 20)))
    return pe + 24 + 64, secs


class Stub:
    """An NSIS installer's stub: S (its first header's offset), the bytes [0, S), its flags word and PE layout."""

    def __init__(self, data):
        s = nsis_first_header(data)
        if s is None:
            raise Fail("no NSIS first header on a 512-byte boundary")
        if s < 0x200:
            raise Fail(f"its first header is at {s:#x}, below 0x200")
        self.s, self.bytes, self.flags = s, data[:s], u32(data, s)
        try:
            self.checksum, self.sections = pe_layout(self.bytes)
        except Fail as e:
            raise Fail(f"its stub [0, {s:#x}) is not a PE: {e}")

    def where(self, off):
        """The part of the stub holding offset `off`."""
        if self.checksum <= off < self.checksum + 4:
            return "CheckSum"
        for name, _, _, size, ptr in self.sections:
            if ptr <= off < ptr + size:
                return name
        first = min((ptr for *_, size, ptr in self.sections if size), default=self.s)
        return "headers" if off < first else "outside any section"


def differing_ranges(a, b):
    """[(start, end)] of the runs where a and b (of one length) differ."""
    runs, start = [], None
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y and start is None:
            start = i
        elif x == y and start is not None:
            runs.append((start, i))
            start = None
    if start is not None:
        runs.append((start, len(a)))
    return runs


# The parts of the stub where the uninstaller may differ from its installer (R391).
ALLOWED_DIFF = (".rsrc", "CheckSum")


def by_part(ranges, stub):
    """[(part, [(start, end)])] of the differing ranges, each split where the stub's part changes, one entry per part
    in the order of its first range (R393: one summary line per part)."""
    parts = {}
    for start, end in ranges:
        b, cur = start, stub.where(start)
        for o in range(start + 1, end + 1):
            w = stub.where(o) if o < end else None
            if w != cur:
                parts.setdefault(cur, []).append((b, o))
                b, cur = o, w
    return list(parts.items())


def uninstaller_check(data, stub):
    """(notes, problems) of an uninstaller's bytes against its installer's Stub: its own first header at the same S
    with NSIS's FH_FLAGS_UNINSTALL set (clear in the installer's), the same section table, and [0, S) equal to the
    installer's apart from .rsrc and the CheckSum field."""
    s = stub.s
    notes, problems = [], []
    magic = data[s + 4 : s + 4 + len(NSIS_MAGIC)] == NSIS_MAGIC
    notes.append(f"its own first-header magic at {s:#x}: {'yes' if magic else 'no'}")
    if not magic:
        problems.append(f"no first-header magic at {s:#x}, where the installer's first header is")
    flags = u32(data, s) if len(data) >= s + 4 else None
    notes.append(f"flags: the installer's {stub.flags:#010x}, the uninstaller's "
                 + (f"{flags:#010x}" if flags is not None else "none"))
    if stub.flags & FH_FLAGS_UNINSTALL:
        problems.append("the installer's own flags carry FH_FLAGS_UNINSTALL")
    if flags is None or not flags & FH_FLAGS_UNINSTALL:
        problems.append("its flags do not carry FH_FLAGS_UNINSTALL")
    head = data[:s]
    try:
        _, sections = pe_layout(head)
    except Fail as e:
        problems.append(f"its [0, {s:#x}) is not a PE: {e}")
        return notes, problems
    if sections != stub.sections:
        problems.append(f"its section table differs from the installer's ({sections} against {stub.sections})")
        return notes, problems
    if len(head) < s:
        problems.append(f"shorter than the installer's stub ({len(head):#x} bytes, not {s:#x})")
        return notes, problems
    outside = []
    for part, runs in by_part(differing_ranges(head, stub.bytes), stub):
        n = sum(e - b for b, e in runs)
        notes.append(f"differs from the installer's stub in {part}: {len(runs)} range{'s' if len(runs) != 1 else ''}, "
                     f"{n} byte{'s' if n != 1 else ''}, first [{runs[0][0]:#x}, {runs[0][1]:#x}), "
                     f"last [{runs[-1][0]:#x}, {runs[-1][1]:#x})")
        if part not in ALLOWED_DIFF:
            outside.append(part)
            notes += [f"differs from the installer's stub at [{b:#x}, {e:#x}) in {part}" for b, e in runs]
    notes.append(f"[0, {s:#x}) equal to the installer's stub apart from .rsrc and CheckSum: "
                 + ("no" if outside else "yes"))
    if outside:
        problems.append(f"differs from the installer's stub outside .rsrc and CheckSum ({', '.join(sorted(set(outside)))})")
    return notes, problems


def classify(rel, path, arch, kind, unsigned, nsis_parts=None, stub=None):
    """(class, machine, sha256, problems, notes) of the PE at `path`, listed as `rel` in an artefact of `arch` and
    `kind`; `nsis_parts` from read_nsis_parts and `stub` the installer's Stub for an installer."""
    data = Path(path).read_bytes()
    h = sha256_bytes(data)
    m = pesec.machine(path)
    name = PurePosixPath(rel).name
    problems, notes = [], []
    if name in SHIPPED and name in HOLDS[kind]:
        cls = "shipped"
        if m != MACHINE[arch]:
            problems.append(f"machine {m:#06x}, not {MACHINE[arch]:#06x}")
        if DEP not in pesec.sections(path):
            problems.append(f"no {DEP} section")
        if h != unsigned[arch].get(name):
            problems.append(f"not the {arch} build's {name} ({unsigned[arch].get(name)})")
    elif kind == "installer" and rel.startswith(PLUGINSDIR):
        inner = rel[len(PLUGINSDIR):]
        if "/" in inner:
            cls = "unclassified"
            problems.append("fits no class (in a subfolder of $PLUGINSDIR)")
        elif not nsis_parts or base(inner) not in nsis_parts:
            cls = "unclassified"
            problems.append("fits no class (not a file of the NSIS installation)")
        else:
            cls = "nsis-plugin"
            if m != X86:
                problems.append(f"machine {m:#06x}, not {X86:#06x}")
            if h not in nsis_parts[base(inner)]:
                problems.append(f"not byte-equal to any {inner} of the NSIS installation")
    elif kind == "installer" and rel.lower() in UNINSTALLER:
        cls = "nsis-uninstaller"
        if m != X86:
            problems.append(f"machine {m:#06x}, not {X86:#06x}")
        if stub is None:
            problems.append("the installer has no usable NSIS stub to compare with")
        else:
            notes, more = uninstaller_check(data, stub)
            problems += more
    else:
        cls = "unclassified"
        problems.append("fits no class")
    return cls, m, h, problems, notes


def pes(artefacts, unsigned, nsis_parts=None):
    """(lines, errors) of the PE classification over unpacked artefacts."""
    lines, errors, total = [], [], Counter()
    for a in artefacts:
        if a.listing:
            lines.append(f"--- 7-Zip's listing of {a.name} ---")
            lines += a.listing.rstrip().splitlines()
            lines.append(f"--- end of {a.name} ---")
        stub = None
        if a.kind == "installer":
            try:
                stub = Stub(a.path.read_bytes())
                lines.append(f"{a.name}: first header at {stub.s:#x}, flags {stub.flags:#010x}, stub sections "
                             + ", ".join(f"{n} [{p:#x}, {p + z:#x})" for n, _, _, z, p in stub.sections))
            except (Fail, OSError) as e:
                errors.append(f"{a.name}: {e}")
        per, seen, plugins = Counter(), Counter(), Counter()
        for rel, f in a.files:
            if not is_pe(f):
                continue
            try:
                cls, m, h, problems, notes = classify(rel, f, a.arch, a.kind, unsigned, nsis_parts, stub)
            except SystemExit as e:
                errors.append(f"{a.name}: {rel}: an unreadable PE: {e}")
                continue
            per[cls] += 1
            if cls == "shipped":
                seen[PurePosixPath(rel).name] += 1
            if cls == "nsis-plugin":
                plugins[base(rel)] += 1
            lines.append(f"{a.name}: {rel}: {cls}, machine {m:#06x}, sha256 {h}")
            lines += [f"{a.name}: {rel}: {n}" for n in notes]
            errors += [f"{a.name}: {rel}: {p}" for p in problems]
        for n in sorted(HOLDS[a.kind]):
            if seen[n] != 1:
                errors.append(f"{a.name}: {n} {seen[n]} times, expected once")
        errors += [f"{a.name}: $PLUGINSDIR's {n} {c} times" for n, c in sorted(plugins.items()) if c > 1]
        if a.kind == "installer" and per["nsis-uninstaller"] != 1:
            errors.append(f"{a.name}: {per['nsis-uninstaller']} uninstallers, expected exactly one")
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


def unpack_all(folder, version, work, sevenzip):
    return [unpack(p, arch, kind, Path(work) / f"{arch}-{kind}", sevenzip)
            for p, arch, kind in find_artefacts(folder, version)]


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--sevenzip")
    sub = ap.add_subparsers(dest="cmd")
    n = sub.add_parser("notices")
    n.add_argument("dir")
    n.add_argument("--version", required=True)
    n.add_argument("--repo", required=True)
    n.add_argument("--sevenzip", dest="sevenzip_sub")
    p = sub.add_parser("pes")
    p.add_argument("dir")
    p.add_argument("--version", required=True)
    p.add_argument("--unsigned-x64", required=True)
    p.add_argument("--unsigned-arm64", required=True)
    p.add_argument("--nsis-parts", required=True)
    p.add_argument("--sevenzip", dest="sevenzip_sub")
    q = sub.add_parser("nsis-parts")
    q.add_argument("dir")
    a = ap.parse_args(argv)
    sz = getattr(a, "sevenzip_sub", None) or a.sevenzip
    if a.self_test:
        return self_test(sz)
    if a.cmd is None:
        ap.error("a command (notices, pes or nsis-parts) or --self-test")
    try:
        if a.cmd == "nsis-parts":
            print("\n".join(nsis_parts_lines(a.dir)))
            return 0
        with tempfile.TemporaryDirectory() as work:
            if a.cmd == "notices":
                return report(*notices(unpack_all(a.dir, a.version, work, sz), a.repo), "notices")
            unsigned = {"x64": read_unsigned(a.unsigned_x64), "arm64": read_unsigned(a.unsigned_arm64)}
            parts = read_nsis_parts(a.nsis_parts)
            return report(*pes(unpack_all(a.dir, a.version, work, sz), unsigned, parts), "pes")
    except (Fail, OSError) as e:
        print(f"e7-artefacts: {e}", file=sys.stderr)
        return 1


def self_test(sevenzip=None):
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
                    info = zipfile.ZipInfo(name)
                    info.filename = name  # as given: a planted `\` survives on every OS
                    info.compress_type = zipfile.ZIP_DEFLATED
                    z.writestr(info, data)

        def zip_arts(folder):
            arts = []
            for p in sorted(folder.glob("*.zip")):
                m = ZIP_RE.match(p.name)
                arts.append(unpack(p, m["arch"], m["kind"], d / "work" / folder.name / p.stem))
            return arts

        parts = {}
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
        p_check = lambda arts: pes(arts, unsigned, parts)
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
        # A member named like a drive or a parent path is written to a fixed name inside the work folder.
        folder = zips("odd-names")
        rewrite(folder / cli, lambda ms: ms + [("C:evil", b"x"), ("../outside", b"y"), ("LitePack/C:evil2", b"z")])
        work = d / "work" / "odd"
        odd = unpack(folder / cli, "x64", "cli", work)
        ok("zip members go to fixed names inside the work folder", all(
            q.name == "member" and q.resolve().is_relative_to(work.resolve()) for _, q in odd.files)
           and {"C:evil", "../outside", "LitePack/C:evil2"} <= set(odd.listed)
           and not (d / "outside").exists() and not (d / "work" / "outside").exists())
        # An MZ file without a PE header is no PE (a README that happens to start with MZ, say).
        folder = zips("not-a-pe")
        rewrite(folder / cli, lambda ms: ms + [("LitePack/MZ.txt", b"MZ" + bytes(200))])
        ok("an MZ file without a PE header is not a PE", pes(zip_arts(folder), unsigned, parts)[1] == [])

        # LOW-2 and LOW-3: another separator or case is the same name; a sole copy below the top folder fails.
        planted("notice-backslash-twice", cli, lambda ms: ms + [("LitePack\\LICENSE-MIT", (repo / "LICENSE-MIT")
                .read_bytes())], n_check, f"{cli}: LICENSE-MIT 2 times")
        planted("notice-case-twice", cli, lambda ms: ms + [("LitePack/license-mit", (repo / "LICENSE-MIT")
                .read_bytes())], n_check, f"{cli}: LICENSE-MIT 2 times")
        planted("notice-in-a-subfolder", port, lambda ms: [("LitePack/doc/LICENSE-MIT", b) if n.endswith("LICENSE-MIT")
                else (n, b) for n, b in ms], n_check, f"{port}: LICENSE-MIT is at LitePack/doc/LICENSE-MIT, not at")
        planted("notice-lower-case-alone", port, lambda ms: [("LitePack/license-mit", b) if n.endswith("LICENSE-MIT")
                else (n, b) for n, b in ms], n_check, f"{port}: LICENSE-MIT is at LitePack/license-mit")

        # The installer's classes, on synthetic members (no installer is built here).
        f = d / "member"
        plugin = pesec.fake_pe([".text"], arch=X86)
        dialogs = plugin + b"nsDialogs"
        # The NSIS installation's list, as `nsis-parts` writes it: the same base name twice (two builds) kept.
        nsis_dir = d / "NSIS"
        for rel, data in [("Plugins/x86-unicode/System.dll", plugin), ("Plugins/x86-ansi/System.dll", plugin + b"a"),
                          ("Plugins/x86-unicode/nsDialogs.dll", dialogs), ("makensis.exe", helper),
                          ("Docs/readme.txt", b"MZ not a PE" + bytes(100))]:
            (nsis_dir / rel).parent.mkdir(parents=True, exist_ok=True)
            (nsis_dir / rel).write_bytes(data)
        part_lines = nsis_parts_lines(nsis_dir)
        ok("nsis-parts: every PE by content, sorted, relative", [l.split("  ")[1] for l in part_lines] == [
            "Plugins/x86-ansi/System.dll", "Plugins/x86-unicode/System.dll", "Plugins/x86-unicode/nsDialogs.dll",
            "makensis.exe"])
        (d / "nsis-parts.sha256").write_text("\n".join(part_lines) + "\n", encoding="utf-8", newline="\n")
        parts = read_nsis_parts(d / "nsis-parts.sha256")
        ok("nsis-parts: both System.dll kept", len(parts["system.dll"]) == 2)

        # A synthetic installer: an x86 stub of 0x400 bytes with a .text [0x200, 0x300) and a .rsrc [0x300, 0x400)
        # (an unaligned magic inside its .text), then its first header with the flags clear.
        def make_stub(rsrc_size=0x100):
            pe, opt = 0x80, 240
            head = bytearray(0x400)
            head[:2] = b"MZ"
            head[0x3C:0x40] = pe.to_bytes(4, "little")
            head[pe : pe + 4] = b"PE\0\0"
            head[pe + 4 : pe + 24] = (X86.to_bytes(2, "little") + (2).to_bytes(2, "little") + bytes(12)
                                      + opt.to_bytes(2, "little") + (0x102).to_bytes(2, "little"))
            table = pe + 24 + opt
            for i, (name, ptr, size) in enumerate([(".text", 0x200, 0x100), (".rsrc", 0x300, rsrc_size)]):
                e = table + 40 * i
                head[e : e + 8] = name.encode().ljust(8, b"\0")
                head[e + 8 : e + 24] = b"".join(v.to_bytes(4, "little") for v in (size, 0x1000 * (i + 1), size, ptr))
            head[0x200:0x300] = bytes(range(256))
            head[0x2C0 : 0x2C0 + len(NSIS_MAGIC)] = NSIS_MAGIC
            head[0x300:0x400] = b"icon" * 64
            return bytes(head)

        def patch(data, off, new):
            return data[:off] + new + data[off + len(new):]

        stub = make_stub()
        setup_bytes = stub + b"\0\0\0\0" + NSIS_MAGIC + b"installer data"
        ok("the first header: the first 512-byte boundary with the magic", nsis_first_header(setup_bytes) == 0x400)
        ok("no first header: None", nsis_first_header(stub[:0x2C0]) is None)
        the_stub = Stub(setup_bytes)
        ok("the installer's stub: S, flags, sections", (the_stub.s, the_stub.flags, [s[0] for s in the_stub.sections])
           == (0x400, 0, [".text", ".rsrc"]))
        uninst = stub + b"\x03\0\0\0" + NSIS_MAGIC + b"uninstaller data"
        checksum = the_stub.checksum

        def cls(rel, data, kind="installer", arch="x64", s=the_stub):
            f.write_bytes(data)
            return classify(rel, f, arch, kind, unsigned, parts, s)

        def stub_fails(label, make, contains):
            try:
                Stub(make)
                ok(label, False)
            except Fail as e:
                ok(label, contains in str(e))

        stub_fails("an installer whose first header is below 0x200 fails", b"\0\0\0\0" + NSIS_MAGIC + bytes(0x400),
                   "below 0x200")
        stub_fails("an installer whose stub is not MZ fails", b"ZM" + stub[2:] + b"\0\0\0\0" + NSIS_MAGIC,
                   "not a PE: no MZ header")
        stub_fails("an installer without a first header fails", stub, "no NSIS first header")

        ok("installer: a listed $PLUGINSDIR x86 DLL is an nsis-plugin",
           cls("$PLUGINSDIR/nsDialogs.dll", dialogs)[::3] == ("nsis-plugin", []))
        ok("installer: either listed System.dll, any case", cls("$PLUGINSDIR/system.DLL", plugin + b"a")[::3] ==
           ("nsis-plugin", []))
        ok("installer: a plugin in a subfolder fits no class", cls("$PLUGINSDIR/x/System.dll", plugin)[::3] ==
           ("unclassified", ["fits no class (in a subfolder of $PLUGINSDIR)"]))
        ok("installer: a plugin name not listed fits no class", cls("$PLUGINSDIR/StartMenu.dll", plugin)[::3] ==
           ("unclassified", ["fits no class (not a file of the NSIS installation)"]))
        ok("installer: a plugin with one changed byte fails", cls("$PLUGINSDIR/nsDialogs.dll", dialogs[:-1] + b"t")[3]
           == ["not byte-equal to any nsDialogs.dll of the NSIS installation"])
        ok("installer: without the list, $PLUGINSDIR fits no class",
           classify("$PLUGINSDIR/System.dll", f, "x64", "installer", unsigned)[0] == "unclassified")
        ok("installer: this installer's uninstaller is the nsis-uninstaller", cls("uninstall.exe", uninst)[::3] ==
           ("nsis-uninstaller", []))
        ok("installer: the uninstaller's results are printed", cls("uninstall.exe", uninst)[4] == [
            "its own first-header magic at 0x400: yes", "flags: the installer's 0x00000000, the uninstaller's 0x00000003",
            "[0, 0x400) equal to the installer's stub apart from .rsrc and CheckSum: yes"])
        # R391: the icon patched into .rsrc, and the CheckSum, may differ; anything else may not.
        icon = patch(uninst, 0x310, b"ICON")
        r = cls("uninstall.exe", icon)
        ok("installer: a difference inside .rsrc passes", r[3] == [] and
           "differs from the installer's stub in .rsrc: 1 range, 4 bytes, first [0x310, 0x314), last [0x310, 0x314)"
           in r[4] and not any(" at [" in n for n in r[4]))
        r = cls("uninstall.exe", patch(icon, checksum, b"\x12\x34\x56\x78"))
        ok("installer: a different CheckSum passes", r[3] == [] and
           f"differs from the installer's stub in CheckSum: 1 range, 4 bytes, first [{checksum:#x}, {checksum + 4:#x}), "
           f"last [{checksum:#x}, {checksum + 4:#x})" in r[4])
        r = cls("uninstall.exe", patch(uninst, 0x250, b"\xff"))
        ok("installer: a difference in .text fails", r[3] == [
            "differs from the installer's stub outside .rsrc and CheckSum (.text)"] and
           "differs from the installer's stub at [0x250, 0x251) in .text" in r[4])
        # R393: many ranges in .rsrc, the CheckSum, and two in .text (one running on into .rsrc): one summary line per
        # part, each part named once, the .rsrc ranges not listed, the .text ones in full; the verdict line unchanged.
        many = patch(uninst, checksum, b"\x12\x34\x56\x78")
        for o in range(0x320, 0x3F0, 4):
            many = patch(many, o, b"I")
        many = patch(patch(many, 0x240, b"\xfe\xfe"), 0x2FE, bytes(4))
        r = cls("uninstall.exe", many)
        summaries = [n for n in r[4] if n.startswith("differs from the installer's stub in ")]
        named = [n.split(" in ", 1)[1].split(":", 1)[0] for n in summaries]
        ok("installer: the summary names each part once", sorted(named) == sorted({"CheckSum", ".text", ".rsrc"})
           and len(named) == len(set(named)))
        ok("installer: the .rsrc summary counts its ranges and bytes", "differs from the installer's stub in .rsrc: "
           "53 ranges, 54 bytes, first [0x300, 0x302), last [0x3ec, 0x3ed)" in r[4])
        ok("installer: only the ranges outside .rsrc and CheckSum are listed in full",
           [n for n in r[4] if " at [" in n] == ["differs from the installer's stub at [0x240, 0x242) in .text",
                                                 "differs from the installer's stub at [0x2fe, 0x300) in .text"])
        ok("installer: the verdict line stays", r[4][-1] ==
           "[0, 0x400) equal to the installer's stub apart from .rsrc and CheckSum: no" and r[3] == [
               "differs from the installer's stub outside .rsrc and CheckSum (.text)"])
        r = cls("uninstall.exe", patch(uninst, 0x100, b"\x01"))
        ok("installer: a difference in the headers fails", r[3] == [
            "differs from the installer's stub outside .rsrc and CheckSum (headers)"])
        r = cls("uninstall.exe", make_stub(rsrc_size=0xF0) + uninst[0x400:])
        ok("installer: a different section table fails", len(r[3]) == 1 and "section table differs" in r[3][0])
        r = cls("uninstall.exe", setup_bytes)
        ok("installer: the installer copied as its own uninstaller fails (flags)", r[3] == [
            "its flags do not carry FH_FLAGS_UNINSTALL"])
        r = cls("uninstall.exe", stub + bytes(64))
        ok("installer: an uninstaller without its own header fails", "no first-header magic at 0x400, where the "
           "installer's first header is" in r[3])
        r = cls("uninstall.exe", uninst, s=Stub(stub + b"\x01\0\0\0" + NSIS_MAGIC))
        ok("installer: an installer whose own flags say uninstaller fails",
           "the installer's own flags carry FH_FLAGS_UNINSTALL" in r[3])
        ok("installer: no installer stub fails the uninstaller", cls("uninstall.exe", uninst, s=None)[3] == [
            "the installer has no usable NSIS stub to compare with"])
        ok("installer: $INSTDIR/Uninstall.exe is the nsis-uninstaller", cls("$INSTDIR/Uninstall.exe", uninst)[0] ==
           "nsis-uninstaller")
        ok("installer: an x64 uninstall.exe fails", "machine 0x8664, not 0x014c" in cls("uninstall.exe", helper)[3])
        ok("installer: uninstall.exe elsewhere fits no class", cls("bin/uninstall.exe", uninst)[0] == "unclassified")
        ok("installer: an x86 PE outside $PLUGINSDIR fits no class", cls("helper.exe", plugin)[0] == "unclassified")
        ok("installer: lpk_shell.dll under shell/<v>/ is shipped",
           cls("shell/0.1.0/lpk_shell.dll", (bins["x64"] / "lpk_shell.dll").read_bytes())[::3] == ("shipped", []))
        ok("portable: lpk_shell.dll fits no class",
           cls("LitePack/lpk_shell.dll", (bins["x64"] / "lpk_shell.dll").read_bytes(), "portable")[0] == "unclassified")

        # An installer-shaped artefact, its members written by hand: the six shipped PEs, the NSIS parts.
        setup = d / "LitePack_0.1.0_x64-setup.exe"
        setup.write_bytes(setup_bytes)

        def installer(extra, tag, uninstaller=True):
            a = Artefact(setup, "x64", "installer")
            root = d / "work" / f"inst-{tag}"
            members = [("lpk-gui.exe", "lpk-gui.exe")] + [(f"bin/{n}", n) for n in CLI_EXES]
            members.append(("shell/0.1.0/lpk_shell.dll", "lpk_shell.dll"))
            for rel, n in members:
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_bytes((bins["x64"] / n).read_bytes())
            for rel, data in [("$PLUGINSDIR/System.dll", plugin), ("$PLUGINSDIR/nsDialogs.dll", dialogs)] + (
                    [("uninstall.exe", uninst)] if uninstaller else []) + extra:
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_bytes(data)
            a.files = [(q.relative_to(root).as_posix(), q) for q in sorted(root.rglob("*")) if q.is_file()]
            a.listed = [rel for rel, _ in a.files]
            a.listing = "a listing\n"
            return a

        lines, errors = pes([installer([], "good")], unsigned, parts)
        ok("installer: six shipped, two plugins, one uninstaller", errors == [] and lines[-1] ==
           "all artefacts: nsis-plugin 2, nsis-uninstaller 1, shipped 6")
        ok("installer: the listing and the first header are printed", "a listing" in lines and
           any(l.startswith(f"{setup.name}: first header at 0x400") for l in lines))
        errors = pes([installer([("$INSTDIR/uninstall.exe", uninst)], "two-uninst")], unsigned, parts)[1]
        ok("installer: two uninstallers fail", any("2 uninstallers, expected exactly one" in e for e in errors))
        errors = pes([installer([], "no-uninst", uninstaller=False)], unsigned, parts)[1]
        ok("installer: no uninstaller fails", any("0 uninstallers, expected exactly one" in e for e in errors))
        lines = pes([installer([], "good-2")], unsigned, parts)[0]
        ok("installer: the stub's facts are printed", any(l.startswith(f"{setup.name}: first header at 0x400, flags "
                                                               "0x00000000, stub sections .text [0x200, 0x300), .rsrc")
                                                          for l in lines))
        # b: a notice 7-Zip lists once that no unpacked file matches: a named failure, no IndexError.
        lost = installer([], "lost")
        lost.listed.append("LICENSE-MIT")
        ok("notices: listed but not unpacked fails by name", any("LICENSE-MIT is listed but no unpacked file matches"
                                                                 in e for e in notices([lost], repo)[1]))
        errors = pes([installer([("$PLUGINSDIR/SYSTEM.dll_1", plugin)], "renamed")], unsigned, parts)[1]
        ok("installer: a duplicate renamed by 7-Zip fits no class", any("SYSTEM.dll_1: fits no class" in e
                                                                         for e in errors))
        twice = installer([], "plugin-twice")
        twice.files.append(("$PLUGINSDIR/system.dll", twice.files[[r for r, _ in twice.files].index(
            "$PLUGINSDIR/System.dll")][1]))
        errors = pes([twice], unsigned, parts)[1]
        ok("installer: a plugin twice fails", any("$PLUGINSDIR's system.dll 2 times" in e for e in errors))

        # 7-Zip's -slt listing.
        slt = ("7-Zip 26.03\n\nListing archive: s.exe\n\n--\nPath = s.exe\nType = Nsis\n\n----------\n"
               "Path = $PLUGINSDIR\\System.dll\nSize = 1\n\nPath = bin\nFolder = +\n\n"
               "Path = bin\\lpk.exe\nSize = 2\nAttributes = A\n\nPath = d\nAttributes = D\n\n"
               "Path = uninstall.exe\nSize = 3\n")
        ok("parse_slt: files only, '/'-separated", parse_slt(slt) == ["$PLUGINSDIR/System.dll", "bin/lpk.exe",
                                                                     "uninstall.exe"])

        # The artefact finder: exactly e7-assets.py's names and SHA256SUMS.
        def finds(folder, version="0.1.0"):
            try:
                return len(find_artefacts(folder, version))
            except Fail as e:
                return str(e)

        ok("find_artefacts: no installers fails", "missing: LitePack_0.1.0_x64-setup.exe" in str(finds(good)))
        for arch in MACHINE:
            (good / f"LitePack_0.1.0_{arch}-setup.exe").write_bytes(b"MZ")
        (good / "SHA256SUMS").write_bytes(b"")
        ok("find_artefacts: the six and SHA256SUMS", finds(good) == 6)
        ok("find_artefacts: another version's names fail", "missing" in str(finds(good, "0.2.0")))
        (good / "notes.txt").write_bytes(b"x")
        ok("find_artefacts: an extra file fails", "not an expected asset: notes.txt" in str(finds(good)))
        (good / "notes.txt").unlink()
        (good / "LitePack_0.1.0_x64-setup.exe").rename(good / "Litepack_0.1.0_x64-setup.exe")
        msg = str(finds(good))
        ok("find_artefacts: a misnamed artefact fails", "missing: LitePack_0.1.0_x64-setup.exe" in msg and
           "not an expected asset: Litepack_0.1.0_x64-setup.exe" in msg)

        # The command line, on the zips alone: it names the missing installers and exits 1.
        r = subprocess.run([sys.executable, "-I", __file__, "notices", str(zips("cmd")), "--version", "0.1.0",
                            "--repo", str(repo)], capture_output=True, text=True)
        ok("notices: exit 1 without the installers", r.returncode == 1 and "missing: LitePack_0.1.0_arm64-setup.exe"
           in r.stderr and "Traceback" not in r.stderr)
        r = subprocess.run([sys.executable, "-I", __file__, "nsis-parts", str(nsis_dir)], capture_output=True,
                           text=True)
        ok("nsis-parts: the command prints the lines", r.returncode == 0 and r.stdout.splitlines() == part_lines)

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
