//! The `bwt` memory term (`lpk_codecs_bwt::layout::memory_term`, spec section 8 "Decoding
//! `bwt`") against the peak heap use of the revision 1.3 decoder on a 64 MiB block, measured by
//! a counting global allocator; and the inverse against libsais's own on the same text. Its own
//! test binary, so no other test allocates meanwhile.
//!
//! What the term must cover beyond the output (the block's own buffer, counted in
//! `max_block_plain`): the step's input (I(O, B) bytes, held by the reader), the `u32` successor
//! table of the largest block and the small tables. The decoder's peak here is the successor
//! table, the per-block index vectors and the output; the input is the test's and is added.
#![allow(clippy::unwrap_used)]

use lpk_codecs_bwt::layout::{encode, input_len, memory_term, MEMORY_ALLOWANCE};
use lpk_core::BwtDecoder;
use lpk_format::{PrimitiveDecoder, Resources};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

/// 64 MiB of generated text: words, line breaks and numbers, as a log would hold.
fn text(n: usize) -> Vec<u8> {
    let words = [
        "alpha ",
        "bravo ",
        "charlie ",
        "delta ",
        "echo ",
        "foxtrot ",
        "golf ",
        "hotel ",
        "2026-10-18 ",
        "INFO ",
        "worker ",
        "\n",
    ];
    let mut x = 7u32;
    let mut out = Vec::with_capacity(n + 16);
    while out.len() < n {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.extend_from_slice(words[(x >> 24) as usize % words.len()].as_bytes());
    }
    out.truncate(n);
    out
}

#[test]
fn decoder_peak_is_within_the_term_plus_the_output_on_a_64_mib_block() {
    let n = 64usize << 20;
    let block_size = n as u64;
    let plain = text(n);
    let input = encode(&plain, block_size as u32).unwrap();
    assert_eq!(input.len() as u64, input_len(n as u64, block_size));
    let term = memory_term(n as u64, block_size);
    let limits = Resources {
        max_block_plain: n as u64,
        memory: n as u64 + term,
        ..Resources::default()
    };
    let params = (block_size as u32).to_le_bytes();
    ALLOC.reset_peak();
    let before = ALLOC.current();
    let out = BwtDecoder
        .decode_step(&params, &input, n as u64, true, &limits)
        .unwrap();
    let peak = (ALLOC.peak() - before) as u64;
    assert_eq!(out, plain);
    drop(out);
    // Beyond the output: the decoder's peak less the output buffer, plus the step's input.
    let needed = input.len() as u64 + peak - n as u64;
    println!(
        "bwt 64 MiB block: input {}, peak {peak}, needed beyond output {needed}, term {term}",
        input.len()
    );
    assert!(needed <= term, "needed {needed} > term {term}");
    // The allowance leaves room: the small tables are far below 1 MiB.
    assert!(
        term - needed >= MEMORY_ALLOWANCE / 2,
        "margin {}",
        term - needed
    );
    // The same text in 4 MiB blocks: the term is smaller (one 4 MiB table) and still holds.
    let block_size = 4u64 << 20;
    let input = encode(&plain, block_size as u32).unwrap();
    let term = memory_term(n as u64, block_size);
    let limits = Resources {
        memory: n as u64 + term,
        ..limits
    };
    let params = (block_size as u32).to_le_bytes();
    ALLOC.reset_peak();
    let before = ALLOC.current();
    let out = BwtDecoder
        .decode_step(&params, &input, n as u64, true, &limits)
        .unwrap();
    let peak = (ALLOC.peak() - before) as u64;
    assert_eq!(out, plain);
    let needed = input.len() as u64 + peak - n as u64;
    println!(
        "bwt 4 MiB blocks: input {}, peak {peak}, needed beyond output {needed}, term {term}",
        input.len()
    );
    assert!(needed <= term, "needed {needed} > term {term}");
}

/// The inverse equals libsais's own inverse on 64 MiB of text (design section 6, task C).
#[test]
fn the_inverse_agrees_with_libsais_on_64_mib() {
    let n = 64usize << 20;
    let plain = text(n);
    let (bwt, primary, theirs) = lpk_codecs_bwt::libsais_round_trip(&plain).unwrap();
    assert!(theirs == plain);
    // The layout's indices come from the forward call with sampling; its transformed bytes and
    // primary index agree with libsais's unsampled run, and the `u32` inverse gives the text
    // the library's inverse gives.
    let t = lpk_codecs_bwt::forward(&plain).unwrap();
    assert!(t.bwt == bwt);
    assert_eq!(t.primary(), primary);
    let mut out = vec![0u8; n];
    let mut next = Vec::new();
    lpk_codecs_bwt::layout::inverse_block(&t.bwt, &t.aux, &mut next, &mut out).unwrap();
    assert!(out == theirs);
}
