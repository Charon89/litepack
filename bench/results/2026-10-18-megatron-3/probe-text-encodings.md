# Probe `text-encodings`

- corpus: `small` (manifest BLAKE3 `8daaaf66f1e06460ce264c73c101aa5dc388668b2006d82c803b7fe016124db0`)
- build `ec002766cded` (release profile, opt-level 3) on `megatron`, 2026-10-18T05:38:49Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 9.6 s elapsed
- libraries: liblzma liblzma 5.8.4 (bundled by liblzma-sys 0.4.9; generic C build, no SIMD or unaligned-access paths), libzstd 1.5.7

E2-17's probe: base64 runs (at least 256 characters over one RFC 4648 alphabet) in text files and UTF-16 texts in every file, priced with the Deflate stage's Balanced part encoder; a stream costs min(M(payload), payload) + record (82 bytes base64, 79 utf16) + 160 overhead and is kept when that is below M(text alone); a file is kept when its kept streams gain more than 256 bytes and at least one part in 32 of it. A is a real Balanced archive of the class; B-cut re-encodes each affected block without its kept streams, split after each peeled file; B-defer without the split. The gate of a primitive opens when B-cut saves at least 1.00% of a class's archive and no class is predicted larger.

Base64 per class:

| class | files | text files | candidates (bytes) | hex-only | regular (bytes) | irregular | kept streams / files | A | B-cut | B-defer | saving | gate |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| archives-nested | 14 | 0 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| audio | 21 | 0 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| backup-versions | 1974 | 1878 | 63 (99255) | 57 | 0 (0) | 6 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| encrypted-random | 2 | 0 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| game-assets | 4660 | 1648 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| logs-text | 4 | 4 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| model-weights | 1 | 0 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| office-pdf | 166 | 6 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| photo-jpeg | 355 | 0 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| photo-jpeg-edited | 60 | 0 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| photo-raw-png | 950 | 0 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| small-files | 20000 | 20000 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| software-installed | 433 | 196 | 956 (1269203) | 538 | 418 (612051) | 0 | 19 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| source-git | 664 | 630 | 22 (34888) | 20 | 0 (0) | 2 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| text-prose | 51 | 51 | 1 (721) | 0 | 0 (0) | 1 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| video | 3 | 0 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |
| vm-image | 2 | 0 | 0 (0) | 0 | 0 (0) | 0 | 0 / 0 | - | - | - | no kept stream: every block is unchanged | no |

Base64 runs by what precedes them (runs, bytes; inflated: in depth-1 texts of Deflate containers, counted only):

| class | mime | pem | data-uri | json | xml | bare | inflated |
|---|---|---|---|---|---|---|---|
| backup-versions | 0 (0) | 0 (0) | 0 (0) | 0 (0) | 0 (0) | 6 (1995) | 0 (0) |
| office-pdf | 0 (0) | 0 (0) | 0 (0) | 0 (0) | 0 (0) | 0 (0) | 1728 (997371) |
| software-installed | 0 (0) | 414 (611027) | 0 (0) | 0 (0) | 0 (0) | 4 (1024) | 0 (0) |
| source-git | 0 (0) | 0 (0) | 0 (0) | 0 (0) | 0 (0) | 2 (665) | 0 (0) |
| text-prose | 0 (0) | 0 (0) | 0 (0) | 0 (0) | 0 (0) | 1 (721) | 0 (0) |

Irregular runs by cause (runs):

| class | other-byte | mixed-alphabet | bare-cr | mixed-line-endings | trailing-line-ending | empty-line | ragged-lines | line-too-long | padding-inside | bad-padding | bad-length | nonzero-unused-bits | mismatch |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| backup-versions | 0 | 0 | 0 | 0 | 0 | 0 | 6 | 0 | 0 | 0 | 0 | 0 | 0 |
| source-git | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| text-prose | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 |

Streams (regular runs; the first rows per class):

| class | file | offset | label | layout | text | payload (class) | M(text) | M(payload) | stream cost | gate |
|---|---|---|---|---|---|---|---|---|---|---|
| software-installed | software-installed/7zip-extra/Far/7zToFar.ini | 325 | bare | standard 0 none pad | 256 | 192 (other) | 13 | 15 | 257 | no |
| software-installed | software-installed/7zip-extra/Far/7zToFar.ini | 1133 | bare | standard 0 none pad | 256 | 192 (other) | 13 | 15 | 257 | no |
| software-installed | software-installed/7zip-extra/Far/7zToFar.ini | 1752 | bare | standard 0 none pad | 256 | 192 (other) | 13 | 15 | 257 | no |
| software-installed | software-installed/7zip-extra/Far/7zToFar.ini | 2368 | bare | standard 0 none pad | 256 | 192 (other) | 13 | 15 | 257 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 47 | pem | standard 64 lf pad | 1917 | 1415 (other) | 1438 | 1327 | 1569 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 2058 | pem | standard 64 lf pad | 849 | 626 (other) | 649 | 470 | 712 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 2975 | pem | standard 64 lf pad | 2717 | 2007 (other) | 1961 | 1615 | 1857 | kept |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 5781 | pem | standard 64 lf pad | 1994 | 1471 (other) | 1450 | 1311 | 1553 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 7850 | pem | standard 64 lf pad | 1133 | 837 (other) | 887 | 749 | 991 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 9058 | pem | standard 64 lf pad | 1828 | 1349 (other) | 1402 | 1261 | 1503 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 10961 | pem | standard 64 lf pad | 601 | 442 (other) | 465 | 353 | 595 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 11637 | pem | standard 64 lf pad | 682 | 502 (other) | 537 | 420 | 662 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 12403 | pem | standard 64 lf pad | 2063 | 1523 (other) | 1571 | 1334 | 1576 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 14562 | pem | standard 64 lf pad | 727 | 537 (other) | 564 | 440 | 682 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 15385 | pem | standard 64 lf pad | 1876 | 1384 (other) | 1439 | 1285 | 1527 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 17377 | pem | standard 64 lf pad | 2112 | 1560 (other) | 1601 | 1414 | 1656 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 19568 | pem | standard 64 lf pad | 1897 | 1400 (other) | 1464 | 1295 | 1537 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 21544 | pem | standard 64 lf pad | 751 | 553 (other) | 589 | 450 | 692 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 22377 | pem | standard 64 lf pad | 1860 | 1373 (other) | 1417 | 1262 | 1504 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 24319 | pem | standard 64 lf pad | 1860 | 1373 (other) | 1414 | 1262 | 1504 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 26254 | pem | standard 64 lf pad | 1880 | 1389 (other) | 1426 | 1272 | 1514 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 28210 | pem | standard 64 lf pad | 686 | 507 (other) | 535 | 415 | 657 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 28972 | pem | standard 64 lf pad | 1836 | 1355 (other) | 1406 | 1260 | 1502 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 30883 | pem | standard 64 lf pad | 2209 | 1631 (other) | 1635 | 1420 | 1662 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 33170 | pem | standard 64 lf pad | 1836 | 1355 (other) | 1371 | 1263 | 1505 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 35081 | pem | standard 64 lf pad | 836 | 617 (other) | 547 | 471 | 713 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 36001 | pem | standard 64 lf pad | 1299 | 959 (other) | 1001 | 792 | 1034 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 37386 | pem | standard 64 lf pad | 2023 | 1494 (other) | 1554 | 1328 | 1570 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 39490 | pem | standard 64 lf pad | 1998 | 1476 (other) | 1431 | 1320 | 1562 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 41559 | pem | standard 64 lf pad | 1929 | 1425 (other) | 1416 | 1298 | 1540 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 43581 | pem | standard 64 lf pad | 885 | 653 (other) | 571 | 489 | 731 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 44559 | pem | standard 64 lf pad | 2031 | 1500 (other) | 1455 | 1332 | 1574 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 46676 | pem | standard 64 lf pad | 1251 | 922 (other) | 878 | 782 | 1024 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 48013 | pem | standard 64 lf pad | 796 | 586 (other) | 531 | 455 | 697 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 48891 | pem | standard 64 lf pad | 1239 | 914 (other) | 947 | 776 | 1018 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 50212 | pem | standard 64 lf pad | 784 | 579 (other) | 604 | 450 | 692 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 51084 | pem | standard 64 lf pad | 735 | 541 (other) | 583 | 443 | 685 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 51906 | pem | standard 64 lf pad | 1876 | 1386 (other) | 1394 | 1284 | 1526 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 53865 | pem | standard 64 lf pad | 1933 | 1428 (other) | 1403 | 1296 | 1538 | no |
| software-installed | software-installed/mingit/ucrt64/etc/ssl/cert.pem | 55882 | pem | standard 64 lf pad | 995 | 735 (other) | 749 | 587 | 829 | no |
| software-installed | ... 378 more |

Blocks (A as written, A re-encoded, B-cut, B-defer):

| class | primitive | block | plain | A | A re-encoded | B-cut | B-defer | streams (bytes) | segments |
|---|---|---|---|---|---|---|---|---|---|

UTF-16 per class (texts; runs inside binaries counted only; cluster rows A, A' (grid), B (per file), B' (one stream)):

| class | texts | bytes | cjk | kept | A | A' | B | B' | binary files / runs (bytes) | A -> B-cut | gate |
|---|---|---|---|---|---|---|---|---|---|---|---|
| audio | 0 | - | - | 0 | - | - | - | - | 7 / 1369 (124538) | - | no |
| backup-versions | 0 | - | - | 0 | - | - | - | - | 6 / 12 (1284) | - | no |
| game-assets | 0 | - | - | 0 | - | - | - | - | 32 / 33 (10044) | - | no |
| office-pdf | 0 | - | - | 0 | - | - | - | - | 69 / 4693 (386940) | - | no |
| photo-jpeg | 0 | - | - | 0 | - | - | - | - | 48 / 69 (6850) | - | no |
| photo-jpeg-edited | 0 | - | - | 0 | - | - | - | - | 10 / 14 (1404) | - | no |
| software-installed | 0 | - | - | 0 | - | - | - | - | 153 / 5367 (676980) | - | no |
| source-git | 0 | - | - | 0 | - | - | - | - | 2 / 4 (428) | - | no |
| vm-image | 0 | - | - | 0 | - | - | - | - | 1 / 28 (2874) | - | no |

Re-encode mismatches: 0

Gate base64: CLOSED

Gate utf16: CLOSED
