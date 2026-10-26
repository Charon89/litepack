# Known open fuzz findings

A target listed here fails the nightly run until its fix lands; it is not skipped. Its crashing
input stays out of `seeds/` until the fix, and joins them with it.

| Target | Finding | Found | Follow-up |
|---|---|---|---|
| `jpeg_reconstruct` | A crafted Lepton stream makes the `jpeg-reconstruct` decode attempt a single 4.28 GB allocation (`malloc(4282667855)`, libFuzzer out-of-memory; an earlier input peaked at 2.03 GB against a decode memory of 112 MiB): the record's decode memory does not bound the Lepton decode | fuzz runs 40267296061 and 40269058613 (E4-6), artefact `fuzz-artifacts-jpeg_reconstruct` | a separate engine fix task: the decode memory must bound the Lepton decode before it starts |
