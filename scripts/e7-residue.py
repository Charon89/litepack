"""E7-6's residue check (R291; docs/notes/e7-design.md sections 3 and 5; E7-5 extends it).

    python -I scripts/e7-residue.py snapshot <out.json>
    python -I scripts/e7-residue.py diff <before.json> <after.json> [--allow-worker-profile]
    python -I scripts/e7-residue.py --self-test

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
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

# The worker's AppContainer profile (crates/lpk-sandbox-sys: APPCONTAINER_NAME).
WORKER_MONIKER = "LitePack.Worker"

FILE_ROOTS = ("APPDATA", "LOCALAPPDATA")
REG_ROOTS = (r"HKCU\Software", r"HKCU\Environment")
APPCONTAINER = (r"HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows"
                r"\CurrentVersion\AppContainer")
MAPPINGS = APPCONTAINER + r"\Mappings"

# Windows' own bookkeeping, which running any program changes. One entry per folder (`file`: the
# folder or file and everything under it), key (`key`: the key and everything under it) or value
# (`value`: a key and one value name). Nothing LitePack writes may be listed here: that is fixed
# instead. Each entry's reason is printed beside every entry it allows.
ALLOW = [
]


def norm(s):
    """Windows' comparison of names: case-insensitive."""
    return s.casefold()


def under(path, root):
    """Whether `path` is `root` or lies under it (backslash-separated, case-insensitive)."""
    p, r = norm(path), norm(root).rstrip("\\")
    return p == r or p.startswith(r + "\\")


# --- snapshot --------------------------------------------------------------------------------


def walk(root_dir, token, out):
    """Record `root_dir`'s tree in `out` under names that start with %token%."""
    stack = [(Path(root_dir), f"%{token}%")]
    while stack:
        d, name = stack.pop()
        try:
            entries = list(os.scandir(d))
        except OSError as e:
            out[name] = {"kind": "unreadable", "error": e.__class__.__name__}
            continue
        for e in entries:
            n = name + "\\" + e.name
            try:
                is_junction = getattr(e, "is_junction", lambda: False)()
                if e.is_symlink() or is_junction:
                    out[n] = {"kind": "link"}
                elif e.is_dir(follow_symlinks=False):
                    out[n] = {"kind": "dir"}
                    stack.append((Path(e.path), n))
                else:
                    st = e.stat(follow_symlinks=False)
                    out[n] = {"kind": "file", "size": st.st_size, "mtime": st.st_mtime_ns}
            except OSError as err:
                out[n] = {"kind": "unreadable", "error": err.__class__.__name__}


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


def snapshot(out):
    if os.name != "nt":
        raise SystemExit("e7-residue: snapshot runs on Windows only")
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
    out.write_text(json.dumps({"files": files, "registry": registry}, sort_keys=True),
                   encoding="utf-8")
    print(f"snapshot {out}: {len(files)} files and folders, {len(registry)} keys")


# --- diff ------------------------------------------------------------------------------------


def entries(snap):
    """A snapshot as {(kind, path, value name): what it records}."""
    e = {}
    for path, rec in snap["files"].items():
        e[("file", path, None)] = rec
    for key, values in snap["registry"].items():
        e[("key", key, None)] = True
        for name, data in values.items():
            e[("value", key, name)] = data
    return e


def profile_sids(*snaps):
    """The SIDs whose Mappings key names the worker's moniker, in any of the snapshots."""
    sids = set()
    for s in snaps:
        for key, values in s["registry"].items():
            parent, _, sid = key.rpartition("\\")
            if norm(parent) == norm(MAPPINGS) and \
                    norm(values.get("Moniker", "")) == norm(f'"{WORKER_MONIKER}"'):
                sids.add(norm(sid))
    return sids


def is_profile(entry, sids):
    """Whether `entry` belongs to the worker's profile: a path component is its moniker (its
    folder under %LOCALAPPDATA%\\Packages, its AppContainer Storage key) or its SID."""
    kind, path, _ = entry
    parts = [norm(p) for p in path.split("\\")]
    if kind == "file":
        return under(path, f"%LOCALAPPDATA%\\Packages\\{WORKER_MONIKER}") or \
            any(p in sids for p in parts)
    return norm(WORKER_MONIKER) in parts or any(p in sids for p in parts)


def allowed(entry):
    """The allow-list's reason for `entry`, or None."""
    kind, path, name = entry
    for a in ALLOW:
        if a["kind"] == "file" and kind == "file" and under(path, a["path"]):
            return a["reason"]
        if a["kind"] == "key" and kind in ("key", "value") and under(path, a["path"]):
            return a["reason"]
        if a["kind"] == "value" and kind == "value" and norm(path) == norm(a["path"]) and \
                norm(name) == norm(a["name"]):
            return a["reason"]
    return None


def show(entry, rec):
    kind, path, name = entry
    if kind == "value":
        return f"value {path} [{name or '@'}] = {rec}"
    if kind == "key":
        return f"key {path}"
    if rec.get("kind") == "file":
        return f"file {path} ({rec['size']} bytes, mtime {rec['mtime']})"
    return f"{rec.get('kind')} {path}"


def diff(before, after, allow_profile):
    """The diff's lines and the number of entries not allowed."""
    b, a = entries(before), entries(after)
    sids = profile_sids(before, after)
    changes = []
    for k in sorted(set(b) | set(a), key=lambda k: (k[0], norm(k[1]), k[2] or "")):
        if k not in b:
            changes.append(("+", k, a[k]))
        elif k not in a:
            changes.append(("-", k, b[k]))
        elif a[k] != b[k]:
            changes.append(("~", k, a[k]))
    # Keys that exist only as the profile's ancestors (created for it, holding no value of their
    # own that changed) belong to it too.
    profile_keys = [k[1] for _, k, _ in changes if k[0] != "file" and is_profile(k, sids)]
    lines, bad = [], 0
    for sign, k, rec in changes:
        text = f"{sign} {show(k, rec)}"
        reason = allowed(k)
        ancestor = k[0] == "key" and sign == "+" and \
            any(under(p, k[1]) and norm(p) != norm(k[1]) for p in profile_keys) and \
            not any(c[1][0] == "value" and norm(c[1][1]) == norm(k[1]) for c in changes)
        if reason:
            lines.append(f"allowed: {text}  -- {reason}")
        elif allow_profile and (is_profile(k, sids) or ancestor):
            lines.append(f"profile: {text}")
        else:
            lines.append(f"RESIDUE: {text}")
            bad += 1
    return lines, bad


def main(argv):
    if argv[:1] == ["--self-test"]:
        r = unittest.main(argv=[sys.argv[0]], module=__name__, exit=False, verbosity=2).result
        return 0 if r.wasSuccessful() else 1
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("snapshot")
    s.add_argument("out")
    d = sub.add_parser("diff")
    d.add_argument("before")
    d.add_argument("after")
    d.add_argument("--allow-worker-profile", action="store_true")
    a = ap.parse_args(argv)
    if a.cmd == "snapshot":
        snapshot(a.out)
        return 0
    load = lambda p: json.loads(Path(p).read_text(encoding="utf-8"))  # noqa: E731
    lines, bad = diff(load(a.before), load(a.after), a.allow_worker_profile)
    for line in lines:
        print(line)
    flag = " (worker profile allowed)" if a.allow_worker_profile else ""
    print(f"{len(lines)} differences{flag}, {bad} not allowed")
    return 1 if bad else 0


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
    reg[APPCONTAINER + r"\Storage\litepack.worker"] = {"x": "dword:00000001"}
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
            # The key's own name differs in case only: the same key.
            before = snap(self.base["files"], {r"HKCU\Software\x": {"v": '"1"'}, MAPPINGS: {}})
            lines, bad = diff(before, after, False)
            self.assertEqual(bad, 0, lines)
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
        self.assertEqual(bad, 8)
        lines, bad = diff(self.base, after, True)
        self.assertEqual(bad, 0, lines)
        self.assertTrue(all(line.startswith("profile: ") for line in lines))

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
