"""E7-2 (release.yml's preflight job): the checks before anything is built.

    python -I scripts/e7-preflight.py version [--repo <root>]
    python -I scripts/e7-preflight.py workflow <file>
    python -I scripts/e7-preflight.py --self-test

`version`: the workspace version (Cargo.toml's `[workspace.package] version`) agrees with workers/Cargo.toml's, with
crates/lpk-gui/tauri.conf.json's `version` and with the shell folder `shell/<v>/` of the NSIS overlay
(crates/lpk-gui/tauri.nsis.conf.json). Prints `version: <v>`, then whether a release could be published:
`releasable: no (unsigned; the updater key is a placeholder)` while tauri.conf.json's updater `pubkey` is the
placeholder string crates/lpk-gui/src/update.rs names as PLACEHOLDER_KEY (the GUI's update gate treats it as "off").
Part 1 of E7-2 signs nothing, so this part never prints `releasable: yes`.

`workflow`: every `uses:` is `owner/repo[/path]@<40 hex>` with the tag in a trailing comment (`# v6.0.0`); the
top-level `permissions:` is `contents: read` and nothing else (and so is any job's own `permissions:`); no
`environment:` key, no `secrets.` and no `id-token` on any line, comments included. A small line-based reader, not a
YAML parser (the standard library has none, and the rules are about lines): it reads keys by their indentation and
does not follow anchors, flow mappings or multi-document files, which release.yml does not use.

Each check exits 1 naming every failure. The standard library only (tomllib: Python 3.11 or later).
"""

import json
import re
import sys
import tempfile
import tomllib
from pathlib import Path

PLACEHOLDER_RE = re.compile(r'pub const PLACEHOLDER_KEY: &str = "([^"]*)";')
USES_RE = re.compile(r"^\s*(?:-\s+)?uses:\s*(?P<value>[^\s#]+)\s*(?:#\s*(?P<comment>.*))?$")
PINNED_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_./-]+)?@[0-9a-f]{40}$")
TAG_RE = re.compile(r"^v\d+(?:\.\d+)*\S*$")
FORBIDDEN = [
    (re.compile(r"(^|[^A-Za-z0-9_-])environment\s*:"), "an `environment:` key"),
    (re.compile(r"secrets\."), "`secrets.`"),
    (re.compile(r"id-token"), "`id-token`"),
]


def workspace_version(path):
    with open(path, "rb") as f:
        return tomllib.load(f)["workspace"]["package"]["version"]


def check_version(repo):
    """(lines to print, errors)."""
    errors = []
    v = workspace_version(repo / "Cargo.toml")
    workers = workspace_version(repo / "workers" / "Cargo.toml")
    if workers != v:
        errors.append(f"workers/Cargo.toml's version is {workers}, the workspace's is {v}")
    gui = repo / "crates" / "lpk-gui"
    conf = json.loads((gui / "tauri.conf.json").read_text(encoding="utf-8"))
    if conf.get("version") != v:
        errors.append(f"tauri.conf.json's version is {conf.get('version')}, the workspace's is {v}")
    nsis = json.loads((gui / "tauri.nsis.conf.json").read_text(encoding="utf-8"))
    shell = [d for d in nsis["bundle"]["resources"].values() if d.startswith("shell/")]
    if shell != [f"shell/{v}/lpk_shell.dll"]:
        errors.append(f"tauri.nsis.conf.json's shell destinations are {shell}, expected shell/{v}/lpk_shell.dll")
    m = PLACEHOLDER_RE.search((gui / "src" / "update.rs").read_text(encoding="utf-8"))
    if not m:
        errors.append("crates/lpk-gui/src/update.rs names no PLACEHOLDER_KEY")
    key = conf.get("plugins", {}).get("updater", {}).get("pubkey")
    if m and key == m.group(1):
        why = "unsigned; the updater key is a placeholder"
    else:
        why = "unsigned"
    return [f"version: {v}", f"releasable: no ({why})"], errors


def top_level_blocks(lines):
    """{key: [(line number, text)]} of the column-0 keys and their indented lines."""
    blocks, key = {}, None
    for n, line in enumerate(lines, 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if not line[0].isspace():
            key = line.split(":", 1)[0].strip()
            blocks.setdefault(key, []).append((n, line))
        elif key is not None:
            blocks[key].append((n, line))
    return blocks


def strip_comment(line):
    return line.split(" #", 1)[0].rstrip() if not line.lstrip().startswith("#") else ""


def permissions_ok(block_lines):
    """block_lines: the `permissions:` line and its indented lines. True when the block is `contents: read` only."""
    first = strip_comment(block_lines[0][1]).split(":", 1)[1].strip()
    body = [strip_comment(l).strip() for _, l in block_lines[1:]]
    body = [b for b in body if b]
    return first == "" and body == ["contents: read"]


def check_workflow(path):
    errors = []
    lines = Path(path).read_text(encoding="utf-8").splitlines()
    uses = 0
    for n, line in enumerate(lines, 1):
        if re.match(r"^\s*(-\s+)?uses:", line):
            uses += 1
            m = USES_RE.match(line)
            if not m or not PINNED_RE.match(m.group("value")):
                errors.append(f"{path}:{n}: not pinned by a 40-hex commit SHA: {line.strip()}")
            elif not (m.group("comment") and TAG_RE.match(m.group("comment").strip())):
                errors.append(f"{path}:{n}: no tag in a trailing comment: {line.strip()}")
        for pattern, what in FORBIDDEN:
            if pattern.search(line):
                errors.append(f"{path}:{n}: {what}: {line.strip()}")
    blocks = top_level_blocks(lines)
    if "permissions" not in blocks:
        errors.append(f"{path}: no top-level permissions")
    elif not permissions_ok(blocks["permissions"]):
        errors.append(f"{path}: the top-level permissions are not `contents: read` alone")
    # Any job's (or step's) own permissions block: the same rule.
    for n, line in enumerate(lines, 1):
        m = re.match(r"^(\s+)permissions:", line)
        if not m:
            continue
        indent = len(m.group(1))
        block = [(n, line)]
        for k in range(n, len(lines)):
            nxt = lines[k]
            if nxt.strip() and len(nxt) - len(nxt.lstrip()) <= indent:
                break
            block.append((k + 1, nxt))
        if not permissions_ok(block):
            errors.append(f"{path}:{n}: a permissions block that is not `contents: read` alone")
    if uses == 0:
        errors.append(f"{path}: no `uses:` line (is this a workflow?)")
    return [f"workflow: {path}: {uses} uses: lines pinned, permissions contents: read, no environment or secrets"], errors


def run(lines_errors):
    lines, errors = lines_errors
    for e in errors:
        print(f"e7-preflight: {e}", file=sys.stderr)
    if errors:
        return 1
    for l in lines:
        print(l)
    return 0


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    if argv and argv[0] == "version" and len(argv) in (1, 3) and (len(argv) == 1 or argv[1] == "--repo"):
        repo = Path(argv[2]) if len(argv) == 3 else Path(__file__).resolve().parent.parent
        return run(check_version(repo))
    if len(argv) == 2 and argv[0] == "workflow":
        return run(check_workflow(argv[1]))
    print(__doc__, file=sys.stderr)
    return 2


GOOD = """\
name: Release
on:
  workflow_dispatch:
permissions:
  contents: read
jobs:
  a:
    runs-on: ubuntu-24.04
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@0123456789abcdef0123456789abcdef01234567 # v6.0.1
      - name: x
        uses: actions/upload-artifact@0123456789abcdef0123456789abcdef01234567  # v7.0.0
        with:
          name: environments
      - run: echo "${{ github.sha }}"
"""


def self_test():
    failures = []

    def check(name, ok):
        print(f"{'ok' if ok else 'FAIL'}: {name}")
        if not ok:
            failures.append(name)

    with tempfile.TemporaryDirectory() as t:
        t = Path(t)

        def wf(text, name):
            p = t / f"{name}.yml"
            p.write_text(text, encoding="utf-8")
            return check_workflow(p)[1]

        check("a pinned workflow passes", wf(GOOD, "good") == [])
        sha = "0123456789abcdef0123456789abcdef01234567"
        cases = [
            ("a tag instead of a SHA", GOOD.replace(f"checkout@{sha} # v6.0.1", "checkout@v6"), "not pinned"),
            ("a short SHA", GOOD.replace(f"checkout@{sha}", "checkout@0123456"), "not pinned"),
            ("a SHA without its tag", GOOD.replace(f"checkout@{sha} # v6.0.1", f"checkout@{sha}"), "no tag"),
            ("a local action", GOOD.replace(f"actions/checkout@{sha}", "./local"), "not pinned"),
            ("write permissions", GOOD.replace("permissions:\n  contents: read", "permissions:\n  contents: write"), "top-level"),
            ("extra top-level permissions", GOOD.replace("permissions:\n  contents: read", "permissions:\n  contents: read\n  packages: read"), "top-level"),
            ("read-all", GOOD.replace("permissions:\n  contents: read", "permissions: read-all"), "top-level"),
            ("no top-level permissions", GOOD.replace("permissions:\n  contents: read\njobs", "jobs"), "no top-level"),
            ("a job's write permissions", GOOD.replace("    permissions:\n      contents: read", "    permissions:\n      contents: write"), "permissions block"),
            ("an environment", GOOD.replace("    runs-on: ubuntu-24.04", "    runs-on: ubuntu-24.04\n    environment: release"), "environment"),
            ("a secret", GOOD.replace("github.sha", "secrets.TOKEN"), "secrets."),
            ("OIDC", GOOD.replace("    permissions:\n      contents: read", "    permissions:\n      contents: read\n      id-token: write"), "id-token"),
        ]
        for name, text, expect in cases:
            errs = wf(text, name.replace(" ", "-").replace("'", ""))
            check(f"{name} fails ({errs[0] if errs else 'no error'})", any(expect in e for e in errs))

        def repo(version="0.0.1", workers="0.0.1", tauri="0.0.1", shell="0.0.1", key="PLACE"):
            r = t / f"repo-{len(list(t.iterdir()))}"
            (r / "workers").mkdir(parents=True)
            (r / "crates/lpk-gui/src").mkdir(parents=True)
            (r / "Cargo.toml").write_text(f'[workspace.package]\nversion = "{version}"\n', encoding="utf-8")
            (r / "workers/Cargo.toml").write_text(f'[workspace.package]\nversion = "{workers}"\n', encoding="utf-8")
            (r / "crates/lpk-gui/tauri.conf.json").write_text(
                json.dumps({"version": tauri, "plugins": {"updater": {"pubkey": key}}}), encoding="utf-8")
            (r / "crates/lpk-gui/tauri.nsis.conf.json").write_text(
                json.dumps({"bundle": {"resources": {"a": "bin/lpk.exe", "b": f"shell/{shell}/lpk_shell.dll"}}}),
                encoding="utf-8")
            (r / "crates/lpk-gui/src/update.rs").write_text('pub const PLACEHOLDER_KEY: &str = "PLACE";\n', encoding="utf-8")
            return r

        lines, errs = check_version(repo())
        check("agreeing versions pass", errs == [] and lines[0] == "version: 0.0.1")
        check("the placeholder key is not releasable",
              lines[1] == "releasable: no (unsigned; the updater key is a placeholder)")
        lines, errs = check_version(repo(key="RWS..."))
        check("a real key is still unsigned", errs == [] and lines[1] == "releasable: no (unsigned)")
        for name, kw, expect in [
            ("the workers' version", {"workers": "0.0.2"}, "workers/Cargo.toml"),
            ("tauri.conf.json's version", {"tauri": "0.0.2"}, "tauri.conf.json's version"),
            ("the overlay's shell folder", {"shell": "0.0.2"}, "shell destinations"),
        ]:
            errs = check_version(repo(**kw))[1]
            check(f"{name} disagreeing fails ({errs[0] if errs else 'no error'})", any(expect in e for e in errs))
        real = Path(__file__).resolve().parent.parent
        lines, errs = check_version(real)
        check(f"the repository's versions agree ({lines[0]})", errs == [])
    if failures:
        print(f"e7-preflight self-test: {len(failures)} failed", file=sys.stderr)
        return 1
    print("e7-preflight self-test: all passed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
