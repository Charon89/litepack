# Phase 1 — E2-22: the D-07 gates with measured `lpk` rows on `full`, 2026-10-21

Every figure below is a median of the runner's repeats from the committed result directories named with it, pooled by
`lpk-bench report` (same corpus manifest, host and thread count): the incumbents' baseline `bench/results/2026-10-17-megatron-15`
(every tool of the catalogue without `lpk`; its `encrypted-random`, `game-assets` and `logs-text` rows replaced by the idle
re-measurement `2026-10-18-megatron` through its `supersedes.toml`, D-74) and the `lpk` rows `2026-10-20-megatron-2` (`fast` and
`balanced` on every class, release build `a9c9c60`: revision 1.3 with the BWT stage, D-81, on an idle machine; 36 measured, 0
failed, every file of every repeat verified). The generated report follows unchanged; its section 6 is the gate table in the
Phase 1 form (measured `lpk` rows, not the Phase 0 estimates).

## The gates

## 6. Gates (D-07)

| gate | numbers | result |
|---|---|---|
| G1 photo/document size (measured `lpk` rows): on photo-jpeg, photo-jpeg-edited and office-pdf together, lpk/balanced at most 90% of the best measured incumbent (the best single tool x setting over the combined bytes; D-07's fallback wording: WinZip and PowerArchiver were not measured) and at most 85% of 7-Zip Ultra; the sum of the per-class bests (stricter) and the three classes alone are shown for information | combined lpk/balanced*: 8195078057 bytes [363,365,367]<br>combined best single tool (zpaqfranz/m5 *): 9696258118 bytes [132-145,148-161,164-177]<br>combined 7-Zip Ultra: 10006337794 bytes [134,150,166]<br>combined lpk/balanced* / best single tool: 84.5% [132-145,148-161,164-177,363,365,367]<br>combined lpk/balanced* / 7-Zip Ultra: 81.9% [134,150,166,363,365,367]<br>stricter, sum of the per-class bests (no single tool): 9696258118 bytes [132-145,148-161,164-177]<br>combined lpk/balanced* / sum of the per-class bests: 84.5% [132-145,148-161,164-177,363,365,367]<br>photo-jpeg alone, lpk/balanced* / best incumbent: 82.5% [148-161,365]<br>photo-jpeg alone, lpk/balanced* / 7-Zip Ultra: 79.9% [150,365]<br>photo-jpeg-edited alone, lpk/balanced / best incumbent: 82.4% [164-177,367]<br>photo-jpeg-edited alone, lpk/balanced / 7-Zip Ultra: 78.6% [166,367]<br>office-pdf alone, lpk/balanced* / best incumbent: 96.9% [132-145,363]<br>office-pdf alone, lpk/balanced* / 7-Zip Ultra: 94.9% [134,363] | PASS |
| G2 versioned backup (D-27, D-43; measured `lpk` rows): on backup-versions-large, lpk/balanced at most 50% of the best measured tool x setting among the tools that do not deduplicate across files | tools treated as deduplicating: tsaur, zpaqfranz<br>lpk/balanced*: 135050542 bytes [353]<br>best incumbent, not deduplicating (7z/ultra): 97948125 bytes [53-58]<br>lpk/balanced* / best incumbent: 137.9% [53-58,353]<br>reference, not compared (deduplicating tool tsaur): no measured row<br>reference, not compared (deduplicating tool zpaqfranz/m5 *): 92280623 bytes [64,65] | FAIL |
| G3 video store speed (D-27; measured `lpk` rows): lpk/fast's compress MB/s on video is at least 80% of the store control's compress MB/s on video (a real program reading and writing the files) in the same run or, pooled per D-48, the same baseline (the directories are named) | rows compared: lpk/fast from `bench/results/2026-10-20-megatron-2` [378], store/store from `bench/results/2026-10-17-megatron-15` [260]: two runs of one pooled baseline (D-27 asks for one run; D-48's pooling stands in for it: same host, corpus manifest, thread count and catalogue, checked when the directories were pooled)<br>lpk/fast compress on video: 1039.7 MB/s [378]<br>store/store compress on video: 1476.3 MB/s [260]<br>video class bytes: 959731214 bytes [378]<br>lpk/fast median compress wall seconds on video: 0.923 s [378]<br>lpk/fast first repeat compress on video: 1039.7 MB/s [378]<br>store/store median compress wall seconds on video: 0.650 s [260]<br>store/store first repeat compress on video: 1353.3 MB/s [260]<br>lpk/fast / store/store: 70.4% [260,378] | FAIL |
| G4 fast-tier extraction (measured `lpk` rows): the lpk/fast extraction MB/s is at least the 7z/mx5 extraction MB/s, blended over each disk mix | photo-doc: lpk/fast* extract: 200.7 MB/s [346,348,362,364,366,368,370,376,382]<br>photo-doc: 7z/mx5 extract: 222.8 MB/s [5,21,133,149,165,181,197,245,382]<br>developer: lpk/fast extract: 266.1 MB/s [346,350,358,360,370,372,374,376,380,382]<br>developer: 7z/mx5 extract: 81.8 MB/s [5,37,117,197,213,229,245,277,328,382]<br>video-heavy: lpk/fast* extract: 607.7 MB/s [348,354,362,364,378,380,382]<br>video-heavy: 7z/mx5 extract: 508.8 MB/s [21,133,149,261,277,296,382] | FAIL |

**NO-GO proposal: failing gates: G2, G3, G4**

- G1 inputs, skipped: tsaur/default (not installed); tsaur/max (not installed)
- G1 input: lpk/balanced on photo-jpeg rests on one measurement (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated)
- G1 input: lpk/balanced on office-pdf rests on one measurement (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated)
- G2 inputs, failed: store/store on backup-versions-large (compress: exit code -1073741819); zstd/3 on backup-versions-large (tar step: exit code -1073741819); zstd/19 on backup-versions-large (tar step: exit code -1073741819); zstd/ultra22-long27 on backup-versions-large (tar step: exit code -1073741819); xz/6 on backup-versions-large (tar step: exit code -1073741819); xz/9 on backup-versions-large (tar step: exit code -1073741819). skipped: tsaur/default (not installed); tsaur/max (not installed)
- G2 input: lpk/balanced on backup-versions-large rests on one measurement (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated)
- G3: the two rows are from different runs (`bench/results/2026-10-20-megatron-2`, `bench/results/2026-10-17-megatron-15`); the comparison rests on D-48's pooling checks, not on D-27's same run

PASS needs the stated numbers; a gate whose data is missing is `not evaluable` and is never a PASS. A `*` after a tool name marks a row that rests on one measurement. The gates are in their Phase 1 form: the pooled baseline holds measured rows of `lpk`, so each gate compares those rows (not the Phase 0 estimates or proxies) with the incumbents measured in the same pooled baseline (D-48). G1 uses the fallback wording of D-07 with the best single tool x setting over the combined bytes; the stricter sum of per-class bests is printed beneath it. G2 compares `lpk/balanced` with the best measured tool that does not deduplicate across files (D-43); deduplicating tools are printed as references. G3 compares `lpk/fast` with the baseline `store` control, a real program reading and writing the files, in the same pooled baseline (D-27); the probe's raw read rate, when a probe is given, is information only. G4 compares `lpk/fast` with 7-Zip `-mx5` (D-07). The verdict is recorded by a person in a decision.

### G3 in one run, as D-27 defines it

The table above compares `lpk/fast` with the store control from two pooled runs (the report says so). D-27 asks for one run, so both rows were measured again in a single run of the runner on the idle machine (`target/acceptance/e2-22-g3.sh`, the catalogue trimmed to `store` and `lpk/fast`, class `video` of `full`, the runner's default repeats):

| results directory | `store/store` compress | `lpk/fast` compress | share | G3 (80%) |
|---|---|---|---|---|
| `2026-10-21-megatron` | 1382.0 MB/s (0.694 s, 3 repeats) | 1025.4 MB/s (0.936 s, 3 repeats) | 74.2% | FAIL |


## What each result says

- **G1 (PASS).** On `photo-jpeg`, `photo-jpeg-edited` and `office-pdf` together, `lpk/balanced` writes 8,195,078,057 bytes:
  84.5% of the best single incumbent over the three classes (zpaqfranz `-m5`, 9,696,258,118; D-07's fallback wording, WinZip
  and PowerArchiver not measured) and 81.9% of 7-Zip Ultra (10,006,337,794), against the thresholds 90% and 85%. The JPEG
  classes carry the pass (82.5% and 82.4% of the best incumbent; the Lepton peel of D-50); `office-pdf` alone is 96.9% of
  the best incumbent and 94.9% of 7-Zip Ultra, so the Deflate peel (D-69 to D-71) wins on documents by a few percent, not by
  the gate's margin.
- **G2 (FAIL).** On `backup-versions-large` (three snapshots of `godotengine/godot`, each larger than every incumbent's
  dictionary window, D-43) `lpk/balanced` writes 135,050,542 bytes, 8.1% of the class, against 7-Zip Ultra's 98,161,589
  (5.9%): 137.6% of the best measured incumbent that does not deduplicate, where the gate asks for at most 50%. The
  deduplicating references are zpaqfranz `-m5` at 5.5% and `-m1` at 8.9%; WinRAR's best solid setting writes 7.1%.
  Two things explain it. First, D-43's premise that a version larger than the window keeps a solid compressor from
  seeing the next version does not hold for 7-Zip's `-mqs` (the catalogue's Ultra setting): it sorts the files of the
  solid stream by type and name, so the three versions of one file sit next to each other inside the 64 MiB window, and
  the cross-version redundancy is removed by the window after all; WinRAR's solid setting (`-s`, files in directory order)
  does worse at 7.1% but still beats `lpk`. Second, `lpk` removes only exact duplicate chunks (FastCDC, D-53): a file
  changed between versions is coded whole again, because the delta stage (E2-9) is not built and the file ordering (E2-8,
  a negative result on `small`) keeps the versions 500 MB apart. The gate's 2× needs both: near-duplicate files coded as
  deltas against their earlier version, and an ordering that puts a file's versions together.
- **G3 (FAIL).** The pooled rows give `lpk/fast` 1039.7 MB/s against the store control's 1476.3 MB/s on `video`,
  70.4% where the gate asks for 80%; D-27 defines the gate in one run, and the single run of both rows on the idle
  machine (`bench/results/2026-10-21-megatron`, below the gate table) gives 1025.4 against 1382.0 MB/s, 74.2%: FAIL.
  E2-18's single run of 2026-10-17 (`phase1-2026-10-17-store-path-r3.md`) had met the gate at 88.3% with `lpk/fast`
  at 1022.0 MB/s against a control at 1157.0: `lpk/fast`'s rate is the same in all three runs (1022 to 1040 MB/s);
  the control's varies by 28% between runs (a 960 MB class that sits in the page cache), and today it is faster.
  To meet 80% of today's control the store path needs about 1106 MB/s, 8% more than it does.
- **G4 (PASS on two mixes, FAIL on `photo-doc`).** `lpk/fast` extracts faster than 7-Zip `-mx5` on 15 of 18 classes and
  on the `developer` (266.1 against 81.8 MB/s) and `video-heavy` (607.7 against 508.8 MB/s) mixes; on `photo-doc` it is
  200.7 against 222.8 MB/s, because `photo-jpeg` extracts at 136 MB/s against 1427 for 7-Zip, which stores JPEGs as they
  are: the Fast tier's JPEG peel (D-50) buys the 22% of G1 with Lepton decoding at extraction, about ten times slower
  than a copy. `vm-image` (978 against 1253 MB/s) and `photo-jpeg-edited` (189 against 291) are the other two classes
  behind.

## Measured against the Phase 0 estimates (D-08)

The Phase 0 report (`bench/reports/phase0-2026-10-03.md`) estimated the Balanced tier per class from the probes, on the
corpus manifest of that date (`backup-versions-large` did not exist yet). Measured `lpk/balanced` against it, in percent of
each class's bytes:

| class | Phase 0 estimate | measured `lpk/balanced` | difference (points) | what the Phase 0 rule used, and what differs |
|---|---|---|---|---|
| archives-nested | 66.5% | 67.8% | +1.3 | the probe's Deflate figure B with every file compressed on its own by xz preset 9; the tier peels the same streams and models 256 MiB blocks with LZMA |
| audio | 70.8% | 76.7% | +5.9 | no rule: the best incumbent, no gain claimed; the tier has no audio stage and is behind zpaqfranz `-m5` and 7-Zip Ultra (72.0%) |
| backup-versions | 7.3% | 7.6% | +0.3 | version 1 at zstd 19 plus `--patch-from` patches; the tier's chunk dedup and LZMA land within a third of a point |
| backup-versions-large | — | 8.1% | — | not in the Phase 0 corpus (D-43); see G2 |
| encrypted-random | 100.0% | 100.1% | +0.1 | stored, as estimated |
| game-assets | 39.6% | 32.8% | −6.8 | the Deflate figure B compressed every file on its own; the tier's solid blocks share context across the assets (the peel itself adds 0.6 points, E2-10's A/B on `small`) |
| logs-text | 5.3% | 4.7% | −0.6 | xz preset 9 on a tar of the class; the tier's BWT stage is 9.35% below the tier without it on `small` (D-82) |
| model-weights | 68.8% | 71.8% | +3.0 | the weights probe's rotated planes; the tier has no weights stage (Phase 2) and equals 7-Zip Ultra |
| office-pdf | 59.2% | 63.1% | +3.9 | the Deflate figure B peeled every stream; the stage keeps only the streams past its gates (D-70), 3.3 points of peel on `small` (E2-10's A/B) |
| photo-jpeg | 78.0% | 78.1% | +0.1 | bytes after Lepton; as estimated |
| photo-jpeg-edited | 75.1% | 75.1% | +0.0 | bytes after Lepton; as estimated |
| photo-raw-png | 43.5% | 43.2% | −0.3 | the Deflate figure B; as estimated (E2-16's row filters are not shipped; their probe saves about 3% more, the margin is the owner's) |
| small-files | 8.3% | 10.1% | +1.8 | xz preset 9 on one tar stream of the class; the tier's archive of 20,000 entries is larger (its per-entry metadata and block cuts are the candidates; not measured apart) |
| software-installed | 32.7% | 17.1% | −15.6 | the Deflate figure B compressed every file on its own; the tier's solid blocks and chunk dedup across the installed trees take it far below (the peel adds nothing here, E2-10's A/B) |
| source-git | 75.9% | 73.6% | −2.3 | no rule: the best incumbent; the tier peels the Deflate inside git's objects (6.1 points on `small`, E2-10's A/B) |
| text-prose | 23.9% | 23.5% | −0.4 | xz preset 9 on a tar of the class; BWT's gain on prose is 2.36% on `small` (D-82) |
| video | 100.0% | 100.0% | +0.0 | stored, as estimated |
| vm-image | 18.7% | 20.6% | +1.9 | no rule: the best incumbent; the tier equals 7-Zip Ultra (20.7%) and is behind zpaqfranz `-m5` |

## The generated report

# LitePack Phase 0 report, baseline run of 2026-10-17

Generated by `lpk-bench report` from result files under bench/results. Every number in sections 2 to 6 is followed by the index of its source in brackets (list in section 7); a number derived from several files carries all of them. Estimates are labelled as estimates and never share a column with measurements. The verdict is recorded by a person in D-08, not by this command.

## 1. Header

- corpus: profile `full`, manifest BLAKE3 `1d24f5c824fad0c85fc3b9231643b2bd80a75437c6b3d38e86ef1f6450a05f43`, 171247 [4,20,36,52,116,132,148,164,180,196,212,228,244,260,276,295,311,327] files, 20787446482 [4,20,36,52,116,132,148,164,180,196,212,228,244,260,276,295,311,327] bytes, 18 classes ([4])
- results directories (rows pooled):
  - `bench/results/2026-10-17-megatron-15`, build `8aaa72e51bfc`: catalogue BLAKE3 `4f4925bd30d1c0121d441b478db14339c91a0d572babc7ebf5265e6bb9f0b45a`, 3 repeats, long-run limit 120 s, 24 threads, settle pause 500 ms per 1000 files, antivirus state changed during the run: false [3]
  - `bench/results/2026-10-18-megatron`, build `8aaa72e51bfc`: catalogue BLAKE3 `4f4925bd30d1c0121d441b478db14339c91a0d572babc7ebf5265e6bb9f0b45a`, 3 repeats, long-run limit 120 s, 24 threads, settle pause 500 ms per 1000 files, antivirus state changed during the run: false [294]
  - `bench/results/2026-10-20-megatron-2`, build `a9c9c60d397d`: catalogue BLAKE3 `b910e67c310ff9d403a964dcb47e64bb32429de2fce036cfbc4eeead36ae4202`, 3 repeats, long-run limit 120 s, 24 threads, settle pause 500 ms per 1000 files, antivirus state changed during the run: false [345]
- Superseded rows (a later directory's re-measurement replaces an earlier directory's row, as listed in its supersedes.toml; the replaced file stays on disk and is cited here only):
  - `7z/mx5/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [69], by `bench/results/2026-10-18-megatron` [296]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `7z/ultra/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [70], by `bench/results/2026-10-18-megatron` [297]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `rar/best/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [72], by `bench/results/2026-10-18-megatron` [299]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `rar/best-rr3/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [74], by `bench/results/2026-10-18-megatron` [301]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `rar/best-solid/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [73], by `bench/results/2026-10-18-megatron` [300]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `rar/m3/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [71], by `bench/results/2026-10-18-megatron` [298]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `store/store/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [68], by `bench/results/2026-10-18-megatron` [295]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `tsaur/default/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [82], by `bench/results/2026-10-18-megatron` [309]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `tsaur/max/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [83], by `bench/results/2026-10-18-megatron` [310]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `xz/6/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [78], by `bench/results/2026-10-18-megatron` [305]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `xz/9/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [79], by `bench/results/2026-10-18-megatron` [306]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `zpaqfranz/m1/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [80], by `bench/results/2026-10-18-megatron` [307]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `zpaqfranz/m5/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [81], by `bench/results/2026-10-18-megatron` [308]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `zstd/19/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [76], by `bench/results/2026-10-18-megatron` [303]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `zstd/3/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [75], by `bench/results/2026-10-18-megatron` [302]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `zstd/ultra22-long27/encrypted-random`: from `bench/results/2026-10-17-megatron-15` [77], by `bench/results/2026-10-18-megatron` [304]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `7z/mx5/game-assets`: from `bench/results/2026-10-17-megatron-15` [85], by `bench/results/2026-10-18-megatron` [312]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `7z/ultra/game-assets`: from `bench/results/2026-10-17-megatron-15` [86], by `bench/results/2026-10-18-megatron` [313]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `rar/best/game-assets`: from `bench/results/2026-10-17-megatron-15` [88], by `bench/results/2026-10-18-megatron` [315]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `rar/best-rr3/game-assets`: from `bench/results/2026-10-17-megatron-15` [90], by `bench/results/2026-10-18-megatron` [317]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `rar/best-solid/game-assets`: from `bench/results/2026-10-17-megatron-15` [89], by `bench/results/2026-10-18-megatron` [316]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `rar/m3/game-assets`: from `bench/results/2026-10-17-megatron-15` [87], by `bench/results/2026-10-18-megatron` [314]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `store/store/game-assets`: from `bench/results/2026-10-17-megatron-15` [84], by `bench/results/2026-10-18-megatron` [311]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `tsaur/default/game-assets`: from `bench/results/2026-10-17-megatron-15` [98], by `bench/results/2026-10-18-megatron` [325]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `tsaur/max/game-assets`: from `bench/results/2026-10-17-megatron-15` [99], by `bench/results/2026-10-18-megatron` [326]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `xz/6/game-assets`: from `bench/results/2026-10-17-megatron-15` [94], by `bench/results/2026-10-18-megatron` [321]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `xz/9/game-assets`: from `bench/results/2026-10-17-megatron-15` [95], by `bench/results/2026-10-18-megatron` [322]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `zpaqfranz/m1/game-assets`: from `bench/results/2026-10-17-megatron-15` [96], by `bench/results/2026-10-18-megatron` [323]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `zpaqfranz/m5/game-assets`: from `bench/results/2026-10-17-megatron-15` [97], by `bench/results/2026-10-18-megatron` [324]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `zstd/19/game-assets`: from `bench/results/2026-10-17-megatron-15` [92], by `bench/results/2026-10-18-megatron` [319]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `zstd/3/game-assets`: from `bench/results/2026-10-17-megatron-15` [91], by `bench/results/2026-10-18-megatron` [318]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `zstd/ultra22-long27/game-assets`: from `bench/results/2026-10-17-megatron-15` [93], by `bench/results/2026-10-18-megatron` [320]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `7z/mx5/logs-text`: from `bench/results/2026-10-17-megatron-15` [101], by `bench/results/2026-10-18-megatron` [328]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `7z/ultra/logs-text`: from `bench/results/2026-10-17-megatron-15` [102], by `bench/results/2026-10-18-megatron` [329]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `rar/best/logs-text`: from `bench/results/2026-10-17-megatron-15` [104], by `bench/results/2026-10-18-megatron` [331]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `rar/best-rr3/logs-text`: from `bench/results/2026-10-17-megatron-15` [106], by `bench/results/2026-10-18-megatron` [333]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `rar/best-solid/logs-text`: from `bench/results/2026-10-17-megatron-15` [105], by `bench/results/2026-10-18-megatron` [332]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `rar/m3/logs-text`: from `bench/results/2026-10-17-megatron-15` [103], by `bench/results/2026-10-18-megatron` [330]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `store/store/logs-text`: from `bench/results/2026-10-17-megatron-15` [100], by `bench/results/2026-10-18-megatron` [327]: measured while builds and tests ran on the machine (2026-10-17 21:59:57-22:31 UTC)
  - `tsaur/default/logs-text`: from `bench/results/2026-10-17-megatron-15` [114], by `bench/results/2026-10-18-megatron` [341]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `tsaur/max/logs-text`: from `bench/results/2026-10-17-megatron-15` [115], by `bench/results/2026-10-18-megatron` [342]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `xz/6/logs-text`: from `bench/results/2026-10-17-megatron-15` [110], by `bench/results/2026-10-18-megatron` [337]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `xz/9/logs-text`: from `bench/results/2026-10-17-megatron-15` [111], by `bench/results/2026-10-18-megatron` [338]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `zpaqfranz/m1/logs-text`: from `bench/results/2026-10-17-megatron-15` [112], by `bench/results/2026-10-18-megatron` [339]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `zpaqfranz/m5/logs-text`: from `bench/results/2026-10-17-megatron-15` [113], by `bench/results/2026-10-18-megatron` [340]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `zstd/19/logs-text`: from `bench/results/2026-10-17-megatron-15` [108], by `bench/results/2026-10-18-megatron` [335]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `zstd/3/logs-text`: from `bench/results/2026-10-17-megatron-15` [107], by `bench/results/2026-10-18-megatron` [334]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - `zstd/ultra22-long27/logs-text`: from `bench/results/2026-10-17-megatron-15` [109], by `bench/results/2026-10-18-megatron` [336]: class re-measured as a whole on the idle machine with the rows that overlapped those builds
  - rows each directory supplies after the replacements: `bench/results/2026-10-17-megatron-15` 240, `bench/results/2026-10-18-megatron` 48, `bench/results/2026-10-20-megatron-2` 36
- rules: rows may come from several runs of the same corpus on the same machine, and the comparison is still per class.
- host `megatron`: Intel(R) Core(TM) Ultra 9 285K, 24 logical cores, 102298484736 bytes RAM, windows Windows 11 Pro [1]
- baseline build: lpk-bench 0.0.1 at commit `8aaa72e51bfc`, rustc 1.99.0 (b940084d7 2026-09-28), run dated 2026-10-17 [1]
- tools: tar (store, control) bsdtar 3.8.8 [2], 7-Zip 26.03 [2], WinRAR (rar) 7.00 [2], Zstandard 1.5.7 [2], XZ Utils 5.8.4 [2], zpaqfranz 65.4 [2], t-saur (skipped) [2], WinZip command line (skipped) [2], PowerArchiver command line (skipped) [2], LitePack 0.0.1+a9c9c60d397d [344]

## 2. Baseline, per class

Archive size is a percentage of the class's bytes. MB/s = class bytes / median wall seconds (MB = 10^6 bytes). Peak memory is the larger of the compress and extract medians (peak working set on Windows, maximum RSS on Linux), in MiB. A tool/setting marked `*` rests on one measurement.

### archives-nested: 14 [4] files, 34975551 bytes [4]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.0% [4] | 1010.7 [4] | 1232.7 [4] | 6.0 [4] | 3 [4] |
| 7z/mx5 | 79.0% [5] | 28.8 [5] | 148.2 [5] | 367.4 [5] | 3 [5] |
| 7z/ultra | 79.0% [6] | 27.1 [6] | 149.3 [6] | 378.2 [6] | 3 [6] |
| rar/m3 | 90.3% [7] | 113.8 [7] | 272.2 [7] | 155.6 [7] | 3 [7] |
| rar/best | 90.3% [8] | 97.7 [8] | 270.6 [8] | 234.5 [8] | 3 [8] |
| rar/best-solid | 80.8% [9] | 61.6 [9] | 264.1 [9] | 1062.2 [9] | 3 [9] |
| rar/best-rr3 | 93.1% [10] | 91.6 [10] | 276.0 [10] | 234.4 [10] | 3 [10] |
| zstd/3 | 91.2% [11] | 530.3 [11] | 848.1 [11] | 75.6 [11] | 3 [11] |
| zstd/19 | 81.3% [12] | 7.1 [12] | 823.2 [12] | 228.7 [12] | 3 [12] |
| zstd/ultra22-long27 | 79.7% [13] | 7.2 [13] | 725.0 [13] | 771.4 [13] | 3 [13] |
| xz/6 | 83.9% [14] | 4.5 [14] | 210.4 [14] | 248.8 [14] | 3 [14] |
| xz/9 | 79.0% [15] | 3.9 [15] | 138.0 [15] | 429.2 [15] | 3 [15] |
| zpaqfranz/m1 | 98.9% [16] | 292.0 [16] | 352.3 [16] | 86.1 [16] | 3 [16] |
| zpaqfranz/m5 * | 82.7% [17] | 0.4 [17] | 0.4 [17] | 818.9 [17] | 1 [17] |
| tsaur/default | skipped: not installed [18] | - | - | - | - |
| tsaur/max | skipped: not installed [19] | - | - | - | - |
| lpk/fast | 98.7% [346] | 474.8 [346] | 784.6 [346] | 71.2 [346] | 3 [346] |
| lpk/balanced | 67.8% [347] | 4.5 [347] | 91.8 [347] | 511.3 [347] | 3 [347] |

### audio: 21 [20] files, 157896311 bytes [20]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.0% [20] | 1129.8 [20] | 1757.6 [20] | 6.0 [20] | 3 [20] |
| 7z/mx5 | 72.0% [21] | 5.4 [21] | 91.0 [21] | 464.1 [21] | 3 [21] |
| 7z/ultra | 72.0% [22] | 5.2 [22] | 91.5 [22] | 728.9 [22] | 3 [22] |
| rar/m3 | 75.0% [23] | 86.8 [23] | 294.9 [23] | 327.3 [23] | 3 [23] |
| rar/best | 74.7% [24] | 59.0 [24] | 313.0 [24] | 598.5 [24] | 3 [24] |
| rar/best-solid | 74.8% [25] | 43.5 [25] | 257.7 [25] | 2407.0 [25] | 3 [25] |
| rar/best-rr3 | 77.0% [26] | 58.2 [26] | 303.2 [26] | 598.2 [26] | 3 [26] |
| zstd/3 | 90.0% [27] | 650.7 [27] | 814.7 [27] | 263.1 [27] | 3 [27] |
| zstd/19 | 81.8% [28] | 8.9 [28] | 633.3 [28] | 685.7 [28] | 3 [28] |
| zstd/ultra22-long27 | 81.8% [29] | 2.5 [29] | 568.9 [29] | 985.4 [29] | 3 [29] |
| xz/6 | 76.9% [30] | 12.3 [30] | 257.1 [30] | 905.1 [30] | 3 [30] |
| xz/9 | 76.5% [31] | 2.0 [31] | 77.5 [31] | 943.5 [31] | 3 [31] |
| zpaqfranz/m1 | 91.7% [32] | 189.4 [32] | 793.4 [32] | 640.5 [32] | 3 [32] |
| zpaqfranz/m5 * | 70.8% [33] | 1.0 [33] | 1.0 [33] | 2264.4 [33] | 1 [33] |
| tsaur/default | skipped: not installed [34] | - | - | - | - |
| tsaur/max | skipped: not installed [35] | - | - | - | - |
| lpk/fast | 90.0% [348] | 329.1 [348] | 500.6 [348] | 311.0 [348] | 3 [348] |
| lpk/balanced | 76.7% [349] | 1.9 [349] | 25.5 [349] | 949.1 [349] | 3 [349] |

### backup-versions: 1974 [36] files, 25541629 bytes [36]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 106.7% [36] | 230.2 [36] | 93.5 [36] | 6.1 [36] | 3 [36] |
| 7z/mx5 | 7.4% [37] | 9.7 [37] | 62.1 [37] | 298.8 [37] | 3 [37] |
| 7z/ultra | 7.4% [38] | 13.5 [38] | 46.3 [38] | 298.8 [38] | 3 [38] |
| rar/m3 | 30.3% [39] | 48.2 [39] | 28.8 [39] | 44.8 [39] | 3 [39] |
| rar/best | 30.2% [40] | 43.0 [40] | 42.0 [40] | 56.7 [40] | 3 [40] |
| rar/best-solid | 8.4% [41] | 36.3 [41] | 51.2 [41] | 549.2 [41] | 3 [41] |
| rar/best-rr3 | 31.2% [42] | 42.4 [42] | 41.8 [42] | 57.1 [42] | 3 [42] |
| zstd/3 | 27.0% [43] | 193.7 [43] | 86.3 [43] | 43.7 [43] | 3 [43] |
| zstd/19 | 21.6% [44] | 4.7 [44] | 86.9 [44] | 118.5 [44] | 3 [44] |
| zstd/ultra22-long27 | 7.4% [45] | 7.7 [45] | 86.6 [45] | 739.4 [45] | 3 [45] |
| xz/6 | 21.5% [46] | 5.4 [46] | 67.2 [46] | 163.6 [46] | 3 [46] |
| xz/9 | 7.4% [47] | 6.2 [47] | 75.2 [47] | 331.0 [47] | 3 [47] |
| zpaqfranz/m1 | 9.4% [48] | 92.2 [48] | 53.2 [48] | 95.7 [48] | 3 [48] |
| zpaqfranz/m5 | 6.7% [49] | 2.2 [49] | 2.1 [49] | 323.6 [49] | 3 [49] |
| tsaur/default | skipped: not installed [50] | - | - | - | - |
| tsaur/max | skipped: not installed [51] | - | - | - | - |
| lpk/fast | 10.6% [350] | 130.8 [350] | 223.7 [350] | 24.8 [350] | 3 [350] |
| lpk/balanced | 7.6% [351] | 4.2 [351] | 93.5 [351] | 515.6 [351] | 3 [351] |

### backup-versions-large: 127153 [52] files, 1663755751 bytes [52]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | FAILED (compress): compress: exit code -1073741819 [52] | - | - | - | - |
| 7z/mx5 | 11.9% [53] | 39.4 [53] | 71.3 [53] | 5744.2 [53] | 3 [53] |
| 7z/ultra | 5.9% [54] | 23.0 [54] | 69.1 [54] | 16125.9 [54] | 3 [54] |
| rar/m3 | 23.1% [55] | 56.5 [55] | 56.9 [55] | 254.4 [55] | 3 [55] |
| rar/best | 23.0% [56] | 48.4 [56] | 57.2 [56] | 397.9 [56] | 3 [56] |
| rar/best-solid | 7.1% [57] | 39.9 [57] | 57.7 [57] | 2446.1 [57] | 3 [57] |
| rar/best-rr3 | 23.7% [58] | 47.0 [58] | 56.1 [58] | 398.1 [58] | 3 [58] |
| zstd/3 | FAILED (compress): tar step: exit code -1073741819 [59] | - | - | - | - |
| zstd/19 | FAILED (compress): tar step: exit code -1073741819 [60] | - | - | - | - |
| zstd/ultra22-long27 | FAILED (compress): tar step: exit code -1073741819 [61] | - | - | - | - |
| xz/6 | FAILED (compress): tar step: exit code -1073741819 [62] | - | - | - | - |
| xz/9 | FAILED (compress): tar step: exit code -1073741819 [63] | - | - | - | - |
| zpaqfranz/m1 | 8.9% [64] | 182.6 [64] | 64.6 [64] | 1192.0 [64] | 3 [64] |
| zpaqfranz/m5 * | 5.5% [65] | 12.4 [65] | 12.1 [65] | 10147.0 [65] | 1 [65] |
| tsaur/default | skipped: not installed [66] | - | - | - | - |
| tsaur/max | skipped: not installed [67] | - | - | - | - |
| lpk/fast | 10.5% [352] | 133.5 [352] | 227.1 [352] | 452.5 [352] | 3 [352] |
| lpk/balanced * | 8.1% [353] | 5.2 [353] | 184.0 [353] | 1111.7 [353] | 1 [353] |

### encrypted-random: 2 [295] files, 419430400 bytes [295]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.0% [295] | 1327.7 [295] | 2009.2 [295] | 6.0 [295] | 3 [295] |
| 7z/mx5 | 100.0% [296] | 51.9 [296] | 1219.4 [296] | 1950.7 [296] | 3 [296] |
| 7z/ultra | 100.0% [297] | 24.0 [297] | 1029.4 [297] | 4531.6 [297] | 3 [297] |
| rar/m3 | 100.0% [298] | 65.5 [298] | 1670.2 [298] | 327.4 [298] | 3 [298] |
| rar/best | 100.0% [299] | 64.2 [299] | 1603.9 [299] | 2410.0 [299] | 3 [299] |
| rar/best-solid | 100.2% [300] | 67.4 [300] | 424.0 [300] | 2406.1 [300] | 3 [300] |
| rar/best-rr3 | 103.1% [301] | 63.3 [301] | 1616.3 [301] | 2410.1 [301] | 3 [301] |
| zstd/3 | 100.0% [302] | 871.0 [302] | 1258.1 [302] | 241.6 [302] | 3 [302] |
| zstd/19 | 100.0% [303] | 51.0 [303] | 1230.0 [303] | 1856.6 [303] | 3 [303] |
| zstd/ultra22-long27 | 100.0% [304] | 4.1 [304] | 1181.6 [304] | 1511.2 [304] | 3 [304] |
| xz/6 | 100.0% [305] | 42.0 [305] | 537.5 [305] | 2384.3 [305] | 3 [305] |
| xz/9 | 100.0% [306] | 4.2 [306] | 530.6 [306] | 2359.0 [306] | 3 [306] |
| zpaqfranz/m1 | 100.1% [307] | 466.6 [307] | 1290.0 [307] | 825.2 [307] | 3 [307] |
| zpaqfranz/m5 * | 100.1% [308] | 2.1 [308] | 2.1 [308] | 5483.0 [308] | 1 [308] |
| tsaur/default | skipped: not installed [309] | - | - | - | - |
| tsaur/max | skipped: not installed [310] | - | - | - | - |
| lpk/fast | 100.1% [354] | 779.1 [354] | 1971.2 [354] | 629.8 [354] | 3 [354] |
| lpk/balanced | 100.1% [355] | 318.2 [355] | 788.2 [355] | 914.6 [355] | 3 [355] |

### game-assets: 4660 [311] files, 52470892 bytes [311]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 107.0% [311] | 258.5 [311] | 99.2 [311] | 6.0 [311] | 3 [311] |
| 7z/mx5 | 31.4% [312] | 16.5 [312] | 54.6 [312] | 386.9 [312] | 3 [312] |
| 7z/ultra | 31.2% [313] | 13.4 [313] | 53.7 [313] | 532.1 [313] | 3 [313] |
| rar/m3 | 43.8% [314] | 38.5 [314] | 28.2 [314] | 60.3 [314] | 3 [314] |
| rar/best | 43.7% [315] | 33.9 [315] | 35.6 [315] | 81.2 [315] | 3 [315] |
| rar/best-solid | 33.4% [316] | 26.9 [316] | 67.8 [316] | 1024.7 [316] | 3 [316] |
| rar/best-rr3 | 45.1% [317] | 33.4 [317] | 29.9 [317] | 81.1 [317] | 3 [317] |
| zstd/3 | 34.9% [318] | 230.6 [318] | 92.5 [318] | 84.1 [318] | 3 [318] |
| zstd/19 | 31.9% [319] | 12.4 [319] | 79.1 [319] | 238.3 [319] | 3 [319] |
| zstd/ultra22-long27 | 31.8% [320] | 3.9 [320] | 94.3 [320] | 782.4 [320] | 3 [320] |
| xz/6 | 31.4% [321] | 12.2 [321] | 74.2 [321] | 326.5 [321] | 3 [321] |
| xz/9 | 31.3% [322] | 6.2 [322] | 65.1 [322] | 620.3 [322] | 3 [322] |
| zpaqfranz/m1 | 35.8% [323] | 78.2 [323] | 52.0 [323] | 215.9 [323] | 3 [323] |
| zpaqfranz/m5 * | 30.8% [324] | 0.7 [324] | 0.7 [324] | 854.3 [324] | 1 [324] |
| tsaur/default | skipped: not installed [325] | - | - | - | - |
| tsaur/max | skipped: not installed [326] | - | - | - | - |
| lpk/fast | 36.0% [356] | 124.0 [356] | 171.5 [356] | 86.7 [356] | 3 [356] |
| lpk/balanced | 32.8% [357] | 1.8 [357] | 67.7 [357] | 567.1 [357] | 3 [357] |

### logs-text: 11 [327] files, 2686418017 bytes [327]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.0% [327] | 1408.6 [327] | 2129.5 [327] | 6.0 [327] | 3 [327] |
| 7z/mx5 | 6.2% [328] | 57.5 [328] | 1758.3 [328] | 5595.2 [328] | 3 [328] |
| 7z/ultra | 5.2% [329] | 28.9 [329] | 1337.3 [329] | 25823.5 [329] | 3 [329] |
| rar/m3 | 6.3% [330] | 202.4 [330] | 1276.4 [330] | 310.9 [330] | 3 [330] |
| rar/best | 5.9% [331] | 92.4 [331] | 1207.5 [331] | 2394.6 [331] | 3 [331] |
| rar/best-solid | 5.6% [332] | 96.9 [332] | 1249.2 [332] | 2394.3 [332] | 3 [332] |
| rar/best-rr3 | 6.1% [333] | 92.4 [333] | 1211.7 [333] | 2394.8 [333] | 3 [333] |
| zstd/3 | 8.4% [334] | 1036.0 [334] | 1152.1 [334] | 265.0 [334] | 3 [334] |
| zstd/19 | 5.3% [335] | 36.8 [335] | 1283.8 [335] | 2879.6 [335] | 3 [335] |
| zstd/ultra22-long27 * | 4.9% [336] | 4.3 [336] | 1153.8 [336] | 6839.9 [336] | 1 [336] |
| xz/6 | 5.5% [337] | 80.8 [337] | 880.7 [337] | 2902.8 [337] | 3 [337] |
| xz/9 | 5.2% [338] | 29.6 [338] | 775.0 [338] | 11480.4 [338] | 3 [338] |
| zpaqfranz/m1 | 8.9% [339] | 502.4 [339] | 1958.6 [339] | 1250.7 [339] | 3 [339] |
| zpaqfranz/m5 * | 3.0% [340] | 14.9 [340] | 14.6 [340] | 15642.6 [340] | 1 [340] |
| tsaur/default | skipped: not installed [341] | - | - | - | - |
| tsaur/max | skipped: not installed [342] | - | - | - | - |
| lpk/fast | 8.1% [358] | 1024.4 [358] | 2380.2 [358] | 862.7 [358] | 3 [358] |
| lpk/balanced * | 4.7% [359] | 1.5 [359] | 104.2 [359] | 1756.2 [359] | 1 [359] |

### model-weights: 2 [116] files, 286816945 bytes [116]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.0% [116] | 1282.1 [116] | 2035.5 [116] | 6.0 [116] | 3 [116] |
| 7z/mx5 | 71.9% [117] | 8.1 [117] | 112.9 [117] | 1339.5 [117] | 3 [117] |
| 7z/ultra * | 71.8% [118] | 2.5 [118] | 56.5 [118] | 2997.7 [118] | 1 [118] |
| rar/m3 | 76.6% [119] | 73.3 [119] | 180.5 [119] | 323.8 [119] | 3 [119] |
| rar/best | 76.2% [120] | 55.0 [120] | 154.4 [120] | 2407.3 [120] | 3 [120] |
| rar/best-solid | 76.2% [121] | 50.6 [121] | 151.7 [121] | 2407.6 [121] | 3 [121] |
| rar/best-rr3 | 78.6% [122] | 48.8 [122] | 150.5 [122] | 2407.7 [122] | 3 [122] |
| zstd/3 | 79.1% [123] | 824.9 [123] | 826.7 [123] | 263.7 [123] | 3 [123] |
| zstd/19 | 77.3% [124] | 17.6 [124] | 469.2 [124] | 1220.6 [124] | 3 [124] |
| zstd/ultra22-long27 * | 78.1% [125] | 1.2 [125] | 538.2 [125] | 1198.7 [125] | 1 [125] |
| xz/6 | 72.3% [126] | 20.9 [126] | 338.1 [126] | 1589.0 [126] | 3 [126] |
| xz/9 * | 71.8% [127] | 1.6 [127] | 60.9 [127] | 1805.8 [127] | 1 [127] |
| zpaqfranz/m1 | 95.6% [128] | 160.5 [128] | 795.2 [128] | 1522.5 [128] | 3 [128] |
| zpaqfranz/m5 * | 67.0% [129] | 2.1 [129] | 2.1 [129] | 3238.2 [129] | 1 [129] |
| tsaur/default | skipped: not installed [130] | - | - | - | - |
| tsaur/max | skipped: not installed [131] | - | - | - | - |
| lpk/fast | 79.2% [360] | 665.0 [360] | 705.2 [360] | 736.7 [360] | 3 [360] |
| lpk/balanced * | 71.8% [361] | 1.2 [361] | 29.2 [361] | 1390.9 [361] | 1 [361] |

### office-pdf: 2334 [132] files, 2114690575 bytes [132]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.1% [132] | 1316.2 [132] | 1492.8 [132] | 6.0 [132] | 3 [132] |
| 7z/mx5 | 66.9% [133] | 107.3 [133] | 740.0 [133] | 6656.0 [133] | 3 [133] |
| 7z/ultra | 66.5% [134] | 45.0 [134] | 566.6 [134] | 21593.1 [134] | 3 [134] |
| rar/m3 | 69.3% [135] | 129.4 [135] | 374.4 [135] | 325.5 [135] | 3 [135] |
| rar/best | 69.2% [136] | 101.5 [136] | 362.2 [136] | 597.1 [136] | 3 [136] |
| rar/best-solid | 67.5% [137] | 62.4 [137] | 370.3 [137] | 2409.4 [137] | 3 [137] |
| rar/best-rr3 | 71.3% [138] | 98.5 [138] | 373.6 [138] | 597.1 [138] | 3 [138] |
| zstd/3 | 70.9% [139] | 927.4 [139] | 890.9 [139] | 361.2 [139] | 3 [139] |
| zstd/19 | 67.7% [140] | 61.4 [140] | 809.8 [140] | 3489.0 [140] | 3 [140] |
| zstd/ultra22-long27 * | 67.0% [141] | 7.2 [141] | 745.8 [141] | 6025.0 [141] | 1 [141] |
| xz/6 | 67.5% [142] | 58.6 [142] | 578.1 [142] | 3574.5 [142] | 3 [142] |
| xz/9 | 66.7% [143] | 23.3 [143] | 483.3 [143] | 10772.2 [143] | 3 [143] |
| zpaqfranz/m1 | 72.4% [144] | 454.3 [144] | 1378.9 [144] | 2307.8 [144] | 3 [144] |
| zpaqfranz/m5 * | 65.1% [145] | 5.2 [145] | 5.4 [145] | 21463.1 [145] | 1 [145] |
| tsaur/default | skipped: not installed [146] | - | - | - | - |
| tsaur/max | skipped: not installed [147] | - | - | - | - |
| lpk/fast | 70.6% [362] | 845.3 [362] | 1975.3 [362] | 798.3 [362] | 3 [362] |
| lpk/balanced * | 63.1% [363] | 1.9 [363] | 22.8 [363] | 1960.1 [363] | 1 [363] |

### photo-jpeg: 3055 [148] files, 8258908517 bytes [148]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.0% [148] | 1373.2 [148] | 1950.3 [148] | 6.0 [148] | 3 [148] |
| 7z/mx5 | 97.8% [149] | 101.4 [149] | 1426.6 [149] | 7214.8 [149] | 3 [149] |
| 7z/ultra | 97.8% [150] | 77.8 [150] | 1148.1 [150] | 34307.9 [150] | 3 [150] |
| rar/m3 | 98.3% [151] | 165.6 [151] | 394.0 [151] | 160.7 [151] | 3 [151] |
| rar/best | 98.3% [152] | 143.9 [152] | 386.1 [152] | 240.0 [152] | 3 [152] |
| rar/best-solid * | 98.1% [153] | 63.4 [153] | 327.8 [153] | 2407.7 [153] | 1 [153] |
| rar/best-rr3 | 101.3% [154] | 142.4 [154] | 358.9 [154] | 321.8 [154] | 3 [154] |
| zstd/3 | 98.7% [155] | 975.7 [155] | 1282.3 [155] | 320.6 [155] | 3 [155] |
| zstd/19 * | 97.7% [156] | 70.2 [156] | 1145.3 [156] | 3670.7 [156] | 1 [156] |
| zstd/ultra22-long27 * | 97.7% [157] | 22.7 [157] | 1000.0 [157] | 25902.8 [157] | 1 [157] |
| xz/6 * | 97.9% [158] | 52.3 [158] | 627.8 [158] | 3882.9 [158] | 1 [158] |
| xz/9 * | 97.8% [159] | 32.8 [159] | 622.8 [159] | 28469.2 [159] | 1 [159] |
| zpaqfranz/m1 | 99.8% [160] | 518.7 [160] | 1971.5 [160] | 1536.2 [160] | 3 [160] |
| zpaqfranz/m5 * | 94.7% [161] | 5.3 [161] | 5.4 [161] | 24323.8 [161] | 1 [161] |
| tsaur/default | skipped: not installed [162] | - | - | - | - |
| tsaur/max | skipped: not installed [163] | - | - | - | - |
| lpk/fast * | 78.1% [364] | 39.4 [364] | 136.5 [364] | 878.7 [364] | 1 [364] |
| lpk/balanced * | 78.1% [365] | 43.8 [365] | 138.3 [365] | 910.9 [365] | 1 [365] |

### photo-jpeg-edited: 600 [164] files, 550674750 bytes [164]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.1% [164] | 1304.5 [164] | 1475.8 [164] | 6.0 [164] | 3 [164] |
| 7z/mx5 | 95.9% [165] | 61.4 [165] | 290.8 [165] | 2483.0 [165] | 3 [165] |
| 7z/ultra | 95.7% [166] | 25.9 [166] | 117.7 [166] | 5798.6 [166] | 3 [166] |
| rar/m3 | 96.9% [167] | 156.1 [167] | 315.5 [167] | 159.9 [167] | 3 [167] |
| rar/best | 96.9% [168] | 130.4 [168] | 322.8 [168] | 238.6 [168] | 3 [168] |
| rar/best-solid | 96.3% [169] | 58.8 [169] | 273.1 [169] | 2406.8 [169] | 3 [169] |
| rar/best-rr3 | 99.9% [170] | 122.6 [170] | 320.8 [170] | 238.8 [170] | 3 [170] |
| zstd/3 | 97.0% [171] | 886.9 [171] | 1004.0 [171] | 267.6 [171] | 3 [171] |
| zstd/19 | 96.1% [172] | 57.5 [172] | 840.1 [172] | 2413.6 [172] | 3 [172] |
| zstd/ultra22-long27 * | 95.5% [173] | 3.3 [173] | 495.9 [173] | 2380.6 [173] | 1 [173] |
| xz/6 | 96.2% [174] | 52.2 [174] | 520.5 [174] | 3083.4 [174] | 3 [174] |
| xz/9 | 95.8% [175] | 5.7 [175] | 164.1 [175] | 3052.8 [175] | 3 [175] |
| zpaqfranz/m1 | 99.3% [176] | 437.0 [176] | 1174.8 [176] | 1069.9 [176] | 3 [176] |
| zpaqfranz/m5 * | 91.2% [177] | 2.7 [177] | 2.7 [177] | 7247.7 [177] | 1 [177] |
| tsaur/default | skipped: not installed [178] | - | - | - | - |
| tsaur/max | skipped: not installed [179] | - | - | - | - |
| lpk/fast | 75.1% [366] | 31.6 [366] | 189.0 [366] | 357.0 [366] | 3 [366] |
| lpk/balanced | 75.1% [367] | 33.8 [367] | 189.4 [367] | 350.8 [367] | 3 [367] |

### photo-raw-png: 1016 [180] files, 537693574 bytes [180]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.1% [180] | 1261.8 [180] | 1350.1 [180] | 6.0 [180] | 3 [180] |
| 7z/mx5 | 46.7% [181] | 42.6 [181] | 453.1 [181] | 2076.7 [181] | 3 [181] |
| 7z/ultra | 46.3% [182] | 16.0 [182] | 255.0 [182] | 5392.5 [182] | 3 [182] |
| rar/m3 | 47.9% [183] | 89.6 [183] | 209.1 [183] | 328.6 [183] | 3 [183] |
| rar/best | 47.5% [184] | 55.1 [184] | 213.6 [184] | 599.7 [184] | 3 [184] |
| rar/best-solid | 47.9% [185] | 29.7 [185] | 253.0 [185] | 2408.1 [185] | 3 [185] |
| rar/best-rr3 | 49.0% [186] | 54.8 [186] | 213.4 [186] | 600.6 [186] | 3 [186] |
| zstd/3 | 56.1% [187] | 805.4 [187] | 736.8 [187] | 359.6 [187] | 3 [187] |
| zstd/19 | 48.9% [188] | 44.4 [188] | 672.1 [188] | 2149.8 [188] | 3 [188] |
| zstd/ultra22-long27 * | 48.5% [189] | 3.9 [189] | 577.7 [189] | 2124.6 [189] | 1 [189] |
| xz/6 | 46.7% [190] | 55.1 [190] | 469.0 [190] | 2802.7 [190] | 3 [190] |
| xz/9 | 46.5% [191] | 9.3 [191] | 306.0 [191] | 2775.9 [191] | 3 [191] |
| zpaqfranz/m1 | 55.4% [192] | 431.1 [192] | 947.5 [192] | 1277.3 [192] | 3 [192] |
| zpaqfranz/m5 * | 40.1% [193] | 3.9 [193] | 3.8 [193] | 7644.9 [193] | 1 [193] |
| tsaur/default | skipped: not installed [194] | - | - | - | - |
| tsaur/max | skipped: not installed [195] | - | - | - | - |
| lpk/fast | 56.0% [368] | 643.2 [368] | 1383.5 [368] | 762.1 [368] | 3 [368] |
| lpk/balanced * | 43.2% [369] | 3.5 [369] | 28.7 [369] | 1600.6 [369] | 1 [369] |

### small-files: 20000 [196] files, 50016512 bytes [196]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 131.5% [196] | 78.4 [196] | 26.9 [196] | 6.0 [196] | 3 [196] |
| 7z/mx5 | 8.8% [197] | 8.2 [197] | 19.7 [197] | 393.7 [197] | 3 [197] |
| 7z/ultra | 7.8% [198] | 7.2 [198] | 20.0 [198] | 519.4 [198] | 3 [198] |
| rar/m3 | 20.4% [199] | 18.3 [199] | 11.2 [199] | 32.3 [199] | 3 [199] |
| rar/best | 20.4% [200] | 17.2 [200] | 11.2 [200] | 35.3 [200] | 3 [200] |
| rar/best-solid | 11.3% [201] | 10.0 [201] | 22.4 [201] | 1010.6 [201] | 3 [201] |
| rar/best-rr3 | 21.0% [202] | 16.8 [202] | 11.1 [202] | 36.2 [202] | 3 [202] |
| zstd/3 | 13.9% [203] | 72.2 [203] | 25.7 [203] | 78.1 [203] | 3 [203] |
| zstd/19 | 9.2% [204] | 5.7 [204] | 26.0 [204] | 235.4 [204] | 3 [204] |
| zstd/ultra22-long27 | 8.0% [205] | 1.6 [205] | 25.5 [205] | 781.1 [205] | 3 [205] |
| xz/6 | 9.3% [206] | 11.9 [206] | 24.7 [206] | 350.2 [206] | 3 [206] |
| xz/9 | 8.3% [207] | 4.2 [207] | 24.4 [207] | 678.3 [207] | 3 [207] |
| zpaqfranz/m1 | 14.2% [208] | 50.7 [208] | 17.1 [208] | 161.1 [208] | 3 [208] |
| zpaqfranz/m5 | 7.6% [209] | 1.1 [209] | 1.1 [209] | 776.5 [209] | 3 [209] |
| tsaur/default | skipped: not installed [210] | - | - | - | - |
| tsaur/max | skipped: not installed [211] | - | - | - | - |
| lpk/fast | 14.0% [370] | 32.0 [370] | 47.6 [370] | 109.3 [370] | 3 [370] |
| lpk/balanced | 10.1% [371] | 2.2 [371] | 40.0 [371] | 532.7 [371] | 3 [371] |

### software-installed: 9671 [212] files, 437043701 bytes [212]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 101.8% [212] | 643.4 [212] | 287.8 [212] | 6.1 [212] | 3 [212] |
| 7z/mx5 | 22.8% [213] | 11.3 [213] | 136.1 [213] | 1365.7 [213] | 3 [213] |
| 7z/ultra | 16.2% [214] | 6.6 [214] | 123.4 [214] | 2891.9 [214] | 3 [214] |
| rar/m3 | 34.2% [215] | 92.7 [215] | 126.5 [215] | 215.6 [215] | 3 [215] |
| rar/best | 34.1% [216] | 72.1 [216] | 128.4 [216] | 358.6 [216] | 3 [216] |
| rar/best-solid | 18.3% [217] | 42.6 [217] | 145.5 [217] | 2410.8 [217] | 3 [217] |
| rar/best-rr3 | 35.2% [218] | 71.0 [218] | 128.7 [218] | 359.0 [218] | 3 [218] |
| zstd/3 | 36.2% [219] | 513.6 [219] | 200.7 [219] | 313.5 [219] | 3 [219] |
| zstd/19 | 26.2% [220] | 34.6 [220] | 194.9 [220] | 1675.7 [220] | 3 [220] |
| zstd/ultra22-long27 * | 18.4% [221] | 3.5 [221] | 198.0 [221] | 1221.7 [221] | 1 [221] |
| xz/6 | 24.5% [222] | 45.7 [222] | 178.0 [222] | 2207.2 [222] | 3 [222] |
| xz/9 | 22.3% [223] | 7.0 [223] | 156.0 [223] | 2296.3 [223] | 3 [223] |
| zpaqfranz/m1 | 26.5% [224] | 273.7 [224] | 200.9 [224] | 846.5 [224] | 3 [224] |
| zpaqfranz/m5 * | 15.1% [225] | 4.5 [225] | 4.5 [225] | 4345.0 [225] | 1 [225] |
| tsaur/default | skipped: not installed [226] | - | - | - | - |
| tsaur/max | skipped: not installed [227] | - | - | - | - |
| lpk/fast | 23.6% [372] | 310.4 [372] | 515.9 [372] | 453.9 [372] | 3 [372] |
| lpk/balanced * | 17.1% [373] | 3.1 [373] | 107.2 [373] | 986.8 [373] | 1 [373] |

### source-git: 663 [228] files, 35365500 bytes [228]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 101.6% [228] | 627.5 [228] | 322.7 [228] | 6.0 [228] | 3 [228] |
| 7z/mx5 | 76.4% [229] | 20.9 [229] | 79.1 [229] | 368.3 [229] | 3 [229] |
| 7z/ultra | 76.3% [230] | 20.2 [230] | 76.1 [230] | 382.1 [230] | 3 [230] |
| rar/m3 | 79.8% [231] | 63.9 [231] | 84.9 [231] | 323.9 [231] | 3 [231] |
| rar/best | 79.8% [232] | 52.3 [232] | 95.3 [232] | 594.6 [232] | 3 [232] |
| rar/best-solid | 77.0% [233] | 49.3 [233] | 129.0 [233] | 1063.0 [233] | 3 [233] |
| rar/best-rr3 | 82.3% [234] | 50.1 [234] | 110.7 [234] | 594.7 [234] | 3 [234] |
| zstd/3 | 80.2% [235] | 398.3 [235] | 305.0 [235] | 73.4 [235] | 3 [235] |
| zstd/19 | 77.5% [236] | 6.7 [236] | 258.5 [236] | 229.2 [236] | 3 [236] |
| zstd/ultra22-long27 | 76.5% [237] | 5.8 [237] | 252.5 [237] | 772.0 [237] | 3 [237] |
| xz/6 | 77.5% [238] | 5.4 [238] | 108.3 [238] | 248.9 [238] | 3 [238] |
| xz/9 | 76.4% [239] | 3.9 [239] | 79.6 [239] | 437.8 [239] | 3 [239] |
| zpaqfranz/m1 | 81.8% [240] | 171.7 [240] | 160.9 [240] | 150.6 [240] | 3 [240] |
| zpaqfranz/m5 * | 75.9% [241] | 0.5 [241] | 0.5 [241] | 831.0 [241] | 1 [241] |
| tsaur/default | skipped: not installed [242] | - | - | - | - |
| tsaur/max | skipped: not installed [243] | - | - | - | - |
| lpk/fast | 83.3% [374] | 314.5 [374] | 621.4 [374] | 72.2 [374] | 3 [374] |
| lpk/balanced | 73.6% [375] | 1.4 [375] | 20.6 [375] | 354.9 [375] | 3 [375] |

### text-prose: 63 [244] files, 360144217 bytes [244]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.0% [244] | 1325.9 [244] | 1874.6 [244] | 6.0 [244] | 3 [244] |
| 7z/mx5 | 24.4% [245] | 5.9 [245] | 375.8 [245] | 1412.5 [245] | 3 [245] |
| 7z/ultra | 24.1% [246] | 4.3 [246] | 220.4 [246] | 3577.5 [246] | 3 [246] |
| rar/m3 | 27.4% [247] | 108.6 [247] | 437.9 [247] | 323.5 [247] | 3 [247] |
| rar/best | 26.5% [248] | 64.4 [248] | 430.0 [248] | 1382.4 [248] | 3 [248] |
| rar/best-solid | 25.9% [249] | 53.4 [249] | 399.2 [249] | 2406.5 [249] | 3 [249] |
| rar/best-rr3 | 27.3% [250] | 63.7 [250] | 421.2 [250] | 1382.5 [250] | 3 [250] |
| zstd/3 | 33.1% [251] | 884.3 [251] | 825.9 [251] | 305.7 [251] | 3 [251] |
| zstd/19 | 25.9% [252] | 15.9 [252] | 776.1 [252] | 1330.1 [252] | 3 [252] |
| zstd/ultra22-long27 * | 25.2% [253] | 1.9 [253] | 690.2 [253] | 1152.5 [253] | 1 [253] |
| xz/6 | 24.9% [254] | 22.3 [254] | 559.4 [254] | 1816.5 [254] | 3 [254] |
| xz/9 | 24.1% [255] | 3.3 [255] | 265.8 [255] | 1777.1 [255] | 3 [255] |
| zpaqfranz/m1 | 34.3% [256] | 322.1 [256] | 1270.9 [256] | 1236.8 [256] | 3 [256] |
| zpaqfranz/m5 * | 19.3% [257] | 3.6 [257] | 3.6 [257] | 5654.9 [257] | 1 [257] |
| tsaur/default | skipped: not installed [258] | - | - | - | - |
| tsaur/max | skipped: not installed [259] | - | - | - | - |
| lpk/fast | 32.8% [376] | 592.9 [376] | 1151.9 [376] | 604.7 [376] | 3 [376] |
| lpk/balanced * | 23.5% [377] | 2.2 [377] | 32.4 [377] | 1421.9 [377] | 1 [377] |

### video: 4 [260] files, 959731214 bytes [260]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.0% [260] | 1476.3 [260] | 2032.0 [260] | 5.9 [260] | 3 [260] |
| 7z/mx5 | 99.9% [261] | 96.2 [261] | 979.6 [261] | 4340.9 [261] | 3 [261] |
| 7z/ultra | 99.9% [262] | 49.7 [262] | 737.3 [262] | 10206.7 [262] | 3 [262] |
| rar/m3 | 100.0% [263] | 72.8 [263] | 853.5 [263] | 328.0 [263] | 3 [263] |
| rar/best | 100.0% [264] | 69.6 [264] | 803.2 [264] | 2410.3 [264] | 3 [264] |
| rar/best-solid | 100.0% [265] | 71.7 [265] | 365.3 [265] | 2406.5 [265] | 3 [265] |
| rar/best-rr3 | 103.0% [266] | 68.4 [266] | 826.7 [266] | 2410.4 [266] | 3 [266] |
| zstd/3 | 100.0% [267] | 904.2 [267] | 1306.6 [267] | 269.2 [267] | 3 [267] |
| zstd/19 | 99.9% [268] | 54.0 [268] | 1280.3 [268] | 3672.2 [268] | 3 [268] |
| zstd/ultra22-long27 * | 99.9% [269] | 6.5 [269] | 1260.8 [269] | 3182.3 [269] | 1 [269] |
| xz/6 | 99.9% [270] | 47.4 [270] | 565.8 [270] | 3698.4 [270] | 3 [270] |
| xz/9 | 99.9% [271] | 9.8 [271] | 551.1 [271] | 5199.5 [271] | 3 [271] |
| zpaqfranz/m1 | 100.1% [272] | 492.9 [272] | 1602.0 [272] | 1534.2 [272] | 3 [272] |
| zpaqfranz/m5 * | 99.7% [273] | 4.1 [273] | 4.1 [273] | 12756.0 [273] | 1 [273] |
| tsaur/default | skipped: not installed [274] | - | - | - | - |
| tsaur/max | skipped: not installed [275] | - | - | - | - |
| lpk/fast | 100.0% [378] | 1039.7 [378] | 2449.0 [378] | 632.5 [378] | 3 [378] |
| lpk/balanced | 100.0% [379] | 855.6 [379] | 798.7 [379] | 1001.2 [379] | 3 [379] |

### vm-image: 4 [276] files, 2155872426 bytes [276]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store | 100.0% [276] | 1397.8 [276] | 2148.5 [276] | 6.0 [276] | 3 [276] |
| 7z/mx5 | 21.5% [277] | 74.1 [277] | 1253.0 [277] | 5869.4 [277] | 3 [277] |
| 7z/ultra | 20.7% [278] | 32.2 [278] | 877.1 [278] | 21067.5 [278] | 3 [278] |
| rar/m3 | 22.8% [279] | 140.8 [279] | 577.6 [279] | 324.9 [279] | 3 [279] |
| rar/best | 22.0% [280] | 75.4 [280] | 563.5 [280] | 2408.8 [280] | 3 [280] |
| rar/best-solid | 22.0% [281] | 75.2 [281] | 565.0 [281] | 2408.3 [281] | 3 [281] |
| rar/best-rr3 | 22.7% [282] | 75.2 [282] | 562.4 [282] | 2408.5 [282] | 3 [282] |
| zstd/3 | 27.5% [283] | 1037.0 [283] | 1041.4 [283] | 349.1 [283] | 3 [283] |
| zstd/19 | 22.9% [284] | 52.1 [284] | 985.9 [284] | 3115.4 [284] | 3 [284] |
| zstd/ultra22-long27 * | 21.6% [285] | 9.0 [285] | 887.5 [285] | 5830.9 [285] | 1 [285] |
| xz/6 | 22.1% [286] | 74.2 [286] | 811.7 [286] | 3156.5 [286] | 3 [286] |
| xz/9 | 21.2% [287] | 27.9 [287] | 625.6 [287] | 9899.9 [287] | 3 [287] |
| zpaqfranz/m1 | 28.5% [288] | 480.2 [288] | 1591.1 [288] | 1829.5 [288] | 3 [288] |
| zpaqfranz/m5 * | 18.7% [289] | 7.9 [289] | 7.9 [289] | 20533.0 [289] | 1 [289] |
| tsaur/default | skipped: not installed [290] | - | - | - | - |
| tsaur/max | skipped: not installed [291] | - | - | - | - |
| lpk/fast | 25.8% [380] | 1160.3 [380] | 977.8 [380] | 660.4 [380] | 3 [380] |
| lpk/balanced * | 20.6% [381] | 3.2 [381] | 189.7 [381] | 1515.5 [381] | 1 [381] |

## 3. Baseline, blended

The disk mixes come from `bench/report-mixes.toml` [382], printed here in full:

```toml
# Disk mixes for `lpk-bench report` (docs/PLAN.md P0-5). Committed; the report prints this file's
# contents as its own source. Each mix lists `class = weight`: the weights are shares of bytes and
# must sum to 100; every class must be a corpus class (docs/CORPUS.md, D-15). A mix is "n/a" in the
# report when the corpus lacks one of its classes.
#
# Blending: a size ratio is the weighted sum of the per-class ratios; a speed is the bytes-weighted
# harmonic mean of the per-class speeds.

[[mix]]
name = "photo-doc"
weights = { photo-jpeg = 45, photo-jpeg-edited = 5, photo-raw-png = 5, office-pdf = 25, text-prose = 5, archives-nested = 5, audio = 5, small-files = 5 }

[[mix]]
name = "developer"
weights = { source-git = 15, backup-versions = 15, software-installed = 20, small-files = 10, logs-text = 15, text-prose = 5, archives-nested = 10, model-weights = 5, vm-image = 5 }

[[mix]]
name = "video-heavy"
weights = { video = 60, photo-jpeg = 15, audio = 10, encrypted-random = 5, office-pdf = 5, vm-image = 5 }
```

Size: the weighted sum of the per-class size ratios. Speed: the bytes-weighted harmonic mean of the per-class speeds. A tool/setting without a measured result for every class of the mix shows `n/a`.

### Mix `photo-doc`

Shares of bytes, in percent [382]: archives-nested 5, audio 5, office-pdf 25, photo-jpeg 45, photo-jpeg-edited 5, photo-raw-png 5, small-files 5, text-prose 5.

| tool/setting | blended size (% of mix bytes) | blended compress MB/s | blended extract MB/s |
|---|---|---|---|
| store/store | 101.6% [4,20,132,148,164,180,196,244,382] | 732.6 [4,20,132,148,164,180,196,244,382] | 412.1 [4,20,132,148,164,180,196,244,382] |
| 7z/mx5 | 77.1% [5,21,133,149,165,181,197,245,382] | 29.1 [5,21,133,149,165,181,197,245,382] | 222.8 [5,21,133,149,165,181,197,245,382] |
| 7z/ultra | 76.8% [6,22,134,150,166,182,198,246,382] | 21.6 [6,22,134,150,166,182,198,246,382] | 197.4 [6,22,134,150,166,182,198,246,382] |
| rar/m3 | 79.4% [7,23,135,151,167,183,199,247,382] | 102.7 [7,23,135,151,167,183,199,247,382] | 140.2 [7,23,135,151,167,183,199,247,382] |
| rar/best | 79.4% [8,24,136,152,168,184,200,248,382] | 83.9 [8,24,136,152,168,184,200,248,382] | 139.7 [8,24,136,152,168,184,200,248,382] |
| rar/best-solid * | 77.9% [9,25,137,153,169,185,201,249,382] | 46.5 [9,25,137,153,169,185,201,249,382] | 193.3 [9,25,137,153,169,185,201,249,382] |
| rar/best-rr3 | 81.8% [10,26,138,154,170,186,202,250,382] | 82.0 [10,26,138,154,170,186,202,250,382] | 137.5 [10,26,138,154,170,186,202,250,382] |
| zstd/3 | 81.2% [11,27,139,155,171,187,203,251,382] | 565.1 [11,27,139,155,171,187,203,251,382] | 347.5 [11,27,139,155,171,187,203,251,382] |
| zstd/19 * | 78.1% [12,28,140,156,172,188,204,252,382] | 27.0 [12,28,140,156,172,188,204,252,382] | 337.9 [12,28,140,156,172,188,204,252,382] |
| zstd/ultra22-long27 * | 77.6% [13,29,141,157,173,189,205,253,382] | 6.0 [13,29,141,157,173,189,205,253,382] | 316.1 [13,29,141,157,173,189,205,253,382] |
| xz/6 * | 77.8% [14,30,142,158,174,190,206,254,382] | 27.5 [14,30,142,158,174,190,206,254,382] | 256.3 [14,30,142,158,174,190,206,254,382] |
| xz/9 * | 77.2% [15,31,143,159,175,191,207,255,382] | 9.7 [15,31,143,159,175,191,207,255,382] | 202.0 [15,31,143,159,175,191,207,255,382] |
| zpaqfranz/m1 | 82.7% [16,32,144,160,176,192,208,256,382] | 310.1 [16,32,144,160,176,192,208,256,382] | 272.3 [16,32,144,160,176,192,208,256,382] |
| zpaqfranz/m5 * | 74.4% [17,33,145,161,177,193,209,257,382] | 2.5 [17,33,145,161,177,193,209,257,382] | 2.5 [17,33,145,161,177,193,209,257,382] |
| tsaur/default | n/a: class `archives-nested` has no measured result | - | - |
| tsaur/max | n/a: class `archives-nested` has no measured result | - | - |
| lpk/fast * | 71.1% [346,348,362,364,366,368,370,376,382] | 65.5 [346,348,362,364,366,368,370,376,382] | 200.7 [346,348,362,364,366,368,370,376,382] |
| lpk/balanced * | 65.7% [347,349,363,365,367,369,371,377,382] | 4.2 [347,349,363,365,367,369,371,377,382] | 46.5 [347,349,363,365,367,369,371,377,382] |

### Mix `developer`

Shares of bytes, in percent [382]: archives-nested 10, backup-versions 15, logs-text 15, model-weights 5, small-files 10, software-installed 20, source-git 15, text-prose 5, vm-image 5.

| tool/setting | blended size (% of mix bytes) | blended compress MB/s | blended extract MB/s |
|---|---|---|---|
| store/store | 104.8% [4,36,116,196,212,228,244,276,327,382] | 357.7 [4,36,116,196,212,228,244,276,327,382] | 149.0 [4,36,116,196,212,228,244,276,327,382] |
| 7z/mx5 | 32.7% [5,37,117,197,213,229,245,277,328,382] | 13.5 [5,37,117,197,213,229,245,277,328,382] | 81.8 [5,37,117,197,213,229,245,277,328,382] |
| 7z/ultra * | 31.1% [6,38,118,198,214,230,246,278,329,382] | 9.6 [6,38,118,198,214,230,246,278,329,382] | 72.5 [6,38,118,198,214,230,246,278,329,382] |
| rar/m3 | 41.7% [7,39,119,199,215,231,247,279,330,382] | 61.7 [7,39,119,199,215,231,247,279,330,382] | 54.3 [7,39,119,199,215,231,247,279,330,382] |
| rar/best | 41.5% [8,40,120,200,216,232,248,280,331,382] | 50.2 [8,40,120,200,216,232,248,280,331,382] | 60.1 [8,40,120,200,216,232,248,280,331,382] |
| rar/best-solid | 32.7% [9,41,121,201,217,233,249,281,332,382] | 36.2 [9,41,121,201,217,233,249,281,332,382] | 91.1 [9,41,121,201,217,233,249,281,332,382] |
| rar/best-rr3 | 42.8% [10,42,122,202,218,234,250,282,333,382] | 48.8 [10,42,122,202,218,234,250,282,333,382] | 60.6 [10,42,122,202,218,234,250,282,333,382] |
| zstd/3 | 42.1% [11,43,123,203,219,235,251,283,334,382] | 292.0 [11,43,123,203,219,235,251,283,334,382] | 132.6 [11,43,123,203,219,235,251,283,334,382] |
| zstd/19 | 36.3% [12,44,124,204,220,236,252,284,335,382] | 9.8 [12,44,124,204,220,236,252,284,335,382] | 131.0 [12,44,124,204,220,236,252,284,335,382] |
| zstd/ultra22-long27 * | 32.0% [13,45,125,205,221,237,253,285,336,382] | 3.5 [13,45,125,205,221,237,253,285,336,382] | 129.1 [13,45,125,205,221,237,253,285,336,382] |
| xz/6 | 35.9% [14,46,126,206,222,238,254,286,337,382] | 10.2 [14,46,126,206,222,238,254,286,337,382] | 102.7 [14,46,126,206,222,238,254,286,337,382] |
| xz/9 * | 32.4% [15,47,127,207,223,239,255,287,338,382] | 5.1 [15,47,127,207,223,239,255,287,338,382] | 88.8 [15,47,127,207,223,239,255,287,338,382] |
| zpaqfranz/m1 | 39.6% [16,48,128,208,224,240,256,288,339,382] | 155.9 [16,48,128,208,224,240,256,288,339,382] | 90.2 [16,48,128,208,224,240,256,288,339,382] |
| zpaqfranz/m5 * | 30.1% [17,49,129,209,225,241,257,289,340,382] | 1.2 [17,49,129,209,225,241,257,289,340,382] | 1.2 [17,49,129,209,225,241,257,289,340,382] |
| tsaur/default | n/a: class `archives-nested` has no measured result | - | - |
| tsaur/max | n/a: class `archives-nested` has no measured result | - | - |
| lpk/fast | 38.2% [346,350,358,360,370,372,374,376,380,382] | 168.1 [346,350,358,360,370,372,374,376,380,382] | 266.1 [346,350,358,360,370,372,374,376,380,382] |
| lpk/balanced * | 29.9% [347,351,359,361,371,373,375,377,381,382] | 2.2 [347,351,359,361,371,373,375,377,381,382] | 51.8 [347,351,359,361,371,373,375,377,381,382] |

### Mix `video-heavy`

Shares of bytes, in percent [382]: audio 10, encrypted-random 5, office-pdf 5, photo-jpeg 15, video 60, vm-image 5.

| tool/setting | blended size (% of mix bytes) | blended compress MB/s | blended extract MB/s |
|---|---|---|---|
| store/store | 100.0% [20,132,148,260,276,295,382] | 1397.4 [20,132,148,260,276,295,382] | 1958.0 [20,132,148,260,276,295,382] |
| 7z/mx5 | 91.2% [21,133,149,261,277,296,382] | 35.2 [21,133,149,261,277,296,382] | 508.8 [21,133,149,261,277,296,382] |
| 7z/ultra | 91.2% [22,134,150,262,278,297,382] | 26.3 [22,134,150,262,278,297,382] | 448.3 [22,134,150,262,278,297,382] |
| rar/m3 | 91.8% [23,135,151,263,279,298,382] | 84.7 [23,135,151,263,279,298,382] | 597.8 [23,135,151,263,279,298,382] |
| rar/best | 91.8% [24,136,152,264,280,299,382] | 75.3 [24,136,152,264,280,299,382] | 583.8 [24,136,152,264,280,299,382] |
| rar/best-solid * | 91.7% [25,137,153,265,281,300,382] | 65.6 [25,137,153,265,281,300,382] | 353.4 [25,137,153,265,281,300,382] |
| rar/best-rr3 | 94.6% [26,138,154,266,282,301,382] | 74.0 [26,138,154,266,282,301,382] | 579.0 [26,138,154,266,282,301,382] |
| zstd/3 | 93.7% [27,139,155,267,283,302,382] | 884.5 [27,139,155,267,283,302,382] | 1186.5 [27,139,155,267,283,302,382] |
| zstd/19 * | 92.3% [28,140,156,268,284,303,382] | 36.7 [28,140,156,268,284,303,382] | 1098.1 [28,140,156,268,284,303,382] |
| zstd/ultra22-long27 * | 92.2% [29,141,157,269,285,304,382] | 6.1 [29,141,157,269,285,304,382] | 1033.7 [29,141,157,269,285,304,382] |
| xz/6 * | 91.8% [30,142,158,270,286,305,382] | 37.9 [30,142,158,270,286,305,382] | 518.3 [30,142,158,270,286,305,382] |
| xz/9 * | 91.7% [31,143,159,271,287,306,382] | 7.6 [31,143,159,271,287,306,382] | 345.2 [31,143,159,271,287,306,382] |
| zpaqfranz/m1 | 94.2% [32,144,160,272,288,307,382] | 424.5 [32,144,160,272,288,307,382] | 1463.9 [32,144,160,272,288,307,382] |
| zpaqfranz/m5 * | 90.3% [33,145,161,273,289,308,382] | 3.2 [33,145,161,273,289,308,382] | 3.2 [33,145,161,273,289,308,382] |
| tsaur/default | n/a: class `audio` has no measured result | - | - |
| tsaur/max | n/a: class `audio` has no measured result | - | - |
| lpk/fast * | 90.6% [348,354,362,364,378,380,382] | 206.1 [348,354,362,364,378,380,382] | 607.7 [348,354,362,364,378,380,382] |
| lpk/balanced * | 88.6% [349,355,363,365,379,381,382] | 10.2 [349,355,363,365,379,381,382] | 120.8 [349,355,363,365,379,381,382] |

## 4. Probes

Each table below is rendered from the probe's JSON by the same function `lpk-bench probe` uses; the figures inside carry the one source named in the line above them.

### Probe `jpeg`

Not available: no `probe-jpeg.json` was given.

### Probe `deflate`

Not available: no `probe-deflate.json` was given.

### Probe `dedup`

Not available: no `probe-dedup.json` was given.

### Probe `text`

Not available: no `probe-text.json` was given.

### Probe `weights`

Not available: no `probe-weights.json` was given.

### Probe `entropy-gate`

Not available: no `probe-entropy-gate.json` was given.

## 5. Estimates

The estimate column is the figure derived from the probes by the rule for the class; it combines component measurements and assumes nothing about the container, deduplication across classes or the Fold stage. Measured and estimated figures are in separate columns. In the per-class table the best measured incumbent is the smallest archive among all measured tools and settings of that class (a `*` marks a row that rests on one measurement). For the disk mixes and for gate G1 the best measured incumbent is the best single tool x setting over the combined bytes, because D-07 names one competitor; the sum of the per-class bests (which no single tool achieves) is printed as a stricter second figure. For a class with no rule the estimate cell reads `= best incumbent, no rule`, for video and encrypted-random `stored, no rule`: no gain is claimed there.

| class | class bytes | best measured incumbent | best incumbent size | best incumbent, % of class | 7-Zip Ultra size | 7-Zip Ultra, % of class | LitePack Balanced, estimate | estimate, % of class |
|---|---|---|---|---|---|---|---|---|
| archives-nested | 34975551 bytes [4] | 7z/ultra | 27628237 bytes [4-17] | 79.0% of class [4-17] | 27628237 bytes [6] | 79.0% of class [4,6] | not available | - |
| audio | 157896311 bytes [20] | zpaqfranz/m5 * | 111771431 bytes [20-33] | 70.8% of class [20-33] | 113622569 bytes [22] | 72.0% of class [20,22] | = best incumbent, no rule [20-33] | 70.8% of class [20-33] |
| backup-versions | 25541629 bytes [36] | zpaqfranz/m5 | 1701958 bytes [36-49] | 6.7% of class [36-49] | 1886039 bytes [38] | 7.4% of class [36,38] | not available | - |
| backup-versions-large | 1663755751 bytes [52] | zpaqfranz/m5 * | 92280623 bytes [53-58,64,65] | 5.5% of class [52-58,64,65] | 97948125 bytes [54] | 5.9% of class [52,54] | not available | - |
| encrypted-random | 419430400 bytes [295] | rar/m3 | 419430773 bytes [295-308] | 100.0% of class [295-308] | 419456591 bytes [297] | 100.0% of class [295,297] | stored, no rule [295] | 100.0% of class [295] |
| game-assets | 52470892 bytes [311] | zpaqfranz/m5 * | 16153972 bytes [311-324] | 30.8% of class [311-324] | 16365194 bytes [313] | 31.2% of class [311,313] | not available | - |
| logs-text | 2686418017 bytes [327] | zpaqfranz/m5 * | 80559666 bytes [327-340] | 3.0% of class [327-340] | 138758054 bytes [329] | 5.2% of class [327,329] | not available | - |
| model-weights | 286816945 bytes [116] | zpaqfranz/m5 * | 192156105 bytes [116-129] | 67.0% of class [116-129] | 205806322 bytes [118] | 71.8% of class [116,118] | not available | - |
| office-pdf | 2114690575 bytes [132] | zpaqfranz/m5 * | 1376087606 bytes [132-145] | 65.1% of class [132-145] | 1405332301 bytes [134] | 66.5% of class [132,134] | not available | - |
| photo-jpeg | 8258908517 bytes [148] | zpaqfranz/m5 * | 7818217296 bytes [148-161] | 94.7% of class [148-161] | 8074268136 bytes [150] | 97.8% of class [148,150] | not available | - |
| photo-jpeg-edited | 550674750 bytes [164] | zpaqfranz/m5 * | 501953216 bytes [164-177] | 91.2% of class [164-177] | 526737357 bytes [166] | 95.7% of class [164,166] | not available | - |
| photo-raw-png | 537693574 bytes [180] | zpaqfranz/m5 * | 215648584 bytes [180-193] | 40.1% of class [180-193] | 249105724 bytes [182] | 46.3% of class [180,182] | not available | - |
| small-files | 50016512 bytes [196] | zpaqfranz/m5 | 3801282 bytes [196-209] | 7.6% of class [196-209] | 3882600 bytes [198] | 7.8% of class [196,198] | not available | - |
| software-installed | 437043701 bytes [212] | zpaqfranz/m5 * | 65988210 bytes [212-225] | 15.1% of class [212-225] | 70776223 bytes [214] | 16.2% of class [212,214] | not available | - |
| source-git | 35365500 bytes [228] | zpaqfranz/m5 * | 26854709 bytes [228-241] | 75.9% of class [228-241] | 26980314 bytes [230] | 76.3% of class [228,230] | = best incumbent, no rule [228-241] | 75.9% of class [228-241] |
| text-prose | 360144217 bytes [244] | zpaqfranz/m5 * | 69437169 bytes [244-257] | 19.3% of class [244-257] | 86704329 bytes [246] | 24.1% of class [244,246] | not available | - |
| video | 959731214 bytes [260] | zpaqfranz/m5 * | 957208600 bytes [260-273] | 99.7% of class [260-273] | 959084649 bytes [262] | 99.9% of class [260,262] | stored, no rule [260] | 100.0% of class [260] |
| vm-image | 2155872426 bytes [276] | zpaqfranz/m5 * | 402886929 bytes [276-289] | 18.7% of class [276-289] | 445693090 bytes [278] | 20.7% of class [276,278] | = best incumbent, no rule [276-289] | 18.7% of class [276-289] |

Rules, one per class group:

- photo-jpeg, photo-jpeg-edited: bytes after Lepton with failed files stored as-is (probe jpeg, `after fallback`).
- office-pdf, archives-nested, software-installed, game-assets, photo-raw-png: probe deflate figure B with xz preset 9: reconstructed streams replaced by plain data, corrections added, whole file compressed.
- backup-versions, backup-versions-large: the smaller of (a) the sum over versions of the new unique chunks compressed with zstd level 19 (version 1 whole) and (b) version 1 at zstd level 19 plus the zstd --patch-from patches of the later versions (probe dedup); the report names which was used. Gate G2 is evaluated on backup-versions-large only (D-43), against the best tool x setting that the catalogue does not flag `dedup`; deduplicating tools are printed as reference, not compared; every version of the class must exceed the largest incumbent dictionary window, 256 MiB (WinRAR -md256m in bench/tools.toml), else G2 is not evaluable.
- text-prose, logs-text, small-files: the smallest verified row of probe text for the class (xz, zstd, bsc or kanzi, whichever ran); the report names the row.
- model-weights: the smaller total of byte planes and rotated planes (probe weights) when the probe parsed at least one file, otherwise not available.
- video, encrypted-random: stored as-is (the class bytes); every other class: the best measured incumbent, no gain claimed.

What each class's estimate used:

- archives-nested: not available: probe deflate is not available.
- audio: no rule: the best measured incumbent (zpaqfranz/m5 *); no gain is claimed.
- backup-versions: not available: probe dedup is not available.
- backup-versions-large: not available: probe dedup is not available.
- encrypted-random: stored as-is.
- game-assets: not available: probe deflate is not available.
- logs-text: not available: probe text is not available.
- model-weights: not available: probe weights is not available.
- office-pdf: not available: probe deflate is not available.
- photo-jpeg: not available: probe jpeg is not available.
- photo-jpeg-edited: not available: probe jpeg is not available.
- photo-raw-png: not available: probe deflate is not available.
- small-files: not available: probe text is not available.
- software-installed: not available: probe deflate is not available.
- source-git: no rule: the best measured incumbent (zpaqfranz/m5 *); no gain is claimed.
- text-prose: not available: probe text is not available.
- video: stored as-is.
- vm-image: no rule: the best measured incumbent (zpaqfranz/m5 *); no gain is claimed.

Per disk mix (ratios of the mix's bytes: the weighted sum of the class ratios above):

| mix | best measured incumbent (single tool x setting) | best single tool, % of mix bytes | stricter: per-class bests | 7-Zip Ultra | LitePack Balanced, estimate |
|---|---|---|---|---|---|
| photo-doc | lpk/balanced * | 65.7% [4-17,20-33,132-145,148-161,164-177,180-193,196-209,244-257,346-349,362-371,376,377,382] | 74.3% [4-17,20-33,132-145,148-161,164-177,180-193,196-209,244-257,382] | 76.8% [4,6,20,22,132,134,148,150,164,166,180,182,196,198,244,246,382] | n/a |
| developer | lpk/balanced * | 29.9% [4-17,36-49,116-129,196-209,212-225,228-241,244-257,276-289,327-340,346,347,350,351,358-361,370-377,380-382] | 29.8% [4-17,36-49,116-129,196-209,212-225,228-241,244-257,276-289,327-340,382] | 31.1% [4,6,36,38,116,118,196,198,212,214,228,230,244,246,276,278,327,329,382] | n/a |
| video-heavy | lpk/balanced * | 88.6% [20-33,132-145,148-161,260-273,276-289,295-308,348,349,354,355,362-365,378-382] | 90.3% [20-33,132-145,148-161,260-273,276-289,295-308,382] | 91.2% [20,22,132,134,148,150,260,262,276,278,295,297,382] | n/a |

- mix photo-doc, estimate: n/a: class `archives-nested`: probe deflate is not available.
- mix developer, estimate: n/a: class `archives-nested`: probe deflate is not available.
- mix video-heavy, estimate: n/a: class `office-pdf`: probe deflate is not available.

## 6. Gates (D-07)

| gate | numbers | result |
|---|---|---|
| G1 photo/document size (measured `lpk` rows): on photo-jpeg, photo-jpeg-edited and office-pdf together, lpk/balanced at most 90% of the best measured incumbent (the best single tool x setting over the combined bytes; D-07's fallback wording: WinZip and PowerArchiver were not measured) and at most 85% of 7-Zip Ultra; the sum of the per-class bests (stricter) and the three classes alone are shown for information | combined lpk/balanced*: 8195078057 bytes [363,365,367]<br>combined best single tool (zpaqfranz/m5 *): 9696258118 bytes [132-145,148-161,164-177]<br>combined 7-Zip Ultra: 10006337794 bytes [134,150,166]<br>combined lpk/balanced* / best single tool: 84.5% [132-145,148-161,164-177,363,365,367]<br>combined lpk/balanced* / 7-Zip Ultra: 81.9% [134,150,166,363,365,367]<br>stricter, sum of the per-class bests (no single tool): 9696258118 bytes [132-145,148-161,164-177]<br>combined lpk/balanced* / sum of the per-class bests: 84.5% [132-145,148-161,164-177,363,365,367]<br>photo-jpeg alone, lpk/balanced* / best incumbent: 82.5% [148-161,365]<br>photo-jpeg alone, lpk/balanced* / 7-Zip Ultra: 79.9% [150,365]<br>photo-jpeg-edited alone, lpk/balanced / best incumbent: 82.4% [164-177,367]<br>photo-jpeg-edited alone, lpk/balanced / 7-Zip Ultra: 78.6% [166,367]<br>office-pdf alone, lpk/balanced* / best incumbent: 96.9% [132-145,363]<br>office-pdf alone, lpk/balanced* / 7-Zip Ultra: 94.9% [134,363] | PASS |
| G2 versioned backup (D-27, D-43; measured `lpk` rows): on backup-versions-large, lpk/balanced at most 50% of the best measured tool x setting among the tools that do not deduplicate across files | tools treated as deduplicating: tsaur, zpaqfranz<br>lpk/balanced*: 135050542 bytes [353]<br>best incumbent, not deduplicating (7z/ultra): 97948125 bytes [53-58]<br>lpk/balanced* / best incumbent: 137.9% [53-58,353]<br>reference, not compared (deduplicating tool tsaur): no measured row<br>reference, not compared (deduplicating tool zpaqfranz/m5 *): 92280623 bytes [64,65] | FAIL |
| G3 video store speed (D-27; measured `lpk` rows): lpk/fast's compress MB/s on video is at least 80% of the store control's compress MB/s on video (a real program reading and writing the files) in the same run or, pooled per D-48, the same baseline (the directories are named) | rows compared: lpk/fast from `bench/results/2026-10-20-megatron-2` [378], store/store from `bench/results/2026-10-17-megatron-15` [260]: two runs of one pooled baseline (D-27 asks for one run; D-48's pooling stands in for it: same host, corpus manifest, thread count and catalogue, checked when the directories were pooled)<br>lpk/fast compress on video: 1039.7 MB/s [378]<br>store/store compress on video: 1476.3 MB/s [260]<br>video class bytes: 959731214 bytes [378]<br>lpk/fast median compress wall seconds on video: 0.923 s [378]<br>lpk/fast first repeat compress on video: 1039.7 MB/s [378]<br>store/store median compress wall seconds on video: 0.650 s [260]<br>store/store first repeat compress on video: 1353.3 MB/s [260]<br>lpk/fast / store/store: 70.4% [260,378] | FAIL |
| G4 fast-tier extraction (measured `lpk` rows): the lpk/fast extraction MB/s is at least the 7z/mx5 extraction MB/s, blended over each disk mix | photo-doc: lpk/fast* extract: 200.7 MB/s [346,348,362,364,366,368,370,376,382]<br>photo-doc: 7z/mx5 extract: 222.8 MB/s [5,21,133,149,165,181,197,245,382]<br>developer: lpk/fast extract: 266.1 MB/s [346,350,358,360,370,372,374,376,380,382]<br>developer: 7z/mx5 extract: 81.8 MB/s [5,37,117,197,213,229,245,277,328,382]<br>video-heavy: lpk/fast* extract: 607.7 MB/s [348,354,362,364,378,380,382]<br>video-heavy: 7z/mx5 extract: 508.8 MB/s [21,133,149,261,277,296,382] | FAIL |

**NO-GO proposal: failing gates: G2, G3, G4**

- G1 inputs, skipped: tsaur/default (not installed); tsaur/max (not installed)
- G1 input: lpk/balanced on photo-jpeg rests on one measurement (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated)
- G1 input: lpk/balanced on office-pdf rests on one measurement (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated)
- G2 inputs, failed: store/store on backup-versions-large (compress: exit code -1073741819); zstd/3 on backup-versions-large (tar step: exit code -1073741819); zstd/19 on backup-versions-large (tar step: exit code -1073741819); zstd/ultra22-long27 on backup-versions-large (tar step: exit code -1073741819); xz/6 on backup-versions-large (tar step: exit code -1073741819); xz/9 on backup-versions-large (tar step: exit code -1073741819). skipped: tsaur/default (not installed); tsaur/max (not installed)
- G2 input: lpk/balanced on backup-versions-large rests on one measurement (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated)
- G3: the two rows are from different runs (`bench/results/2026-10-20-megatron-2`, `bench/results/2026-10-17-megatron-15`); the comparison rests on D-48's pooling checks, not on D-27's same run

PASS needs the stated numbers; a gate whose data is missing is `not evaluable` and is never a PASS. A `*` after a tool name marks a row that rests on one measurement. The gates are in their Phase 1 form: the pooled baseline holds measured rows of `lpk`, so each gate compares those rows (not the Phase 0 estimates or proxies) with the incumbents measured in the same pooled baseline (D-48). G1 uses the fallback wording of D-07 with the best single tool x setting over the combined bytes; the stricter sum of per-class bests is printed beneath it. G2 compares `lpk/balanced` with the best measured tool that does not deduplicate across files (D-43); deduplicating tools are printed as references. G3 compares `lpk/fast` with the baseline `store` control, a real program reading and writing the files, in the same pooled baseline (D-27); the probe's raw read rate, when a probe is given, is information only. G4 compares `lpk/fast` with 7-Zip `-mx5` (D-07). The verdict is recorded by a person in a decision.

## 7. Sources

1. `bench/results/2026-10-17-megatron-15/host.json`
2. `bench/results/2026-10-17-megatron-15/tools.json`
3. `bench/results/2026-10-17-megatron-15/run.json`
4. `bench/results/2026-10-17-megatron-15/store-store-archives-nested.json`
5. `bench/results/2026-10-17-megatron-15/7z-mx5-archives-nested.json`
6. `bench/results/2026-10-17-megatron-15/7z-ultra-archives-nested.json`
7. `bench/results/2026-10-17-megatron-15/rar-m3-archives-nested.json`
8. `bench/results/2026-10-17-megatron-15/rar-best-archives-nested.json`
9. `bench/results/2026-10-17-megatron-15/rar-best-solid-archives-nested.json`
10. `bench/results/2026-10-17-megatron-15/rar-best-rr3-archives-nested.json`
11. `bench/results/2026-10-17-megatron-15/zstd-3-archives-nested.json`
12. `bench/results/2026-10-17-megatron-15/zstd-19-archives-nested.json`
13. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-archives-nested.json`
14. `bench/results/2026-10-17-megatron-15/xz-6-archives-nested.json`
15. `bench/results/2026-10-17-megatron-15/xz-9-archives-nested.json`
16. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-archives-nested.json`
17. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-archives-nested.json`
18. `bench/results/2026-10-17-megatron-15/tsaur-default-archives-nested.json`
19. `bench/results/2026-10-17-megatron-15/tsaur-max-archives-nested.json`
20. `bench/results/2026-10-17-megatron-15/store-store-audio.json`
21. `bench/results/2026-10-17-megatron-15/7z-mx5-audio.json`
22. `bench/results/2026-10-17-megatron-15/7z-ultra-audio.json`
23. `bench/results/2026-10-17-megatron-15/rar-m3-audio.json`
24. `bench/results/2026-10-17-megatron-15/rar-best-audio.json`
25. `bench/results/2026-10-17-megatron-15/rar-best-solid-audio.json`
26. `bench/results/2026-10-17-megatron-15/rar-best-rr3-audio.json`
27. `bench/results/2026-10-17-megatron-15/zstd-3-audio.json`
28. `bench/results/2026-10-17-megatron-15/zstd-19-audio.json`
29. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-audio.json`
30. `bench/results/2026-10-17-megatron-15/xz-6-audio.json`
31. `bench/results/2026-10-17-megatron-15/xz-9-audio.json`
32. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-audio.json`
33. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-audio.json`
34. `bench/results/2026-10-17-megatron-15/tsaur-default-audio.json`
35. `bench/results/2026-10-17-megatron-15/tsaur-max-audio.json`
36. `bench/results/2026-10-17-megatron-15/store-store-backup-versions.json`
37. `bench/results/2026-10-17-megatron-15/7z-mx5-backup-versions.json`
38. `bench/results/2026-10-17-megatron-15/7z-ultra-backup-versions.json`
39. `bench/results/2026-10-17-megatron-15/rar-m3-backup-versions.json`
40. `bench/results/2026-10-17-megatron-15/rar-best-backup-versions.json`
41. `bench/results/2026-10-17-megatron-15/rar-best-solid-backup-versions.json`
42. `bench/results/2026-10-17-megatron-15/rar-best-rr3-backup-versions.json`
43. `bench/results/2026-10-17-megatron-15/zstd-3-backup-versions.json`
44. `bench/results/2026-10-17-megatron-15/zstd-19-backup-versions.json`
45. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-backup-versions.json`
46. `bench/results/2026-10-17-megatron-15/xz-6-backup-versions.json`
47. `bench/results/2026-10-17-megatron-15/xz-9-backup-versions.json`
48. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-backup-versions.json`
49. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-backup-versions.json`
50. `bench/results/2026-10-17-megatron-15/tsaur-default-backup-versions.json`
51. `bench/results/2026-10-17-megatron-15/tsaur-max-backup-versions.json`
52. `bench/results/2026-10-17-megatron-15/store-store-backup-versions-large.json`
53. `bench/results/2026-10-17-megatron-15/7z-mx5-backup-versions-large.json`
54. `bench/results/2026-10-17-megatron-15/7z-ultra-backup-versions-large.json`
55. `bench/results/2026-10-17-megatron-15/rar-m3-backup-versions-large.json`
56. `bench/results/2026-10-17-megatron-15/rar-best-backup-versions-large.json`
57. `bench/results/2026-10-17-megatron-15/rar-best-solid-backup-versions-large.json`
58. `bench/results/2026-10-17-megatron-15/rar-best-rr3-backup-versions-large.json`
59. `bench/results/2026-10-17-megatron-15/zstd-3-backup-versions-large.json`
60. `bench/results/2026-10-17-megatron-15/zstd-19-backup-versions-large.json`
61. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-backup-versions-large.json`
62. `bench/results/2026-10-17-megatron-15/xz-6-backup-versions-large.json`
63. `bench/results/2026-10-17-megatron-15/xz-9-backup-versions-large.json`
64. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-backup-versions-large.json`
65. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-backup-versions-large.json`
66. `bench/results/2026-10-17-megatron-15/tsaur-default-backup-versions-large.json`
67. `bench/results/2026-10-17-megatron-15/tsaur-max-backup-versions-large.json`
68. `bench/results/2026-10-17-megatron-15/store-store-encrypted-random.json`
69. `bench/results/2026-10-17-megatron-15/7z-mx5-encrypted-random.json`
70. `bench/results/2026-10-17-megatron-15/7z-ultra-encrypted-random.json`
71. `bench/results/2026-10-17-megatron-15/rar-m3-encrypted-random.json`
72. `bench/results/2026-10-17-megatron-15/rar-best-encrypted-random.json`
73. `bench/results/2026-10-17-megatron-15/rar-best-solid-encrypted-random.json`
74. `bench/results/2026-10-17-megatron-15/rar-best-rr3-encrypted-random.json`
75. `bench/results/2026-10-17-megatron-15/zstd-3-encrypted-random.json`
76. `bench/results/2026-10-17-megatron-15/zstd-19-encrypted-random.json`
77. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-encrypted-random.json`
78. `bench/results/2026-10-17-megatron-15/xz-6-encrypted-random.json`
79. `bench/results/2026-10-17-megatron-15/xz-9-encrypted-random.json`
80. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-encrypted-random.json`
81. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-encrypted-random.json`
82. `bench/results/2026-10-17-megatron-15/tsaur-default-encrypted-random.json`
83. `bench/results/2026-10-17-megatron-15/tsaur-max-encrypted-random.json`
84. `bench/results/2026-10-17-megatron-15/store-store-game-assets.json`
85. `bench/results/2026-10-17-megatron-15/7z-mx5-game-assets.json`
86. `bench/results/2026-10-17-megatron-15/7z-ultra-game-assets.json`
87. `bench/results/2026-10-17-megatron-15/rar-m3-game-assets.json`
88. `bench/results/2026-10-17-megatron-15/rar-best-game-assets.json`
89. `bench/results/2026-10-17-megatron-15/rar-best-solid-game-assets.json`
90. `bench/results/2026-10-17-megatron-15/rar-best-rr3-game-assets.json`
91. `bench/results/2026-10-17-megatron-15/zstd-3-game-assets.json`
92. `bench/results/2026-10-17-megatron-15/zstd-19-game-assets.json`
93. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-game-assets.json`
94. `bench/results/2026-10-17-megatron-15/xz-6-game-assets.json`
95. `bench/results/2026-10-17-megatron-15/xz-9-game-assets.json`
96. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-game-assets.json`
97. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-game-assets.json`
98. `bench/results/2026-10-17-megatron-15/tsaur-default-game-assets.json`
99. `bench/results/2026-10-17-megatron-15/tsaur-max-game-assets.json`
100. `bench/results/2026-10-17-megatron-15/store-store-logs-text.json`
101. `bench/results/2026-10-17-megatron-15/7z-mx5-logs-text.json`
102. `bench/results/2026-10-17-megatron-15/7z-ultra-logs-text.json`
103. `bench/results/2026-10-17-megatron-15/rar-m3-logs-text.json`
104. `bench/results/2026-10-17-megatron-15/rar-best-logs-text.json`
105. `bench/results/2026-10-17-megatron-15/rar-best-solid-logs-text.json`
106. `bench/results/2026-10-17-megatron-15/rar-best-rr3-logs-text.json`
107. `bench/results/2026-10-17-megatron-15/zstd-3-logs-text.json`
108. `bench/results/2026-10-17-megatron-15/zstd-19-logs-text.json`
109. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-logs-text.json`
110. `bench/results/2026-10-17-megatron-15/xz-6-logs-text.json`
111. `bench/results/2026-10-17-megatron-15/xz-9-logs-text.json`
112. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-logs-text.json`
113. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-logs-text.json`
114. `bench/results/2026-10-17-megatron-15/tsaur-default-logs-text.json`
115. `bench/results/2026-10-17-megatron-15/tsaur-max-logs-text.json`
116. `bench/results/2026-10-17-megatron-15/store-store-model-weights.json`
117. `bench/results/2026-10-17-megatron-15/7z-mx5-model-weights.json`
118. `bench/results/2026-10-17-megatron-15/7z-ultra-model-weights.json`
119. `bench/results/2026-10-17-megatron-15/rar-m3-model-weights.json`
120. `bench/results/2026-10-17-megatron-15/rar-best-model-weights.json`
121. `bench/results/2026-10-17-megatron-15/rar-best-solid-model-weights.json`
122. `bench/results/2026-10-17-megatron-15/rar-best-rr3-model-weights.json`
123. `bench/results/2026-10-17-megatron-15/zstd-3-model-weights.json`
124. `bench/results/2026-10-17-megatron-15/zstd-19-model-weights.json`
125. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-model-weights.json`
126. `bench/results/2026-10-17-megatron-15/xz-6-model-weights.json`
127. `bench/results/2026-10-17-megatron-15/xz-9-model-weights.json`
128. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-model-weights.json`
129. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-model-weights.json`
130. `bench/results/2026-10-17-megatron-15/tsaur-default-model-weights.json`
131. `bench/results/2026-10-17-megatron-15/tsaur-max-model-weights.json`
132. `bench/results/2026-10-17-megatron-15/store-store-office-pdf.json`
133. `bench/results/2026-10-17-megatron-15/7z-mx5-office-pdf.json`
134. `bench/results/2026-10-17-megatron-15/7z-ultra-office-pdf.json`
135. `bench/results/2026-10-17-megatron-15/rar-m3-office-pdf.json`
136. `bench/results/2026-10-17-megatron-15/rar-best-office-pdf.json`
137. `bench/results/2026-10-17-megatron-15/rar-best-solid-office-pdf.json`
138. `bench/results/2026-10-17-megatron-15/rar-best-rr3-office-pdf.json`
139. `bench/results/2026-10-17-megatron-15/zstd-3-office-pdf.json`
140. `bench/results/2026-10-17-megatron-15/zstd-19-office-pdf.json`
141. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-office-pdf.json`
142. `bench/results/2026-10-17-megatron-15/xz-6-office-pdf.json`
143. `bench/results/2026-10-17-megatron-15/xz-9-office-pdf.json`
144. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-office-pdf.json`
145. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-office-pdf.json`
146. `bench/results/2026-10-17-megatron-15/tsaur-default-office-pdf.json`
147. `bench/results/2026-10-17-megatron-15/tsaur-max-office-pdf.json`
148. `bench/results/2026-10-17-megatron-15/store-store-photo-jpeg.json`
149. `bench/results/2026-10-17-megatron-15/7z-mx5-photo-jpeg.json`
150. `bench/results/2026-10-17-megatron-15/7z-ultra-photo-jpeg.json`
151. `bench/results/2026-10-17-megatron-15/rar-m3-photo-jpeg.json`
152. `bench/results/2026-10-17-megatron-15/rar-best-photo-jpeg.json`
153. `bench/results/2026-10-17-megatron-15/rar-best-solid-photo-jpeg.json`
154. `bench/results/2026-10-17-megatron-15/rar-best-rr3-photo-jpeg.json`
155. `bench/results/2026-10-17-megatron-15/zstd-3-photo-jpeg.json`
156. `bench/results/2026-10-17-megatron-15/zstd-19-photo-jpeg.json`
157. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-photo-jpeg.json`
158. `bench/results/2026-10-17-megatron-15/xz-6-photo-jpeg.json`
159. `bench/results/2026-10-17-megatron-15/xz-9-photo-jpeg.json`
160. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-photo-jpeg.json`
161. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-photo-jpeg.json`
162. `bench/results/2026-10-17-megatron-15/tsaur-default-photo-jpeg.json`
163. `bench/results/2026-10-17-megatron-15/tsaur-max-photo-jpeg.json`
164. `bench/results/2026-10-17-megatron-15/store-store-photo-jpeg-edited.json`
165. `bench/results/2026-10-17-megatron-15/7z-mx5-photo-jpeg-edited.json`
166. `bench/results/2026-10-17-megatron-15/7z-ultra-photo-jpeg-edited.json`
167. `bench/results/2026-10-17-megatron-15/rar-m3-photo-jpeg-edited.json`
168. `bench/results/2026-10-17-megatron-15/rar-best-photo-jpeg-edited.json`
169. `bench/results/2026-10-17-megatron-15/rar-best-solid-photo-jpeg-edited.json`
170. `bench/results/2026-10-17-megatron-15/rar-best-rr3-photo-jpeg-edited.json`
171. `bench/results/2026-10-17-megatron-15/zstd-3-photo-jpeg-edited.json`
172. `bench/results/2026-10-17-megatron-15/zstd-19-photo-jpeg-edited.json`
173. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-photo-jpeg-edited.json`
174. `bench/results/2026-10-17-megatron-15/xz-6-photo-jpeg-edited.json`
175. `bench/results/2026-10-17-megatron-15/xz-9-photo-jpeg-edited.json`
176. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-photo-jpeg-edited.json`
177. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-photo-jpeg-edited.json`
178. `bench/results/2026-10-17-megatron-15/tsaur-default-photo-jpeg-edited.json`
179. `bench/results/2026-10-17-megatron-15/tsaur-max-photo-jpeg-edited.json`
180. `bench/results/2026-10-17-megatron-15/store-store-photo-raw-png.json`
181. `bench/results/2026-10-17-megatron-15/7z-mx5-photo-raw-png.json`
182. `bench/results/2026-10-17-megatron-15/7z-ultra-photo-raw-png.json`
183. `bench/results/2026-10-17-megatron-15/rar-m3-photo-raw-png.json`
184. `bench/results/2026-10-17-megatron-15/rar-best-photo-raw-png.json`
185. `bench/results/2026-10-17-megatron-15/rar-best-solid-photo-raw-png.json`
186. `bench/results/2026-10-17-megatron-15/rar-best-rr3-photo-raw-png.json`
187. `bench/results/2026-10-17-megatron-15/zstd-3-photo-raw-png.json`
188. `bench/results/2026-10-17-megatron-15/zstd-19-photo-raw-png.json`
189. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-photo-raw-png.json`
190. `bench/results/2026-10-17-megatron-15/xz-6-photo-raw-png.json`
191. `bench/results/2026-10-17-megatron-15/xz-9-photo-raw-png.json`
192. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-photo-raw-png.json`
193. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-photo-raw-png.json`
194. `bench/results/2026-10-17-megatron-15/tsaur-default-photo-raw-png.json`
195. `bench/results/2026-10-17-megatron-15/tsaur-max-photo-raw-png.json`
196. `bench/results/2026-10-17-megatron-15/store-store-small-files.json`
197. `bench/results/2026-10-17-megatron-15/7z-mx5-small-files.json`
198. `bench/results/2026-10-17-megatron-15/7z-ultra-small-files.json`
199. `bench/results/2026-10-17-megatron-15/rar-m3-small-files.json`
200. `bench/results/2026-10-17-megatron-15/rar-best-small-files.json`
201. `bench/results/2026-10-17-megatron-15/rar-best-solid-small-files.json`
202. `bench/results/2026-10-17-megatron-15/rar-best-rr3-small-files.json`
203. `bench/results/2026-10-17-megatron-15/zstd-3-small-files.json`
204. `bench/results/2026-10-17-megatron-15/zstd-19-small-files.json`
205. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-small-files.json`
206. `bench/results/2026-10-17-megatron-15/xz-6-small-files.json`
207. `bench/results/2026-10-17-megatron-15/xz-9-small-files.json`
208. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-small-files.json`
209. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-small-files.json`
210. `bench/results/2026-10-17-megatron-15/tsaur-default-small-files.json`
211. `bench/results/2026-10-17-megatron-15/tsaur-max-small-files.json`
212. `bench/results/2026-10-17-megatron-15/store-store-software-installed.json`
213. `bench/results/2026-10-17-megatron-15/7z-mx5-software-installed.json`
214. `bench/results/2026-10-17-megatron-15/7z-ultra-software-installed.json`
215. `bench/results/2026-10-17-megatron-15/rar-m3-software-installed.json`
216. `bench/results/2026-10-17-megatron-15/rar-best-software-installed.json`
217. `bench/results/2026-10-17-megatron-15/rar-best-solid-software-installed.json`
218. `bench/results/2026-10-17-megatron-15/rar-best-rr3-software-installed.json`
219. `bench/results/2026-10-17-megatron-15/zstd-3-software-installed.json`
220. `bench/results/2026-10-17-megatron-15/zstd-19-software-installed.json`
221. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-software-installed.json`
222. `bench/results/2026-10-17-megatron-15/xz-6-software-installed.json`
223. `bench/results/2026-10-17-megatron-15/xz-9-software-installed.json`
224. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-software-installed.json`
225. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-software-installed.json`
226. `bench/results/2026-10-17-megatron-15/tsaur-default-software-installed.json`
227. `bench/results/2026-10-17-megatron-15/tsaur-max-software-installed.json`
228. `bench/results/2026-10-17-megatron-15/store-store-source-git.json`
229. `bench/results/2026-10-17-megatron-15/7z-mx5-source-git.json`
230. `bench/results/2026-10-17-megatron-15/7z-ultra-source-git.json`
231. `bench/results/2026-10-17-megatron-15/rar-m3-source-git.json`
232. `bench/results/2026-10-17-megatron-15/rar-best-source-git.json`
233. `bench/results/2026-10-17-megatron-15/rar-best-solid-source-git.json`
234. `bench/results/2026-10-17-megatron-15/rar-best-rr3-source-git.json`
235. `bench/results/2026-10-17-megatron-15/zstd-3-source-git.json`
236. `bench/results/2026-10-17-megatron-15/zstd-19-source-git.json`
237. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-source-git.json`
238. `bench/results/2026-10-17-megatron-15/xz-6-source-git.json`
239. `bench/results/2026-10-17-megatron-15/xz-9-source-git.json`
240. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-source-git.json`
241. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-source-git.json`
242. `bench/results/2026-10-17-megatron-15/tsaur-default-source-git.json`
243. `bench/results/2026-10-17-megatron-15/tsaur-max-source-git.json`
244. `bench/results/2026-10-17-megatron-15/store-store-text-prose.json`
245. `bench/results/2026-10-17-megatron-15/7z-mx5-text-prose.json`
246. `bench/results/2026-10-17-megatron-15/7z-ultra-text-prose.json`
247. `bench/results/2026-10-17-megatron-15/rar-m3-text-prose.json`
248. `bench/results/2026-10-17-megatron-15/rar-best-text-prose.json`
249. `bench/results/2026-10-17-megatron-15/rar-best-solid-text-prose.json`
250. `bench/results/2026-10-17-megatron-15/rar-best-rr3-text-prose.json`
251. `bench/results/2026-10-17-megatron-15/zstd-3-text-prose.json`
252. `bench/results/2026-10-17-megatron-15/zstd-19-text-prose.json`
253. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-text-prose.json`
254. `bench/results/2026-10-17-megatron-15/xz-6-text-prose.json`
255. `bench/results/2026-10-17-megatron-15/xz-9-text-prose.json`
256. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-text-prose.json`
257. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-text-prose.json`
258. `bench/results/2026-10-17-megatron-15/tsaur-default-text-prose.json`
259. `bench/results/2026-10-17-megatron-15/tsaur-max-text-prose.json`
260. `bench/results/2026-10-17-megatron-15/store-store-video.json`
261. `bench/results/2026-10-17-megatron-15/7z-mx5-video.json`
262. `bench/results/2026-10-17-megatron-15/7z-ultra-video.json`
263. `bench/results/2026-10-17-megatron-15/rar-m3-video.json`
264. `bench/results/2026-10-17-megatron-15/rar-best-video.json`
265. `bench/results/2026-10-17-megatron-15/rar-best-solid-video.json`
266. `bench/results/2026-10-17-megatron-15/rar-best-rr3-video.json`
267. `bench/results/2026-10-17-megatron-15/zstd-3-video.json`
268. `bench/results/2026-10-17-megatron-15/zstd-19-video.json`
269. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-video.json`
270. `bench/results/2026-10-17-megatron-15/xz-6-video.json`
271. `bench/results/2026-10-17-megatron-15/xz-9-video.json`
272. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-video.json`
273. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-video.json`
274. `bench/results/2026-10-17-megatron-15/tsaur-default-video.json`
275. `bench/results/2026-10-17-megatron-15/tsaur-max-video.json`
276. `bench/results/2026-10-17-megatron-15/store-store-vm-image.json`
277. `bench/results/2026-10-17-megatron-15/7z-mx5-vm-image.json`
278. `bench/results/2026-10-17-megatron-15/7z-ultra-vm-image.json`
279. `bench/results/2026-10-17-megatron-15/rar-m3-vm-image.json`
280. `bench/results/2026-10-17-megatron-15/rar-best-vm-image.json`
281. `bench/results/2026-10-17-megatron-15/rar-best-solid-vm-image.json`
282. `bench/results/2026-10-17-megatron-15/rar-best-rr3-vm-image.json`
283. `bench/results/2026-10-17-megatron-15/zstd-3-vm-image.json`
284. `bench/results/2026-10-17-megatron-15/zstd-19-vm-image.json`
285. `bench/results/2026-10-17-megatron-15/zstd-ultra22-long27-vm-image.json`
286. `bench/results/2026-10-17-megatron-15/xz-6-vm-image.json`
287. `bench/results/2026-10-17-megatron-15/xz-9-vm-image.json`
288. `bench/results/2026-10-17-megatron-15/zpaqfranz-m1-vm-image.json`
289. `bench/results/2026-10-17-megatron-15/zpaqfranz-m5-vm-image.json`
290. `bench/results/2026-10-17-megatron-15/tsaur-default-vm-image.json`
291. `bench/results/2026-10-17-megatron-15/tsaur-max-vm-image.json`
292. `bench/results/2026-10-18-megatron/host.json`
293. `bench/results/2026-10-18-megatron/tools.json`
294. `bench/results/2026-10-18-megatron/run.json`
295. `bench/results/2026-10-18-megatron/store-store-encrypted-random.json`
296. `bench/results/2026-10-18-megatron/7z-mx5-encrypted-random.json`
297. `bench/results/2026-10-18-megatron/7z-ultra-encrypted-random.json`
298. `bench/results/2026-10-18-megatron/rar-m3-encrypted-random.json`
299. `bench/results/2026-10-18-megatron/rar-best-encrypted-random.json`
300. `bench/results/2026-10-18-megatron/rar-best-solid-encrypted-random.json`
301. `bench/results/2026-10-18-megatron/rar-best-rr3-encrypted-random.json`
302. `bench/results/2026-10-18-megatron/zstd-3-encrypted-random.json`
303. `bench/results/2026-10-18-megatron/zstd-19-encrypted-random.json`
304. `bench/results/2026-10-18-megatron/zstd-ultra22-long27-encrypted-random.json`
305. `bench/results/2026-10-18-megatron/xz-6-encrypted-random.json`
306. `bench/results/2026-10-18-megatron/xz-9-encrypted-random.json`
307. `bench/results/2026-10-18-megatron/zpaqfranz-m1-encrypted-random.json`
308. `bench/results/2026-10-18-megatron/zpaqfranz-m5-encrypted-random.json`
309. `bench/results/2026-10-18-megatron/tsaur-default-encrypted-random.json`
310. `bench/results/2026-10-18-megatron/tsaur-max-encrypted-random.json`
311. `bench/results/2026-10-18-megatron/store-store-game-assets.json`
312. `bench/results/2026-10-18-megatron/7z-mx5-game-assets.json`
313. `bench/results/2026-10-18-megatron/7z-ultra-game-assets.json`
314. `bench/results/2026-10-18-megatron/rar-m3-game-assets.json`
315. `bench/results/2026-10-18-megatron/rar-best-game-assets.json`
316. `bench/results/2026-10-18-megatron/rar-best-solid-game-assets.json`
317. `bench/results/2026-10-18-megatron/rar-best-rr3-game-assets.json`
318. `bench/results/2026-10-18-megatron/zstd-3-game-assets.json`
319. `bench/results/2026-10-18-megatron/zstd-19-game-assets.json`
320. `bench/results/2026-10-18-megatron/zstd-ultra22-long27-game-assets.json`
321. `bench/results/2026-10-18-megatron/xz-6-game-assets.json`
322. `bench/results/2026-10-18-megatron/xz-9-game-assets.json`
323. `bench/results/2026-10-18-megatron/zpaqfranz-m1-game-assets.json`
324. `bench/results/2026-10-18-megatron/zpaqfranz-m5-game-assets.json`
325. `bench/results/2026-10-18-megatron/tsaur-default-game-assets.json`
326. `bench/results/2026-10-18-megatron/tsaur-max-game-assets.json`
327. `bench/results/2026-10-18-megatron/store-store-logs-text.json`
328. `bench/results/2026-10-18-megatron/7z-mx5-logs-text.json`
329. `bench/results/2026-10-18-megatron/7z-ultra-logs-text.json`
330. `bench/results/2026-10-18-megatron/rar-m3-logs-text.json`
331. `bench/results/2026-10-18-megatron/rar-best-logs-text.json`
332. `bench/results/2026-10-18-megatron/rar-best-solid-logs-text.json`
333. `bench/results/2026-10-18-megatron/rar-best-rr3-logs-text.json`
334. `bench/results/2026-10-18-megatron/zstd-3-logs-text.json`
335. `bench/results/2026-10-18-megatron/zstd-19-logs-text.json`
336. `bench/results/2026-10-18-megatron/zstd-ultra22-long27-logs-text.json`
337. `bench/results/2026-10-18-megatron/xz-6-logs-text.json`
338. `bench/results/2026-10-18-megatron/xz-9-logs-text.json`
339. `bench/results/2026-10-18-megatron/zpaqfranz-m1-logs-text.json`
340. `bench/results/2026-10-18-megatron/zpaqfranz-m5-logs-text.json`
341. `bench/results/2026-10-18-megatron/tsaur-default-logs-text.json`
342. `bench/results/2026-10-18-megatron/tsaur-max-logs-text.json`
343. `bench/results/2026-10-20-megatron-2/host.json`
344. `bench/results/2026-10-20-megatron-2/tools.json`
345. `bench/results/2026-10-20-megatron-2/run.json`
346. `bench/results/2026-10-20-megatron-2/lpk-fast-archives-nested.json`
347. `bench/results/2026-10-20-megatron-2/lpk-balanced-archives-nested.json`
348. `bench/results/2026-10-20-megatron-2/lpk-fast-audio.json`
349. `bench/results/2026-10-20-megatron-2/lpk-balanced-audio.json`
350. `bench/results/2026-10-20-megatron-2/lpk-fast-backup-versions.json`
351. `bench/results/2026-10-20-megatron-2/lpk-balanced-backup-versions.json`
352. `bench/results/2026-10-20-megatron-2/lpk-fast-backup-versions-large.json`
353. `bench/results/2026-10-20-megatron-2/lpk-balanced-backup-versions-large.json`
354. `bench/results/2026-10-20-megatron-2/lpk-fast-encrypted-random.json`
355. `bench/results/2026-10-20-megatron-2/lpk-balanced-encrypted-random.json`
356. `bench/results/2026-10-20-megatron-2/lpk-fast-game-assets.json`
357. `bench/results/2026-10-20-megatron-2/lpk-balanced-game-assets.json`
358. `bench/results/2026-10-20-megatron-2/lpk-fast-logs-text.json`
359. `bench/results/2026-10-20-megatron-2/lpk-balanced-logs-text.json`
360. `bench/results/2026-10-20-megatron-2/lpk-fast-model-weights.json`
361. `bench/results/2026-10-20-megatron-2/lpk-balanced-model-weights.json`
362. `bench/results/2026-10-20-megatron-2/lpk-fast-office-pdf.json`
363. `bench/results/2026-10-20-megatron-2/lpk-balanced-office-pdf.json`
364. `bench/results/2026-10-20-megatron-2/lpk-fast-photo-jpeg.json`
365. `bench/results/2026-10-20-megatron-2/lpk-balanced-photo-jpeg.json`
366. `bench/results/2026-10-20-megatron-2/lpk-fast-photo-jpeg-edited.json`
367. `bench/results/2026-10-20-megatron-2/lpk-balanced-photo-jpeg-edited.json`
368. `bench/results/2026-10-20-megatron-2/lpk-fast-photo-raw-png.json`
369. `bench/results/2026-10-20-megatron-2/lpk-balanced-photo-raw-png.json`
370. `bench/results/2026-10-20-megatron-2/lpk-fast-small-files.json`
371. `bench/results/2026-10-20-megatron-2/lpk-balanced-small-files.json`
372. `bench/results/2026-10-20-megatron-2/lpk-fast-software-installed.json`
373. `bench/results/2026-10-20-megatron-2/lpk-balanced-software-installed.json`
374. `bench/results/2026-10-20-megatron-2/lpk-fast-source-git.json`
375. `bench/results/2026-10-20-megatron-2/lpk-balanced-source-git.json`
376. `bench/results/2026-10-20-megatron-2/lpk-fast-text-prose.json`
377. `bench/results/2026-10-20-megatron-2/lpk-balanced-text-prose.json`
378. `bench/results/2026-10-20-megatron-2/lpk-fast-video.json`
379. `bench/results/2026-10-20-megatron-2/lpk-balanced-video.json`
380. `bench/results/2026-10-20-megatron-2/lpk-fast-vm-image.json`
381. `bench/results/2026-10-20-megatron-2/lpk-balanced-vm-image.json`
382. `bench/report-mixes.toml`

## 8. Caveats

- tools skipped: t-saur (not installed), WinZip command line (manual (see docs/BASELINES.md)), PowerArchiver command line (manual (see docs/BASELINES.md))
- combinations that failed: store/store on backup-versions-large, zstd/3 on backup-versions-large, zstd/19 on backup-versions-large, zstd/ultra22-long27 on backup-versions-large, xz/6 on backup-versions-large, xz/9 on backup-versions-large
- rows resting on one measurement (D-20 item 3): zpaqfranz/m5 on archives-nested (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on audio (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on backup-versions-large (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); 7z/ultra on model-weights (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zstd/ultra22-long27 on model-weights (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); xz/9 on model-weights (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on model-weights (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zstd/ultra22-long27 on office-pdf (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on office-pdf (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); rar/best-solid on photo-jpeg (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zstd/19 on photo-jpeg (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zstd/ultra22-long27 on photo-jpeg (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); xz/6 on photo-jpeg (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); xz/9 on photo-jpeg (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on photo-jpeg (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zstd/ultra22-long27 on photo-jpeg-edited (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on photo-jpeg-edited (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zstd/ultra22-long27 on photo-raw-png (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on photo-raw-png (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zstd/ultra22-long27 on software-installed (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on software-installed (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on source-git (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zstd/ultra22-long27 on text-prose (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on text-prose (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zstd/ultra22-long27 on video (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on video (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zstd/ultra22-long27 on vm-image (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on vm-image (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on encrypted-random (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on game-assets (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zstd/ultra22-long27 on logs-text (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); zpaqfranz/m5 on logs-text (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); lpk/balanced on backup-versions-large (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); lpk/balanced on logs-text (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); lpk/balanced on model-weights (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); lpk/balanced on office-pdf (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); lpk/fast on photo-jpeg (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); lpk/balanced on photo-jpeg (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); lpk/balanced on photo-raw-png (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); lpk/balanced on software-installed (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); lpk/balanced on text-prose (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated); lpk/balanced on vm-image (the first repeat's compress plus extract wall time reached --long-run-s (120 s): not repeated)
- probes not available: jpeg, deflate, dedup, text, weights, entropy-gate
- classes without an estimate rule (the best measured incumbent is used, no gain claimed): audio, source-git, vm-image
- antivirus: the scanner state did not change during the run; scanner state at the start of the baseline run: Windows Defender (scanner off, state 0x60100); Avast Antivirus (scanner snoozed, state 0x42000); at the end: Windows Defender (scanner off, state 0x60100); Avast Antivirus (scanner snoozed, state 0x42000)
- unclean inputs (dirty, unknown or unoptimised builds, no settle pause, directories outside bench/results): none
- the baseline's build profile is not recorded in its host file, so whether it was a release build cannot be checked from the result files
- the `small-files` text estimate compresses a tar stream of the class, so tar header bytes are included in it; the baseline sizes are of each tool's own archive
- probe text timing: `in-process` rows time a library call on a stream in memory, `process-wall` rows time a whole external program including start-up and file I/O; the two bases are not comparable, and the estimates here use sizes only
- estimates are derived from component measurements on one corpus; they are not archives that were written and read back

