# LitePack and libarchive 3.8.11

This folder holds the subset of libarchive 3.8.11 that `crates/lpk-libarchive-sys` compiles (E4-4),
with its licence (`COPYING`: two-clause BSD for the code compiled here, see `docs/LICENSING.md`).

- Source: `libarchive-3.8.11.tar.gz`, 9,341,028 bytes, SHA-256
  `900fe9fc7237c2b04f090f3c3648de8fe597525771f5035016537ce2d667d12b` (equal to the digest GitHub
  publishes for the release asset; its PGP signature, by Martin Matuska <mm@FreeBSD.org>, key
  `659C84C0E23EA1FA97E0B58CC040B508D63D2B36`, verified good).
- What is here: every `*.h` of the tarball's `libarchive/`, the 40 `.c` files `build.rs` compiles,
  `COPYING`, and `build-cmake/config.h.in` (the template the `config.h` files below were made from).
  `FILES.txt` lists each file with its SHA-256.
- **Changes to libarchive's files: none.** Every file listed in `FILES.txt` is byte-identical to the
  tarball's (compared with `cmp` when vendored).
- Added by LitePack (not libarchive's): `litepack-config/<target>/config.h` for
  `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` and `x86_64-unknown-linux-gnu`; the header
  comment of each says how it was made.
- Not compiled and not vendored: every writer, every `*_disk*` file, the external-program filter,
  bzip2, lrzip, lz4, lzop, grzip, mtree, zip, 7-Zip, xar and warc readers, the tools (`tar`, `cpio`, `cat`,
  `unzip`), the tests and the build system. No unRAR code and no 7-Zip handler code is used.
