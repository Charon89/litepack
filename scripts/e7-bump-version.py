"""E7-5 (CI's `installer` job only): move the checkout from version <old> to <new> everywhere a binary takes it from,
so that the N+1 installer's binaries all differ from N's: both workspaces' `[workspace.package] version`, and every
path dependency's `version = "<old>"` requirement (0.0.x requirements match one version only). The lockfiles are then
updated with `cargo update --workspace --offline` by the caller. Edits files in place; never run it on a working copy
you want to keep.

    python -I scripts/e7-bump-version.py <old> <new>
"""

import sys
from pathlib import Path


def bump(text, old, new):
    out, in_package = [], False
    for line in text.splitlines(keepends=True):
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[workspace.package]"
        if in_package and stripped == f'version = "{old}"':
            line = line.replace(f'"{old}"', f'"{new}"')
        elif "path =" in line and f'version = "{old}"' in line:
            line = line.replace(f'version = "{old}"', f'version = "{new}"')
        out.append(line)
    return "".join(out)


def main(argv):
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    old, new = argv
    changed = 0
    for manifest in sorted([Path("Cargo.toml"), Path("workers/Cargo.toml"),
                            *Path("crates").glob("*/Cargo.toml"), *Path("workers").glob("*/Cargo.toml")]):
        text = manifest.read_text(encoding="utf-8")
        new_text = bump(text, old, new)
        if new_text != text:
            manifest.write_text(new_text, encoding="utf-8")
            changed += 1
            print(f"{manifest}: {old} -> {new}")
    print(f"{changed} manifests moved to {new}")
    return 0 if changed else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
