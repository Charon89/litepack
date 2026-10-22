# LIC-2 design: closed-source readiness (D-89 (6))

2026-10-22. For the implementer of LIC-2; docs only. Every claim about the tree was checked on `origin/main`
f243e06 with `cargo tree`, `cargo metadata` and `grep`.

## 1. Inventory

`cargo tree -i cabac -e normal` prints one path: `cabac 0.15.0 <- preflate-rs 0.7.6 (vendor/preflate-rs-0.7.6)
<- {lpk-check, lpk-core <- {lpk-bench, lpk-cli}}`. A census over `cargo metadata` (normal dependencies, every
target, no build or dev dependencies) finds 190 crates behind `lpk` and 114 behind `lpk-check`; `cabac` is
the only copyleft crate, no MPL-2.0 crate is in the tree, and `r-efi` (`MIT OR Apache-2.0 OR LGPL-2.1-or-later`) is
taken under MIT.

| Family | Crates and path | A closed build must |
|---|---|---|
| LGPL-3.0-or-later | `cabac` 0.15.0, through `preflate-rs` only: `cabac::vp8` and its two traits (`vendor/preflate-rs-0.7.6/src/cabac_codec.rs:7`, `stream_processor.rs:12`); `cabac::debug` in test code (`cabac_codec.rs:313-377`, `stream_processor.rs:521-714`) | keep it out of every closed binary (section 2); any shipped binary containing it: a prominent notice, the GPL-3.0 and LGPL-3.0 texts, its complete source (D-26) |
| Apache-2.0 only | `lepton_jpeg` 0.5.8 (no licence file in its package; copyright line `src/lib.rs:2`), `preflate-rs` 0.7.6 (vendored and modified; the 4(b) change statement is `LITEPACK-CHANGES.md`), `lzma-rust2`, `rpassword`, `rtoolbox`, `sevenz-rust2`, `zopfli`; none ships a NOTICE file | the licence text and each copyright line; the change statement with any redistributed source |
| BSD-3-Clause | `zstd-sys` (libzstd inside it, `LICENSE.BSD-3-Clause`), `zstd`, `zstd-safe`; `reed-solomon-simd` (`MIT AND BSD-3-Clause`: both) | reproduce the notice, conditions and disclaimer |
| BSD-2-Clause | `git-version`, `git-version-macro` | the same |
| C libraries in `MIT OR Apache-2.0` crates | liblzma (0BSD) in `liblzma-sys`; zlib (Zlib) in `libz-sys`; libsais (Apache-2.0 WITH LLVM-exception, `crates/lpk-codecs-bwt/Cargo.toml:15`) in `libsais` | the C library's own licence; an override each in the generator (section 4) |
| Zlib | `zlib-rs`, `foldhash` | the notice (include it) |
| Unicode-3.0 | `unicode-ident` (proc-macro, compile time only) | the notice; harmless |
| MIT family (about 170) | the rest, MIT chosen (`Unlicense OR MIT` crates too: `deny.toml` lists MIT, not Unlicense) | each crate's copyright and permission notice |
| No obligation | `blake3` (CC0-1.0 chosen), `constant_time_eq` (CC0-1.0), `adler2` (0BSD) | nothing |
| Build-only (`cc`, `vcpkg`, ...) | not linked | nothing |
| Not in the tree yet | libarchive 3.8 (E4; BSD families, no LGPL: `docs/LICENSING.md:60`); Tauri and the WebView2 runtime (E5-2; Microsoft's terms, not copyleft) | the same census then |

## 2. The boundary for `cabac`

Preflate needs little from the coder: two trait bounds (`cabac_codec.rs:111-263`), three concrete types
(`VP8Context`, `VP8Reader`, `VP8Writer`; `stream_processor.rs:12`) and six methods (`grep -ohE
"\.(put|get)(_[a-z_]+)?\(" src/*.rs | sort | uniq -c`): `put`/`get`, `put_n_bits`/`get_n_bits`,
`put_unary_encoded`/`get_unary_encoded`. Both callers wrap every library call in `catch_unwind`
(`crates/lpk-core/src/peel/deflate/analyse.rs:280,319`, `decode.rs:31`, `crates/lpk-check/src/deflate.rs:100`) and
call once per stream (`analyse.rs:280-283`, `decode.rs:37`).

**Option A, the dynamic boundary** (the PLAN's wording). `crates/lpk-deflate-reconstruct`: `crate-type =
["cdylib", "rlib"]`, the only crate listing `preflate-rs`, exporting a C ABI of five functions:
`lpk_dr_abi_version`; `lpk_dr_library_version` (the preflate pin, `docs/spec/lpk-v1.md:912-916`);
`lpk_dr_analyse(stream, len, cfg)` returning plain text and corrections in DLL-owned buffers;
`lpk_dr_recreate(plain, corrections, limit, out, out_cap)` writing into the caller's `original_len` buffer;
`lpk_dr_free`. Every export catches panics and returns a code; no global state. `crates/lpk-deflate-reconstruct-sys`:
the loader (`libloading`, ISC, allow-listed), loading by absolute path next to the executable, never by bare
name (bare names are also searched in `PATH`), `$ORIGIN` on Linux, an ABI and version
check, a `OnceLock`, and a safe API returning `DeflateReconstructUnavailable { path, reason }` when the file is
missing, foreign or old. `lpk-core` depends on the `-sys` crate only: everything but the step keeps working; a
revision 1.2 block naming it fails with that error. `lpk-check` links the `rlib` face statically (open source, so
D-26 holds; no loader); `cargo tree -i preflate-rs -e normal` then has one parent, and `cargo tree -i
lpk-deflate-reconstruct -e normal` shows `lpk-check` alone. The `unsafe`: the loader's symbol lookups (a `-sys`
crate, allowed) and the exports' pointer-to-slice conversions, outside the `forbid(unsafe_code)` rule's exemptions;
the D-NN admits them for that crate's `ffi` module. Cost per stream: one indirect call, no copy on recreation, one
buffer hand-off on analysis; preflate's loops stay inside the library with their own LTO, so only the boundary
call's inlining is lost, small beside the matcher re-run; the PLAN's `office-pdf` run (static
build as control, same run, three repeats) measures it. Proof of no static LGPL code: (1) the one-parent `cargo
tree` line, asserted by a CI script; (2) `deny.toml` `[bans] deny = [{ crate = "preflate-rs", wrappers =
["lpk-deflate-reconstruct"] }, { crate = "cabac", wrappers = ["preflate-rs"] }]`, so any other user fails `cargo
deny check`; (3) release `lpk.exe` and GUI builds without `--remap-path-prefix` contain neither the
`cabac-0.15.0` nor the `preflate-rs-0.7.6` panic-location strings, while the DLL does (the positive control).
Costs: two crates, an FFI, a loader, a missing-library failure mode, the DLL in E7-2's artefacts and E7-3's
signing, D-26's duties, the measurement.

**Option B, a permissive coder as a format revision.** The record already carries a `library` byte, `0 = the
correction format of preflate-rs 0.7.6; no other value` (`lpk-v1.md:1513`); a new coder is `library = 1` in a
revision (D-49: a minor, a vector, CHANGES, `lpk-check`'s gate). Rejected: revisions 1.2 to 1.4 keep
`library = 0`, so the `cabac` path stays and B does not remove A.

**B-prime, the same coder without `cabac`.** `cabac`'s `NOTICE.txt` names libwebp (BSD-3-Clause) and libde265
(LGPL-3.0) as its origins; its `src/vp8.rs:1-5` calls the module Google's VP8 coder "as used by the JPEG Lepton
compressor", under the WebM BSD header; preflate never names the H.265 coder (no `h265` in its `src`).
`lepton_jpeg` 0.5.8 (Apache-2.0, same author, already in `lpk`) carries that coder: `Branch`
(`src/structs/branch.rs:32-51,101-126`: counts start at `0x0101`, the same `(a << 8) / (a + b)` table, the same
update), `VPXBoolWriter::put` (`vpx_bool_writer.rs:90-112`: the same `split` and renormalisation),
`put_n_bits`/`put_unary_encoded` in the same bit order (`:215-270`). Two conventions differ in source: the stream
tail (`cabac` pads with bypass zeros until its low register is empty; `lepton_jpeg` writes the minimal bytes with a
carry) and the context index of the composite methods (`cabac` clamps it to `A - 1`). Both are short functional
rules, stated in the spec and implemented from it. The design: `crates/lpk-codecs-vp8`
(Apache-2.0: three files ported from `lepton_jpeg` with their headers, never from `cabac`; `lepton_jpeg` keeps them
private, `src/lib.rs:38`), providing the two traits and six methods; the vendored preflate retargeted in its two
files, the change statement extended; `cabac` and both exceptions (`deny.toml:27`, `fuzz/deny.toml:31`) leave the
tree. The proof is the committed vectors: (a) decoding: every `deflate` record of `deflate-peel.lpk` and
`bwt-text.lpk` (`docs/spec/CONFORMANCE.md:112,114`), of the fixtures and of a committed set of today's corrections
recreates to `original_hash` (`lpk-check`'s existing check, `crates/lpk-check/src/deflate.rs:92-110`); (b) writing:
`the_vector_regenerates_identically` (`crates/lpk-core/tests/deflate_vector.rs:505-512`) stays green, so the new
writer's bytes equal `cabac`'s. If (b) fails in the tail only, the owner chooses between matching the tail rule and
a text revision with regenerated vectors (readers unchanged, as on 2026-10-16, `CHANGES.md:127-136`). If (a) fails
on any record, B-prime is dead and A is built.

**Option C, a dual licence from `cabac`'s author.** Cheap to ask (the author publishes preflate and lepton_jpeg
under Apache-2.0), not plannable: ask in parallel, plan nothing on it.

**Recommendation: B-prime first, A as the fallback.** B-prime removes the copyleft instead of fencing it: no loader,
no missing-library state, no FFI `unsafe`, no per-stream call, no LGPL texts, `lpk-check` untouched, the format
unchanged; committed vectors decide it before any boundary is built. The PLAN's
acceptance ("the boundary crate is a `cdylib` and the only user of `preflate-rs`") becomes "`cargo tree -i cabac`
prints nothing": an acceptance change for the owner. The implementer ports from `lepton_jpeg` and the spec without
opening `cabac`'s source (read here for the facts above; LGPL code may be read, never copied,
`docs/LICENSING.md:13`); counsel's review before a closed release (`LICENSING.md:87`) covers the port.

## 3. Ownership

Every crate carries `license.workspace = true`, `Apache-2.0 OR MIT` (`Cargo.toml:19`), the two licence files at the
root; no source file has a licence header (1 of 246 `.rs` files contains "Copyright": a tool banner,
`crates/lpk-bench/src/run/discover.rs:497`); all 636 commits carry the owner's two author strings (`git shortlog
-sn`), 572 with the Claude trailer; D-89 (6) reads Claude's output as the owner's work. No CONTRIBUTING.md or CLA
exists; no crate is published (`publish = false`). Third-party code in the tree (the vendored preflate, B-prime's
port, libsais) keeps its permissive licence and notices: no obstacle.

Relicensing a later version needs the owner holding every copyright (true today), the released versions staying
under `Apache-2.0 OR MIT` (irrevocable), and every outside contribution covered by a CLA granting the right to
relicense: one uncovered contribution blocks it. Recommendation: accept no outside code until the owner decides; a
one-paragraph CONTRIBUTING.md says so. Per-file headers add nothing with one owner; a copyright line in
README waits for the publisher's legal name (E7-1). The `deny.toml` comment "we are open source, so relinking is
satisfied" (`deny.toml:24`) is wrong for a closed build and is rewritten; under A the exception stays, confined by
section 2's `[bans]` wrappers; under B-prime it is deleted from both policy files.

## 4. Notices

`cargo about` (Embark, `Apache-2.0 OR MIT`), version-pinned in `ci.yml`'s tool install
(`.github/workflows/ci.yml:35`), a step after `cargo deny check` (`:44-45`). `about.toml`: `accepted` =
`deny.toml`'s list plus `Apache-2.0 WITH LLVM-exception` (libsais's C) and, only while `cabac` is present,
`LGPL-3.0-or-later`; `targets` = the three shipped triples; `ignore-build-dependencies = true`,
`ignore-dev-dependencies = true`; `[crates.<name>] clarify` entries for `lepton_jpeg` (the Apache-2.0 text and the
copyright line of `src/lib.rs:2`), `liblzma-sys` (0BSD), `libz-sys` (Zlib) and `libsais` (Apache-2.0 with the LLVM
exception). `about.hbs`: a header without a date (it must regenerate byte-identically); per licence id, the text
once and the crates under it (name, version, repository, the crate's own copyright line); the contents of each
NOTICE file a crate ships (Apache-2.0 section 4(d); none today); the bundled C libraries; and, while `cabac` ships,
the LGPL-3.0 and GPL-3.0 texts, its `NOTICE.txt` and D-26's prominent notice. Output `THIRD-PARTY-NOTICES` at the
repository root, committed. CI regenerates it to a temporary file and runs `git diff --no-index --exit-code`
against the committed one: red when `Cargo.lock` changed without it; E7-8 proves the red-then-green on a PR and
ships the file in every artefact. The same step adds P0-6's GPL-text grep over `crates/` and `vendor/`, absent from
CI today (`grep -rni gpl .github/workflows` finds nothing).

## 5. Tasks

1. **The B-prime experiment.** `crates/lpk-codecs-vp8`; preflate's two files retargeted; `LITEPACK-CHANGES.md`
   extended; today's corrections committed under `crates/lpk-core/tests/fixtures/vp8-cross/`. Acceptance: `cargo
   tree -i cabac -e normal` prints nothing; `cargo nextest run --workspace` green, `the_vector_regenerates_identically`,
   `preflate_patch`, `expected-1.2.toml` and `expected-1.3.toml` included; the cross-decoding test green. A tail-only
   difference goes to the owner.
2. **A, only if task 1 fails.** The two crates of section 2. Acceptance: the one-parent `cargo tree` line asserted
   in CI; `cargo deny check` green with the wrappers; the strings check green, the DLL its positive control; with
   the DLL renamed, `lpk x` on `deflate-peel.lpk` fails with `DeflateReconstructUnavailable` while `lpk l` and `lpk
   t` pass; the `office-pdf` run committed under `bench/results/`.
3. **Policy files.** `deny.toml` and `fuzz/deny.toml` without the exception (or with it confined); the comment
   rewritten. Acceptance: `cargo deny check` green in both.
4. **Notices.** `about.toml`, `about.hbs`, `THIRD-PARTY-NOTICES`, the CI step and the GPL grep. Acceptance: the step
   green; `cargo update -p <any crate>` without regeneration makes it red locally.
5. **Docs and decisions.** `docs/LICENSING.md` rows (`cabac` removed or marked as behind the boundary; the bundled C
   libraries; `lepton_jpeg`'s missing licence file); CONTRIBUTING.md; `docs/spec/lpk-v1.md` and `CHANGES.md` only if
   the tail rule or a text revision is chosen; PLAN LIC-2's acceptance text. D-NNs: one amending D-26 with what a
   closed build must keep open (B-prime: nothing of the engine's dependencies is copyleft, the rule kept for later
   LGPL wants; A: the DLL's complete source, the loader's path rule, the notices); the contribution policy; the
   `unsafe` admission (A only); the acceptance change (B-prime).

Out of Phase 1: the closing itself, the CLA text, counsel's review, the copyright line (needs E7-1's name), the Pro
process boundary the breakdown note asks to confirm (`docs/notes/phase1-e3-e9-breakdown.md:53-54`: none is needed
for licensing under B-prime), and the request to `cabac`'s author (C).
