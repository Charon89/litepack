"""E7-5 (CI's `installer` job): read a `sha256sum` output file, whatever mode wrote it (R334). Git Bash's
`sha256sum` writes `<hash> *<name>` (binary mode, the `*` marker), GNU's on Linux `<hash>  <name>`; both are read,
the marker stripped. A lookup that finds nothing, or more than one line, fails loudly, naming the file and the name,
so a step never stops on a silent `set -e` abort or compares an empty string.

    python -I scripts/e7-sha256.py <file> <name>    the hash of <name> (exit 1 with a message if absent or repeated)
    python -I scripts/e7-sha256.py --names <file>   the names, one per line, without the marker
    python -I scripts/e7-sha256.py --self-test
"""

import re
import subprocess
import sys
import tempfile
from pathlib import Path

LINE = re.compile(r"^\\?([0-9a-fA-F]{64}) [ *](.+)$")


def entries(path):
    """The (hash, name) pairs of a sha256sum file; any other non-empty line is an error."""
    out = []
    text = Path(path).read_text(encoding="utf-8")
    for n, line in enumerate(text.splitlines(), 1):
        line = line.rstrip("\r")
        if not line.strip():
            continue
        m = LINE.match(line)
        if not m:
            raise SystemExit(f"e7-sha256: {path} line {n} is not a sha256sum line: {line!r}")
        out.append((m.group(1).lower(), m.group(2)))
    if not out:
        raise SystemExit(f"e7-sha256: {path} holds no sha256sum line")
    return out


def lookup(path, name):
    hits = [h for h, n in entries(path) if n == name]
    if len(hits) != 1:
        raise SystemExit(f"e7-sha256: {path} has {len(hits)} lines for {name!r}, not one")
    return hits[0]


def self_test():
    a, b = "a" * 64, "B" * 64
    with tempfile.TemporaryDirectory() as d:
        bin_mode = Path(d, "bin.sha256")
        bin_mode.write_text(f"{a} *lpk.exe\n{b} *lpk-check.exe\n", encoding="utf-8")
        text_mode = Path(d, "text.sha256")
        text_mode.write_text(f"{a}  lpk.exe\r\n{b}  lpk-check.exe\r\n", encoding="utf-8")
        for f in (bin_mode, text_mode):
            assert lookup(f, "lpk.exe") == a, f
            assert lookup(f, "lpk-check.exe") == b.lower(), f
            assert [n for _, n in entries(f)] == ["lpk.exe", "lpk-check.exe"], f
            # What bash reads: LF line ends only (a CR would stay in `$(...)` and in `for name in $names`).
            run = [sys.executable, "-I", __file__]
            out = subprocess.run(run + [str(f), "lpk.exe"], capture_output=True, check=True).stdout
            assert out == f"{a}\n".encode(), out
            out = subprocess.run(run + ["--names", str(f)], capture_output=True, check=True).stdout
            assert out == b"lpk.exe\nlpk-check.exe\n", out
            miss = subprocess.run(run + [str(f), "lpk-gui.exe"], capture_output=True)
            assert miss.returncode == 1 and b"lpk-gui.exe" in miss.stderr, miss
        for f, name in ((bin_mode, "lpk-worker.exe"), (bin_mode, "*lpk.exe"), (bin_mode, "check.exe")):
            try:
                lookup(f, name)
            except SystemExit as e:
                assert str(f) in str(e) and repr(name) in str(e), e
            else:
                raise AssertionError(f"{name} found in {f}")
        twice = Path(d, "twice.sha256")
        twice.write_text(f"{a} *lpk.exe\n{b} *lpk.exe\n", encoding="utf-8")
        empty = Path(d, "empty.sha256")
        empty.write_text("\n", encoding="utf-8")
        bad = Path(d, "bad.sha256")
        bad.write_text("not a hash line\n", encoding="utf-8")
        for f in (twice, empty, bad):
            try:
                lookup(f, "lpk.exe")
            except SystemExit as e:
                assert str(f) in str(e), e
            else:
                raise AssertionError(f"{f} accepted")
    print("e7-sha256 self-test: ok")


def emit(text):
    """`text` and a bare LF: Python's text stdout on Windows writes CRLF, and bash's `$(...)` keeps the CR."""
    sys.stdout.buffer.write((text + "\n").encode("utf-8"))


def main(argv):
    if argv == ["--self-test"]:
        self_test()
    elif len(argv) == 2 and argv[0] == "--names":
        emit("\n".join(n for _, n in entries(argv[1])))
    elif len(argv) == 2 and not argv[0].startswith("--"):
        emit(lookup(argv[0], argv[1]))
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
