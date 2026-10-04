//! The block API a parallel extraction uses (E2-19): `Archive::fork`, `Archive::block_chunks`
//! and `Archive::decode_block_checked`; additive, so the archive bytes are those of any writer.
#![allow(clippy::unwrap_used)]

use lpk_format::{Archive, EntryFlags, FormatError, Resources, Writer, WriterOptions};
use std::io::Cursor;

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

fn build() -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let files: Vec<(String, Vec<u8>)> = (0..10)
        .map(|i| (format!("f{i:02}"), noise(i, 3000 + i as usize * 5000)))
        .collect();
    let mut out = Vec::new();
    let mut w = Writer::new(
        &mut out,
        WriterOptions {
            chunk_size: 4096,
            block_size: 16384,
            archive_id: [3; 16],
            ..WriterOptions::default()
        },
    )
    .unwrap();
    for (p, d) in &files {
        w.add_file(p, EntryFlags::EMPTY, 0, &mut &d[..]).unwrap();
    }
    w.finish().unwrap();
    (out, files)
}

fn rehash_frame(bytes: &mut [u8], offset: u64, len: u64) {
    let (o, l) = (offset as usize, len as usize);
    let vl = (1..=10)
        .find(|&vl| lpk_format::varint::len((l - 36 - vl) as u64) == vl)
        .unwrap();
    let payload = bytes[o + 4 + vl..o + l - 32].to_vec();
    let h = blake3::hash(&payload);
    bytes[o + l - 32..o + l].copy_from_slice(h.as_bytes());
}

#[test]
fn every_block_decodes_to_its_chunks_through_a_fork() {
    let (bytes, files) = build();
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let mut f = a.fork(Cursor::new(bytes.clone()));
    let all: Vec<u8> = files.iter().flat_map(|(_, d)| d.clone()).collect();
    let n = a.index().blocks.len();
    assert!(n >= 4);
    let mut joined = Vec::new();
    for b in 0..n {
        let range = a.block_chunks(b).unwrap();
        let plain = f.decode_block_checked(b).unwrap();
        let loc = a.index().blocks[b];
        assert_eq!(plain.len() as u64, loc.plain_len);
        assert_eq!(range.end - range.start, loc.chunk_count);
        for c in range {
            let p = a.chunks().locate(c).unwrap();
            assert_eq!(p.block, b);
            let s = p.offset_in_block as usize;
            let rec = a.chunks().record(c).unwrap();
            assert_eq!(
                blake3::hash(&plain[s..s + p.plain_len as usize]).as_bytes(),
                &rec.hash
            );
        }
        joined.extend_from_slice(&plain);
    }
    // Without dedup, the blocks in order are the files in order.
    assert_eq!(joined, all);
    assert!(a.block_chunks(n).is_none());
    assert!(matches!(
        f.decode_block_checked(n),
        Err(FormatError::BlockCoverage { .. })
    ));
}

#[test]
fn a_corrupted_chunk_is_a_chunk_mismatch_and_a_bad_frame_a_hash_mismatch() {
    let (bytes, _) = build();
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let victim = a.index().blocks[2];
    let last = victim.first_chunk + victim.chunk_count - 1;
    let mut bad = bytes.clone();
    bad[(victim.frame_offset + victim.frame_len - 33) as usize] ^= 0x55;
    let mut f = a.fork(Cursor::new(bad.clone()));
    assert!(matches!(
        f.decode_block_checked(2),
        Err(FormatError::HashMismatch { kind: 2 })
    ));
    rehash_frame(&mut bad, victim.frame_offset, victim.frame_len);
    let mut f = a.fork(Cursor::new(bad));
    assert!(matches!(
        f.decode_block_checked(2),
        Err(FormatError::ChunkMismatch { chunk }) if chunk == last
    ));
    // The other blocks are untouched.
    assert!(f.decode_block_checked(1).is_ok());
}
