"""E7-7 test 6: the SHA-256 an installed lpk-gui.exe had before tauri-bundler packed it.

tauri-bundler 2.10.4 (bundle.rs, patch_binary) writes the bundle type into the main binary it packs: the
token __TAURI_BUNDLE_TYPE_VAR_UNK becomes __TAURI_BUNDLE_TYPE_VAR_NSS (the updater plugin reads it to know it
was installed by NSIS), then restores target/release's file. So the installed lpk-gui.exe never has the hash
of the one the build left; it has it once the token is turned back. Run 42939076647 showed this: every bin\\
binary had N+1's hash, lpk-gui.exe alone did not.

Prints that hash. Exits 1, printing why, unless the file holds the NSIS token exactly once and no unpatched
token (an installed binary that was not patched would leave the updater unable to install the next update).

Usage: python -I scripts/e7-bundled-hash.py <installed lpk-gui.exe>
"""

import hashlib
import sys

UNK = b"__TAURI_BUNDLE_TYPE_VAR_UNK"
NSS = b"__TAURI_BUNDLE_TYPE_VAR_NSS"


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__.strip().splitlines()[-1])
        return 2
    data = open(sys.argv[1], "rb").read()
    nss, unk = data.count(NSS), data.count(UNK)
    if nss != 1 or unk != 0:
        print(f"bundle-type tokens in {sys.argv[1]}: NSS {nss}, UNK {unk} (want 1 and 0)")
        return 1
    print(hashlib.sha256(data.replace(NSS, UNK)).hexdigest())
    return 0


if __name__ == "__main__":
    sys.exit(main())
