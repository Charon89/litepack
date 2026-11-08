#!/usr/bin/env bash
# Writes THIRD-PARTY-NOTICES to stdout (PLAN LIC-2, E7-8): cargo-about over the shipped packages only, never the
# workspace, then the installer's static section. Usage, from the repository root:
#   scripts/third-party-notices.sh > THIRD-PARTY-NOTICES          (regenerate)
#   scripts/third-party-notices.sh --check                         (CI: fail on a difference or GPL text)
set -euo pipefail
cd "$(dirname "$0")/.."

# E7-8: the line of about-gui.hbs that stands for the WebView2 SDK's LICENSE.txt (notices/webview2.toml's pin).
webview2_marker='@@WEBVIEW2_LICENSE_TXT@@'

# The value of `key = "value"` in notices/webview2.toml; fails when it is missing or empty.
webview2_pin() {
  local value
  value="$(sed -n "s/^$1 = \"\\([^\"]*\\)\"\$/\\1/p" notices/webview2.toml)"
  if [[ -z "$value" ]]; then
    echo "notices/webview2.toml: no value for $1" >&2
    exit 1
  fi
  printf '%s' "$value"
}
webview2_com_sys_version="$(webview2_pin crate_version)"
webview2_version="$(webview2_pin version)"

# Replaces the marker line with notices/webview2/LICENSE.txt, verbatim (line ends LF, like the rest of the file),
# and the version markers with the pin's values (index/substr, not gsub, so no character of a value is special);
# fails if a marker is left.
with_webview2_licence() {
  awk -v marker="$webview2_marker" -v file="notices/webview2/LICENSE.txt" \
    -v crate="$webview2_com_sys_version" -v package="$webview2_version" '
    function put(s, m, v,   i) {
      while ((i = index(s, m)) > 0) s = substr(s, 1, i - 1) v substr(s, i + length(m))
      return s
    }
    $0 == marker { while ((getline line < file) > 0) print line; close(file); next }
    {
      line = put(put($0, "@@WEBVIEW2_COM_SYS_VERSION@@", crate), "@@WEBVIEW2_VERSION@@", package)
      if (line ~ /@@WEBVIEW2_[A-Z0-9_]*@@/) { print "about-gui.hbs: marker left unreplaced: " line > "/dev/stderr"; failed = 1 }
      print line
    }
    END { exit failed }
  '
}

# E7-8: the NSIS installer's own code (the stub with the zlib compressor, the NSIS plugins and nsis_tauri_utils),
# from the texts and pins in notices/nsis/ (pin.toml; CI checks them against the archives the bundler fetches).
nsis_section() {
  cat <<'EOF'
================================================================================
The installer (the NSIS setup program, Windows)
================================================================================

LitePack's installer is built by tauri-bundler 2.10.4 (through tauri-cli 2.12.4) with NSIS 3.13
(https://nsis.sourceforge.io/). The setup program contains NSIS's installer stub with its zlib compression
module, the NSIS plugins System, nsDialogs, StartMenu and NSISdl, and Tauri's plugin nsis_tauri_utils 0.5.3
(notices/nsis/pin.toml). The stub and those plugins are under the zlib/libpng licence of NSIS's COPYING,
reproduced verbatim below (LZMA is not used); nsis_tauri_utils is licensed MIT OR Apache-2.0, and its MIT text
follows. Its crates windows-sys and semver are licensed MIT OR Apache-2.0, texts reproduced above.

--- NSIS 3.13: COPYING ---

EOF
  cat notices/nsis/COPYING
  cat <<'EOF'

--- nsis_tauri_utils 0.5.3 (https://github.com/tauri-apps/nsis-tauri-utils): LICENSE_MIT ---

EOF
  cat notices/nsis/nsis_tauri_utils-LICENSE_MIT
}

render() {
  # --locked: the notices follow Cargo.lock, never a fresh resolution; --fail: an unidentified licence is an error.
  # tr: LF only, whatever line ends a licence file has (the file is committed with LF on every machine).
  cargo about generate --locked --fail --config about.toml -m crates/lpk-cli/Cargo.toml about.hbs | tr -d '\r'
  cargo about generate --locked --fail --config about.toml -m crates/lpk-check/Cargo.toml about-check.hbs | tr -d '\r'
  # E4-4: the libarchive worker that ships beside lpk (its C codecs; libarchive's COPYING is in about.hbs).
  cargo about generate --locked --fail --config about.toml -m crates/lpk-worker-libarchive/Cargo.toml about-libarchive.hbs | tr -d '\r'
  # E7-8: the sandboxed worker (lpk-worker.exe, with lpk-foreign linked in), from the workers/ workspace and its
  # own lockfile, workers/Cargo.lock: a change there without a regenerated file fails --check.
  cargo about generate --locked --fail --config about.toml -m workers/lpk-worker/Cargo.toml about-worker.hbs | tr -d '\r'
  # E5-2: the GUI, for the Windows triples it ships on (about-gui.toml), with the WebView2 loader's notice.
  cargo about generate --locked --fail --config about-gui.toml -m crates/lpk-gui/Cargo.toml about-gui.hbs | tr -d '\r' | with_webview2_licence
  # E7-8: the Explorer handler lpk_shell.dll, for the same Windows triples (about-gui.toml).
  cargo about generate --locked --fail --config about-gui.toml -m crates/lpk-shell/Cargo.toml about-shell.hbs | tr -d '\r'
  nsis_section | tr -d '\r'
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
  if grep -rnE "GNU (AFFERO |LESSER |LIBRARY )?GENERAL PUBLIC LICENSE" crates vendor notices; then
    echo "crates/, vendor/ or notices/ contains a GPL-family licence text" >&2
    exit 1
  fi
  # E4-4 (R207): every vendored libarchive file carries a notice the notices reproduce.
  bash scripts/libarchive-licence-scan.sh --check
  echo "THIRD-PARTY-NOTICES is current; no GPL-family licence text"
else
  render
fi
