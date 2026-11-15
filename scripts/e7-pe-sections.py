"""E7-2 (scripts/repro-build.sh --auditable): read the section table of a Windows PE and check that a section is
there. cargo-auditable embeds the dependency list of a binary in a `.dep-v0` section; a shipped PE without it was
built without `cargo auditable`, so the build fails. The standard library alone: the DOS header's `e_lfanew`, the
`PE\\0\\0` signature, the COFF header's section count and optional-header size, then 40-byte section headers whose
first 8 bytes are the name (an image's names are at most 8 bytes, no string-table form).

    python -I scripts/e7-pe-sections.py --list <pe>                     the section names, one per line
    python -I scripts/e7-pe-sections.py --require <section> <pe>...     exit 1 naming each PE without the section
    python -I scripts/e7-pe-sections.py --self-test
"""

import struct
import subprocess
import sys
import tempfile
from pathlib import Path


def sections(path):
    """The section names of the PE at `path`; a file that is not a PE is an error (SystemExit)."""
    try:
        data = Path(path).read_bytes()
    except OSError as e:
        raise SystemExit(f"e7-pe-sections: cannot read {path}: {e.strerror or e}")
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise SystemExit(f"e7-pe-sections: {path} is not a PE (no MZ header)")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if pe + 24 > len(data) or data[pe : pe + 4] != b"PE\0\0":
        raise SystemExit(f"e7-pe-sections: {path} is not a PE (no PE signature at {pe:#x})")
    count, opt_size = struct.unpack_from("<H", data, pe + 6)[0], struct.unpack_from("<H", data, pe + 20)[0]
    table = pe + 24 + opt_size
    if table + 40 * count > len(data):
        raise SystemExit(f"e7-pe-sections: {path}: the section table runs past the end of the file")
    names = []
    for i in range(count):
        raw = data[table + 40 * i : table + 40 * i + 8]
        names.append(raw.rstrip(b"\0").decode("ascii", "replace"))
    return names


def missing(section, paths):
    """The paths whose PE has no section named `section`."""
    return [p for p in paths if section not in sections(p)]


def fake_pe(names, opt_size=240):
    """A minimal PE image: DOS header, signature, COFF header, an optional header of zeros, the section table."""
    pe = 0x80
    head = bytearray(pe)
    head[:2] = b"MZ"
    struct.pack_into("<I", head, 0x3C, pe)
    coff = struct.pack("<HHIIIHH", 0x8664, len(names), 0, 0, 0, opt_size, 0x22)
    table = b"".join(n.encode("ascii").ljust(8, b"\0") + bytes(32) for n in names)
    return bytes(head) + b"PE\0\0" + coff + bytes(opt_size) + table + bytes(64)


def self_test():
    with tempfile.TemporaryDirectory() as d:
        with_dep = Path(d, "with.exe")
        with_dep.write_bytes(fake_pe([".text", ".rdata", ".data", ".dep-v0", ".reloc"]))
        without = Path(d, "without.dll")
        without.write_bytes(fake_pe([".text", ".rdata", ".pdata"], opt_size=224))
        eight = Path(d, "eight.exe")
        eight.write_bytes(fake_pe(["12345678", ".dep-v0"]))
        not_pe = Path(d, "text.exe")
        not_pe.write_bytes(b"MZ" + bytes(100))
        assert sections(with_dep) == [".text", ".rdata", ".data", ".dep-v0", ".reloc"]
        assert sections(without) == [".text", ".rdata", ".pdata"]
        assert sections(eight) == ["12345678", ".dep-v0"]
        assert missing(".dep-v0", [with_dep, without, eight]) == [without]
        try:
            sections(not_pe)
        except SystemExit as e:
            assert "no PE signature" in str(e), e
        else:
            raise AssertionError("a file without the PE signature was read")
        # The command line: exit codes and what it prints.
        run = [sys.executable, "-I", __file__]
        ok = subprocess.run(run + ["--require", ".dep-v0", str(with_dep), str(eight)], capture_output=True)
        assert ok.returncode == 0, ok
        bad = subprocess.run(run + ["--require", ".dep-v0", str(with_dep), str(without)], capture_output=True)
        assert bad.returncode == 1 and b"without.dll" in bad.stderr and b"with.exe" not in bad.stderr, bad
        out = subprocess.run(run + ["--list", str(without)], capture_output=True, check=True).stdout
        assert out.replace(b"\r\n", b"\n") == b".text\n.rdata\n.pdata\n", out
        # A valid header whose section table runs past the end of the data: refused with the message, no traceback.
        truncated = Path(d, "truncated.exe")
        truncated.write_bytes(fake_pe([".text", ".rdata", ".dep-v0"])[: 0x80 + 24 + 240 + 40 * 2])
        try:
            sections(truncated)
        except SystemExit as e:
            assert "runs past the end" in str(e), e
        else:
            raise AssertionError("a truncated section table was read")
        cut = subprocess.run(run + ["--list", str(truncated)], capture_output=True)
        assert cut.returncode != 0 and b"runs past the end" in cut.stderr and b"Traceback" not in cut.stderr, cut
        # A path that does not exist: a non-zero exit with a message, no traceback.
        gone = subprocess.run(run + ["--require", ".dep-v0", str(Path(d, "absent.exe"))], capture_output=True)
        assert gone.returncode != 0 and b"cannot read" in gone.stderr and b"Traceback" not in gone.stderr, gone
    print("e7-pe-sections: self-test passed")


def main(argv):
    if argv == ["--self-test"]:
        self_test()
        return 0
    if len(argv) == 2 and argv[0] == "--list":
        for name in sections(argv[1]):
            print(name)
        return 0
    if len(argv) >= 3 and argv[0] == "--require":
        section, paths = argv[1], argv[2:]
        absent = missing(section, paths)
        for p in paths:
            if p not in absent:
                print(f"{p}: carries {section}")
        for p in absent:
            print(f"error: {p} carries no {section} section (sections: {' '.join(sections(p))})", file=sys.stderr)
        return 1 if absent else 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
