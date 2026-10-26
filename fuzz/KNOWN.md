# Known open fuzz findings

A target listed here fails the nightly run until its fix lands; it is not skipped. Its crashing
input stays out of `seeds/` until the fix, and joins them with it.

| Target | Finding | Found | Follow-up |
|---|---|---|---|
| `deflate_recreate` | A 63-byte input (output limit 0x7900, empty plain text, crafted corrections) makes the limited recreation (D-66) attempt a single `malloc(4294967295)` (libFuzzer out-of-memory), past the writer's `limit + 8` cap: some allocation sized from a decoded field is not bounded by the limit. Not yet triaged (the site is unknown: the CI binaries are unsymbolised, and the record does not decode the same way in a Windows debug test) | fuzz run 40270390275 (E4-6), artefact `fuzz-artifacts-deflate_recreate` | to be filed by the controller |
| `jpeg_reconstruct` | A crafted Lepton stream makes the `jpeg-reconstruct` decode attempt a single 4.28 GB allocation (`malloc(4282667855)`, libFuzzer out-of-memory; an earlier input peaked at 2.03 GB against a decode memory of 112 MiB): the record's decode memory does not bound the Lepton decode | fuzz runs 40267296061 and 40269058613 (E4-6), artefact `fuzz-artifacts-jpeg_reconstruct` | a separate engine fix task: the decode memory must bound the Lepton decode before it starts |
