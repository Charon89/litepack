//! Revision 1.3's BWT stage against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test bwt_corpus -- --ignored --nocapture`
//!
//! `every_class_round_trips_through_the_engine`: every class of the corpus through the default
//! Balanced pipeline (the BWT stage on), extracted by the D-54 engine and checked bit for bit;
//! prints per class the archive bytes, the block outcomes and the wall seconds.
//!
//! `the_trials_choice_against_whole_block_encodings_on_the_text_classes` (informative, design
//! section 6 task D): on `text-prose` and `logs-text`, for every compressed block of the
//! archive the pipeline wrote, the block's plain bytes are encoded whole both ways (raw LZMA at
//! the tier's dictionary; BWT then LZMA) and the smaller is compared with what the four-stripe
//! trial chose; prints the agreement count and the bytes the disagreements cost.
#![allow(clippy::unwrap_used)]

use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::time::Instant;

use lpk_core::{
    archive_balanced_file, extract_file, register_full_reader, BalancedOptions, DefaultPolicy,
    ExtractOptions, LzmaEncoder,
};
use lpk_format::{Archive, BlockEncoder, BlockHeader, EntryKind, PrimitiveId, Resources};

fn hash_file(p: &Path) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    std::io::copy(&mut BufReader::new(File::open(p).unwrap()), &mut h).unwrap();
    h.finalize()
}

fn classes(root: &Path) -> Vec<PathBuf> {
    let mut classes: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    classes.sort();
    assert!(!classes.is_empty());
    classes
}

#[test]
#[ignore]
fn every_class_round_trips_through_the_engine() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    println!(
        "| class | files | MB | archive B | lzma/zstd/bwt/class/no-gain | bwt sample B | minor | max_bwt_block | decode_memory | s compress | s extract |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for dir in classes(&root) {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let work = tempfile::tempdir().unwrap();
        let arch = work.path().join("a.lpk");
        let t0 = Instant::now();
        let (ws, b) = archive_balanced_file(&dir, &arch, BalancedOptions::default()).unwrap();
        let compress = t0.elapsed().as_secs_f64();
        let out = work.path().join("out");
        let t0 = Instant::now();
        let s = extract_file(
            &arch,
            &out,
            &[],
            None,
            &DefaultPolicy,
            &ExtractOptions::default(),
        )
        .unwrap();
        let extract = t0.elapsed().as_secs_f64();
        let mut a = Archive::open(File::open(&arch).unwrap(), &Resources::default()).unwrap();
        let (minor, env) = (a.header().version.minor, a.index().envelope);
        let t = a.entry_table().unwrap();
        let mut files = 0u64;
        for e in t.table().unwrap().iter() {
            let e = e.unwrap();
            if e.kind == EntryKind::File {
                assert_eq!(
                    hash_file(&out.join(&e.path)),
                    hash_file(&dir.join(&e.path)),
                    "{name}/{}",
                    e.path
                );
                files += 1;
            }
        }
        assert_eq!(files, s.files, "{name}");
        assert_eq!(s.blocks_decoded, s.blocks_needed, "{name}");
        println!(
            "| {name} | {files} | {:.1} | {} | {}/{}/{}/{}/{} | {} | {minor} | {} | {} | {compress:.2} | {extract:.2} |",
            s.bytes as f64 / 1e6,
            ws.archive_len,
            b.lzma_blocks,
            b.zstd_blocks,
            b.bwt_blocks,
            b.stored_by_class,
            b.stored_no_gain,
            b.bwt_sample_bytes,
            env.max_bwt_block,
            env.decode_memory
        );
    }
}

#[test]
#[ignore]
fn the_trials_choice_against_whole_block_encodings_on_the_text_classes() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let o = BalancedOptions::default();
    let lzma = LzmaEncoder::with_dict(o.dict_size).unwrap();
    println!("| class | block | plain B | trial chose | whole lzma B | whole bwt B | whole prefers | agree | cost B |");
    println!("|---|---|---|---|---|---|---|---|---|");
    let (mut agree, mut total, mut cost) = (0u64, 0u64, 0i64);
    for class in ["text-prose", "logs-text"] {
        let dir = root.join(class);
        let work = tempfile::tempdir().unwrap();
        let arch = work.path().join("a.lpk");
        archive_balanced_file(&dir, &arch, o.clone()).unwrap();
        let bytes = std::fs::read(&arch).unwrap();
        let mut a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
        register_full_reader(&mut a);
        let limits = *a.resources();
        let blocks = a.index().blocks.clone();
        for (i, b) in blocks.iter().enumerate() {
            let frame = &bytes[b.frame_offset as usize..(b.frame_offset + b.frame_len) as usize];
            let Some(lpk_format::frame::ReadFrame::Known(f)) =
                lpk_format::Frame::read(&mut Cursor::new(frame), &Default::default()).unwrap()
            else {
                panic!("not a block frame")
            };
            let (header, used) = BlockHeader::parse(&f.payload, i, 0).unwrap();
            let steps: Vec<PrimitiveId> = header.graph.steps.iter().map(|s| s.primitive).collect();
            // Only the blocks the trial decided: a plain model step, or [lzma, bwt].
            let chose_bwt = match steps.as_slice() {
                [PrimitiveId::Lzma | PrimitiveId::Zstd] => false,
                [PrimitiveId::Lzma, PrimitiveId::Bwt] => true,
                _ => continue,
            };
            let plain =
                lpk_format::decode_block(a.registry_mut(), &header, i, &f.payload[used..], &limits)
                    .unwrap();
            assert_eq!(plain.len() as u64, b.plain_len);
            let whole_lzma = lzma.clone().encode(&plain).unwrap().bytes.len();
            let whole_bwt = lzma.encode_bwt(&plain, o.bwt_block).unwrap().bytes.len();
            let whole_prefers_bwt = whole_bwt < whole_lzma;
            let agreed = whole_prefers_bwt == chose_bwt;
            let block_cost = if agreed {
                0
            } else {
                (whole_lzma as i64 - whole_bwt as i64).abs()
            };
            println!(
                "| {class} | {i} | {} | {} | {whole_lzma} | {whole_bwt} | {} | {agreed} | {block_cost} |",
                b.plain_len,
                steps.iter().map(|p| format!("{p:?}")).collect::<Vec<_>>().join("+"),
                if whole_prefers_bwt { "bwt" } else { "lzma" }
            );
            total += 1;
            agree += u64::from(agreed);
            cost += block_cost;
        }
    }
    println!("agreement: {agree} of {total} blocks; the disagreements cost {cost} bytes in total");
}
