//! Revision 1.3's BWT stage through the Balanced pipeline (E2-15 task D, design
//! `docs/notes/e2-15-revision-1.3-design.md` section 6): text blocks are written `[lzma, bwt]`
//! and extract through the D-54 engine; the archive is byte-identical at one and eight blocks
//! in flight and sealed; an append onto a revision 1.2 archive runs without the stage and says
//! so; the D-67 plan counts the stage's terms only when a `Text` cluster is present.
#![allow(clippy::unwrap_used)]

use std::io::Cursor;
use std::path::Path;

use lpk_core::{
    extract_file, register_full_reader, BalancedOptions, DefaultPolicy, ExtractOptions,
    FastOptions, ModelStage, Pipeline, RunSummary,
};
use lpk_format::{
    Archive, Argon2Params, Credentials, EntryKind, PrimitiveId, Resources, SealOptions, Suite,
};

fn prose(seed: u64, len: usize) -> Vec<u8> {
    const WORDS: [&str; 16] = [
        "the", "king", "of", "France", "and", "his", "army", "marched", "over", "a", "long",
        "road", "toward", "the", "sea", "while",
    ];
    let mut s = 0x9E37_79B9_7F4A_7C15u64 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
    let mut out = Vec::with_capacity(len + 16);
    while out.len() < len {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        out.extend_from_slice(WORDS[(s % 16) as usize].as_bytes());
        out.push(if s & 0x700 == 0 { b'\n' } else { b' ' });
    }
    out.truncate(len);
    out
}

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut s = seed | 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

/// Text files of several megabytes (several blocks at the tests' block size) and one file of
/// noise, stored by the no-gain rule.
fn text_tree(root: &Path) {
    for i in 0..6u64 {
        std::fs::write(
            root.join(format!("chapter-{i}.txt")),
            prose(100 + i, 700_000),
        )
        .unwrap();
    }
    std::fs::write(root.join("noise.bin"), noise(7, 300_000)).unwrap();
    pin_mtimes(root);
}

/// A tree without a `Text` cluster.
fn binary_tree(root: &Path) {
    std::fs::write(root.join("noise.bin"), noise(8, 300_000)).unwrap();
    std::fs::write(root.join("more.bin"), noise(9, 300_000)).unwrap();
    pin_mtimes(root);
}

/// Pin every file's mtime (the trees are flat: a directory's last-write time was observed to
/// move on NTFS between two runs, which differed in the entry table).
fn pin_mtimes(root: &Path) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(1000);
    for p in walkdir(root).into_iter().filter(|p| p.is_file()) {
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(t).unwrap();
    }
}

fn balanced(threads: usize) -> Pipeline {
    Pipeline::balanced(BalancedOptions {
        block_size: 1 << 20,
        dict_size: 1 << 20,
        sample_len: 1 << 16,
        bwt_block: 1 << 18,
        threads: Some(threads),
        ..BalancedOptions::default()
    })
}

fn run(p: Pipeline, root: &Path) -> (Vec<u8>, RunSummary) {
    let mut out = Vec::new();
    let s = p.run(root, &mut out).unwrap();
    (out, s)
}

fn open(bytes: &[u8], credentials: Option<&Credentials>) -> Archive<Cursor<Vec<u8>>> {
    let mut a = Archive::open_with(
        Cursor::new(bytes.to_vec()),
        &Resources::default(),
        credentials,
    )
    .unwrap();
    register_full_reader(&mut a);
    a
}

/// The blocks whose graph names `bwt`: each is exactly `[lzma, bwt]`.
fn bwt_blocks(bytes: &[u8]) -> usize {
    let a = open(bytes, None);
    let mut n = 0;
    for b in a.index().blocks.clone() {
        let frame = &bytes[b.frame_offset as usize..(b.frame_offset + b.frame_len) as usize];
        let Some(lpk_format::frame::ReadFrame::Known(f)) =
            lpk_format::Frame::read(&mut Cursor::new(frame), &Default::default()).unwrap()
        else {
            panic!("not a block frame")
        };
        let steps = lpk_format::Graph::parse(&f.payload).unwrap().0.steps;
        if steps.iter().any(|s| s.primitive == PrimitiveId::Bwt) {
            assert_eq!(steps.len(), 2, "[lzma, bwt]");
            assert_eq!(steps[0].primitive, PrimitiveId::Lzma);
            assert_eq!(steps[1].primitive, PrimitiveId::Bwt);
            n += 1;
        }
    }
    n
}

/// Every file of `root` reads back through the full reader and `verify` is clean.
fn check(bytes: &[u8], root: &Path, credentials: Option<&Credentials>) {
    let mut a = open(bytes, credentials);
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut v = Vec::new();
        a.extract(e, &mut v).unwrap();
        assert!(
            v == std::fs::read(root.join(&e.path)).unwrap(),
            "{} differs",
            e.path
        );
    }
    a.verify().unwrap();
}

/// The D-54 engine extracts a tree written to `archive` into a fresh directory equal to `root`.
fn engine_extracts(archive: &Path, root: &Path, credentials: Option<&Credentials>) {
    let out = tempfile::tempdir().unwrap();
    let s = extract_file(
        archive,
        out.path(),
        &[],
        credentials,
        &DefaultPolicy,
        &ExtractOptions::default(),
    )
    .unwrap();
    assert_eq!(s.blocks_decoded, s.blocks_needed);
    let mut files = 0;
    for e in walkdir(root) {
        let rel = e.strip_prefix(root).unwrap();
        if e.is_file() {
            assert_eq!(
                std::fs::read(out.path().join(rel)).unwrap(),
                std::fs::read(&e).unwrap(),
                "{}",
                rel.display()
            );
            files += 1;
        }
    }
    assert_eq!(files, s.files);
}

fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p.clone());
            }
            out.push(p);
        }
    }
    out
}

#[test]
fn text_blocks_are_written_bwt_and_extract_through_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    text_tree(dir.path());
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("t.lpk");
    let s = balanced(1).run_file(dir.path(), &archive).unwrap();
    let b = s.balanced.unwrap();
    assert!(b.bwt_blocks >= 2, "{b:?}");
    assert!(b.bwt_sample_bytes > 0 && b.bwt_sample_bytes <= b.sample_bytes);
    let bytes = std::fs::read(&archive).unwrap();
    assert_eq!(bwt_blocks(&bytes) as u64, b.bwt_blocks);
    let a = open(&bytes, None);
    assert_eq!(a.header().version.minor, 3);
    let env = a.index().envelope;
    assert_eq!(env.max_bwt_block, 1 << 18, "B = min(bwt_block, plain_len)");
    assert!(env.decode_memory > env.max_block_plain);
    check(&bytes, dir.path(), None);
    engine_extracts(&archive, dir.path(), None);
    // A revision 1.2 reader (the format crate's own) reports the primitive on those blocks and
    // extracts the stored noise.
    let mut r = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let t = r.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut v = Vec::new();
        let r = r.extract(e, &mut v);
        if e.path == "noise.bin" {
            r.unwrap();
        } else {
            assert!(
                matches!(
                    r,
                    Err(lpk_format::FormatError::UnimplementedPrimitive { id: 3, .. })
                ),
                "{}: {r:?}",
                e.path
            );
        }
    }
}

#[test]
fn one_and_eight_threads_write_the_same_bytes_and_so_do_sealed_archives() {
    let dir = tempfile::tempdir().unwrap();
    text_tree(dir.path());
    // The control: the same tree with the stage off.
    let (c1, _) = run(balanced_no_bwt(), dir.path());
    let (c8, _) = run(
        {
            let mut p = balanced(8);
            if let ModelStage::Balanced(o) = &mut p.model {
                o.bwt = false;
            }
            p
        },
        dir.path(),
    );
    let at = c1.iter().zip(&c8).position(|(a, b)| a != b);
    assert!(
        c1 == c8,
        "the control archives differ at {at:?} of {}: {}",
        c1.len(),
        explain(&c1, &c8)
    );
    let (one, s1) = run(balanced(1), dir.path());
    let (eight, s8) = run(balanced(8), dir.path());
    assert_eq!(s1.balanced, s8.balanced);
    assert!(s1.balanced.as_ref().unwrap().bwt_blocks >= 2);
    if one != eight {
        let at = one.iter().zip(&eight).position(|(a, b)| a != b);
        let graphs = |bytes: &[u8]| -> Vec<(u64, Vec<PrimitiveId>)> {
            let a = open(bytes, None);
            a.index()
                .blocks
                .iter()
                .map(|b| {
                    let frame =
                        &bytes[b.frame_offset as usize..(b.frame_offset + b.frame_len) as usize];
                    let Some(lpk_format::frame::ReadFrame::Known(f)) =
                        lpk_format::Frame::read(&mut Cursor::new(frame), &Default::default())
                            .unwrap()
                    else {
                        panic!()
                    };
                    let steps = lpk_format::Graph::parse(&f.payload).unwrap().0.steps;
                    (b.frame_len, steps.iter().map(|s| s.primitive).collect())
                })
                .collect()
        };
        panic!(
            "the archives differ at {at:?} (lengths {} and {}); envelopes {:?} / {:?}; blocks at 1: {:?}; at 8: {:?}; summaries {:?} / {:?}",
            one.len(),
            eight.len(),
            open(&one, None).index().envelope,
            open(&eight, None).index().envelope,
            graphs(&one),
            graphs(&eight),
            s1.balanced,
            s8.balanced
        );
    }
    // Sealed, through the engine.
    let credentials = Credentials::password("correct horse battery staple");
    let work = tempfile::tempdir().unwrap();
    let mut archives = Vec::new();
    for threads in [1usize, 8] {
        let mut p = balanced(threads);
        p.seal.writer.seal = Some(SealOptions {
            suite: Suite::AesGcm,
            argon2: Argon2Params {
                t: 1,
                m_kib: 8192,
                p: 1,
            },
            listable: false,
            credentials: credentials.clone(),
        });
        let archive = work.path().join(format!("s{threads}.lpk"));
        let s = p.run_file(dir.path(), &archive).unwrap();
        assert!(s.balanced.unwrap().bwt_blocks >= 2);
        let bytes = std::fs::read(&archive).unwrap();
        check(&bytes, dir.path(), Some(&credentials));
        engine_extracts(&archive, dir.path(), Some(&credentials));
        archives.push(bytes);
    }
    // Sealed archives carry a random salt, so only their lengths compare.
    assert_eq!(archives[0].len(), archives[1].len());
}

#[test]
fn an_append_onto_a_revision_1_2_archive_runs_without_the_stage_and_says_so() {
    let v1 = tempfile::tempdir().unwrap();
    std::fs::write(v1.path().join("readme.txt"), prose(1, 20_000)).unwrap();
    let work = tempfile::tempdir().unwrap();
    for (name, first, minor, dropped) in [
        ("1.2", balanced_no_bwt(), 2, vec!["bwt"]),
        (
            "1.1",
            Pipeline::fast(FastOptions {
                threads: Some(1),
                ..FastOptions::default()
            }),
            1,
            vec!["bwt", "deflate"],
        ),
        ("1.3", balanced(1), 3, vec![]),
    ] {
        let archive = work.path().join(format!("{name}.lpk"));
        first.run_file(v1.path(), &archive).unwrap();
        let v2 = tempfile::tempdir().unwrap();
        text_tree(v2.path());
        let s = balanced(1).append_file(v2.path(), &archive, None).unwrap();
        assert_eq!(s.peel.dropped_stages, dropped, "{name}");
        let b = s.balanced.unwrap();
        let bytes = std::fs::read(&archive).unwrap();
        assert_eq!(open(&bytes, None).header().version.minor, minor, "{name}");
        if dropped.contains(&"bwt") {
            assert_eq!((b.bwt_blocks, b.bwt_sample_bytes), (0, 0), "{name}: {b:?}");
            assert_eq!(bwt_blocks(&bytes), 0, "{name}");
            assert!(b.lzma_blocks + b.zstd_blocks >= 2, "{name}: {b:?}");
        } else {
            assert!(b.bwt_blocks >= 2, "{name}: {b:?}");
        }
        check(&bytes, v2.path(), None);
        engine_extracts(&archive, v2.path(), None);
    }
}

/// What differs between two archives of the same tree (a diagnostic for the identity tests).
fn explain(a: &[u8], b: &[u8]) -> String {
    let (mut x, mut y) = (open(a, None), open(b, None));
    let (ix, iy) = (x.index().clone(), y.index().clone());
    let mut out = Vec::new();
    if ix.chunk_table != iy.chunk_table {
        out.push("chunk table".to_string());
    }
    if ix.merkle_root != iy.merkle_root {
        out.push("merkle root".to_string());
    }
    if ix.blocks != iy.blocks {
        out.push(format!("blocks {:?} / {:?}", ix.blocks, iy.blocks));
    }
    if ix.entry_table != iy.entry_table {
        out.push(format!(
            "entry table at {:?} / {:?}",
            ix.entry_table, iy.entry_table
        ));
    }
    if ix.entry_table_hash != iy.entry_table_hash {
        out.push("entry table hash".to_string());
    }
    if ix.generations != iy.generations {
        out.push(format!(
            "generations {:?} / {:?}",
            ix.generations, iy.generations
        ));
    }
    let entries = |a: &mut Archive<Cursor<Vec<u8>>>| -> Vec<String> {
        let t = a.entry_table().unwrap();
        t.table()
            .unwrap()
            .iter()
            .map(|e| format!("{:?}", e.unwrap()))
            .collect()
    };
    let (ex, ey) = (entries(&mut x), entries(&mut y));
    for (p, q) in ex.iter().zip(&ey) {
        if p != q {
            out.push(format!("entry {p} / {q}"));
        }
    }
    if ex.len() != ey.len() {
        out.push(format!("entry counts {} / {}", ex.len(), ey.len()));
    }
    out.join("; ")
}

fn balanced_no_bwt() -> Pipeline {
    let mut p = balanced(1);
    if let ModelStage::Balanced(o) = &mut p.model {
        o.bwt = false;
    }
    p
}

/// The D-67 plan with and without a `Text` cluster (the stage's terms enter only with one),
/// and with the stage off; the blocks in flight and the terms are printed.
#[test]
fn the_plan_counts_the_stage_only_with_a_text_cluster() {
    let text = tempfile::tempdir().unwrap();
    text_tree(text.path());
    let binary = tempfile::tempdir().unwrap();
    binary_tree(binary.path());
    let plan = |p: Pipeline, root: &Path| run(p, root).1.memory;
    let with_text = plan(balanced(8), text.path());
    let without_text = plan(balanced(8), binary.path());
    let stage_off = plan(
        {
            let mut p = balanced(8);
            if let ModelStage::Balanced(o) = &mut p.model {
                o.bwt = false;
            }
            p
        },
        text.path(),
    );
    for (name, m) in [
        ("Text cluster, stage on", &with_text),
        ("no Text cluster, stage on", &without_text),
        ("Text cluster, stage off", &stage_off),
    ] {
        println!(
            "{name}: budget {} ({}), per block {}, fixed {}, blocks in flight {}",
            m.budget.bytes,
            m.budget.source.name(),
            m.per_block,
            m.fixed(),
            m.blocks_in_flight
        );
    }
    assert!(with_text.per_block > without_text.per_block);
    assert_eq!(without_text.per_block, stage_off.per_block);
    assert!(with_text.blocks_in_flight >= 1);
}
