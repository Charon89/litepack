# Known open fuzz findings

A target listed here fails the nightly run until its fix lands; it is not skipped. Its crashing
input stays out of `seeds/` until the fix, and joins them with it.

`.github/workflows/fuzz.yml` labels a failing target's job as this known finding only when the
log's libFuzzer `SUMMARY` line names the row's kind and the log contains the row's signature (a
panic message or an allocation, as text); any other failure, also in a listed target, is labelled
"new, or a known target with a different signature". One row per signature; keep the three
first cells in backticks (the workflow reads them).

| Target | SUMMARY kind | Signature | Finding | Found | Follow-up |
|---|---|---|---|---|---|
| `jpeg_reconstruct` | `out-of-memory` | `out-of-memory (malloc(` | A crafted Lepton stream makes the `jpeg-reconstruct` decode attempt a single allocation of about 4 GB (`malloc(4282667855)`): the record's decode memory does not bound the Lepton decode | fuzz run 40269058613 (E4-6), artefact `fuzz-artifacts-jpeg_reconstruct` | branch `fix/jpeg-decode-memory` |
| `jpeg_reconstruct` | `deadly signal` | `over the decode memory` | The same finding seen by the target's counting allocator: a 2.03 GB peak against a decode memory of 112 MiB | fuzz run 40267296061 (E4-6) | branch `fix/jpeg-decode-memory` |
| `deflate_recreate` | `out-of-memory` | `malloc(4294967295)` | A 63-byte input (output limit 0x7900, empty plain text, crafted corrections) makes the limited recreation (D-66) attempt `malloc(4294967295)`, past the writer's `limit + 8` cap: `recreate_tree_for_block` resizes the literal and distance length tables to decoded counts | fuzz run 40270390275 (E4-6), artefact `fuzz-artifacts-deflate_recreate` | branch `fix/deflate-decode-memory` |
