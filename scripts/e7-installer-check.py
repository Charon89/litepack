"""E7-5: what an installed LitePack must have registered, read from HKCU (CI's `installer` job only; read-only).

    python -I scripts/e7-installer-check.py installed <install folder> <version>

Checks the Uninstall key (DisplayVersion), HKCU\\Environment's Path (holding <install>\\bin exactly once, compared
case-insensitively without a trailing backslash; its type printed), route C (the classic class's InProcServer32 is
<install>\\shell\\<version>\\lpk_shell.dll, the three shell keys name it), the .lpk association and GuiPath. Prints
every value it reads; exits 1 on the first list of failures.
"""

import os
import sys

CLSID = "{01E42821-7D51-4B9B-AD9F-FFCA769A6025}"


def read(key, name):
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key) as k:
            return winreg.QueryValueEx(k, name)
    except OSError:
        return None


def same(a, b):
    n = lambda s: os.path.normcase(s.replace("/", "\\").rstrip("\\"))  # noqa: E731
    return n(a) == n(b)


def installed(inst, version):
    inst = os.path.abspath(inst)
    bad = []

    def expect(what, got, ok):
        print(f"{what}: {got!r}")
        if not ok:
            bad.append(what)

    v = read(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\LitePack", "DisplayVersion")
    expect("Uninstall\\LitePack DisplayVersion", v, v is not None and v[0] == version)
    path = read("Environment", "Path")
    entries = path[0].split(";") if path else []
    hits = [e for e in entries if same(e, os.path.join(inst, "bin"))]
    expect("Environment Path (value, type)", path, len(hits) == 1)
    dll = read(rf"Software\Classes\CLSID\{CLSID}\InProcServer32", "")
    expect("InProcServer32", dll, dll is not None and
           same(dll[0], os.path.join(inst, "shell", version, "lpk_shell.dll")))
    for target in ("*", "Directory", r"Directory\Background"):
        h = read(rf"Software\Classes\{target}\shell\LitePack", "ExplorerCommandHandler")
        expect(f"{target}\\shell\\LitePack", h, h is not None and h[0] == CLSID)
    lpk = read(r"Software\Classes\.lpk", "")
    expect(".lpk", lpk, lpk is not None and lpk[0] == "LitePack.lpk")
    gui = read(r"Software\LitePack", "GuiPath")
    expect("GuiPath", gui, gui is not None and same(gui[0], os.path.join(inst, "lpk-gui.exe")))
    if bad:
        print(f"FAILED: {', '.join(bad)}")
        return 1
    print(f"installed {version} at {inst}: registered as expected")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 4 or sys.argv[1] != "installed":
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    sys.exit(installed(sys.argv[2], sys.argv[3]))
