# Phase 0 verdict review: bench/reports/phase0-2026-10-03.md (commit d83ca39)

Read-only review. How it was checked:
- run --validate passes on both directories.
- The regenerated report is byte-identical to the committed one.
- All figures were recomputed in Python from repeats[] in the JSON.

## 1. Arithmetic check: no mismatches

Coverage:

| Section | What was checked | Numbers |
|---|---|---|
| 2 | all 238 rows × 5 cells | 1,190 |
| 3 | all blended figures | 126 |
| 5 | per-class table and mix rows | 126 |
| 6 | every gate number | 36 |

Mismatches: 0. One tie: encrypted-random rar/m3 = rar/best = 419,430,773 B.

Selected figures:

| Figure | Report | Recomputed |
|---|---|---|
| video store compress / extract | 1498.6 / 2092.2 MB/s [244] | same (median 0.640 s) |
| backup-versions zpaqfranz m5 | 6.7% | 1,701,919 B |
| backup-versions 7z/ultra | 7.4% | 1,885,954 B |
| photo-jpeg estimate | 6,442,281,810 | Lepton bytes + 15 failed files stored (38,406,035 B) |
| photo-jpeg-edited estimate | 413,388,307 | 600 of 600 OK |
| office-pdf estimate | 1,252,603,928 | sum of (B xz + corrections) |
| archives-nested estimate | 23,243,034 | same |
| software-installed estimate | 143,018,149 | same |
| backup-versions estimate | 1,874,765 | (b) 1,868,839 + 1,397 + 4,529; (a) 2,000,890 |
| model-weights estimate | 197,269,279 | rotated 179,482,350 + header 30,536 + stored 17,756,393 |
| mix estimates | 64.3 / 32.9 / 87.7% | 64.31 / 32.87 / 87.68 |
| G1 | 83.6% / 81.0% | 8,108,274,045 / 9,696,257,622 and / 10,006,337,515 |
| G1 office-pdf alone | 91.0 / 89.1% | 91.03 / 89.13 |
| G2 | 110.2% | 1,874,765 / 1,701,919 |
| G3 raw read | 4663.4 | median of 3559.2, 4663.4, 4743.9 |
| G3 ratio | 32.1% | 1498.6 / 4663.4 |
| G4 extract | 333.7 vs 218.6; 130.6 vs 82.1; 1152.8 vs 502.3 | same |

**D-23 rules: all applied as stated.**
- Per-file min(A, B) would lower office-pdf by only 501,452 B.
- "Version 1 at zstd 19" in the report is in fact v1's unique chunks at zstd 19, with no tar headers.

## 2. Completeness findings

1. **Background programs are not recorded.** host.json and run.json hold no process list and no clock policy. STATUS.md lists cam_helper, 2x mpv and another Claude session at start. D-24 wants the report to say what ran.
   - Effect on the gates: none.
   - G1 and G2 are byte counts.
   - G4 margins are 1.53x / 1.59x / 2.29x.
   - G3 misses by 2.5x: the store run is 98% kernel CPU, and its fastest repeat (1548.8 MB/s) reaches 33.2%.
2. **Two preflate-rs panics are missing from section 8.** They hit office-pdf files 863 (3,998,625 B) and 1940 (234,317 B). The panics were caught inside preflate-rs 0.7.6 (walker panics 0) and those streams were kept as Deflate.
   - Effect on the gates: none, and conservative.
   - Phase 1: the engine must contain library panics (catch_unwind with panic=unwind, or a separate process) and fuzz the library.
3. **Skips inside probes are not listed.** bsc, kanzi and hdiffz were not installed, and XWRT was excluded as GPL.
   - Text estimates are therefore plain xz -9.
   - D-05's BWT claim remains untested.
4. **The G3 gate-cost lines use whole-corpus gate rates.** Video's own rates and their share of its raw read:
   - entropy: 4668.1 MB/s, 100.1% (report: 96.6%)
   - sampled entropy: 70957.7 MB/s, 1521.6%
   - zstd1: 5714.6 MB/s, 122.5% (report: 30.9%)
5. **The G1 fallback bar is weaker than D-07's primary bar.** WinZip and PowerArchiver both recompress JPEG (Method doc lines 32-33). The measured incumbents do not.
6. **G1 passes only on combined bytes.** office-pdf alone is at 91.0% / 89.1% and fails both thresholds. photo-jpeg is 76% of the combined bytes.
7. **Deflate estimates are per-file xz with no solid or dedup context.** For example, software-installed is estimated at 32.7% against 15.1% for the best incumbent. The footnote does not say this.
8. **photo-jpeg-edited was made by the in-tree encoder (D-18).** Without it, G1 is 83.7% / 81.2%, so the verdict is unchanged.
9. **Minor:** the release-build profile of the baseline rests on STATUS.md, not on the result files.

## 3. D-08 proposal

- D-08 — 2026-10-04 — **Phase 0 verdict: NO-GO under D-07 as written —
  G1 PASS, G2 FAIL, G3 FAIL, G4 PASS (bench/reports/phase0-2026-10-03.md, full profile,
  baseline 2026-10-03-megatron-4, probes -megatron-5, build 78bc960);
  [owner: confirm NO-GO, or amend D-07 for G2/G3 and record GO].**
  G1 (photo-jpeg + photo-jpeg-edited + office-pdf combined; fallback wording, WinZip and PowerArchiver not measured):
  83.6% of zpaqfranz m5 and 81.0% of 7-Zip Ultra, PASS (§6);
  per class photo-jpeg 82.4/79.8%, edited 82.4/78.5%, office-pdf 91.0/89.1% (§6).
  G2: 1,874,765 B vs zpaqfranz m5 1,701,919 B = 110.2%, FAIL (§6).
  G3: store 1498.6 MB/s vs uncached raw read 4663.4 MB/s = 32.1%, FAIL (§6, §4).
  G4: zstd/3 vs 7z/mx5 extraction 333.7/218.6, 130.6/82.1, 1152.8/502.3 MB/s, PASS (§6, §3).
  — GO requires every gate to pass. Both failures are questions of what the gate means, not measurement errors.
  G2: solid incumbents whose window covers the class already remove the redundancy between versions.
  50% of the best would need v1 compressed to 10.0% of its bytes; zpaqfranz reaches 20.0% of v1 for all three versions.
  G3: the proxy compares a cache-warm read+write process with an uncached read-only pass (D-23).
  Run conditions: Defender off, Avast snoozed, owner's background programs running (cam_helper, 2x mpv, another Claude session at start).
  None of this moves a gate.
  Carried forward: WinZip/PowerArchiver unmeasured; 2 preflate-rs panics (caught); text estimates xz-only.

## 4. Alternative readings

**G2 (estimate 1,874,765 B; PASS at ≤ 50% of the incumbent):**

| Incumbent | Estimate / incumbent | Result |
|---|---|---|
| zpaqfranz m5 | 110.2% | FAIL |
| 7z/ultra | 99.4% | FAIL |
| 7z/mx5 | 99.4% | FAIL |
| xz/9 | 99.6% | FAIL |
| zstd ultra-long | 98.6% | FAIL |
| rar best-solid | 87.3% | FAIL |
| zpaqfranz m1 | 77.8% | FAIL |
| xz/6 | 34.2% | PASS |
| zstd/19 | 33.9% | PASS |
| zstd/3 | 27.2% | PASS |
| rar/best (non-solid) | 24.3% | PASS |
| rar/m3 | 24.2% | PASS |
| raw class | 7.3% (13.6x smaller) | PASS |

7-Zip Ultra does not avoid dedup here: its dictionary covers the class.

**G3 (raw read 4663.4 MB/s):**

| Definition | MB/s | Share | Result | Basis |
|---|---|---|---|---|
| store, measured | 1498.6 | 32.1% | FAIL | measured |
| store, fastest repeat | — | 33.2% | FAIL | measured |
| store extract | 2092.2 | 44.9% | FAIL | measured |
| read only, no write | — | 100% | PASS | by definition |
| read + entropy, serial | — | 50.0% | FAIL | arithmetic, not measured |
| read + zstd1, serial | — | 55.1% | FAIL | arithmetic, not measured |
| read + sampled, serial | — | 93.8% | PASS | arithmetic, not measured |
| read overlapped with any gate | — | ~100% | PASS | arithmetic, not measured |

**G1, per-class reading:** office-pdf fails, at 91.0% of the best incumbent and 89.1% of 7-Zip Ultra.

Not checked: the probe tables in section 4 that no gate or estimate uses.
