"""E7-5 (CI's `installer` job only): move the checkout from version <old> to <new> everywhere a binary takes it from,
so that the N+1 installer's binaries all differ from N's (R329, R331):
- both workspaces' `[workspace.package] version` (the root's and workers/'s);
- every version requirement on a path dependency of one of LitePack's own crates, in every manifest of the
  repository (members, workers/, fuzz/ and the patched crates under vendor/), whatever its form: an inline table,
  a `[dependencies.<name>]` section, in [dependencies], [dev-dependencies], [build-dependencies] or a
  `[target.<cfg>.*]` table (0.0.x requirements match one version only).
The requirements are found by parsing each manifest (tomllib, read only); then exactly those lines are edited, and
every manifest is parsed again to prove none is left at <old>. The caller then runs `cargo update --workspace
--offline` in both workspaces. Edits files in place: run it only on a disposable checkout.

    python -I scripts/e7-bump-version.py <old> <new> [<repository root>]
    python -I scripts/e7-bump-version.py --self-test
"""

import re
import sys
import tempfile
import tomllib
from pathlib import Path

DEP_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")
SKIP = {"target", ".git", "node_modules"}


def manifests(root):
    out = []
    for p in sorted(root.rglob("Cargo.toml")):
        if not SKIP.intersection(p.relative_to(root).parts[:-1]):
            out.append(p)
    return out


def own_crates(root, old):
    """LitePack's own crates: every package whose version is <old> or the workspace's, outside vendor/."""
    names = set()
    for p in manifests(root):
        if "vendor" in p.relative_to(root).parts:
            continue
        pkg = tomllib.loads(p.read_text(encoding="utf-8")).get("package", {})
        v = pkg.get("version")
        if pkg.get("name") and (v == old or (isinstance(v, dict) and v.get("workspace"))):
            names.add(pkg["name"])
    return names


def findings(doc, own, old):
    """[(header, dependency key, inline)] of `doc`'s path requirements on `own` crates at <old>. `header` is the
    table's dotted path as written in the manifest's section headers (without the dependency key)."""
    found = []

    def scan(tables, prefix):
        for t in DEP_TABLES:
            for key, spec in tables.get(t, {}).items():
                if not isinstance(spec, dict) or "path" not in spec or spec.get("version") != old:
                    continue
                if spec.get("package", key) in own:
                    found.append((prefix + t, key))
    scan(doc, "")
    for cfg, tables in doc.get("target", {}).items():
        scan(tables, f"target.{cfg}.")
    if doc.get("workspace", {}).get("package", {}).get("version") == old:
        found.append(("workspace.package", None))
    return found


def header_of(line):
    """A section header's dotted path, with quoted parts unquoted; None for other lines."""
    m = re.fullmatch(r"\s*\[([^\[\]]+)\]\s*(#.*)?", line)
    if not m:
        return None
    parts = re.findall(r"""'([^']*)'|"([^"]*)"|([^.\s]+)""", m.group(1))
    return ".".join(a or b or c for a, b, c in parts)


def bump_text(text, wanted, old, new):
    """Edit exactly the version lines of `wanted` [(header, key)]; returns (text, edited count)."""
    want = set(wanted)
    req = f'version = "{old}"'
    lines = text.splitlines(keepends=True)
    section, edited = None, 0
    for i, line in enumerate(lines):
        h = header_of(line)
        if h is not None:
            section = h
            continue
        if req not in line:
            continue
        stripped = line.strip()
        # `version = "old"` alone in [workspace.package] or in a [<table>.<key>] section.
        if stripped.startswith(req):
            if section == "workspace.package" and ("workspace.package", None) in want:
                lines[i] = line.replace(req, f'version = "{new}"', 1)
                edited += 1
                continue
            head, _, key = (section or "").rpartition(".")
            if (head, key) in want:
                lines[i] = line.replace(req, f'version = "{new}"', 1)
                edited += 1
            continue
        # `<key> = { ..., version = "old", path = ... }` in a [<table>] section.
        m = re.match(r"""\s*(?:"([^"]+)"|'([^']+)'|([A-Za-z0-9_-]+))\s*=\s*\{""", line)
        if m and (section, m.group(1) or m.group(2) or m.group(3)) in want:
            lines[i] = line.replace(req, f'version = "{new}"', 1)
            edited += 1
    return "".join(lines), edited


def bump(root, old, new):
    root = Path(root)
    own = own_crates(root, old)
    total = 0
    for p in manifests(root):
        text = p.read_text(encoding="utf-8")
        wanted = findings(tomllib.loads(text), own, old)
        if not wanted:
            continue
        new_text, edited = bump_text(text, wanted, old, new)
        if edited != len(wanted):
            raise SystemExit(f"{p}: {len(wanted)} requirements at {old}, {edited} edited: {wanted}")
        p.write_text(new_text, encoding="utf-8", newline="")
        total += edited
        print(f"{p.relative_to(root)}: {edited} moved to {new}")
    # Proof: nothing left at <old>.
    left = [(p, f) for p in manifests(root) for f in findings(tomllib.loads(p.read_text(encoding="utf-8")), own, old)]
    if left:
        raise SystemExit(f"left at {old}: {left}")
    print(f"{total} version requirements and workspace versions moved from {old} to {new}; none left")
    return total


def self_test():
    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        (root / "a").mkdir()
        (root / "b").mkdir()
        (root / "vendor" / "v").mkdir(parents=True)
        (root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["a", "b"]\nexclude = ["vendor"]\n\n[workspace.package]\nversion = "0.0.1"\n',
            encoding="utf-8")
        (root / "a" / "Cargo.toml").write_text(
            '[package]\nname = "a"\nversion.workspace = true\n', encoding="utf-8")
        (root / "b" / "Cargo.toml").write_text(
            '[package]\nname = "b"\nversion.workspace = true\n\n[dependencies]\n'
            'a = { path = "../a", version = "0.0.1" }\nserde = { version = "0.0.1" }\n\n'
            "[target.'cfg(windows)'.dev-dependencies]\nrenamed = { package = \"a\", version = \"0.0.1\", path = \"../a\" }\n",
            encoding="utf-8")
        (root / "vendor" / "v" / "Cargo.toml").write_text(
            '[package]\nname = "v"\nversion = "0.0.1"\n\n[dependencies.a]\nversion = "0.0.1"\npath = "../../a"\n\n'
            '[dependencies.other]\nversion = "0.0.1"\n', encoding="utf-8")
        assert bump(root, "0.0.1", "0.0.2") == 4
        b = tomllib.loads((root / "b" / "Cargo.toml").read_text(encoding="utf-8"))
        assert b["dependencies"]["a"]["version"] == "0.0.2"
        assert b["dependencies"]["serde"]["version"] == "0.0.1"
        assert b["target"]["cfg(windows)"]["dev-dependencies"]["renamed"]["version"] == "0.0.2"
        v = tomllib.loads((root / "vendor" / "v" / "Cargo.toml").read_text(encoding="utf-8"))
        assert v["dependencies"]["a"]["version"] == "0.0.2"
        assert v["dependencies"]["other"]["version"] == "0.0.1"
        assert v["package"]["version"] == "0.0.1"
        ws = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
        assert ws["workspace"]["package"]["version"] == "0.0.2"
    print("e7-bump-version self-test: ok")


def main(argv):
    if argv == ["--self-test"]:
        self_test()
        return 0
    if len(argv) not in (2, 3):
        print(__doc__, file=sys.stderr)
        return 2
    bump(argv[2] if len(argv) == 3 else ".", argv[0], argv[1])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
