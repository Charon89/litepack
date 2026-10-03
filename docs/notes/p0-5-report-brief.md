# P0-5a — the report generator (brief)

Prepared 2026-10-02 as the requirements for the first sub-task of P0-5 in `docs/PLAN.md`: `lpk-bench report`.
The report is the document the Phase 0 GO/NO-GO verdict (D-08) rests on, so two rules dominate everything
below: **every number in the report comes from a committed JSON result file and says which**, and
**estimates are labelled as estimates** and never mixed with measurements in one column.

## Inputs
- `--results <DIR>`: a baseline results directory written by `lpk-bench run` (P0-3 format: one JSON per
  tool × setting × class, `host.json`, `tools.json`, `run.json`). Required.
- `--probes <DIR>` (repeatable): directories holding probe files written by `lpk-bench probe` (P0-4 format:
  `probe-<name>.json` plus `host.json`). Optional; a missing probe leaves its sections and the estimates
  that need it marked "not available", never guessed.
- `--mixes <FILE>`: the disk-mix definitions (default `bench/report-mixes.toml`, committed; see below).
- `--out <FILE>`: default `bench/reports/phase0-<date>.md` (date of the baseline run, not of today).
- The generator validates every input directory with the existing validator first and refuses on any
  problem. Baseline and probe directories must come from the same host and the same corpus manifest; they
  may come from different builds (one build per directory is already enforced), and the report states each
  build. Results from a dirty or debug build are refused unless `--allow-unclean` is given and then marked.

## Output: one Markdown file, these sections in this order
1. **Header.** Corpus (profile, manifest hash, files, bytes, classes), host (name, CPU, cores, RAM, OS),
   builds and dates of every input directory, tool versions (`tools.json`), library versions from the probe
   files, antivirus state at start and end of the baseline run, the catalogue hash, the run settings
   (repeats, long-run and settle values).
2. **Baseline, per class.** One table per class: rows = tool × setting; columns = archive size as % of the
   class bytes, compress MB/s, extract MB/s, peak RSS (MiB), repeats used. Failed combinations show the
   failure reason; skipped ones the skip reason; a row that rests on one measurement (D-20 item 3) is
   marked. MB/s = class bytes / median wall seconds (MB = 10^6 bytes, as in `docs/BASELINES.md`).
3. **Baseline, blended.** For each disk mix: the blended size % and blended MB/s of every tool × setting
   (weights below). A tool that lacks a measured result for a class in the mix gets "n/a" for that mix.
4. **Probes.** Each probe's own table, rendered by the probe framework's renderer from the JSON (the same
   function `probe` uses), introduced by one line saying what the probe measured and on which build.
5. **Estimates.** Per class, two columns of measured incumbents and one column of estimate:
   - "best measured incumbent" = the smallest archive among all measured tools and settings for that class
     (name it);
   - "7-Zip Ultra" = the `7z/ultra` row;
   - "LitePack Balanced, estimate" = the figure derived from the probes by the rule for that class below.
   Then the same three for each disk mix (weighted sums of bytes).
   Rules per class (state each rule in the report, next to its table):
   - `photo-jpeg`, `photo-jpeg-edited`: bytes after Lepton with failed files stored as-is (`probe-jpeg`,
     "after fallback" per class).
   - `office-pdf`, `archives-nested`, `software-installed`, `game-assets`, `photo-raw-png`: the Deflate
     probe's B figure with xz preset 9 (reconstructed streams replaced by plain data, corrections added,
     whole file compressed), per class.
   - `backup-versions`: the smaller of (a) the sum over versions of "new unique chunks, zstd level 19"
     bytes (version 1 counts whole) and (b) version 1 compressed at level 19 plus the zstd `--patch-from`
     patches of the later versions, from `probe-dedup`; name which one was used.
   - `text-prose`, `logs-text`, `small-files`: the smallest measured in-process or process row of
     `probe-text` for that class (xz, zstd, bsc or kanzi, whichever ran and verified); name the row.
   - `model-weights`: the best plane variant of `probe-weights` (byte planes or rotated planes) when the
     probe parsed at least one file; otherwise "not available" (the `small` profile has no safetensors).
   - `video`, `encrypted-random`: stored as-is (the class bytes).
   - every other class (`audio`, `vm-image`, `source-git` and any class without a rule): the best measured
     incumbent — no gain is claimed.
   The estimate column is headed "estimate" and footnoted: it combines component measurements and assumes
   nothing about the container, dedup across classes or the Fold stage.
6. **Gates (D-07).** One row per gate with the numbers, their sources and PASS / FAIL:
   - **G1 photo/document size.** On `photo-jpeg`, `photo-jpeg-edited` and `office-pdf`, together by bytes
     and each alone: estimate ≤ 90% of the best measured incumbent (the fallback wording of D-07, because
     WinZip and PowerArchiver were not measured — say so) and ≤ 85% of 7-Zip Ultra. PASS only if both hold
     on the combined bytes.
   - **G2 versioned backup.** On `backup-versions`: estimate ≤ 50% of the best measured incumbent.
   - **G3 video store speed.** The measured proxy for "stored": the `store` tool's compress MB/s on the
     `video` class (a real program reading and writing the files, from the baseline), divided by the video
     class's own raw read rate from `probe-entropy-gate` (median of the three passes, the "including opens"
     figure). PASS if ≥ 80%. Also print the gate costs from the probe (full entropy, sampled entropy,
     zstd level 1, MB/s single-threaded) so a reader can see what a gate would add; they do not enter the
     PASS/FAIL.
   - **G4 fast-tier extraction.** The Fast tier is zstd-based, so the proxy is the baseline `zstd/3`
     extraction MB/s against `7z/mx5` extraction MB/s, blended over each disk mix. PASS if zstd is at least
     as fast on every mix.
   Below the table: "Verdict proposal: GO" only if every gate passes; otherwise "NO-GO proposal" naming the
   failing gates. The verdict itself is recorded by a person in D-08, not by this command.
7. **Sources.** A numbered list of every result file used (relative path inside `bench/results`) and the
   mixes file. Every number in sections 2–6 carries the index of its source in brackets after the value,
   e.g. `12.3% [7]`; derived numbers carry every source they used. A number without a source is a bug.
8. **Caveats.** Generated from the data, not typed: tools skipped (not installed), probes not available,
   classes without a rule, rows on one measurement, antivirus state, debug or dirty builds, the small-files
   tar overhead, and the in-process versus process timing bases of `probe-text`.

## Disk mixes (`bench/report-mixes.toml`, committed; the report prints it)
Three named mixes, each a list of `class = weight` (weights are shares of bytes, summing to 100). First
version:
- `photo-doc`: photo-jpeg 45, photo-jpeg-edited 5, photo-raw-png 5, office-pdf 25, text-prose 5,
  archives-nested 5, audio 5, small-files 5.
- `developer`: source-git 15, backup-versions 15, software-installed 20, small-files 10, logs-text 15,
  text-prose 5, archives-nested 10, model-weights 5, vm-image 5.
- `video-heavy`: video 60, photo-jpeg 15, audio 10, encrypted-random 5, office-pdf 5, vm-image 5.
Blending: for a size ratio, Σ weight_c × ratio_c; for a speed, bytes-weighted harmonic mean (Σ weight_c /
Σ (weight_c / speed_c)). A class absent from the corpus makes the mix "n/a" and the report says why.

## Traceability in code
Represent every reported quantity as a value with its sources (`struct Traced { value, sources: Vec<SourceId> }`);
arithmetic on traced values unions the sources; the renderer prints the brackets. A test renders a tiny
synthetic report and asserts, with a regular expression, that every number inside a table cell is followed
by a source bracket (the mixes' weights and the header's metadata are the only exceptions, and they are
outside the tables or carry the mixes file as source).

## Tests (no corpus needed)
Build a tiny baseline directory and probe files in a temp directory (the existing test helpers of
`run` and `probe` can write them), then: the report renders; the per-class and blended figures equal
hand-computed values; a failed row shows its reason; a single-measurement row is marked; a missing probe
makes its estimate "not available" and G-rows that need it "not evaluable" (never PASS); each gate's
PASS/FAIL logic is tested at both sides of its threshold; the traceability regex; validation refusal on a
bad input directory; the mixes file parsed and checked (weights sum to 100, known class names).

## Constraints
- `#![forbid(unsafe_code)]`; no `unwrap` outside tests; no figures in docs, comments, test names or commit
  messages (test fixtures carry their own synthetic numbers); the generator never prints a number that is
  not in a committed JSON file or in the mixes file.
- Do not touch the run loop, the probe measurements or their formats. Reading them through their existing
  types is the point.
- Do not tick PLAN boxes or edit `docs/DECISIONS.md`.
