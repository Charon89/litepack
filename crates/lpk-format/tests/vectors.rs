//! The committed zstd and LZMA vectors under `tests/vectors/`: they decode to their
//! generated contents, verify with the reference tool, and are reproduced
//! byte for byte by the writer (regenerate on purpose with
//! `cargo test -p lpk-format --test gen_vectors -- --ignored`).
#![allow(clippy::unwrap_used)]

mod common;

use common::{build_vector, vector_files, vectors_dir, DICT_FILE, VECTORS};
use lpk_format::{prior_id, Archive, MemoryPriors, Resources};
use std::io::Cursor;

fn read(name: &str) -> Vec<u8> {
    std::fs::read(vectors_dir().join(name)).unwrap()
}

fn open(name: &str) -> Archive<Cursor<Vec<u8>>> {
    let mut a = Archive::open(Cursor::new(read(name)), &Resources::default()).unwrap();
    if name == "zstd-dict.lpk" {
        let mut store = MemoryPriors::new();
        store.insert(read(DICT_FILE));
        a.set_priors(Box::new(store));
    }
    a
}

#[test]
fn every_vector_decodes_to_its_generated_contents() {
    for name in VECTORS {
        let mut a = open(name);
        let want = vector_files(name);
        let table = a.entry_table().unwrap();
        let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
        assert_eq!(entries.len(), want.len(), "{name}");
        for (e, (path, data)) in entries.iter().zip(&want) {
            assert_eq!(&e.path, path, "{name}");
            let mut got = Vec::new();
            a.extract(e, &mut got).unwrap();
            assert!(&got == data, "{name}: {path}");
        }
        a.verify().unwrap();
    }
}

/// The graph of block `i` as its `ChunkData` header says.
fn block_graph(a: &mut Archive<Cursor<Vec<u8>>>, i: usize) -> lpk_format::Graph {
    let b = a.index().blocks[i];
    let at = lpk_format::FrameLocation {
        offset: b.frame_offset,
        len: b.frame_len,
    };
    let f = a
        .read_frame_at(at, lpk_format::FrameKind::ChunkData)
        .unwrap();
    lpk_format::BlockHeader::parse(&f.payload, i, 0)
        .unwrap()
        .0
        .graph
}

fn lzma_params(a: &mut Archive<Cursor<Vec<u8>>>, i: usize) -> Vec<u8> {
    let g = block_graph(a, i);
    assert_eq!(g.steps.len(), 1);
    assert_eq!(g.steps[0].primitive, lpk_format::PrimitiveId::Lzma);
    g.steps[0].params.clone()
}

#[test]
fn the_writer_is_deterministic() {
    let build = || {
        common::write_archive(
            lpk_format::WriterOptions {
                chunk_size: 4096,
                block_size: 16 * 1024,
                archive_id: [9; 16],
                ..lpk_format::WriterOptions::default()
            },
            &vector_files("zstd-multiblock.lpk"),
        )
    };
    assert!(build() == build());
}

#[test]
fn the_window_vector_frame_declares_its_window() {
    use lpk_format::BlockEncoder;
    let plain = &vector_files("zstd-window.lpk")[0].1;
    let frame = common::ZstdTestEncoder::new(3, 24, None)
        .encode(plain)
        .unwrap();
    // Magic (4), descriptor (not single-segment), window descriptor: 2^24.
    assert_eq!(frame[4] & 0x20, 0);
    assert_eq!(frame[5], 0x70);
}

/// Reproduces the committed bytes; only valid with the zstd library version
/// that wrote them (for the LZMA ones, the liblzma version), so it runs on purpose, not in the normal suite.
#[test]
#[ignore = "bytes depend on the zstd and liblzma library versions"]
fn the_writer_reproduces_every_vector_byte_for_byte() {
    let dict = read(DICT_FILE);
    for name in VECTORS {
        let again = build_vector(name, Some(&dict));
        assert!(
            again == read(name),
            "{name} differs from the committed bytes"
        );
    }
}

#[test]
fn what_each_vector_exercises() {
    let a = open("zstd-basic.lpk");
    assert_eq!(a.index().blocks.len(), 1);
    assert!(a.priors().is_empty());

    let a = open("zstd-multiblock.lpk");
    assert!(a.index().blocks.len() >= 2);
    // Some file's chunk list crosses a block boundary.
    let mut a = a;
    let table = a.entry_table().unwrap();
    let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
    let spans = a.index().blocks.iter().skip(1).any(|b| {
        entries.iter().any(|e| {
            e.chunks.iter().any(|&c| c < b.first_chunk)
                && e.chunks.iter().any(|&c| c >= b.first_chunk)
        })
    });
    assert!(spans);

    let a = open("zstd-dict.lpk");
    assert_eq!(a.index().blocks.len(), 1);
    assert_eq!(a.priors(), &[prior_id(&read(DICT_FILE))]);

    let a = open("zstd-window.lpk");
    assert_eq!(a.index().envelope.max_window, 1 << 24);
    assert!(a.priors().is_empty());

    let mut a = open("lzma-basic.lpk");
    assert_eq!(a.index().blocks.len(), 1);
    // dict_size 8 MiB, lc 3, lp 0, pb 2.
    assert_eq!(lzma_params(&mut a, 0), [0, 0, 0x80, 0, 3, 0, 2]);
    assert_eq!(a.index().envelope.max_window, 1 << 23);
    assert!(a.priors().is_empty());

    let mut a = open("lzma-multiblock.lpk");
    assert!(a.index().blocks.len() >= 2);
    let table = a.entry_table().unwrap();
    let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
    let spans = a.index().blocks.iter().skip(1).any(|b| {
        entries.iter().any(|e| {
            e.chunks.iter().any(|&c| c < b.first_chunk)
                && e.chunks.iter().any(|&c| c >= b.first_chunk)
        })
    });
    assert!(spans);

    // lc 0, lp 2, pb 0: its dictionary is 1 MiB.
    let mut a = open("lzma-props.lpk");
    // dict_size 1 MiB, lc 0, lp 2, pb 0.
    assert_eq!(lzma_params(&mut a, 0), [0, 0, 0x10, 0, 0, 2, 0]);
    assert_eq!(a.index().envelope.max_window, 1 << 20);
    assert!(a.priors().is_empty());
}

#[test]
fn a_prior_less_reader_reports_the_dictionary_vector() {
    let mut a = Archive::open(Cursor::new(read("zstd-dict.lpk")), &Resources::default()).unwrap();
    assert!(matches!(
        a.verify(),
        Err(lpk_format::FormatError::MissingPrior { .. })
    ));
}

fn tool(args: &[&str]) -> (i32, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = lpk_format::cli::run(args.iter().copied(), &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn lpk_decode_verifies_every_vector() {
    let dir = vectors_dir();
    let prior = dir.join(DICT_FILE);
    for name in VECTORS {
        let archive = dir.join(name);
        let mut args = vec!["lpk-decode", "verify"];
        if name == "zstd-dict.lpk" {
            args.extend(["--prior", prior.to_str().unwrap()]);
        }
        args.push(archive.to_str().unwrap());
        let (code, out, err) = tool(&args);
        assert_eq!((code, err.as_str()), (0, ""), "{name}");
        assert!(out.starts_with("ok: "), "{name}: {out}");
    }
    // Without the prior the dictionary vector fails and says why.
    let archive = dir.join("zstd-dict.lpk");
    let (code, _, err) = tool(&["lpk-decode", "verify", archive.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.contains("is not available"), "{err}");
    // `info` lists the prior the archive needs.
    let (code, out, _) = tool(&["lpk-decode", "info", archive.to_str().unwrap()]);
    assert_eq!(code, 0);
    let id: String = prior_id(&read(DICT_FILE))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(out.contains("priors: 1"), "{out}");
    assert!(out.contains(&format!("prior: {id}")), "{out}");
}
