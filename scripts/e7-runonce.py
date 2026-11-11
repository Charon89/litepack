"""E7-5 (CI's `installer` job; Windows only): LitePack's per-user RunOnce values (R335).

The installer writes one value per cleanup action, named `LitePack <group> <n>` (installer.nsh), because Windows
runs a Run/RunOnce command line of at most 260 characters.

    python -I scripts/e7-runonce.py show    each `LitePack*` value as `<name><TAB><length><TAB><command>`, in name
                                            order; exit 1, naming the value, if any is over 260 characters
    python -I scripts/e7-runonce.py run     what the next logon does: each value in name order, its command run,
                                            then the value deleted (writes HKCU: CI only)
    python -I scripts/e7-runonce.py --self-test

Output is LF only (bash's `$(...)` keeps a CR).
"""

import re
import subprocess
import sys

KEY = r"Software\Microsoft\Windows\CurrentVersion\RunOnce"
LIMIT = 260


def emit(text):
    sys.stdout.buffer.write((text + "\n").encode("utf-8"))
    sys.stdout.buffer.flush()


def order(name):
    """Name order with the trailing action number compared as a number (`x 2` before `x 10`)."""
    m = re.match(r"^(.*?)(\d+)$", name)
    return (m.group(1), int(m.group(2))) if m else (name, -1)


def values():
    import winreg

    out = []
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, KEY) as k:
        i = 0
        while True:
            try:
                name, data, _ = winreg.EnumValue(k, i)
            except OSError:
                break
            if name.startswith("LitePack"):
                out.append((name, str(data)))
            i += 1
    return sorted(out, key=lambda v: order(v[0]))


def over_limit(vals):
    return [n for n, d in vals if len(d) > LIMIT]


def show():
    vals = values()
    for name, data in vals:
        emit(f"{name}\t{len(data)}\t{data}")
    bad = over_limit(vals)
    if bad:
        raise SystemExit(f"e7-runonce: over {LIMIT} characters: {', '.join(bad)}")


def run():
    import winreg

    for name, data in values():
        rc = subprocess.run(data).returncode
        emit(f"{name}: exit {rc}")
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, KEY, 0, winreg.KEY_SET_VALUE) as k:
            winreg.DeleteValue(k, name)


def self_test():
    names = ["LitePack uninstall 10", "LitePack bin 2", "LitePack uninstall 2", "LitePack bin 1"]
    assert sorted(names, key=order) == [
        "LitePack bin 1", "LitePack bin 2", "LitePack uninstall 2", "LitePack uninstall 10"], names
    assert over_limit([("a", "x" * LIMIT), ("b", "x" * (LIMIT + 1))]) == ["b"]
    emit("e7-runonce self-test: ok")


def main(argv):
    if argv == ["--self-test"]:
        self_test()
    elif argv == ["show"]:
        show()
    elif argv == ["run"]:
        run()
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
