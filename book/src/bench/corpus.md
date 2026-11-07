# The benchmark corpus

The benchmark corpus is built from public sources, each pinned by address and BLAKE3 in `bench/corpus.lock`; the sources, their classes and licences are in `bench/corpus-sources.toml`, and the specification is `docs/CORPUS.md`. The lock lists downloads (archives, files and git commits), not the files of the built corpus: one download can yield many files, and some corpus files are derived locally. The built corpus has two profiles, `small` for quick runs and `full` for the published results. A bracketed number is the index of a result file of the published run, listed under Sources.

## The built corpus of the published run

- profile `full`, manifest BLAKE3 `a52af02774b9134362467adc7b6bf273f6f89592085ce71fe51f05e10574e12d`: 174309 [4,6,8,10,12,14,16,18,23,25,30,32,34,36,38,40,42,44] files, 20782669079 [4,6,8,10,12,14,16,18,23,25,30,32,34,36,38,40,42,44] bytes, 18 classes [4]

## Profile `full`

### Classes

A pin of a git commit has no size and adds no bytes.

| class | description | downloads (lock entries) | download bytes |
|---|---|---|---|
| `archives-nested` | ZIP/JAR/APK/EPUB/7z containing the above | 2 | 772447 |
| `audio` | WAV/PCM, FLAC, MP3 | 14 | 68358513 |
| `backup-versions` | Derived from `source-git` | 3 | 0 |
| `backup-versions-large` | Three working-tree snapshots of a large project, as folders `node-v1`, `node-v2`, `node-v3` | 3 | 0 |
| `encrypted-random` | Derived from `video`: control for the entropy gate | 0 | 0 |
| `game-assets` | Textures, audio, models, scripts | 5 | 24143917 |
| `logs-text` | Server/app logs, CSV, JSON lines | 5 | 2008128920 |
| `model-weights` | fp16/bf16 tensors | 2 | 286816945 |
| `office-pdf` | PDF, DOCX, XLSX, PPTX, DOC/XLS/PPT | 205 | 2034349594 |
| `photo-jpeg` | Camera JPEGs, mixed sizes, baseline + progressive | 3007 | 8254028975 |
| `photo-jpeg-edited` | Derived from `photo-jpeg`: lossy edits → near-duplicates | 0 | 0 |
| `photo-raw-png` | Lossless images: PNG screenshots/UI, PNG photos, BMP/TIFF | 26 | 16765936 |
| `small-files` | Derived from `logs-text`: the small-file case | 0 | 0 |
| `software-installed` | Installed application bits | 3 | 74323881 |
| `source-git` | A source tree with full git history | 1 | 0 |
| `text-prose` | Natural-language text and markup | 52 | 152285008 |
| `video` | H.264/HEVC/AV1 in MP4/MKV | 2 | 956809598 |
| `vm-image` | A small virtual disk | 3 | 635885990 |

### Sources

| id | class | licence | downloads (lock entries) | download bytes |
|---|---|---|---|---|
| `enwik8` | `text-prose` | CC-BY-SA-3.0 | 1 | 36445475 |
| `gutenberg-books` | `text-prose` | Public domain (Project Gutenberg, US) | 50 | 48205637 |
| `silesia` | `text-prose` | Silesia corpus: free for any use (mattmahoney.net/dc/silesia.html) | 1 | 67633896 |
| `loghub-apache` | `logs-text` | CC-BY-4.0 (Zenodo record 8196385; cite Loghub, github.com/logpai/loghub) | 1 | 262905 |
| `loghub-linux` | `logs-text` | CC-BY-4.0 (Zenodo record 8196385; cite Loghub, github.com/logpai/loghub) | 1 | 232039 |
| `loghub-hdfs` | `logs-text` | CC-BY-4.0 (Zenodo record 8196385; cite Loghub, github.com/logpai/loghub) | 1 | 186645559 |
| `loghub-windows` | `logs-text` | CC-BY-4.0 (Zenodo record 8196385; cite Loghub, github.com/logpai/loghub) | 1 | 1670098945 |
| `nyc-taxi-full` | `logs-text` | NYC Open Data terms of use | 1 | 150889472 |
| `python-embed` | `software-installed` | PSF-2.0; bundled components (OpenSSL, libffi and others) under their own licences (test data only) | 1 | 12601679 |
| `portablegit` | `software-installed` | GPL-2.0-only; bundled components (curl, OpenSSL, MSYS2, Perl and others) under their own licences (test data only, never linked) | 1 | 59958024 |
| `7zip-extra` | `software-installed` | LGPL-2.1-or-later (7za.exe also contains BSD-3-Clause and BSD-2-Clause code, per the package License.txt; test data only) | 1 | 1764178 |
| `kenney-platformer-kit` | `game-assets` | CC0-1.0 | 1 | 4640135 |
| `kenney-nature-kit` | `game-assets` | CC0-1.0 | 1 | 10537521 |
| `kenney-sci-fi-sounds` | `game-assets` | CC0-1.0 | 1 | 5875104 |
| `kenney-music-jingles` | `game-assets` | CC0-1.0 | 1 | 1239525 |
| `kenney-voiceover-pack` | `game-assets` | CC0-1.0 | 1 | 1851632 |
| `kenney-ui-pack` | `photo-raw-png` | CC0-1.0 | 1 | 1229750 |
| `kenney-pixel-ui-pack` | `photo-raw-png` | CC0-1.0 | 1 | 141881 |
| `kodak` | `photo-raw-png` | Kodak Lossless True Color Image Suite; believed released by Eastman Kodak for unrestricted usage (r0k.us) | 24 | 15394305 |
| `libultrahdr-jpegs` | `photo-jpeg` | CC-BY-4.0 (libultrahdr tests/data/LICENSE) | 7 | 220848 |
| `commons-featured-full` | `photo-jpeg` | CC0-1.0, CC-BY or CC-BY-SA (each file carries its own licence and author) | 3000 | 8253808127 |
| `arxiv-cc-by-full` | `office-pdf` | CC-BY-4.0 (each paper carries its authors) | 200 | 354643949 |
| `govdocs-000` | `office-pdf` | US government documents via Digital Corpora (freely available for research; redistributable to the best of their knowledge) | 1 | 486762106 |
| `govdocs-001` | `office-pdf` | US government documents via Digital Corpora (freely available for research; redistributable to the best of their knowledge) | 1 | 290926059 |
| `govdocs-002` | `office-pdf` | US government documents via Digital Corpora (freely available for research; redistributable to the best of their knowledge) | 1 | 284730475 |
| `govdocs-003` | `office-pdf` | US government documents via Digital Corpora (freely available for research; redistributable to the best of their knowledge) | 1 | 293902467 |
| `govdocs-004` | `office-pdf` | US government documents via Digital Corpora (freely available for research; redistributable to the best of their knowledge) | 1 | 323384538 |
| `v1` | `backup-versions` | BSD-3-Clause OR GPL-2.0-only | 1 | 0 |
| `v2` | `backup-versions` | BSD-3-Clause OR GPL-2.0-only | 1 | 0 |
| `v3` | `backup-versions` | BSD-3-Clause OR GPL-2.0-only | 1 | 0 |
| `node-v1` | `backup-versions-large` | MIT | 1 | 0 |
| `node-v2` | `backup-versions-large` | MIT | 1 | 0 |
| `node-v3` | `backup-versions-large` | MIT | 1 | 0 |
| `zstd-history-full` | `source-git` | BSD-3-Clause OR GPL-2.0-only | 1 | 0 |
| `librivox-art-of-war` | `audio` | Public domain (LibriVox) | 7 | 34705507 |
| `commons-flac` | `audio` | CC0-1.0 | 7 | 33653006 |
| `bbb-1080p` | `video` | CC-BY-3.0 (Blender Foundation) | 1 | 275524128 |
| `sintel-720p` | `video` | CC-BY-3.0 (Blender Foundation) | 1 | 681285470 |
| `alpine-virt` | `vm-image` | Alpine Linux image: various free-software licences per package (test data only) | 1 | 69206016 |
| `cirros` | `vm-image` | GPL-2.0-only (Linux kernel, busybox); bundled components under their own licences (test data only) | 1 | 20809932 |
| `ubuntu-cloudimg` | `vm-image` | Ubuntu cloud image: various free-software licences per package (test data only) | 1 | 545870042 |
| `bert-tiny` | `model-weights` | MIT | 1 | 17756393 |
| `smollm2-135m` | `model-weights` | Apache-2.0 | 1 | 269060552 |
| `gson-jar` | `archives-nested` | Apache-2.0 | 1 | 298435 |
| `gutenberg-epub` | `archives-nested` | Public domain (Project Gutenberg, US) | 1 | 474012 |
| `random-full` | `encrypted-random` | CC0-1.0 | 0 | 0 |
| `small-files` | `small-files` | Derived from the logs-text class (licences as listed for its sources) | 0 | 0 |
| `flac-wav` | `audio` | Derived from the Commons FLAC files (CC0 / public domain) | 0 | 0 |
| `kodak-jpeg` | `photo-jpeg` | Derived from the Kodak Lossless True Color Image Suite (see source kodak) | 0 | 0 |
| `edited-full` | `photo-jpeg-edited` | Derived from Commons photos (CC0 / CC BY / CC BY-SA; per-file licences in bench/corpus.lock) | 0 | 0 |
| `built-zips` | `archives-nested` | Derived from the office-pdf, backup-versions and logs-text classes (licences as listed for their sources) | 0 | 0 |
| `bbb-encodes-full` | `video` | CC-BY-3.0 (Blender Foundation, derived) | 0 | 0 |
| `convert-full` | `photo-raw-png` | Derived from Commons photos (CC0 / CC BY / CC BY-SA; per-file licences in bench/corpus.lock) | 0 | 0 |

## Profile `small`

### Classes

A pin of a git commit has no size and adds no bytes.

| class | description | downloads (lock entries) | download bytes |
|---|---|---|---|
| `archives-nested` | ZIP/JAR/APK/EPUB/7z containing the above | 2 | 772447 |
| `audio` | WAV/PCM, FLAC, MP3 | 14 | 68358513 |
| `backup-versions` | Derived from `source-git` | 3 | 0 |
| `encrypted-random` | Derived from `video`: control for the entropy gate | 0 | 0 |
| `game-assets` | Textures, audio, models, scripts | 5 | 24143917 |
| `logs-text` | Server/app logs, CSV, JSON lines | 4 | 227148370 |
| `model-weights` | fp16/bf16 tensors | 1 | 17756393 |
| `office-pdf` | PDF, DOCX, XLSX, PPTX, DOC/XLS/PPT | 21 | 525085102 |
| `photo-jpeg` | Camera JPEGs, mixed sizes, baseline + progressive | 307 | 322267275 |
| `photo-jpeg-edited` | Derived from `photo-jpeg`: lossy edits → near-duplicates | 0 | 0 |
| `photo-raw-png` | Lossless images: PNG screenshots/UI, PNG photos, BMP/TIFF | 26 | 16765936 |
| `small-files` | Derived from `logs-text`: the small-file case | 0 | 0 |
| `software-installed` | Installed application bits | 3 | 53967930 |
| `source-git` | A source tree with full git history | 1 | 0 |
| `text-prose` | Natural-language text and markup | 51 | 84651112 |
| `video` | H.264/HEVC/AV1 in MP4/MKV | 1 | 64657225 |
| `vm-image` | A small virtual disk | 2 | 90015948 |

### Sources

| id | class | licence | downloads (lock entries) | download bytes |
|---|---|---|---|---|
| `enwik8` | `text-prose` | CC-BY-SA-3.0 | 1 | 36445475 |
| `gutenberg-books` | `text-prose` | Public domain (Project Gutenberg, US) | 50 | 48205637 |
| `loghub-apache` | `logs-text` | CC-BY-4.0 (Zenodo record 8196385; cite Loghub, github.com/logpai/loghub) | 1 | 262905 |
| `loghub-linux` | `logs-text` | CC-BY-4.0 (Zenodo record 8196385; cite Loghub, github.com/logpai/loghub) | 1 | 232039 |
| `loghub-hdfs-small` | `logs-text` | CC-BY-4.0 (Zenodo record 8196385; cite Loghub, github.com/logpai/loghub) | 1 | 186645559 |
| `nyc-taxi-small` | `logs-text` | NYC Open Data terms of use | 1 | 40007867 |
| `python-embed` | `software-installed` | PSF-2.0; bundled components (OpenSSL, libffi and others) under their own licences (test data only) | 1 | 12601679 |
| `mingit` | `software-installed` | GPL-2.0-only; bundled components (curl, OpenSSL, MSYS2, Perl and others) under their own licences (test data only, never linked) | 1 | 39602073 |
| `7zip-extra` | `software-installed` | LGPL-2.1-or-later (7za.exe also contains BSD-3-Clause and BSD-2-Clause code, per the package License.txt; test data only) | 1 | 1764178 |
| `kenney-platformer-kit` | `game-assets` | CC0-1.0 | 1 | 4640135 |
| `kenney-nature-kit` | `game-assets` | CC0-1.0 | 1 | 10537521 |
| `kenney-sci-fi-sounds` | `game-assets` | CC0-1.0 | 1 | 5875104 |
| `kenney-music-jingles` | `game-assets` | CC0-1.0 | 1 | 1239525 |
| `kenney-voiceover-pack` | `game-assets` | CC0-1.0 | 1 | 1851632 |
| `kenney-ui-pack` | `photo-raw-png` | CC0-1.0 | 1 | 1229750 |
| `kenney-pixel-ui-pack` | `photo-raw-png` | CC0-1.0 | 1 | 141881 |
| `kodak` | `photo-raw-png` | Kodak Lossless True Color Image Suite; believed released by Eastman Kodak for unrestricted usage (r0k.us) | 24 | 15394305 |
| `libultrahdr-jpegs` | `photo-jpeg` | CC-BY-4.0 (libultrahdr tests/data/LICENSE) | 7 | 220848 |
| `commons-featured-small` | `photo-jpeg` | CC0-1.0, CC-BY or CC-BY-SA (each file carries its own licence and author) | 300 | 322046427 |
| `arxiv-cc-by-small` | `office-pdf` | CC-BY-4.0 (each paper carries its authors) | 20 | 38322996 |
| `govdocs-small` | `office-pdf` | US government documents via Digital Corpora (freely available for research; redistributable to the best of their knowledge) | 1 | 486762106 |
| `v1` | `backup-versions` | BSD-3-Clause OR GPL-2.0-only | 1 | 0 |
| `v2` | `backup-versions` | BSD-3-Clause OR GPL-2.0-only | 1 | 0 |
| `v3` | `backup-versions` | BSD-3-Clause OR GPL-2.0-only | 1 | 0 |
| `zstd-history-small` | `source-git` | BSD-3-Clause OR GPL-2.0-only | 1 | 0 |
| `librivox-art-of-war` | `audio` | Public domain (LibriVox) | 7 | 34705507 |
| `commons-flac` | `audio` | CC0-1.0 | 7 | 33653006 |
| `bbb-320x180` | `video` | CC-BY-3.0 (Blender Foundation) | 1 | 64657225 |
| `alpine-virt` | `vm-image` | Alpine Linux image: various free-software licences per package (test data only) | 1 | 69206016 |
| `cirros` | `vm-image` | GPL-2.0-only (Linux kernel, busybox); bundled components under their own licences (test data only) | 1 | 20809932 |
| `bert-tiny` | `model-weights` | MIT | 1 | 17756393 |
| `gson-jar` | `archives-nested` | Apache-2.0 | 1 | 298435 |
| `gutenberg-epub` | `archives-nested` | Public domain (Project Gutenberg, US) | 1 | 474012 |
| `random-small` | `encrypted-random` | CC0-1.0 | 0 | 0 |
| `small-files` | `small-files` | Derived from the logs-text class (licences as listed for its sources) | 0 | 0 |
| `flac-wav` | `audio` | Derived from the Commons FLAC files (CC0 / public domain) | 0 | 0 |
| `kodak-jpeg` | `photo-jpeg` | Derived from the Kodak Lossless True Color Image Suite (see source kodak) | 0 | 0 |
| `edited-small` | `photo-jpeg-edited` | Derived from Commons photos (CC0 / CC BY / CC BY-SA; per-file licences in bench/corpus.lock) | 0 | 0 |
| `built-zips` | `archives-nested` | Derived from the office-pdf, backup-versions and logs-text classes (licences as listed for their sources) | 0 | 0 |
| `bbb-encodes-small` | `video` | CC-BY-3.0 (Blender Foundation, derived) | 0 | 0 |
| `convert-small` | `photo-raw-png` | Derived from Commons photos (CC0 / CC BY / CC BY-SA; per-file licences in bench/corpus.lock) | 0 | 0 |

## Sources

4. `2026-10-30 run 1 lpk-fast-archives-nested.json`
6. `2026-10-30 run 1 lpk-fast-audio.json`
8. `2026-10-30 run 1 lpk-fast-backup-versions.json`
10. `2026-10-30 run 1 lpk-fast-backup-versions-large.json`
12. `2026-10-30 run 1 lpk-fast-encrypted-random.json`
14. `2026-10-30 run 1 lpk-fast-game-assets.json`
16. `2026-10-30 run 1 lpk-fast-logs-text.json`
18. `2026-10-30 run 1 lpk-fast-model-weights.json`
23. `2026-10-30 run 2 lpk-fast-office-pdf.json`
25. `2026-10-30 run 2 lpk-fast-photo-jpeg.json`
30. `2026-10-30 run 3 lpk-fast-photo-jpeg-edited.json`
32. `2026-10-30 run 3 lpk-fast-photo-raw-png.json`
34. `2026-10-30 run 3 lpk-fast-small-files.json`
36. `2026-10-30 run 3 lpk-fast-software-installed.json`
38. `2026-10-30 run 3 lpk-fast-source-git.json`
40. `2026-10-30 run 3 lpk-fast-text-prose.json`
42. `2026-10-30 run 3 lpk-fast-video.json`
44. `2026-10-30 run 3 lpk-fast-vm-image.json`

{{#include ../footer.md}}
