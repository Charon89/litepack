# Foreign formats: what LitePack reads, and how

The archive formats WinRAR and 7-Zip document as readable (WinRAR's help page "Supported archive formats";
7-Zip 26.05's "Supported formats" list), each with the route LitePack reads it by, or a named gap with the
reason. Every foreign reader runs in a sandboxed worker (D-97): the pure-Rust readers of E4-3 in `lpk-worker`
(LPAC by default, the restricted token as an opt-in), libarchive 3.8.9 in `lpk-worker-libarchive` (E4-4; LPAC
only — the host refuses the restricted token for it). The host routes by magic (`lpk_core::foreign::detect_file`),
never by extension. Source of the design: `docs/notes/e4-design.md` §3; the decisions: D-97, D-99, D-106.

| Format | WinRAR | 7-Zip | Route | Named gaps (reason) |
|---|---|---|---|---|
| ZIP, JAR (and EPUB, Office, APK as ZIP) | yes | yes | pure Rust (`zip`) | encrypted bzip2/zstd members; spanned `.z01` sets; WinZip JPEG, WavPack methods |
| 7z | yes | yes | pure Rust (`sevenz-rust2`) | BZIP2, zstd, Deflate64, LZS, Lizard coders |
| tar | yes | yes | pure Rust (`tar`) | GNU sparse members; devices and FIFOs left out |
| gzip, bzip2, xz, lzma, zstd, lzip | yes (gz, bz2, xz, zst, lz) | yes | pure Rust | the bzip2 stream CRC is not checked |
| RAR 4 (2.9–4.x) | yes | yes | libarchive worker (volumes `.rNN` and `.partN.rar`, SFX) | encrypted (headers or data); solid archives; RAR 1.5/2.x compression; filters inside PPMd data |
| RAR 5 | yes | yes | libarchive worker (volumes, SFX) | encrypted (headers or data); dictionaries above 64 MiB |
| ISO 9660 | yes | yes | libarchive worker (Rock Ridge, Joliet) | UDF and UDF-only images (libarchive reads ISO 9660 only) |
| CAB | yes | yes | libarchive worker (stored, MSZIP, LZX) | Quantum compression; multi-cabinet sets |
| LZH/LHA | yes | yes | libarchive worker | — |
| ARJ | yes | yes | — | not built: libarchive has no ARJ reader; unRAR and 7-Zip handler code are excluded (D-89 (a)) |
| AR (`.a`, `.deb`) | — | yes | libarchive worker | — |
| CPIO | — | yes | libarchive worker (odc, newc, crc, binary) | — |
| RPM | — | yes | libarchive worker (cpio payload in gzip, xz or zstd) | bzip2 payloads (libarchive's bzip2 filter is not compiled) |
| Z (Unix `compress`) | yes | yes | libarchive worker (a single file or `.tar.Z`) | — |
| UUE (uuencode, base64) | yes | — | libarchive worker (a single file, or a tar inside) | — |
| XZ inside RPM/UUE, gzip inside UUE | — | — | libarchive worker (its filters) | bzip2 inside them |
| XAR | — | yes | — | not built: libarchive's XAR reader needs an XML parser (not linked) |
| WIM, CHM, MSI (Compound), NSIS, IHEX | — | yes | — | not built: no reader in libarchive 3.8.9; 7-Zip handlers excluded (D-89 (a)) |
| 7-Zip's disk images and file systems (APFS, DMG, FAT, GPT, HFS, MBR, NTFS, QCOW2, SquashFS, VDI, VHD, VHDX, VMDK, ext, CramFS, UEFI, …) | — | yes | — | not built: no reader in libarchive 3.8.9 for these; 7-Zip handlers excluded (D-89 (a)) |
| ZIPX (WinZip's extended methods) | yes | partly | pure Rust for the methods `zip` reads | the methods above |

Limits that are not gaps of a format: a volume set holds at most 13 volumes (the worker's bootstrap page carries
16 handles, three of them pipes; a larger set is refused with a named error); the restricted-token sandbox is never
used for the libarchive worker (`WorkerError::Sandbox`, `lpk_core::foreign::RESTRICTED_REFUSED`).

How the RAR gaps are found: the worker walks the first volume's block headers in safe Rust before libarchive sees the
archive (`crates/lpk-worker-libarchive/src/rar.rs`, from RARLAB's published format descriptions) and reports each
gap as an `Unsupported` error naming it; filters inside PPMd data are reported from libarchive's own error. The
owner may later allow unRAR for these (out of scope for E4-4).

Fixtures: `crates/lpk-worker-libarchive/tests/fixtures/README.md` (which tool made each one), E4-3's under
`workers/lpk-foreign/tests/fixtures/`.
