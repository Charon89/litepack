# LIC-2 design: closed-source readiness (D-89 (6))

2026-10-22. For the implementer of LIC-2; docs only. Every claim about the tree was checked on `origin/main`
f243e06 with `cargo tree`, `cargo metadata` and `grep`.

## 1. Inventory

`cargo tree -i cabac -e normal` prints one path: `cabac 0.15.0 <- preflate-rs 0.7.6 (vendor/preflate-rs-0.7.6)
<- {lpk-check, lpk-core <- {lpk-bench, lpk-cli}}`. A census over `cargo metadata` (normal dependencies, every
target, no build or dev dependencies) finds 190 crates behind `lpk` and 114 behind `lpk-check`; `cabac` is the
only copyleft crate, no MPL-2.0 crate is in the tree, and `r-efi` (`MIT OR Apache-2.0 OR LGPL-2.1-or-later`) is
taken under MIT.

| Family | Crates and path | A closed build must |
|---|---|---|
| LGPL-3.0-or-later | `cabac` 0.15.0, through `preflate-rs` only: `cabac::vp8` and its two traits (`vendor/preflate-rs-0.7.6/src/cabac_codec.rs:7`, `stream_processor.rs:12`); `cabac::debug` in test code (`cabac_codec.rs:313-377`, `stream_processor.rs:521-714`) | keep it out of every closed binary (section 2); a shipped binary containing it: prominent notice, the GPL-3.0 and LGPL-3.0 texts, its complete source (D-26) |
| Apache-2.0 only | `lepton_jpeg` 0.5.8 (no licence file in its package; copyright line `src/lib.rs:2`), `preflate-rs` 0.7.6 (vendored and modified; the 4(b) change statement is `LITEPACK-CHANGES.md`), `lzma-rust2`, `rpassword`, `rtoolbox`, `sevenz-rust2`, `zopfli`; none ships a NOTICE file | the licence text and each copyright line; the change statement with any redistributed source |
| BSD-3-Clause | `zstd-sys` (libzstd inside it, `LICENSE.BSD-3-Clause`), `zstd`, `zstd-safe`; `reed-solomon-simd` (`MIT AND BSD-3-Clause`: both) | reproduce the notice, conditions and disclaimer |
| BSD-2-Clause | `git-version`, `git-version-macro` | the same |
| C libraries in `MIT OR Apache-2.0` crates | liblzma (0BSD) in `liblzma-sys`; zlib (Zlib) in `libz-sys`; libsais (Apache-2.0 WITH LLVM-exception, `crates/lpk-codecs-bwt/Cargo.toml:15`) in `libsais` | the C library's own licence; an override each in the generator (section 4) |
| Zlib; Unicode-3.0 | `zlib-rs`, `foldhash`; `unicode-ident` (proc-macro, compile time only) | the notice (include it) |
| MIT family (about 170) | the rest, MIT chosen (`Unlicense OR MIT` crates too: `deny.toml` lists MIT, not Unlicense) | each crate's copyright and permission notice |
| No obligation | `blake3` (CC0-1.0 chosen), `constant_time_eq` (CC0-1.0), `adler2` (0BSD); build-only crates (`cc`, `vcpkg`, ...) | nothing |
| Not yet in the tree | libarchive 3.8 (E4; BSD families, no LGPL: `docs/LICENSING.md:60`); Tauri, WebView2 (E5-2; not copyleft) | the same census then |

## 2. The boundary for `cabac`

Preflate needs little from the coder: two trait bounds (`cabac_codec.rs:111-263`), three concrete types
(`VP8Context`, `VP8Reader`, `VP8Writer`; `stream_processor.rs:12`) and six methods (`grep -ohE
"\.(put|get)(_[a-z_]+)?\(" cabac_codec.rs stream_processor.rs | sort | uniq -c`): `put`/`get`,
`put_n_bits`/`get_n_bits`, `put_unary_encoded`/`get_unary_encoded`. Both callers wrap every library call in
`catch_unwind` (`crates/lpk-core/src/peel/deflate/analyse.rs:280,319`, `decode.rs:31`,
`crates/lpk-check/src/deflate.rs:100`) and call once per stream (`analyse.rs:280-283`, `decode.rs:37`). The spec
defines the codec by citation, lets a conforming decoder use the cited library (`docs/spec/lpk-v1.md:912-917`) and
names `cabac` three times (`:921,1420,1431`).

**Option A, the dynamic boundary** (the PLAN's wording). `crates/lpk-deflate-reconstruct`: a `cdylib`, the only
product crate listing `preflate-rs`, exporting a C ABI: an ABI and library version (the preflate pin),
`analyse(stream)` returning plain text and corrections in DLL-owned buffers, `recreate(plain, corrections, limit)`
writing into the caller's `original_len` buffer, `free`; every export catches panics and returns a code; no global
state. `crates/lpk-deflate-reconstruct-sys`: the loader (`libloading`, ISC, allow-listed) by absolute path next to
the executable, never by bare name (also searched in `PATH`), `$ORIGIN` on Linux, an ABI and version check, a
`OnceLock`, a safe API returning `DeflateReconstructUnavailable { path, reason }` when the file is missing, foreign
or old. `lpk-core` depends on the `-sys` crate only: listing keeps working; extraction and `lpk t` (`Tool::Verify`
decodes blocks, `crates/lpk-cli/src/lib.rs:1777-1781`) of a block naming the step fail with that error.
`lpk-check` keeps depending on `preflate-rs` directly (D-49 (5): the independent decoder links the named library,
never an lpk-authored face) and stays open source, so D-26 holds for it; `cargo tree -i preflate-rs -e normal`
then shows exactly two parents. The `unsafe` (the loader's symbol lookups in a `-sys` crate, allowed; the exports'
pointer-to-slice conversions, which the `forbid(unsafe_code)` rule does not exempt) is admitted by the D-NN for
that crate's `ffi` module. Cost per stream: one indirect call, no copy on recreation, one buffer hand-off on
analysis; preflate's loops keep their own LTO inside the library, so only the boundary call's inlining is lost,
small beside the matcher re-run; the PLAN's `office-pdf` run (static build as control, same run, three repeats)
measures it. Proof of no static LGPL code: (1) the two-parent `cargo tree` line, asserted in CI; (2) `deny.toml`
`[bans] deny = [{ crate = "preflate-rs", wrappers = ["lpk-deflate-reconstruct", "lpk-check"] }, { crate =
"cabac", wrappers = ["preflate-rs"] }]`, so any other user fails `cargo deny check`; (3) release `lpk.exe` and GUI
builds without `--remap-path-prefix` contain neither the `cabac-0.15.0` nor the `preflate-rs-0.7.6` panic-location
strings, while the DLL does (the positive control). Costs: two crates, an FFI, a loader, a missing-library failure
mode, the DLL in E7-2's artefacts and E7-3's signing, D-26's duties, the measurement.

**Option B, a permissive coder as a format revision.** The record already carries a `library` byte, `0 = the
correction format of preflate-rs 0.7.6; no other value` (`lpk-v1.md:1513`); a new coder is `library = 1` in a
revision (D-49: a minor, a vector, CHANGES, `lpk-check`'s gate). Rejected: revisions 1.2 to 1.4 keep
`library = 0`, so the `cabac` path stays and B does not remove A.

**B-prime, the same coder without `cabac`.** `lepton_jpeg` 0.5.8 (Apache-2.0, same author, already in `lpk`)
carries the VP8 bool coder preflate uses: `Branch` (`src/structs/branch.rs:32-51,101-126`: counts from `0x0101`,
the same `(a << 8) / (a + b)` table and update), `VPXBoolWriter::put` (`vpx_bool_writer.rs:90-112`: the same
`split` and renormalisation), `put_n_bits`/`put_unary_encoded` in the same bit order (`:215-270`), under
Microsoft's Apache-2.0 header and the WebM BSD-3-Clause banner (`vpx_bool_writer.rs:1-20`). Two conventions
differ: the stream tail (`cabac` bypass-codes zero bits until its low register is empty, then flushes;
`lepton_jpeg` writes the minimal bytes with a carry) and the context index of the composite methods (`cabac`
clamps it to `A - 1`). Provenance: `cabac`'s `NOTICE.txt` names libwebp (BSD-3-Clause) and libde265 (LGPL-3.0)
among its origins; that changes nothing: its Rust translation is LGPL-3.0-or-later whatever banner a file carries,
and nothing is taken from it. The design: `crates/lpk-codecs-vp8` (Apache-2.0: the three coder files ported from
`lepton_jpeg`, which keeps them private, `src/lib.rs:38`, both headers intact, plus `lepton_jpeg`'s repository
`NOTICE.txt`, absent from the package), providing the two traits and six methods; the vendored preflate retargeted
in its two files, the change statement extended; `cabac` and both exceptions (`deny.toml:27`, `fuzz/deny.toml:31`)
leave the tree and both lockfiles. The tail rule is a conformance requirement, not a choice: `lpk-v1.md:916-917`
lets a conforming decoder use the cited library, so the ported writer implements the cited flush by construction,
bit for bit; a text revision with regenerated vectors would also require dropping that clause, which is not
planned. Proof: (a) a randomised cross set in both directions (task 1), since nothing shows the committed vectors
exercise the clamp path or wide `put_n_bits` values; (b) every `deflate` record of `deflate-peel.lpk`,
`bwt-text.lpk` (`docs/spec/CONFORMANCE.md:112,114`) and the fixtures recreates to `original_hash` (`lpk-check`'s
existing check, `crates/lpk-check/src/deflate.rs:92-110`); (c) `the_vector_regenerates_identically`
(`crates/lpk-core/tests/deflate_vector.rs:505-512`) stays green. If any fails, B-prime is dead and A is built.
Clean room: the port is executed by an agent that has not read `cabac`, never opens its source (the registry
path, GitHub, docs.rs's source view) and uses no `cabac` identifier; the fixtures of (a), not `cabac`'s source, are
the oracle a mismatch is matched against; this note's author read `cabac` for the comparison above and writes no
code of the port.

**Option C, a dual licence from `cabac`'s author.** Cheap to ask (the author publishes preflate and lepton_jpeg
under Apache-2.0), not plannable: ask in parallel, plan nothing on it.

**Recommendation: B-prime first, A as the fallback.** B-prime removes the copyleft instead of fencing it: no loader,
no missing-library state, no FFI `unsafe`, no per-stream call, no LGPL texts, `lpk-check` untouched, the format
unchanged; the cross set decides it before any boundary is built. Counsel's review before a closed release
(`docs/LICENSING.md:87`) covers the port.

## 3. Ownership

Every crate carries `license.workspace = true`, `Apache-2.0 OR MIT` (`Cargo.toml:19`), the two licence files at the
root; no source file has a licence header (1 of 246 `.rs` files contains "Copyright": a tool banner,
`crates/lpk-bench/src/run/discover.rs:497`); all 636 commits carry the owner's two author strings (`git shortlog
-sn`), 572 with the Claude trailer. No CONTRIBUTING.md or CLA exists; no crate is published (`publish = false`).
Third-party code in the tree (the vendored preflate, B-prime's port, libsais) keeps its permissive licence and
notices: no obstacle.

Closing a later version relicenses nobody else's code: an outside PR arrives under `Apache-2.0 OR MIT` (GitHub's
terms, D.6, inbound as outbound) and may stay in a closed build with attribution; it blocks sole ownership, not
closing. Released versions stay under `Apache-2.0 OR MIT` (irrevocable). For counsel, rather than resting on
D-89 (6)'s reading: the copyrightability of AI-generated output is unsettled (the US Copyright Office's 2025
report), no barrier to closing but thinner enforcement of the open versions; and whether a CLA is wanted before any
outside code is accepted. Recommendation: accept no outside code until the owner decides; a one-paragraph
CONTRIBUTING.md says so. Per-file headers add nothing with one owner; a copyright line in README waits for the
publisher's legal name (E7-1). The `deny.toml` comment "we are open source, so relinking is satisfied"
(`deny.toml:24`) is wrong for a closed build and is rewritten; under A the exception stays, confined by section 2's
`[bans]` wrappers; under B-prime it is deleted from both policy files.

## 4. Notices

`cargo about` (Embark, `Apache-2.0 OR MIT`), version-pinned in `ci.yml`'s tool install
(`.github/workflows/ci.yml:35`), a step after `cargo deny check` (`:44-45`), generated from the shipped packages'
manifests (`lpk-cli`, later the GUI), not the workspace: the `icu_*` Unicode-3.0 family reaches only `lpk-bench`,
through `reqwest` (`cargo tree -i icu_normalizer -e normal`). `about.toml`: `accepted` = `deny.toml`'s list plus
`Apache-2.0 WITH LLVM-exception` (libsais's C) and, only while `cabac` is present, `LGPL-3.0-or-later`; `targets`
= the three shipped triples; build and dev dependencies ignored; `[crates.<name>] clarify` entries for
`lepton_jpeg` (the Apache-2.0 text and the copyright line of `src/lib.rs:2`), `liblzma-sys` (0BSD), `libz-sys`
(Zlib) and `libsais` (Apache-2.0 with the LLVM exception). `about.hbs`: a header without a date (it must regenerate
byte-identically); per licence id, the text once and the crates under it (name, version, repository, copyright
line); the NOTICE file of each crate that ships one (Apache-2.0 section 4(d); none today); the bundled C
libraries; a static section for the third-party code inside our own crates, which `cargo about` skips as private
(`lpk-codecs-vp8`'s Microsoft Apache-2.0 and WebM BSD-3-Clause banners, `lepton_jpeg`'s `NOTICE.txt`); and, while
`cabac` ships, the LGPL-3.0 and GPL-3.0 texts, its `NOTICE.txt` and D-26's prominent notice. Output
`THIRD-PARTY-NOTICES` at the repository root, committed. CI regenerates it to a temporary file and runs `git diff
--no-index --exit-code` against the committed one: red when `Cargo.lock` changed without it; E7-8 proves the
red-then-green on a PR and ships the file in every artefact. The same step adds P0-6's GPL-text grep over `crates/`
and `vendor/`, absent from CI today (`grep -rni gpl .github/workflows` finds nothing).

## 5. Tasks

1. **The B-prime port and its cross set.** `crates/lpk-codecs-vp8` under section 2's clean-room rules;
   preflate's two files retargeted; `LITEPACK-CHANGES.md` extended. The cross set is generated once by a scratch
   crate outside the workspace with `cabac` 0.15.0 as a dev-dependency (never in the product's graph): randomised
   sequences through all six methods with edge values (`v = A`, `num_bits > A`, long runs of one symbol, empty
   streams), both directions (new writer to `cabac` reader, `cabac` writer to new reader), the symbols and
   `cabac`'s bytes committed under `crates/lpk-core/tests/fixtures/vp8-cross/` with a replay test; afterwards
   `cabac` leaves `Cargo.lock` and `fuzz/Cargo.lock`. Acceptance: `cabac` absent from both lockfiles; the replay
   test, `the_vector_regenerates_identically`, `preflate_patch`, `expected-1.2.toml` and `expected-1.3.toml` green
   under `cargo nextest run --workspace`.
2. **The spec.** Unconditional: section 8 states the two coder rules as normative text (the tail: after the last
   symbol, bypass-code zero bits until the low register is zero, then flush; the composite methods: n-bit fields
   most-significant bit first, unary as ones then a zero, the context index `min(A - 1, i)`), so the format no
   longer depends on `cabac`'s behaviour, with a dated `CHANGES.md` entry (no reader outcome changes). Following
   the outcome: the three naming lines `lpk-v1.md:921,1420,1431` name `lpk-codecs-vp8` under B-prime and keep
   `cabac` under A.
3. **A, only if task 1 fails.** The two crates of section 2. Acceptance: the two-parent `cargo tree` line asserted
   in CI; `cargo deny check` green with the wrappers; the strings check green, the DLL its positive control; with
   the DLL renamed, `lpk l` passes and `lpk x` and `lpk t` on `deflate-peel.lpk` fail with
   `DeflateReconstructUnavailable` while both pass on `jpeg-peel.lpk`; the `office-pdf` run committed under
   `bench/results/`.
4. **Policy files.** `deny.toml` and `fuzz/deny.toml` without the exception (or with it confined); the comment
   rewritten. Acceptance: `cargo deny check` green in both.
5. **Notices.** `about.toml`, `about.hbs`, `THIRD-PARTY-NOTICES`, the CI step and the GPL grep. Acceptance: the step
   green; `cargo update -p <any crate>` without regeneration makes it red locally; the file names no bench-only
   crate.
6. **Docs and decisions.** `docs/LICENSING.md` rows (`cabac` removed or marked as behind the boundary; the bundled C
   libraries; `lepton_jpeg`'s missing licence file; `lpk-codecs-vp8`'s banners); CONTRIBUTING.md; PLAN LIC-2's
   acceptance, proposed as: "Acceptance (B-prime): `cabac` absent from `Cargo.lock` and `fuzz/Cargo.lock`, both
   `deny.toml` exceptions deleted and `cargo deny check` green in the workspace and in `fuzz/`; the `vp8-cross`
   replay test and the vectors green; spec section 8 states the coder rules and names no `cabac`; the notices file
   generated in CI; D-26 amended by a `D-NN` naming what a closed build must keep open. Fallback (A, only if the
   cross set fails): `cargo deny check` green with the LGPL exception confined to `lpk-deflate-reconstruct` by
   `[bans] wrappers`; `cargo tree -i preflate-rs -e normal` shows exactly `lpk-deflate-reconstruct` and
   `lpk-check`; `lpk.exe` and the GUI binary carry no `cabac-0.15.0` or `preflate-rs-0.7.6` path string while the
   DLL does; the `office-pdf` cost measured and committed." D-NNs: one amending D-26 with what a closed build must
   keep open (B-prime: nothing of the engine's dependencies is copyleft, the rule kept for later LGPL wants; A: the
   DLL's complete source, the loader's path rule, the notices); the contribution policy; the `unsafe` admission (A
   only); the acceptance change (B-prime).

Out of Phase 1: the closing itself, the CLA text, counsel's review, the copyright line (needs E7-1's name), the Pro
process boundary the breakdown note asks to confirm (`docs/notes/phase1-e3-e9-breakdown.md:53-54`: none is needed
for licensing under B-prime), and the request to `cabac`'s author (C).
