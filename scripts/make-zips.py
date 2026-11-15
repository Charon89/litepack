"""E7-6: the portable zip and the CLI zip (docs/notes/e7-design.md section 5, docs/RELEASING.md).

    python -I scripts/make-zips.py --bin <folder of the built .exe files> --arch x64|arm64 \
        --version <version> --timestamp <seconds since 1970, e.g. git log -1 --format=%ct> --out <folder>
    python -I scripts/make-zips.py --self-test

Writes litepack-<version>-windows-<arch>-portable.zip (a top folder LitePack\ with lpk-gui.exe, the
command-line tools, the workers, the empty `portable` marker, README.txt, THIRD-PARTY-NOTICES and the
licences; never lpk_shell.dll, since a portable copy registers no shell, R292) and
litepack-<version>-windows-<arch>-cli.zip (the same without lpk-gui.exe and the marker, with its own
README.txt). Both are deterministic (R293): sorted entries, one timestamp for all of them, fixed
attributes, Deflate at level 9. Every input is checked before anything is written, and each zip is
written to a temporary name and renamed, so a missing input leaves no zip, partial or whole.
"""

import argparse
import os
import re
import sys
import tempfile
import time
import unittest
import unittest.mock
import zipfile
from pathlib import Path

# The product's name, in one place (E7-1 may change it): the zips' top folder and their file names.
PRODUCT = "LitePack"

ARCHES = ("x64", "arm64")

# The executables, from --bin.
GUI = "lpk-gui.exe"
CLI_EXES = ("lpk.exe", "lpk-check.exe", "lpk-worker.exe", "lpk-worker-libarchive.exe")
# The repository's files, from --repo.
REPO_FILES = ("THIRD-PARTY-NOTICES", "LICENSE-APACHE", "LICENSE-MIT")
# The marker that makes the GUI keep its settings and state in data\ (crates/lpk-gui/src/paths.rs).
MARKER = "portable"
README = "README.txt"

# MS-DOS attributes, fixed: a folder, and a file with the archive bit.
DIR_ATTR = 0x10
FILE_ATTR = 0x20

PORTABLE_README = f"""{PRODUCT} (portable copy)

This folder is a complete copy of {PRODUCT} that runs from wherever you put it: a USB stick, a
shared folder, your desktop. Nothing is installed.

- Start lpk-gui.exe for the window, or use lpk.exe from a command prompt.
- Everything the window keeps (its settings, its recent archives, the browser engine's files and
  the record of interrupted writes) goes into the folder data\\ beside lpk-gui.exe. The file named
  "portable" is what tells {PRODUCT} to do that; delete it and this copy behaves like an installed
  one. If data\\ cannot be created or written, {PRODUCT} says so and does not start. If data\\
  is a junction or a link to another folder, {PRODUCT} follows it and keeps everything there.
- This copy registers nothing in Explorer: no context menu, no file types, no "Open with" entry.
  The installer does that.
- The update check only tells you that a newer release exists; it never installs anything.

One thing is kept outside this folder. The first time you open an archive that is not a {PRODUCT}
archive (zip, 7z, rar, tar and so on), Windows creates a sandbox profile named LitePack.Worker for
the program that reads it, so that a hostile archive cannot reach your files. To remove the
profile, press "Remove the sandbox profile" in the window's About box (Settings, then About
{PRODUCT}), or run this in this folder:

    lpk-worker.exe --remove-profile

To remove {PRODUCT} completely, run that command, then delete this folder.

Windows and the WebView2 runtime (the browser engine the window is drawn with, part of Windows)
keep a few records of their own, as they do for any program you run: for example the program's
name in Windows' display cache, and the runtime's own settings. While the window starts,
Windows' account broker may also refresh its own sign-in records, most likely for the WebView2
runtime. {PRODUCT}'s own data stays in data\\.

The licences are in LICENSE-APACHE and LICENSE-MIT; the licences of the third-party code inside
are in THIRD-PARTY-NOTICES.
"""

CLI_README = f"""{PRODUCT} command-line tools

lpk.exe creates, lists, tests, extracts and repairs {PRODUCT} archives, and reads other archives
(zip, 7z, rar, tar and so on); lpk-check.exe verifies archives. Run "lpk.exe --help" for the
commands. Nothing is installed: put the folder anywhere, and add it to PATH if you like.

- The tools keep no settings and write nothing but the files you ask for.
- They make no network request.

One thing is kept outside this folder. The first time lpk.exe opens an archive that is not a
{PRODUCT} archive, Windows creates a sandbox profile named LitePack.Worker for the program that
reads it (lpk-worker.exe), so that a hostile archive cannot reach your files. To remove the
profile, run this in this folder:

    lpk-worker.exe --remove-profile

The licences are in LICENSE-APACHE and LICENSE-MIT; the licences of the third-party code inside
are in THIRD-PARTY-NOTICES.
"""


class InputError(Exception):
    """A missing or unusable input; nothing was written."""


def zip_names(version, arch):
    """The two zips' file names."""
    stem = f"{PRODUCT.lower()}-{version}-windows-{arch}"
    return f"{stem}-portable.zip", f"{stem}-cli.zip"


def crlf(text):
    """A README with Windows line ends."""
    return text.replace("\r\n", "\n").replace("\n", "\r\n").encode("utf-8")


def contents(bin_dir, repo):
    """The two zips' contents: {name inside the top folder: bytes}. Raises InputError, naming every
    missing input, before anything is written."""
    bin_dir, repo = Path(bin_dir), Path(repo)
    sources = {n: bin_dir / n for n in (GUI,) + CLI_EXES}
    sources.update({n: repo / n for n in REPO_FILES})
    missing = [str(p) for p in sources.values() if not p.is_file()]
    if missing:
        raise InputError("missing: " + ", ".join(missing))
    data = {n: p.read_bytes() for n, p in sources.items()}
    shared = {n: data[n] for n in CLI_EXES + REPO_FILES}
    portable = dict(shared)
    portable[GUI] = data[GUI]
    portable[MARKER] = b""
    portable[README] = crlf(PORTABLE_README)
    cli = dict(shared)
    cli[README] = crlf(CLI_README)
    return portable, cli


def zip_time(timestamp):
    """The entries' timestamp (UTC), clamped to what ZIP can hold."""
    t = time.gmtime(int(timestamp))[:6]
    return max(t, (1980, 1, 1, 0, 0, 0))


def write_zip(path, files, timestamp):
    """Write `files` under the top folder PRODUCT to `path`, deterministically."""
    when = zip_time(timestamp)

    def info(name, attr):
        i = zipfile.ZipInfo(name, date_time=when)
        i.create_system = 0
        i.external_attr = attr
        i.compress_type = zipfile.ZIP_DEFLATED
        return i

    with zipfile.ZipFile(path, "w", compresslevel=9) as z:
        top = info(PRODUCT + "/", DIR_ATTR)
        top.compress_type = zipfile.ZIP_STORED
        z.writestr(top, b"")
        for name in sorted(files):
            z.writestr(info(f"{PRODUCT}/{name}", FILE_ATTR), files[name])


def temp_beside(path, suffix):
    """A fresh temporary name in `path`'s folder."""
    fd, tmp = tempfile.mkstemp(prefix=path.name + ".", suffix=suffix, dir=path.parent)
    os.close(fd)
    return Path(tmp)


def publish(pairs):
    """Rename each (temporary, target) pair into place as one set: the old targets are moved
    aside first and put back if any rename fails, so the folder holds either the old pair or the
    new one, never one of each."""
    backups = []
    placed = []
    try:
        for _, target in pairs:
            if target.exists():
                b = temp_beside(target, ".old")
                try:
                    os.replace(target, b)
                except BaseException:
                    b.unlink(missing_ok=True)
                    raise
                backups.append((b, target))
        for tmp, target in pairs:
            os.replace(tmp, target)
            placed.append(target)
    except BaseException:
        for target in placed:
            target.unlink(missing_ok=True)
        for b, target in backups:
            os.replace(b, target)
        raise
    for b, _ in backups:
        b.unlink(missing_ok=True)


def make(bin_dir, arch, version, timestamp, out, repo):
    """Write both zips into `out`; their paths. Both are written to temporary names first, then
    renamed together."""
    if arch not in ARCHES:
        raise InputError(f"unknown architecture {arch!r} (one of {', '.join(ARCHES)})")
    if not re.fullmatch(r"[0-9A-Za-z][0-9A-Za-z.+-]*", version):
        raise InputError(f"unusable version {version!r}")
    portable, cli = contents(bin_dir, repo)
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    targets = [out / n for n in zip_names(version, arch)]
    temps = []
    try:
        for target, files in zip(targets, (portable, cli)):
            tmp = temp_beside(target, ".tmp")
            temps.append(tmp)
            write_zip(tmp, files, timestamp)
        publish(list(zip(temps, targets)))
    finally:
        for tmp in temps:
            tmp.unlink(missing_ok=True)
    return targets


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--bin", help="the folder of the built executables")
    ap.add_argument("--arch", choices=ARCHES)
    ap.add_argument("--version")
    ap.add_argument("--timestamp", type=int, help="the entries' time, seconds since 1970 (UTC)")
    ap.add_argument("--out", help="the folder the zips go to")
    ap.add_argument("--repo", default=str(Path(__file__).resolve().parent.parent),
                    help="the repository root (notices and licences)")
    a = ap.parse_args(argv)
    if a.self_test:
        r = unittest.main(argv=[sys.argv[0]], module=__name__, exit=False, verbosity=2).result
        return 0 if r.wasSuccessful() else 1
    if None in (a.bin, a.arch, a.version, a.timestamp, a.out):
        ap.error("--bin, --arch, --version, --timestamp and --out are required")
    try:
        for p in make(a.bin, a.arch, a.version, a.timestamp, a.out, a.repo):
            print(f"{p} ({p.stat().st_size} bytes)")
    except InputError as e:
        print(f"make-zips: {e}", file=sys.stderr)
        return 1
    return 0


class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.bin = root / "bin"
        self.repo = root / "repo"
        self.bin.mkdir()
        self.repo.mkdir()
        for n in (GUI,) + CLI_EXES + ("lpk_shell.dll",):
            (self.bin / n).write_bytes(b"MZ dummy " + n.encode())
        for n in REPO_FILES:
            (self.repo / n).write_bytes(b"text of " + n.encode())

    def tearDown(self):
        self.tmp.cleanup()

    def build(self, out, ts=1_700_000_000):
        return make(self.bin, "x64", "0.1.0", ts, Path(self.tmp.name) / out, self.repo)

    def names(self, path):
        with zipfile.ZipFile(path) as z:
            return z.namelist()

    def test_two_builds_are_byte_identical(self):
        first = self.build("a")
        # Different file times on disk must not matter.
        for p in self.bin.iterdir():
            os.utime(p, (1_000_000_000, 1_000_000_000))
        second = self.build("b")
        for x, y in zip(first, second):
            self.assertEqual(x.name, y.name)
            self.assertEqual(x.read_bytes(), y.read_bytes())

    def test_the_names_follow_version_and_arch(self):
        p, c = self.build("o")
        self.assertEqual(p.name, "litepack-0.1.0-windows-x64-portable.zip")
        self.assertEqual(c.name, "litepack-0.1.0-windows-x64-cli.zip")

    def test_the_portable_zip_holds_the_gui_and_the_marker_but_no_shell_dll(self):
        p, _ = self.build("o")
        expected = ["LitePack/"] + sorted(
            f"LitePack/{n}" for n in (GUI, MARKER, README) + CLI_EXES + REPO_FILES)
        self.assertEqual(self.names(p), expected)
        with zipfile.ZipFile(p) as z:
            self.assertEqual(z.read("LitePack/portable"), b"")
            self.assertIn(b"data\\", z.read("LitePack/README.txt"))
            self.assertIn(b"--remove-profile", z.read("LitePack/README.txt"))
            self.assertIn(b"Remove the sandbox profile", z.read("LitePack/README.txt"))

    def test_the_cli_zip_has_neither_the_gui_nor_the_marker(self):
        _, c = self.build("o")
        expected = ["LitePack/"] + sorted(
            f"LitePack/{n}" for n in (README,) + CLI_EXES + REPO_FILES)
        self.assertEqual(self.names(c), expected)
        with zipfile.ZipFile(c) as z:
            readme = z.read("LitePack/README.txt")
            self.assertIn(b"no network", readme)
            self.assertIn(b"--remove-profile", readme)
            self.assertNotEqual(readme, crlf(PORTABLE_README))

    def test_every_entry_has_the_one_timestamp_and_fixed_attributes(self):
        p, c = self.build("o", ts=1_762_560_000)
        for path in (p, c):
            with zipfile.ZipFile(path) as z:
                for i in z.infolist():
                    self.assertEqual(i.date_time, zip_time(1_762_560_000))
                    self.assertEqual(i.create_system, 0)
                    self.assertEqual(i.external_attr, DIR_ATTR if i.is_dir() else FILE_ATTR)

    def test_a_missing_input_writes_no_zip(self):
        (self.bin / "lpk-worker.exe").unlink()
        (self.repo / "LICENSE-MIT").unlink()
        out = Path(self.tmp.name) / "o"
        with self.assertRaises(InputError) as e:
            self.build("o")
        self.assertIn("lpk-worker.exe", str(e.exception))
        self.assertIn("LICENSE-MIT", str(e.exception))
        self.assertEqual(list(out.iterdir()) if out.exists() else [], [])

    def test_a_failed_rename_keeps_the_old_pair(self):
        old = [p.read_bytes() for p in self.build("o", ts=1_000_000_000)]
        real = os.replace
        calls = []

        def flaky(src, dst):
            calls.append((src, dst))
            # The fourth rename puts the second new zip in place (two backups, then two zips).
            if len(calls) == 4:
                raise OSError("simulated")
            return real(src, dst)

        with unittest.mock.patch.object(os, "replace", flaky):
            with self.assertRaises(OSError):
                self.build("o")
        out = Path(self.tmp.name) / "o"
        names = zip_names("0.1.0", "x64")
        self.assertEqual(sorted(p.name for p in out.iterdir()), sorted(names))
        self.assertEqual([(out / n).read_bytes() for n in names], old)

    def test_a_failed_second_write_keeps_the_old_pair(self):
        old = [p.read_bytes() for p in self.build("o", ts=1_000_000_000)]
        real = write_zip
        calls = []

        def flaky(path, files, ts):
            calls.append(path)
            if len(calls) == 2:
                raise OSError("disk full")
            return real(path, files, ts)

        with unittest.mock.patch(f"{__name__}.write_zip", flaky):
            with self.assertRaises(OSError):
                self.build("o")
        out = Path(self.tmp.name) / "o"
        names = zip_names("0.1.0", "x64")
        self.assertEqual(sorted(p.name for p in out.iterdir()), sorted(names))
        self.assertEqual([(out / n).read_bytes() for n in names], old)

    def test_bad_arch_and_version_are_refused(self):
        with self.assertRaises(InputError):
            make(self.bin, "x86", "0.1.0", 0, Path(self.tmp.name) / "o", self.repo)
        with self.assertRaises(InputError):
            make(self.bin, "x64", "../x", 0, Path(self.tmp.name) / "o", self.repo)

    def test_an_early_timestamp_is_clamped(self):
        self.assertEqual(zip_time(0), (1980, 1, 1, 0, 0, 0))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
