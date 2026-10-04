# E0-1 — proposal for the larger versioned set (needs the owner's approval before it is built)

D-27 restated gate G2 "on a versioned set whose versions together exceed the largest dictionary window of the
measured incumbents, so that solid compression alone cannot remove the cross-version redundancy", and said the
set is added to the corpus by its own task with the owner's approval of the spec. This is that spec, proposed.

## The windows to exceed
The incumbents' largest windows in `bench/tools.toml`: WinRAR `-m5 -md256m` (256 MiB), zstd `--long=27`
(128 MiB), 7-Zip `-mx9` (64 MiB), xz `-9` (64 MiB). zpaqfranz deduplicates globally, whatever the size — see
"What the gate can then mean" below.

## The class: `backup-versions-large` (profile `full` only)
- **Content:** three working-tree snapshots of one large, permissively licensed source repository at three
  release tags several months apart — the same kind of data as the existing `backup-versions` (three snapshots
  of facebook/zstd, about 9 MB each), scaled so that **each version alone exceeds 256 MiB** and the three
  together exceed 1 GiB. Proposed repository: `godotengine/godot` (MIT) at tags `4.2-stable`, `4.3-stable`,
  `4.4-stable` — a working tree of roughly a gigabyte each (source, third-party libraries, editor assets:
  text, binaries and images mixed, which is what a real backup looks like). The builder's existing git-export
  source kind handles it (shallow clone at a pinned commit, working tree exported without `.git`), so no new
  builder code is needed beyond the lock entries.
- **Determinism:** pinned commit hashes in `bench/corpus.lock`, exported the same way as `backup-versions`
  (`v1`, `v2`, `v3` subfolders), built twice with identical manifests (the P0-2 rule).
- **Cost:** about 3 GB on disk, three shallow clones to download (a few hundred megabytes each); the `full`
  profile grows from 19 GB to about 22 GB. The baseline runners spend most of their added time in zpaqfranz
  `-m5` (its throughput on `small` suggests roughly an hour more per run).
- **Report:** the class joins the `developer` mix of `bench/report-mixes.toml` (its weight to be set when the
  class exists; the existing `backup-versions` keeps its row), and G2's row uses it.

## What the gate can then mean (the owner's call, to be recorded with the approval)
With the versions larger than every window, 7-Zip, WinRAR, xz and zstd can no longer remove the cross-version
redundancy, so LitePack's Fold (chunk dedup and deltas) has something to win against them. zpaqfranz, however,
deduplicates chunks over the whole archive regardless of size: against it, "2× smaller" would require compressing
the *unique* content twice as well as zpaq's context mixing, which no LZ- or LZMA-class method does. Two readings:
- **(a) D-27 as written:** G2 = 50% of the best measured incumbent on the set — almost certainly zpaqfranz — and
  the gate will likely fail whatever Fold achieves.
- **(b) The non-dedup incumbents:** G2 = 50% of the best measured incumbent *that does not deduplicate* (7-Zip,
  WinRAR, xz, zstd on this set), with zpaqfranz's figure printed beside it as the reference point. This is the
  comparison the gate was written for ("solid compression alone cannot remove the redundancy").
Recommendation: (b), recorded as an amendment to D-27 when the class is approved.

## What approval means
"Approve" starts task E0-1: the lock entries and `docs/CORPUS.md` row, a build of the class, the baseline run of
every incumbent on it (committed), and the report's G2 row switched to it. Nothing is downloaded or built before.
