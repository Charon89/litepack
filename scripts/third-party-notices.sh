#!/usr/bin/env bash
# Writes THIRD-PARTY-NOTICES to stdout (PLAN LIC-2): cargo-about over the shipped packages only, never the
# workspace. Usage, from the repository root:
#   scripts/third-party-notices.sh > THIRD-PARTY-NOTICES          (regenerate)
#   scripts/third-party-notices.sh --check                         (CI: fail on a difference or GPL text)
set -euo pipefail
cd "$(dirname "$0")/.."

render() {
  # --locked: the notices follow Cargo.lock, never a fresh resolution; --fail: an unidentified licence is an error.
  # tr: LF only, whatever line ends a licence file has (the file is committed with LF on every machine).
  cargo about generate --locked --fail --config about.toml -m crates/lpk-cli/Cargo.toml about.hbs | tr -d '\r'
  cargo about generate --locked --fail --config about.toml -m crates/lpk-check/Cargo.toml about-check.hbs | tr -d '\r'
  # E5-2: the GUI, for the Windows triples it ships on (about-gui.toml), with the WebView2 loader's notice.
  cargo about generate --locked --fail --config about-gui.toml -m crates/lpk-gui/Cargo.toml about-gui.hbs | tr -d '\r'
}

if [[ "${1:-}" == "--check" ]]; then
  tmp="$(mktemp)"
  trap 'rm -f "$tmp"' EXIT
  render > "$tmp"
  if ! git diff --no-index --exit-code THIRD-PARTY-NOTICES "$tmp"; then
    echo "THIRD-PARTY-NOTICES is out of date: run scripts/third-party-notices.sh > THIRD-PARTY-NOTICES" >&2
    exit 1
  fi
  # No GPL-family licence text may ship (P0-6): the title line of the GPL, LGPL and AGPL texts, in the
  # notices and in our own and vendored sources.
  if grep -nE "GNU (AFFERO |LESSER |LIBRARY )?GENERAL PUBLIC LICENSE" THIRD-PARTY-NOTICES; then
    echo "THIRD-PARTY-NOTICES contains a GPL-family licence text" >&2
    exit 1
  fi
  if grep -rnE "GNU (AFFERO |LESSER |LIBRARY )?GENERAL PUBLIC LICENSE" crates vendor; then
    echo "crates/ or vendor/ contains a GPL-family licence text" >&2
    exit 1
  fi
  echo "THIRD-PARTY-NOTICES is current; no GPL-family licence text"
else
  render
fi
