# Fixtures of the libarchive worker (E4-4)

All made by `make_fixtures.py` (`python -I make_fixtures.py <this dir> <scratch dir>`), which first writes the
same source tree every time (`hello.txt`, `Mixed Case Name.txt`, `sub/nested.txt`, `data.bin` of 48,000
pseudo-random bytes; mtime 2024-01-02 03:04:05 UTC). `manifest.b3` holds the BLAKE3 of each file of that tree
(written by the ignored test `fixtures::write_manifest`).

| Fixture | Made by | Notes |
|---|---|---|
| `rar5.rar` | WinRAR `rar` 7.00 x64 (`a -r -m3 -ep1 -ma5`) | found on PATH or through `RAR` |
| `rar5-vol.part1.rar` … `part3.rar` | `rar` 7.00 (`-ma5 -v20k`) | a three-volume set |
| `gap-rar5-encrypted.rar` | `rar` 7.00 (`-ma5 -pLitePack`) | named gap: encrypted data |
| `gap-rar5-encrypted-headers.rar` | `rar` 7.00 (`-ma5 -hpLitePack`) | named gap: encrypted headers |
| `rar5-sfx.sfx` | the generator: a 4 KiB stub of our own (`MZ`, `PE\0\0`) + `rar5.rar` | WinRAR's SFX module is not redistributable, so it is not used |
| `rar4.rar`, `rar4-vol.rar`/`.r00`/`.r01` | the generator, stored members, from RARLAB's RAR 4.x format description | RAR 7.00 no longer writes RAR 4 (`-ma4` exits 7) |
| `gap-rar4-solid.rar`, `gap-rar4-encrypted.rar`, `gap-rar4-encrypted-headers.rar`, `gap-rar20.rar` | the generator (the flag or version byte of each gap) | named gaps |
| `cab-stored.cab`, `cab-mszip.cab`, `cab-lzx.cab` | Windows `makecab` (Windows 11 Pro 10.0.26200), a `.ddf` per file | `LZX` with `CompressionMemory=21` |
| `iso-rockridge.iso`, `iso-joliet.iso` | the generator's ISO 9660 writer (one with Rock Ridge `SP`/`RR`/`NM`/`PX` entries, one with a Joliet tree) | no ISO tool on the machine |
| `fixture.a`, `fixture.cpio` (newc) | the generator | |
| `hello.txt.uue` | the generator (`binascii.b2a_uu`) | |
| `hello.txt.Z`, `small.tar.Z` | the generator's `compress` (9-bit codes only) | checked with `gzip -dc` |

Every fixture except the gaps was cross-checked with 7-Zip 26.03 (`7z t` / `7z l`).
