# Foreign formats: what LitePack reads, and how

The archive formats WinRAR and 7-Zip document as readable (WinRAR's help page "Supported archive formats";
7-Zip 26.05's "Supported formats" list), each with the route LitePack reads it by, or a named gap with the
reason. Every foreign reader runs in a sandboxed worker (D-97): the pure-Rust readers of E4-3 in `lpk-worker`
(LPAC by default, the restricted token as an opt-in), libarchive 3.8.11 in `lpk-worker-libarchive` (E4-4; LPAC
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
| WIM, CHM, MSI (Compound), NSIS, IHEX | — | yes | — | not built: no reader in libarchive 3.8.11; 7-Zip handlers excluded (D-89 (a)) |
| 7-Zip's disk images and file systems (APFS, DMG, FAT, GPT, HFS, MBR, NTFS, QCOW2, SquashFS, VDI, VHD, VHDX, VMDK, ext, CramFS, UEFI, …) | — | yes | — | not built: no reader in libarchive 3.8.11 for these; 7-Zip handlers excluded (D-89 (a)) |
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

**Split archives (`x.7z.001`, `x.zip.001`, `x.001`).** Only RAR volume sets are joined (`volume_set`: `partN.rar`, and
`.rar` with `.r00`). A `.001` file is read alone as whatever its first bytes say (a 7z or ZIP prefix), so a listing or
extraction stops with that reader's named truncation error; joining raw splits is not implemented (a gap, not a
route).

**RAR self-extracting archives.** libarchive finds the signature only from offset 0x10000, every 16 bytes, within 512 KiB
(`archive_read_support_format_rar.c:806-826`, `archive_read_support_format_rar5.c:166`). The host recognises a CRC-valid RAR
anywhere in the first 4 MiB of a PE file; one beyond libarchive's window is the named gap "RAR self-extracting archive
beyond libarchive's search window" (`lpk_proto::rar_sfx` holds the one window definition, used by the host and the worker).

**Entry names (R204).** The same archive gives the same names on every machine; no code page or locale decides them. Names the
format declares are decoded as declared (RAR 5 UTF-8, RAR 4 Unicode, Joliet UTF-16BE, CAB's UTF-8 attribute, LHA's UTF-16, pax).
Undeclared names are read as UTF-8 (`hdrcharset=UTF-8` for tar, CAB, LHA and RAR 4; the stored bytes for ar, cpio and ISO
Rock Ridge); a name that is not valid UTF-8 is dropped and the host refuses the entry. No reader names a charset for its
undeclared names that would justify a table (rule 3 of R204 does not apply: `archive_string_default_conversion_for_read`
is the machine's OEM page, which is exactly what the ruling forbids). Proven by tests for cpio, ar and ustar (UTF-8 exact,
Latin-1 refused) and by fixtures with `café ü 日本.txt` in RAR 5 (also `naïve/`), ISO Rock Ridge, ISO Joliet and LZH, listed
and extracted with the exact names through the host; a CAB fixture could not be made (`makecab` reads its directive file
through the ANSI code page).

**Untested.** LZH has fixtures for stored (`-lh0-`) members with level 0 and level 1 headers only: `-lh5-`, `-lh6-`, `-lh7-` decompression and level 2 headers are not exercised by a fixture. The RPM fixture is a minimal one (lead, empty signature and header, gzip'd cpio payload). CAB checksums are not claimed verified
(a stored CFDATA checksum of 0 means none).

**No UTF-8 locale on Linux.** The worker asks for `C.UTF-8`, then `en_US.UTF-8` at start and writes one line to stderr if
neither exists. Names the format declares in UTF-16 or UTF-8 (Joliet, RAR 5 and RAR 4 Unicode, CAB's UTF-8 attribute, LHA's
UTF-16, pax) and tar, CAB, LHA and RAR 4 names read with `hdrcharset=UTF-8` need libarchive's conversion to the locale's
charset; without a UTF-8 locale those with non-ASCII characters are refused with the named error, never guessed. Stored-byte
formats (cpio, ar, ISO Rock Ridge) are unaffected. On Windows nothing is set.

**LZH names in a code page.** An LZH header that declares a code page (`EXT_CODEPAGE`, for example CP932) decodes on Windows,
where libarchive uses the system's tables, and is refused with the named error on Linux (no iconv is linked). Accepted
(R211): no iconv and no encoding crate for one rare header.
