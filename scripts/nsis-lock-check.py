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

import json
import re
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
    # Manifests only: workspace = true resolved, target tables read, path dependencies skipped.
    ws = [
        '[workspace]\nmembers = ["a", "m"]\n[workspace.dependencies]\nsemver = "1"\nsyn = "2"\n',
        '[package]\nname = "a"\n[dependencies]\nsemver = { workspace = true }\nm = { path = "../m" }\n'
        '[target."cfg(windows)".dependencies]\nwindows-sys = "0.59"\n[build-dependencies]\ncc = "1"\n',
        '[package]\nname = "m"\n[lib]\nproc-macro = true\n[dependencies]\nsyn = { workspace = true }\n',
    ]
    lines, missing = check_direct(ws, notices)
    assert missing == [], (missing, lines)
    assert "syn 2 (m's [dependencies]): not shipped (m: proc-macro = true)" in lines, lines
    assert "cc 1 (a's [build-dependencies]): not shipped ([build-dependencies])" in lines, lines
    assert any(x.startswith("windows-sys 0.59 (a's [target.cfg(windows).dependencies]): in the notices as 0.61.2")
               for x in lines), lines
    lines, missing = check_direct(ws[:2] + [ws[2].replace("proc-macro = true", "")], notices)
    assert missing == ["syn"], missing
    # cargo metadata: normal edges only, never through a proc-macro; the licence must match a title.
    def pkg(i, name, lic, source="registry+x", macro=False):
        kind = ["proc-macro"] if macro else ["lib"]
        return {"id": i, "name": name, "version": "1.0.0", "license": lic, "source": source,
                "targets": [{"kind": kind}]}

    def edge(i, *kinds):
        return {"pkg": i, "dep_kinds": [{"kind": k} for k in kinds]}
    meta = {
        "packages": [pkg("r", "plugin", "MIT OR Apache-2.0", None), pkg("s", "semver", "MIT OR Apache-2.0"),
                     pkg("m", "derive", "MIT", macro=True), pkg("q", "quote", "MIT"),
                     pkg("c", "cc", "MIT"), pkg("g", "gpl-thing", "GPL-3.0")],
        "resolve": {"nodes": [
            {"id": "r", "deps": [edge("s", None), edge("m", None), edge("c", "build"), edge("g", None)]},
            {"id": "m", "deps": [edge("q", None)]},
        ]},
    }
    notices_meta = notices.replace("Apache License 2.0", "Apache License 2.0 (Apache-2.0)").replace(
        "MIT License", "MIT License (MIT)") + "\n- gpl-thing 1.0.0 <https://example>\n"
    lines, missing = check_metadata(meta, notices_meta, "plugin")
    assert missing == ["gpl-thing 1.0.0"], (missing, lines)
    assert any(x.startswith("semver 1.0.0 (MIT OR Apache-2.0): in the notices under") for x in lines), lines
    assert not any(x.startswith(("quote", "derive", "cc ")) for x in lines), lines
    print("nsis-lock-check self-test: ok")


def direct_dependencies(manifest_texts):
    """Without a lock (the plugin's tag has none, CI run 42486016300): each workspace crate's direct registry
    dependencies from its manifests, [target.*] tables and `workspace = true` included. Returns
    (crate -> proc_macro), [(crate, table, dependency, requirement)]; path dependencies are the workspace's own."""
    parsed = [tomllib.loads(t) for t in manifest_texts]
    ws = {}
    for m in parsed:
        ws.update(m.get("workspace", {}).get("dependencies", {}))
    crates, out = {}, []
    for m in parsed:
        pkg = m.get("package", {}).get("name")
        if not pkg:
            continue
        crates[pkg] = bool(m.get("lib", {}).get("proc-macro"))
        tables = [(t, m.get(t, {})) for t in ("dependencies", "build-dependencies", "dev-dependencies")]
        for target, sub in m.get("target", {}).items():
            tables += [(f"target.{target}.{t}", sub.get(t, {}))
                       for t in ("dependencies", "build-dependencies", "dev-dependencies")]
        for table, deps in tables:
            for name, spec in deps.items():
                if isinstance(spec, dict) and spec.get("workspace"):
                    spec = ws.get(name, spec)
                if isinstance(spec, dict) and "path" in spec:
                    continue
                req = spec if isinstance(spec, str) else spec.get("version", "?")
                out.append((pkg, table, spec.get("package", name) if isinstance(spec, dict) else name, req))
    return crates, out


def check_direct(manifest_texts, notices_text):
    """(lines, missing) for the manifests-only check: a dependency of a proc-macro crate or in a build or dev table
    does not ship (the manifest says so); every other must be in the notices under some version."""
    crates, deps = direct_dependencies(manifest_texts)
    versions = {}
    for (name, version), titles in notice_licences(notices_text).items():
        versions.setdefault(name, {})[version] = titles
    lines, missing = [], []
    for pkg, table, name, req in sorted(set(deps)):
        where = f"{name} {req} ({pkg}'s [{table}])"
        if crates.get(pkg):
            lines.append(f"{where}: not shipped ({pkg}: proc-macro = true)")
        elif "build-dependencies" in table or "dev-dependencies" in table:
            lines.append(f"{where}: not shipped ([{table.rsplit('.', 1)[-1]}])")
        elif name in versions:
            listed = "; ".join(f"{v}: {', '.join(sorted(t))}" for v, t in sorted(versions[name].items()))
            lines.append(f"{where}: in the notices as {listed}")
        else:
            lines.append(f"{where}: NOT IN THE NOTICES")
            missing.append(name)
    lines.append("no Cargo.lock at the tag: the exact versions and the transitive crates are not checked")
    return lines, sorted(set(missing))


def check_metadata(meta, notices_text, root):
    """R325: (lines, missing) over `cargo metadata --filter-platform <the DLL's target>` of the plugin's
    workspace: the crates `root` links, through normal dependencies only (no build or dev dependency) and never
    through a proc-macro crate (which runs in the compiler, not in the DLL). Each registry crate must be listed in
    the notices under a licence whose SPDX id is in the crate's own `license` expression."""
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    is_macro = lambda p: any("proc-macro" in t.get("kind", []) for t in p.get("targets", []))  # noqa: E731
    same = lambda a, b: a.replace("-", "_") == b.replace("-", "_")  # noqa: E731
    start = [i for i, p in pkgs.items() if same(p["name"], root) and p.get("source") is None]
    if len(start) != 1:
        return [f"no workspace crate named {root}"], [root]
    seen, stack = set(), list(start)
    while stack:
        i = stack.pop()
        if i in seen:
            continue
        seen.add(i)
        for d in nodes.get(i, {}).get("deps", []):
            normal = any(k.get("kind") is None for k in d.get("dep_kinds", []))
            if normal and not is_macro(pkgs[d["pkg"]]):
                stack.append(d["pkg"])
    titles = {}
    for (name, _version), ts in notice_licences(notices_text).items():
        titles.setdefault(name, set()).update(ts)
    lines, missing = [], []
    for i in sorted(seen, key=lambda i: (pkgs[i]["name"], pkgs[i]["version"])):
        p = pkgs[i]
        name, version, lic = p["name"], p["version"], p.get("license") or "(none)"
        if p.get("source") is None:
            lines.append(f"{name} {version}: the plugin's own workspace ({lic})")
            continue
        ids = set(re.findall(r"[A-Za-z0-9.+-]+", lic)) - {"OR", "AND", "WITH"}
        match = sorted(t for t in titles.get(name, ()) if any(t.endswith(f"({x})") for x in ids))
        if match:
            lines.append(f"{name} {version} ({lic}): in the notices under {'; '.join(match)}")
        else:
            listed = "; ".join(sorted(titles.get(name, ()))) or "not listed"
            lines.append(f"{name} {version} ({lic}): NOT COVERED (notices: {listed})")
            missing.append(f"{name} {version}")
    lines.append("resolved today from the tag's manifests (no Cargo.lock is published): these are today's "
                 "versions, not necessarily the ones the DLL was built with")
    return lines, missing


def main(argv):
    if argv == ["--self-test"]:
        self_test()
        return 0
    if len(argv) == 4 and argv[0] == "--metadata":
        with open(argv[1], encoding="utf-8") as f:
            meta = json.load(f)
        with open(argv[2], encoding="utf-8") as f:
            lines, missing = check_metadata(meta, f.read(), argv[3])
        for line in lines:
            print(line)
        if missing:
            print(f"not covered by the notices: {', '.join(missing)}", file=sys.stderr)
            return 1
        print(f"{argv[3]}: every registry crate it links is in the notices under its licence")
        return 0
    if len(argv) == 3 and argv[0] == "--manifests-only":
        import pathlib
        texts = [p.read_text(encoding="utf-8") for p in sorted(pathlib.Path(argv[1]).rglob("Cargo.toml"))]
        with open(argv[2], encoding="utf-8") as f:
            lines, missing = check_direct(texts, f.read())
        for line in lines:
            print(line)
        if missing:
            print(f"not in the notices: {', '.join(missing)}", file=sys.stderr)
            return 1
        print("nsis_tauri_utils' manifests: every direct dependency that ships is in the notices")
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
