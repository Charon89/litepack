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

None open. The `jpeg_reconstruct` findings (out-of-memory in `read_lepton_compressed_header`, and the
counting allocator's peak over the decode memory) are fixed by PR #121 (`fix/jpeg-decode-memory`): the
Lepton header is checked before the library decodes it; the crashing input joined
`seeds/jpeg_reconstruct/` with the fix.
