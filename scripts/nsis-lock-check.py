#!/usr/bin/env python3
"""R272 (E7-5): every crate of nsis_tauri_utils' Cargo.lock appears, with its licence, in THIRD-PARTY-NOTICES.

    python3 -I scripts/nsis-lock-check.py <Cargo.lock> <THIRD-PARTY-NOTICES> [--manifests <source folder>]
    python3 -I scripts/nsis-lock-check.py --self-test

The lock is the plugin's own, at its release tag (scripts/nsis-notice.sh downloads it in CI and checks its pinned
SHA-256 first; this script reads files only). Each registry crate of the lock (name and version) must be listed under
a licence of the notices ("- <name> <version> <url>" below a licence's title); the licence printed is that title. The
crates of the plugin's own workspace (no `source`) are the plugin, whose licence text is the notices' installer
section. Prints one line per crate; exits 1 listing the crates the notices do not cover.
"""

import sys
import tomllib

RULE = "=" * 80


def notice_licences(text):
    """(name, version) -> the licence titles the notices list the crate under."""
    found = {}
    title = None
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if line == RULE:
            continue
        if i > 0 and lines[i - 1] == RULE:
            title = line.strip()
            continue
        if line.startswith("- ") and title:
            parts = line[2:].split()
            if len(parts) >= 2:
                found.setdefault((parts[0], parts[1]), set()).add(title)
    return found


def read_manifests(texts):
    """The plugin's own manifests: package name -> {proc_macro, build, dev} (dependency names)."""
    out = {}
    for text in texts:
        m = tomllib.loads(text)
        pkg = m.get("package", {}).get("name")
        if not pkg:
            continue
        out[pkg] = {
            "proc_macro": bool(m.get("lib", {}).get("proc-macro")),
            "build": set(m.get("build-dependencies", {})),
            "dev": set(m.get("dev-dependencies", {})),
        }
    return out


def not_shipped(lock, manifests):
    """Registry crates that do not ship in the DLL, by the plugin's manifests alone: reached from the plugin's
    workspace only through a workspace crate with `proc-macro = true` or through a workspace crate's
    [build-dependencies] or [dev-dependencies]. name -> the reason. The lock alone cannot tell a registry proc-macro
    from a library: what such a crate pulls in stays "shipped" here, for the controller to judge."""
    deps = {p["name"]: [d.split()[0] for d in p.get("dependencies", [])] for p in lock.get("package", [])}
    own = {p["name"] for p in lock.get("package", []) if "source" not in p}

    def walk(starts, skip_workspace_edges):
        seen, stack = set(), list(starts)
        while stack:
            n = stack.pop()
            if n in seen:
                continue
            seen.add(n)
            info = manifests.get(n)
            for d in deps.get(n, []):
                if skip_workspace_edges and n in own and info and (
                        d in info["build"] or d in info["dev"] or manifests.get(d, {}).get("proc_macro")):
                    continue
                stack.append(d)
        return seen

    roots = [n for n in own if not manifests.get(n, {}).get("proc_macro")]
    shipped = walk(roots, True)
    reasons = {}
    for n in own:
        info = manifests.get(n)
        if not info:
            continue
        starts = [(d, f"{n}'s [build-dependencies]") for d in info["build"]]
        starts += [(d, f"{n}'s [dev-dependencies]") for d in info["dev"]]
        if info["proc_macro"]:
            starts.append((n, f"{n}: proc-macro = true"))
        for start, why in starts:
            for c in walk([start], False) - shipped:
                reasons.setdefault(c, set()).add(why)
    return {n: "; ".join(sorted(r)) for n, r in reasons.items()}


def check(lock_text, notices_text, manifest_texts=()):
    """(lines to print, missing crates)."""
    lock = tomllib.loads(lock_text)
    licences = notice_licences(notices_text)
    skipped = not_shipped(lock, read_manifests(manifest_texts)) if manifest_texts else {}
    names = {}
    for (name, version), titles in licences.items():
        names.setdefault(name, set()).add(version)
    out, missing = [], []
    for pkg in sorted(lock.get("package", []), key=lambda p: (p["name"], p["version"])):
        name, version = pkg["name"], pkg["version"]
        if "source" not in pkg:
            out.append(f"{name} {version}: the plugin's own workspace (the notices' installer section)")
            continue
        if name in skipped:
            out.append(f"{name} {version}: not shipped (only behind {skipped[name]})")
            continue
        titles = licences.get((name, version))
        if titles:
            out.append(f"{name} {version}: {'; '.join(sorted(titles))}")
        else:
            others = ", ".join(sorted(names.get(name, ()))) or "none"
            out.append(f"{name} {version}: NOT IN THE NOTICES (versions listed: {others})")
            missing.append(f"{name} {version}")
    return out, missing


def self_test():
    notices = "\n".join([
        RULE, "Licences of the crates in lpk", RULE, RULE, "MIT License", "", "Used by:",
        "- semver 1.0.28 <https://example>", "- windows-sys 0.61.2 <https://example>", "", "text", RULE,
        "Apache License 2.0", "", "Used by:", "- semver 1.0.28 <https://example>", "",
    ])
    lock = "\n".join([
        "version = 4", "[[package]]", 'name = "nsis_tauri_utils"', 'version = "0.5.3"',
        "[[package]]", 'name = "semver"', 'version = "1.0.28"', 'source = "registry+https://example"',
        "[[package]]", 'name = "windows-sys"', 'version = "0.59.0"', 'source = "registry+https://example"',
    ])
    out, missing = check(lock, notices)
    assert missing == ["windows-sys 0.59.0"], missing
    assert "semver 1.0.28: Apache License 2.0; MIT License" in out, out
    assert any("windows-sys 0.59.0: NOT IN THE NOTICES (versions listed: 0.61.2)" == o for o in out), out
    assert any(o.startswith("nsis_tauri_utils 0.5.3: the plugin's own") for o in out), out
    out, missing = check(lock.replace("0.59.0", "0.61.2"), notices)
    assert missing == [], missing
    # The manifests: a workspace proc-macro's dependency and a build-dependency do not ship; a crate also reached
    # from the library does.
    lock2 = "\n".join([
        "version = 4",
        "[[package]]", 'name = "plugin"', 'version = "0.1.0"', 'dependencies = ["fnmacro", "semver", "cc"]',
        "[[package]]", 'name = "fnmacro"', 'version = "0.1.0"', 'dependencies = ["syn", "semver"]',
        "[[package]]", 'name = "syn"', 'version = "2.0.0"', 'source = "registry+x"',
        "[[package]]", 'name = "cc"', 'version = "1.0.0"', 'source = "registry+x"',
        "[[package]]", 'name = "semver"', 'version = "1.0.28"', 'source = "registry+x"',
    ])
    manifests = [
        '[package]\nname = "plugin"\n[dependencies]\nfnmacro = "0.1"\nsemver = "1"\n[build-dependencies]\ncc = "1"\n',
        '[package]\nname = "fnmacro"\n[lib]\nproc-macro = true\n[dependencies]\nsyn = "2"\nsemver = "1"\n',
    ]
    out, missing = check(lock2, notices, manifests)
    assert missing == [], (missing, out)
    assert "syn 2.0.0: not shipped (only behind fnmacro: proc-macro = true)" in out, out
    assert "cc 1.0.0: not shipped (only behind plugin's [build-dependencies])" in out, out
    assert "semver 1.0.28: Apache License 2.0; MIT License" in out, out
    out, missing = check(lock2, notices)
    assert missing == ["cc 1.0.0", "syn 2.0.0"], missing
    print("nsis-lock-check self-test: ok")


def main(argv):
    if argv == ["--self-test"]:
        self_test()
        return 0
    manifest_texts = []
    if len(argv) == 4 and argv[2] == "--manifests":
        import pathlib
        manifest_texts = [p.read_text(encoding="utf-8") for p in sorted(pathlib.Path(argv[3]).rglob("Cargo.toml"))]
        argv = argv[:2]
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    with open(argv[0], encoding="utf-8") as f:
        lock_text = f.read()
    with open(argv[1], encoding="utf-8") as f:
        notices_text = f.read()
    out, missing = check(lock_text, notices_text, manifest_texts)
    for line in out:
        print(line)
    if missing:
        print(f"{len(missing)} crate(s) of nsis_tauri_utils' Cargo.lock not in the notices: {', '.join(missing)}",
              file=sys.stderr)
        return 1
    print(f"nsis_tauri_utils' Cargo.lock: {len(out)} crates, every registry crate in the notices")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
