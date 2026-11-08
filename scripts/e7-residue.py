"""E7-6's residue check (R291; docs/notes/e7-design.md sections 3 and 5; E7-5 extends it).

    python -I scripts/e7-residue.py snapshot <out.json> [--on-ci]
    python -I scripts/e7-residue.py diff <before.json> <after.json>
        [--allow-worker-profile | --require-worker-profile]
    python -I scripts/e7-residue.py --self-test

`snapshot` refuses unless GITHUB_ACTIONS=true or --on-ci is given: use it only on a disposable
machine. Every path it cannot read is printed and recorded; `diff` reports such a path as
residue (`?`) even when both snapshots record it alike. --require-worker-profile allows the
profile and fails unless both its folder and its Mappings key were added.

`snapshot` records every file and folder under %APPDATA% and %LOCALAPPDATA% (path, size and
modification time; a folder by its path alone, a link or junction by its path, never followed)
and the keys and values of HKCU\\Software (with Classes) and HKCU\\Environment, through
`reg.exe export` into the output's folder, parsed from UTF-16. Windows only; it holds the
profile's registry, so it is run on CI's runner alone (R294), never on a developer's PC.

`diff` prints every added (+), removed (-) or changed (~) entry. An entry is allowed when the
allow-list below names it (Windows' own bookkeeping, one folder, key or value per entry, each
with its reason), or, with --allow-worker-profile, when it belongs to the LitePack.Worker
AppContainer profile (matched by its moniker and the SID its Mappings key names, never by a
pattern over every AppContainer). Allowed entries are printed with their reason; the exit code
is 1 if any entry is not allowed.
"""

import argparse
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
import unittest.mock
from pathlib import Path

# The worker's AppContainer profile (crates/lpk-sandbox-sys: APPCONTAINER_NAME).
WORKER_MONIKER = "LitePack.Worker"

REPARSE_POINT = stat.FILE_ATTRIBUTE_REPARSE_POINT

FILE_ROOTS = ("APPDATA", "LOCALAPPDATA")
# HKCU\Software\Classes is the user's classes hive (UsrClass.dat) linked into HKCU\Software: it is
# exported on its own as well, and `snapshot` refuses unless its `Local Settings` key (where the
# AppContainer profiles are mapped) is in the result.
REG_ROOTS = (r"HKCU\Software", r"HKCU\Software\Classes", r"HKCU\Environment")
LOCAL_SETTINGS = r"HKCU\Software\Classes\Local Settings"
APPCONTAINER = LOCAL_SETTINGS + r"\Software\Microsoft\Windows\CurrentVersion\AppContainer"
MAPPINGS = APPCONTAINER + r"\Mappings"
STORAGE = APPCONTAINER + r"\Storage"

# Windows' own AppContainer bookkeeping (R301): after `DeleteAppContainerProfile`, Windows may keep
# these parent keys; Windows decides their lifetime, not LitePack. Allowed only when empty in the
# later snapshot (no value, no subkey); any value or subkey left under one is residue.
APPCONTAINER_REASON = ("Windows' own AppContainer bookkeeping: an empty parent key whose lifetime "
                       "DeleteAppContainerProfile decides (R301)")

# Windows' own bookkeeping, which running any program changes. One entry per folder (`file`: the
# folder or file and everything under it), key (`key`: the key and everything under it), value
# (`value`: a key and one value name) or empty key (`empty-key`: that key alone, added and holding
# nothing). Nothing LitePack writes may be listed here: that is fixed instead. Each entry's reason
# is printed beside every entry it allows.
ALLOW = [
    {"kind": "empty-key", "path": APPCONTAINER, "reason": APPCONTAINER_REASON},
    {"kind": "empty-key", "path": MAPPINGS, "reason": APPCONTAINER_REASON},
    {"kind": "empty-key", "path": STORAGE, "reason": APPCONTAINER_REASON},
]

# The environment variable that marks a disposable CI runner (GitHub Actions sets it).
CI_ENV = "GITHUB_ACTIONS"


def norm(s):
    """Windows' comparison of names: case-insensitive."""
    return s.casefold()


def under(path, root):
    """Whether `path` is `root` or lies under it (backslash-separated, case-insensitive)."""
    p, r = norm(path), norm(root).rstrip("\\")
    return p == r or p.startswith(r + "\\")


# --- snapshot --------------------------------------------------------------------------------


def long_path(p):
    """`p` with the `\\\\?\\` prefix, so that no path length limit applies to the walk."""
    p = os.path.abspath(p)
    if p.startswith("\\\\?\\"):
        return p
    if p.startswith("\\\\"):
        return "\\\\?\\UNC\\" + p[2:]
    return "\\\\?\\" + p


def record(path):
    """What the walk records for `path`, never following it: (record, descend)."""
    st = os.lstat(path)
    if getattr(st, "st_file_attributes", 0) & REPARSE_POINT or stat.S_ISLNK(st.st_mode):
        # A junction or a symbolic link (any reparse point): recorded, never followed.
        return {"kind": "reparse", "mtime": st.st_mtime_ns}, False
    if stat.S_ISDIR(st.st_mode):
        return {"kind": "dir"}, True
    return {"kind": "file", "size": st.st_size, "mtime": st.st_mtime_ns}, False


def walk(root_dir, token, out):
    """Record `root_dir`'s tree in `out` under names that start with %token%; every path that
    cannot be read is recorded as `unreadable` and printed."""
    stack = [(long_path(root_dir), f"%{token}%")]
    while stack:
        d, name = stack.pop()
        try:
            names = sorted(os.listdir(d))
        except OSError as e:
            out[name] = {"kind": "unreadable", "error": e.__class__.__name__}
            print(f"unreadable: {name} ({e.__class__.__name__})")
            continue
        for child in names:
            n = name + "\\" + child
            p = d + "\\" + child
            try:
                rec, descend = record(p)
            except OSError as err:
                out[n] = {"kind": "unreadable", "error": err.__class__.__name__}
                print(f"unreadable: {n} ({err.__class__.__name__})")
                continue
            out[n] = rec
            if descend:
                stack.append((p, n))


def parse_reg(text):
    """A `reg export` file's text: {key: {value name ('' for the default): data as written}}.
    Key names are given as HKCU\\...; values keep their exported form (`"text"`, `dword:...`,
    `hex(n):...`), continuation lines joined."""
    keys = {}
    current = None
    last = None  # (key, name) of the last value, for its continuation lines
    pending = None  # a value whose line ended in a backslash
    for raw in text.splitlines():
        line = raw.rstrip("\r")
        if pending is not None:
            k, name, data = pending
            part = line.strip()
            if part.endswith("\\"):
                pending = (k, name, data + part[:-1])
            else:
                keys[k][name] = data + part
                pending = None
            continue
        if not line.strip() or line.startswith("Windows Registry Editor"):
            continue
        if line.startswith("[") and line.endswith("]"):
            current = line[1:-1]
            for long, short in (("HKEY_CURRENT_USER", "HKCU"),):
                if current.upper().startswith(long):
                    current = short + current[len(long):]
            keys.setdefault(current, {})
            last = None
            continue
        if current is not None and (line.startswith('"') or line.startswith("@=")):
            if line.startswith("@="):
                name, rest = "", line[2:]
            else:
                i, chars = 1, []
                while i < len(line) and line[i] != '"':
                    if line[i] == "\\" and i + 1 < len(line):
                        i += 1
                    chars.append(line[i])
                    i += 1
                name = "".join(chars)
                rest = line[i + 1:]
                if not rest.startswith("="):
                    raise ValueError(f"unparsed .reg line: {line!r}")
                rest = rest[1:]
            if rest.endswith("\\") and not rest.startswith('"'):
                pending = (current, name, rest[:-1])
            else:
                keys[current][name] = rest
            last = (current, name)
            continue
        if last is not None:
            # A string value with a line break inside it.
            keys[last[0]][last[1]] += "\n" + line
            continue
        raise ValueError(f"unparsed .reg line: {line!r}")
    return keys


def read_reg_file(path):
    """A `reg export` file (UTF-16 with its byte order mark)."""
    data = Path(path).read_bytes()
    enc = "utf-16" if data[:2] in (b"\xff\xfe", b"\xfe\xff") else "utf-8-sig"
    return parse_reg(data.decode(enc))


def snapshot(out, on_ci):
    if os.name != "nt":
        raise SystemExit("e7-residue: snapshot runs on Windows only")
    if not on_ci and os.environ.get(CI_ENV) != "true":
        raise SystemExit(f"e7-residue: snapshot records this profile's registry; it runs only on a "
                         f"disposable machine ({CI_ENV}=true, or --on-ci)")
    out = Path(out)
    files = {}
    for token in FILE_ROOTS:
        root = os.environ.get(token)
        if not root:
            raise SystemExit(f"e7-residue: %{token}% is not set")
        walk(root, token, files)
    registry = {}
    with tempfile.TemporaryDirectory(dir=out.parent) as tmp:
        for i, root in enumerate(REG_ROOTS):
            reg = Path(tmp) / f"{i}.reg"
            subprocess.run(["reg.exe", "export", root, str(reg), "/y"], check=True,
                           stdout=subprocess.DEVNULL)
            registry.update(read_reg_file(reg))
    if not any(under(k, LOCAL_SETTINGS) for k in registry):
        raise SystemExit(f"e7-residue: the export holds no key under {LOCAL_SETTINGS}")
    out.write_text(json.dumps({"files": files, "registry": registry}, sort_keys=True),
                   encoding="utf-8")
    unreadable = sum(1 for r in files.values() if r["kind"] == "unreadable")
    print(f"snapshot {out}: {len(files)} files and folders ({unreadable} unreadable), "
          f"{len(registry)} keys")


# --- diff ------------------------------------------------------------------------------------


def entries(snap):
    """A snapshot as {(kind, path, value name) compared case-insensitively: (the path and name as
    recorded, what it records)}."""
    e = {}
    for path, rec in snap["files"].items():
        e[("file", norm(path), None)] = (path, None, rec)
    for key, values in snap["registry"].items():
        e[("key", norm(key), None)] = (key, None, True)
        for name, data in values.items():
            e[("value", norm(key), norm(name))] = (key, name, data)
    return e


def profile_sids(*snaps):
    """The SIDs whose Mappings key names the worker's moniker, in any of the snapshots."""
    sids = set()
    for s in snaps:
        for key, values in s["registry"].items():
            parent, _, sid = key.rpartition("\\")
            if norm(parent) != norm(MAPPINGS):
                continue
            moniker = [v for n, v in values.items() if norm(n) == "moniker"]
            if any(norm(v) == norm(f'"{WORKER_MONIKER}"') for v in moniker):
                sids.add(norm(sid))
    return sids


def is_profile(kind, path, sids):
    """Whether an entry belongs to the worker's profile: a path component is its moniker (its
    folder under %LOCALAPPDATA%\\Packages, its AppContainer Storage key) or its SID."""
    parts = [norm(p) for p in path.split("\\")]
    if kind == "file":
        return under(path, f"%LOCALAPPDATA%\\Packages\\{WORKER_MONIKER}") or \
            any(p in sids for p in parts)
    return norm(WORKER_MONIKER) in parts or any(p in sids for p in parts)


def empty_key(snap, key):
    """Whether `key` is in `snap` holding nothing: no value, and no subkey but the R301 parents,
    each holding nothing in turn."""
    parents = {norm(a["path"]) for a in ALLOW if a["kind"] == "empty-key"}
    k = norm(key)
    found = False
    for path, values in snap["registry"].items():
        p = norm(path)
        if p == k:
            found = True
            if values:
                return False
        elif p.startswith(k + "\\"):
            if p not in parents or "\\" in p[len(k) + 1:] or not empty_key(snap, path):
                return False
    return found


def allowed(kind, path, name, sign, after):
    """The allow-list's reason for an entry, or None."""
    for a in ALLOW:
        if a["kind"] == "file" and kind == "file" and under(path, a["path"]):
            return a["reason"]
        if a["kind"] == "key" and kind in ("key", "value") and under(path, a["path"]):
            return a["reason"]
        if a["kind"] == "value" and kind == "value" and norm(path) == norm(a["path"]) and \
                norm(name) == norm(a["name"]):
            return a["reason"]
        if a["kind"] == "empty-key" and kind == "key" and sign == "+" and \
                norm(path) == norm(a["path"]) and empty_key(after, path):
            return a["reason"]
    return None


def show(kind, path, name, rec):
    if kind == "value":
        return f"value {path} [{name or '@'}] = {rec}"
    if kind == "key":
        return f"key {path}"
    if rec.get("kind") == "file":
        return f"file {path} ({rec['size']} bytes, mtime {rec['mtime']})"
    if rec.get("kind") == "unreadable":
        return f"unreadable {path} ({rec.get('error')})"
    return f"{rec.get('kind')} {path}"


def diff(before, after, allow_profile):
    """The diff's lines and the number of entries not allowed. A path unreadable in either
    snapshot is listed (and counted unless allowed) even when both record it alike: what lies
    inside it was not seen."""
    b, a = entries(before), entries(after)
    sids = profile_sids(before, after)
    changes = []
    for k in sorted(set(b) | set(a), key=lambda k: (k[0], k[1], k[2] or "")):
        if k not in b:
            changes.append(("+",) + a[k] + (k[0],))
        elif k not in a:
            changes.append(("-",) + b[k] + (k[0],))
        elif a[k][2] != b[k][2]:
            changes.append(("~",) + a[k] + (k[0],))
        elif k[0] == "file" and a[k][2].get("kind") == "unreadable":
            changes.append(("?",) + a[k] + (k[0],))
    # Keys that exist only as the profile's ancestors (created for it, holding no value of their
    # own that changed) belong to it too.
    profile_keys = [c[1] for c in changes if c[4] != "file" and is_profile(c[4], c[1], sids)]
    valued = {norm(c[1]) for c in changes if c[4] == "value"}
    lines, bad = [], 0
    for sign, path, name, rec, kind in changes:
        text = f"{sign} {show(kind, path, name, rec)}"
        reason = allowed(kind, path, name, sign, after)
        ancestor = kind == "key" and sign in "+-" and norm(path) not in valued and \
            any(under(p, path) and norm(p) != norm(path) for p in profile_keys)
        if reason:
            lines.append(f"allowed: {text}  -- {reason}")
        elif allow_profile and (is_profile(kind, path, sids) or ancestor) and \
                not (kind == "file" and rec.get("kind") == "unreadable"):
            lines.append(f"profile: {text}")
        else:
            lines.append(f"RESIDUE: {text}")
            bad += 1
    return lines, bad


def profile_present(lines):
    """What the worker's profile lacks in a diff's lines: both its folder under
    %LOCALAPPDATA%\\Packages and its Mappings key must be added. Empty when both are."""
    folder = norm(f"profile: + dir %LOCALAPPDATA%\\Packages\\{WORKER_MONIKER}")
    mapping = norm(f"profile: + key {MAPPINGS}\\S-1-15-2-")
    missing = []
    if not any(norm(line) == folder for line in lines):
        missing.append(f"no added folder %LOCALAPPDATA%\\Packages\\{WORKER_MONIKER}")
    if not any(norm(line).startswith(mapping) and "\\" not in line[len(mapping):]
               for line in lines):
        missing.append(f"no added key {MAPPINGS}\\S-1-15-2-...")
    return missing


def main(argv):
    if argv[:1] == ["--self-test"]:
        r = unittest.main(argv=[sys.argv[0]], module=__name__, exit=False, verbosity=2).result
        return 0 if r.wasSuccessful() else 1
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("snapshot")
    s.add_argument("out")
    s.add_argument("--on-ci", action="store_true",
                   help="this is a disposable machine (implied by GITHUB_ACTIONS=true)")
    d = sub.add_parser("diff")
    d.add_argument("before")
    d.add_argument("after")
    d.add_argument("--allow-worker-profile", action="store_true")
    d.add_argument("--require-worker-profile", action="store_true",
                   help="allow the profile, and fail unless its folder and Mappings key were added")
    a = ap.parse_args(argv)
    if a.cmd == "snapshot":
        snapshot(a.out, a.on_ci)
        return 0
    load = lambda p: json.loads(Path(p).read_text(encoding="utf-8"))  # noqa: E731
    allow = a.allow_worker_profile or a.require_worker_profile
    lines, bad = diff(load(a.before), load(a.after), allow)
    for line in lines:
        print(line)
    flag = " (worker profile allowed)" if allow else ""
    print(f"{len(lines)} differences{flag}, {bad} not allowed")
    missing = profile_present(lines) if a.require_worker_profile else []
    for m in missing:
        print(f"PROFILE MISSING: {m}")
    return 1 if bad or missing else 0


# --- tests (synthetic snapshots only, R294) --------------------------------------------------


SID = "S-1-15-2-1-2-3-4-5-6-7"


def snap(files=None, registry=None):
    return {"files": files or {}, "registry": registry or {}}


def f(size=1, mtime=1):
    return {"kind": "file", "size": size, "mtime": mtime}


def with_profile(s):
    files = dict(s["files"])
    files[r"%LOCALAPPDATA%\Packages\litepack.worker"] = {"kind": "dir"}
    files[r"%LOCALAPPDATA%\Packages\litepack.worker\AC\Temp"] = {"kind": "dir"}
    reg = dict(s["registry"])
    reg[MAPPINGS + "\\" + SID] = {"DisplayName": '"LitePack.Worker"',
                                   "Moniker": '"LitePack.Worker"'}
    reg[MAPPINGS + "\\" + SID + r"\Children"] = {}
    # A parent created for the profile alone, with no value of its own.
    reg[STORAGE] = {}
    reg[STORAGE + r"\litepack.worker"] = {"x": "dword:00000001"}
    return snap(files, reg)


class Tests(unittest.TestCase):
    base = snap({r"%APPDATA%\a.txt": f()}, {r"HKCU\Software\X": {"v": '"1"'}, MAPPINGS: {}})

    def test_no_change_is_clean(self):
        self.assertEqual(diff(self.base, self.base, False), ([], 0))

    def test_an_added_file_is_residue(self):
        after = snap(dict(self.base["files"], **{r"%APPDATA%\LitePack\settings.json": f()}),
                     self.base["registry"])
        lines, bad = diff(self.base, after, True)
        self.assertEqual(bad, 1)
        self.assertEqual(lines, [r"RESIDUE: + file %APPDATA%\LitePack\settings.json "
                                 "(1 bytes, mtime 1)"])

    def test_a_removed_and_a_changed_file(self):
        after = snap({r"%LOCALAPPDATA%\b": f()}, self.base["registry"])
        before = snap(dict(self.base["files"], **{r"%LOCALAPPDATA%\b": f(mtime=0)}),
                      self.base["registry"])
        lines, bad = diff(before, after, False)
        self.assertEqual(bad, 2)
        self.assertTrue(lines[0].startswith(r"RESIDUE: - file %APPDATA%\a.txt"))
        self.assertTrue(lines[1].startswith(r"RESIDUE: ~ file %LOCALAPPDATA%\b"))

    def test_a_changed_value_is_residue(self):
        after = snap(self.base["files"], dict(self.base["registry"],
                                              **{r"HKCU\Software\X": {"v": '"2"'}}))
        lines, bad = diff(self.base, after, True)
        self.assertEqual((lines, bad), ([r'RESIDUE: ~ value HKCU\Software\X [v] = "2"'], 1))

    def test_an_allow_listed_entry_is_allowed_with_its_reason(self):
        global ALLOW
        saved = ALLOW
        ALLOW = [{"kind": "value", "path": r"HKCU\Software\X", "name": "V",
                  "reason": "a test's reason"},
                 {"kind": "file", "path": r"%LOCALAPPDATA%\Cache", "reason": "cache"}]
        try:
            after = snap(dict(self.base["files"], **{r"%LOCALAPPDATA%\cache\c\d": f()}),
                         {r"HKCU\Software\x": {"v": '"2"'}, MAPPINGS: {}})
            # The key's and the value's names differ in case only: the same key and value.
            before = snap(self.base["files"], {r"HKCU\Software\X": {"V": '"1"'}, MAPPINGS: {}})
            lines, bad = diff(before, after, False)
            self.assertEqual(bad, 0, lines)
            self.assertEqual(len(lines), 2, lines)
            self.assertIn("-- a test's reason", lines[1])
            self.assertIn("-- cache", lines[0])
            # A sibling of the allowed folder is not allowed.
            after["files"][r"%LOCALAPPDATA%\Cache2"] = f()
            self.assertEqual(diff(before, after, False)[1], 1)
        finally:
            ALLOW = saved

    def test_the_profile_needs_the_flag(self):
        after = with_profile(self.base)
        lines, bad = diff(self.base, after, False)
        self.assertEqual(bad, len(lines))
        self.assertEqual(bad, 9)
        lines, bad = diff(self.base, after, True)
        self.assertEqual(bad, 0, lines)
        self.assertTrue(all(line.startswith("profile: ") for line in lines))
        self.assertEqual(profile_present(lines), [])

    def test_a_parent_key_made_for_the_profile_needs_the_flag(self):
        after = with_profile(self.base)
        parent = f"+ key {STORAGE}"
        self.assertIn(f"RESIDUE: {parent}", diff(self.base, after, False)[0])
        self.assertIn(f"profile: {parent}", diff(self.base, after, True)[0])

    def test_a_parent_key_with_a_value_of_its_own_is_residue(self):
        after = with_profile(self.base)
        after["registry"][STORAGE] = {"own": '"x"'}
        lines, bad = diff(self.base, after, True)
        self.assertEqual(bad, 2, lines)
        self.assertIn(f"RESIDUE: + key {STORAGE}", lines)
        self.assertIn(f'RESIDUE: + value {STORAGE} [own] = "x"', lines)

    def test_empty_appcontainer_parents_left_by_windows_are_allowed(self):
        # R301: after --remove-profile, the parents alone, empty.
        after = snap(self.base["files"], dict(self.base["registry"], **{APPCONTAINER: {},
                                                                        STORAGE: {}}))
        lines, bad = diff(self.base, after, False)
        self.assertEqual(bad, 0, lines)
        self.assertTrue(all(line.startswith("allowed: + key ") for line in lines), lines)
        # Anything left under one is residue, and the parent is no longer empty.
        after["registry"][STORAGE + r"\litepack.worker"] = {}
        lines, bad = diff(self.base, after, False)
        self.assertIn(f"RESIDUE: + key {STORAGE}", lines)
        self.assertIn(f"RESIDUE: + key {STORAGE}\\litepack.worker", lines)
        # Its parent AppContainer holds it, so it is not empty either.
        self.assertEqual(bad, 3, lines)
        # A value in the parent itself too.
        after = snap(self.base["files"], dict(self.base["registry"], **{STORAGE: {"v": '"1"'}}))
        self.assertEqual(diff(self.base, after, False)[1], 2)

    def test_the_profile_requirement_needs_its_folder_and_its_mapping(self):
        after = with_profile(self.base)
        del after["registry"][MAPPINGS + "\\" + SID]
        del after["registry"][MAPPINGS + "\\" + SID + r"\Children"]
        missing = profile_present(diff(self.base, after, True)[0])
        self.assertEqual(len(missing), 1, missing)
        self.assertIn("Mappings", missing[0])
        files_gone = with_profile(self.base)
        files_gone["files"] = dict(self.base["files"])
        missing = profile_present(diff(self.base, files_gone, True)[0])
        self.assertEqual(len(missing), 1, missing)
        self.assertIn("Packages", missing[0])

    def test_the_moniker_value_is_matched_without_case(self):
        after = with_profile(self.base)
        after["registry"][MAPPINGS + "\\" + SID] = {"moniker": '"litepack.WORKER"'}
        lines, bad = diff(self.base, after, True)
        self.assertEqual(bad, 0, lines)

    def test_an_unreadable_path_is_reported_even_unchanged(self):
        s = snap(dict(self.base["files"], **{r"%LOCALAPPDATA%\locked": {
            "kind": "unreadable", "error": "PermissionError"}}), self.base["registry"])
        lines, bad = diff(s, s, False)
        self.assertEqual(lines, [r"RESIDUE: ? unreadable %LOCALAPPDATA%\locked (PermissionError)"])
        self.assertEqual(bad, 1)
        # Inside the profile's folder too, with the flag.
        s["files"] = {r"%LOCALAPPDATA%\Packages\litepack.worker\x": {"kind": "unreadable",
                                                                    "error": "OSError"}}
        self.assertEqual(diff(snap(registry=s["registry"]), s, True)[1], 1)

    def test_paths_get_the_long_path_prefix(self):
        if os.name != "nt":
            self.skipTest("Windows paths")
        self.assertEqual(long_path("C:\\a\\b"), "\\\\?\\C:\\a\\b")
        self.assertEqual(long_path("\\\\?\\C:\\a"), "\\\\?\\C:\\a")
        self.assertEqual(long_path("\\\\srv\\share\\a"), "\\\\?\\UNC\\srv\\share\\a")

    def test_a_reparse_point_is_recorded_and_not_followed(self):
        if os.name != "nt":
            self.skipTest("Windows reparse points")
        with tempfile.TemporaryDirectory() as d:
            target = Path(d) / "target"
            target.mkdir()
            (target / "inside").write_bytes(b"x")
            link = Path(d) / "link"
            try:
                os.symlink(target, link, target_is_directory=True)
            except OSError:
                self.skipTest("no symlink privilege")
            out = {}
            walk(d, "T", out)
        self.assertEqual(out["%T%\\link"]["kind"], "reparse")
        self.assertNotIn("%T%\\link\\inside", out)
        self.assertEqual(out["%T%\\target\\inside"]["kind"], "file")

    def test_snapshot_refuses_off_ci(self):
        env = {k: v for k, v in os.environ.items() if k != CI_ENV}
        with tempfile.TemporaryDirectory() as d, \
                unittest.mock.patch.dict(os.environ, env, clear=True):
            with self.assertRaises(SystemExit):
                snapshot(Path(d) / "s.json", on_ci=False)
            self.assertEqual(list(Path(d).iterdir()), [])

    def test_the_removed_profile_is_matched_too(self):
        lines, bad = diff(with_profile(self.base), self.base, True)
        self.assertEqual(bad, 0, lines)
        self.assertTrue(all(line.startswith("profile: - ") for line in lines))

    def test_another_appcontainer_is_never_the_profile(self):
        after = snap(dict(self.base["files"], **{r"%LOCALAPPDATA%\Packages\other.app": {"kind": "dir"}}),
                     dict(self.base["registry"], **{MAPPINGS + r"\S-1-15-2-9": {"Moniker": '"other.app"'}}))
        lines, bad = diff(self.base, after, True)
        self.assertEqual(bad, 3, lines)

    def test_a_reg_export_in_utf16_is_parsed(self):
        text = ("Windows Registry Editor Version 5.00\r\n\r\n"
                "[HKEY_CURRENT_USER\\Software]\r\n\r\n"
                "[HKEY_CURRENT_USER\\Software\\A b]\r\n"
                '@="default"\r\n'
                '"Na\\"me"="va\\\\l"\r\n'
                '"D"=dword:0000002a\r\n'
                '"Bin"=hex:00,01,\\\r\n  02,03\r\n'
                '"Multi"="line one\r\nline two"\r\n'
                "\r\n[HKEY_CURRENT_USER\\Environment]\r\n"
                '"Path"=hex(2):25,00,\\\r\n  00,00\r\n')
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "x.reg"
            p.write_bytes(b"\xff\xfe" + text.encode("utf-16-le"))
            keys = read_reg_file(p)
        self.assertEqual(keys[r"HKCU\Software"], {})
        self.assertEqual(keys[r"HKCU\Software\A b"], {
            "": '"default"', 'Na"me': '"va\\\\l"', "D": "dword:0000002a",
            "Bin": "hex:00,01,02,03", "Multi": '"line one\nline two"'})
        self.assertEqual(keys[r"HKCU\Environment"], {"Path": "hex(2):25,00,00,00"})

    def test_a_snapshot_round_trips_through_json(self):
        s = with_profile(self.base)
        self.assertEqual(diff(s, json.loads(json.dumps(s)), False), ([], 0))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
