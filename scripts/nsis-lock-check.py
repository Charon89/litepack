#!/usr/bin/env python3
"""R272 (E7-5): every crate of nsis_tauri_utils' Cargo.lock appears, with its licence, in THIRD-PARTY-NOTICES.

    python3 -I scripts/nsis-lock-check.py <Cargo.lock> <THIRD-PARTY-NOTICES>
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


def check(lock_text, notices_text):
    """(lines to print, missing crates)."""
    lock = tomllib.loads(lock_text)
    licences = notice_licences(notices_text)
    names = {}
    for (name, version), titles in licences.items():
        names.setdefault(name, set()).add(version)
    out, missing = [], []
    for pkg in sorted(lock.get("package", []), key=lambda p: (p["name"], p["version"])):
        name, version = pkg["name"], pkg["version"]
        if "source" not in pkg:
            out.append(f"{name} {version}: the plugin's own workspace (the notices' installer section)")
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
    print("nsis-lock-check self-test: ok")


def main(argv):
    if argv == ["--self-test"]:
        self_test()
        return 0
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    with open(argv[0], encoding="utf-8") as f:
        lock_text = f.read()
    with open(argv[1], encoding="utf-8") as f:
        notices_text = f.read()
    out, missing = check(lock_text, notices_text)
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
