# E4-3 reader fixtures

Committed archives written by 7-Zip 26.03 (`C:\Program Files\7-Zip\7z.exe`, Windows 11) on 2026-10-25,
from four files: `hello.txt` (`hello world\n`), `dir/data.bin` (65,536 bytes of a linear congruential
generator, seed 12345, `x = (x * 1103515245 + 12345) & 0x7fffffff`, byte `x >> 16`), `dir/empty.txt` (empty)
and `dir/text.txt` (`line <i> of the LitePack fixture text\n` for i = 0..4999). The tests rebuild the same
content and compare every extracted member by BLAKE3 (`../readers.rs`, `content()`); `MANIFEST.blake3` pins
each fixture's own bytes (`LPK_BLESS_FIXTURES=1 cargo test -p lpk-foreign --test readers` rewrites it when
a fixture is deliberately added).

| File | 7-Zip command (from the source folder) |
|---|---|
| `7zip-deflate.zip` | `7z a -tzip -mm=Deflate` |
| `7zip-deflate64.zip` | `7z a -tzip -mm=Deflate64` |
| `7zip-bzip2.zip` | `7z a -tzip -mm=BZip2` (read raw, decoded by `bzip2-rs`) |
| `7zip-lzma.zip` | `7z a -tzip -mm=LZMA` |
| `7zip-ppmd.zip` | `7z a -tzip -mm=PPMd` |
| `7zip-zipcrypto.zip` | `7z a -tzip -pLitePack -mem=ZipCrypto` |
| `7zip-aes256.zip` | `7z a -tzip -pLitePack -mem=AES256` |
| `7zip-lzma2-solid.7z` | `7z a -t7z -ms=on` |
| `7zip-ppmd.7z` | `7z a -t7z -m0=PPMd` |
| `7zip-aes-content.7z` | `7z a -t7z -pLitePack` |
| `7zip-aes-header.7z` | `7z a -t7z -pLitePack -mhe=on` |
| `7zip-bzip2-gap.7z` | `7z a -t7z -m0=BZip2` (a named gap: refused as unsupported) |
| `7zip.tar` | `7z a -ttar` |
| `7zip.tar.bz2` | `7z a -tbzip2` of `7zip.tar` |
| `7zip-hello.txt.gz` | `7z a -tgzip hello.txt` |
| `7zip-hello.txt.xz` | `7z a -txz hello.txt` |

Not committed, written by the tests instead: ZIP (stored, Deflate, AES) by the `zip` crate; tar (with a
directory, a symlink and a hard link) by the `tar` crate (GNU tar is not installed here), and its gzip, xz,
lzip and zstd compressions by `flate2`, `lzma-rust2` and `ruzstd`; 7z by `sevenz-rust2`, also behind a
fake self-extractor stub. No Windows Explorer ZIP is committed (Explorer's "Send to compressed folder"
could not be driven from the session; a follow-up).
