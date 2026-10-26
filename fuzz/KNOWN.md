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
| `jpeg_reconstruct` | `out-of-memory` | `read_lepton_compressed_header` | A crafted Lepton stream makes the `jpeg-reconstruct` decode attempt a single allocation of about 4 GB (`malloc(3840140001)`, in Lepton's compressed-header read): the record's decode memory does not bound the Lepton decode | fuzz run 40272877553 (E4-6, symbolised stack), artefact `fuzz-artifacts-jpeg_reconstruct` | PR #121 (`fix/jpeg-decode-memory`) |
| `jpeg_reconstruct` | `deadly signal` | `over the decode memory` | The same finding seen by the target's counting allocator: a peak of 1.2 to 2.0 GB against a decode memory of 112 MiB | fuzz runs 40267296061 and 40274460627 (E4-6) | PR #121 (`fix/jpeg-decode-memory`) |
| `deflate_recreate` | `out-of-memory` | `recreate_tree_for_block` | Crafted corrections make the limited recreation (D-66) attempt an allocation of about 4 GB (`malloc(4294967290)`), past the writer's `limit + 8` cap: `recreate_tree_for_block` resizes the literal and distance length tables to decoded counts | fuzz run 40274460627 (E4-6, `--no-cfg-fuzzing`, symbolised stack), artefact `fuzz-artifacts-deflate_recreate` | PR #123 (`fix/deflate-decode-memory`, vendor change 12) |
