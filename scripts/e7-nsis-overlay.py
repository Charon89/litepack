"""E7-2 (release.yml's package job): write the NSIS bundle overlay of one architecture.

    python -I scripts/e7-nsis-overlay.py --arch x64|arm64 --bin <dir> --repo <root> --out <json>
    python -I scripts/e7-nsis-overlay.py --self-test

The overlay is crates/lpk-gui/tauri.nsis.conf.json (the one ci.yml's `installer` job builds with) with two changes:
every resource source becomes an absolute path, the five non-GUI PEs' to the files in <dir> (the build job's
artifact, not target\\release), the notices' and licences' to the repository's own files; and `installerHooks` names
the architecture's hooks file by its absolute path (installer.nsh for x64, installer-arm64.nsh for arm64: the hooks
cannot see the template's ${ARCH}, installer-arm64.nsh defines it). Both are literal paths written with forward slashes
for uniformity (tauri-utils reads a resource source as a glob only when it contains `*`, resources.rs). Every other key, every destination included, is
unchanged. It fails (exit 1) on a missing source, on a resource key it does not recognise, and when the shell
DLL's `shell/<version>/` destination is not the workspace version (Cargo.toml's `[workspace.package] version`).
The standard library only (tomllib: Python 3.11 or later).
"""

import argparse
import json
import sys
import tempfile
import tomllib
from pathlib import Path

HOOKS = {"x64": "installer.nsh", "arm64": "installer-arm64.nsh"}
# The PEs the overlay installs (its keys' file names under target/release/), and the repository files.
PES = ["lpk.exe", "lpk-check.exe", "lpk-worker.exe", "lpk-worker-libarchive.exe", "lpk_shell.dll"]
REPO_FILES = ["THIRD-PARTY-NOTICES", "LICENSE-APACHE", "LICENSE-MIT"]


class OverlayError(Exception):
    pass


def workspace_version(repo):
    with open(repo / "Cargo.toml", "rb") as f:
        return tomllib.load(f)["workspace"]["package"]["version"]


def overlay(arch, bin_dir, repo):
    """The overlay as a dict; OverlayError names what is wrong."""
    if arch not in HOOKS:
        raise OverlayError(f"unknown architecture {arch!r} (x64 or arm64)")
    repo = repo.resolve()
    bin_dir = bin_dir.resolve()
    gui = repo / "crates" / "lpk-gui"
    conf = json.loads((gui / "tauri.nsis.conf.json").read_text(encoding="utf-8"))
    version = workspace_version(repo)
    resources = conf["bundle"]["resources"]
    out = {}
    for key, dest in resources.items():
        name = key.rsplit("/", 1)[-1]
        if key == f"../../target/release/{name}" and name in PES:
            src = bin_dir / name
        elif key == f"../../{name}" and name in REPO_FILES:
            src = repo / name
        else:
            raise OverlayError(f"tauri.nsis.conf.json: a resource this script does not know: {key}")
        if not src.is_file():
            raise OverlayError(f"missing source of {dest}: {src}")
        if dest.startswith("shell/") and dest != f"shell/{version}/{name}":
            raise OverlayError(f"{dest} is not shell/{version}/{name} (the workspace version is {version})")
        out[src.as_posix()] = dest
    names = sorted(k.rsplit("/", 1)[-1] for k in resources)
    if names != sorted(PES + REPO_FILES):
        raise OverlayError(f"tauri.nsis.conf.json's resources are {names}, expected {sorted(PES + REPO_FILES)}")
    if not any(d == f"shell/{version}/lpk_shell.dll" for d in out.values()):
        raise OverlayError(f"no resource goes to shell/{version}/lpk_shell.dll")
    hooks = gui / HOOKS[arch]
    if not hooks.is_file():
        raise OverlayError(f"missing hooks file: {hooks}")
    conf["bundle"]["resources"] = out
    conf["bundle"]["windows"]["nsis"]["installerHooks"] = hooks.as_posix()
    return conf


def main(argv):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--arch")
    p.add_argument("--bin", type=Path)
    p.add_argument("--repo", type=Path)
    p.add_argument("--out", type=Path)
    p.add_argument("--self-test", action="store_true")
    a = p.parse_args(argv)
    if a.self_test:
        return self_test()
    if not (a.arch and a.bin and a.repo and a.out):
        p.error("--arch, --bin, --repo and --out are required")
    try:
        conf = overlay(a.arch, a.bin, a.repo)
    except OverlayError as e:
        print(f"e7-nsis-overlay: {e}", file=sys.stderr)
        return 1
    a.out.write_text(json.dumps(conf, indent=2) + "\n", encoding="utf-8")
    print(f"e7-nsis-overlay: {a.arch}: {a.out}")
    for src, dest in conf["bundle"]["resources"].items():
        print(f"  {dest} <- {src}")
    print(f"  installerHooks: {conf['bundle']['windows']['nsis']['installerHooks']}")
    return 0


def self_test():
    real = Path(__file__).resolve().parent.parent
    base = json.loads((real / "crates/lpk-gui/tauri.nsis.conf.json").read_text(encoding="utf-8"))
    failures = []

    def check(name, ok):
        print(f"{'ok' if ok else 'FAIL'}: {name}")
        if not ok:
            failures.append(name)

    with tempfile.TemporaryDirectory() as t:
        t = Path(t)

        def layout(conf=None, version="0.0.1", skip=None):
            repo = t / f"repo{len(list(t.iterdir()))}"
            gui = repo / "crates/lpk-gui"
            gui.mkdir(parents=True)
            (repo / "Cargo.toml").write_text(f'[workspace.package]\nversion = "{version}"\n', encoding="utf-8")
            (gui / "tauri.nsis.conf.json").write_text(json.dumps(conf or base), encoding="utf-8")
            for h in HOOKS.values():
                (gui / h).write_text("; hooks\n", encoding="utf-8")
            for f in REPO_FILES:
                (repo / f).write_text(f, encoding="utf-8")
            bin_dir = repo / "bin"
            bin_dir.mkdir()
            for pe in PES:
                if pe != skip:
                    (bin_dir / pe).write_bytes(b"MZ")
            return repo, bin_dir

        def fails(name, *args, contains):
            try:
                overlay(*args)
            except OverlayError as e:
                check(f"{name} ({e})", contains in str(e))
            else:
                check(name, False)

        repo, bin_dir = layout()
        for arch in HOOKS:
            c = overlay(arch, bin_dir, repo)
            res = c["bundle"]["resources"]
            check(f"{arch}: every source is absolute and exists", all(Path(s).is_absolute() and Path(s).is_file() for s in res))
            check(f"{arch}: the destinations are unchanged", sorted(res.values()) == sorted(base["bundle"]["resources"].values()))
            check(f"{arch}: the PEs come from --bin", all(Path(s).parent == bin_dir.resolve() for s, d in res.items() if d.startswith(("bin/", "shell/"))))
            check(f"{arch}: no backslash in a source or the hooks path",
                  not any("\\" in s for s in list(res) + [c["bundle"]["windows"]["nsis"]["installerHooks"]]))
            hooks = Path(c["bundle"]["windows"]["nsis"]["installerHooks"])
            check(f"{arch}: installerHooks is {HOOKS[arch]}", hooks.name == HOOKS[arch] and hooks.is_absolute())
            stripped = json.loads(json.dumps(c))
            stripped["bundle"].pop("resources")
            stripped["bundle"]["windows"]["nsis"].pop("installerHooks")
            orig = json.loads(json.dumps(base))
            orig["bundle"].pop("resources")
            orig["bundle"]["windows"]["nsis"].pop("installerHooks")
            check(f"{arch}: every other key is unchanged", stripped == orig)
        fails("an unknown architecture", "x86", bin_dir, repo, contains="unknown architecture")
        repo, bin_dir = layout(skip="lpk-worker.exe")
        fails("a missing PE", "x64", bin_dir, repo, contains="missing source")
        repo, bin_dir = layout()
        (repo / "LICENSE-MIT").unlink()
        fails("a missing licence", "arm64", bin_dir, repo, contains="missing source")
        repo, bin_dir = layout(version="0.0.2")
        fails("a shell folder of another version", "x64", bin_dir, repo, contains="workspace version is 0.0.2")
        extra = json.loads(json.dumps(base))
        extra["bundle"]["resources"]["../../README.md"] = "README.md"
        repo, bin_dir = layout(conf=extra)
        fails("an unknown resource", "x64", bin_dir, repo, contains="does not know: ../../README.md")
        moved = json.loads(json.dumps(base))
        r = moved["bundle"]["resources"]
        r["../../target/debug/lpk.exe"] = r.pop("../../target/release/lpk.exe")
        repo, bin_dir = layout(conf=moved)
        fails("a PE from another folder", "x64", bin_dir, repo, contains="does not know: ../../target/debug/lpk.exe")
        # The real repository's overlay validates against the real version (the files in --bin are planted).
        _, bin_dir = layout()
        if all((real / f).is_file() for f in REPO_FILES):
            c = overlay("arm64", bin_dir, real)
            check("the real tauri.nsis.conf.json is accepted", len(c["bundle"]["resources"]) == len(PES) + len(REPO_FILES))
    if failures:
        print(f"e7-nsis-overlay self-test: {len(failures)} failed", file=sys.stderr)
        return 1
    print("e7-nsis-overlay self-test: all passed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
