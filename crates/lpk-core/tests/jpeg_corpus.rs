//! The JPEG peel on the corpus's photo classes, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test jpeg_corpus -- --ignored --nocapture`
//!
//! Each of `photo-jpeg` and `photo-jpeg-edited` goes through `archive_fast` (with the JPEG peel),
//! is extracted through the full reader and compared file by file, and verified. Prints the
//! peeled and fallback counts by cause and the sizes (a report only, no figure is asserted).
#![allow(clippy::unwrap_used)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use lpk_core::{archive_fast, register_full_reader, Cause, FastOptions, PeelStage};
use lpk_format::{Archive, EntryKind, Resources};

fn join(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in rel.split('/') {
        p.push(c);
    }
    p
}

#[test]
#[ignore = "needs LPK_CORPUS; run in release by hand"]
fn photo_classes_round_trip_with_the_jpeg_peel() {
    let root = PathBuf::from(std::env::var("LPK_CORPUS").expect("set LPK_CORPUS"));
    for class in ["photo-jpeg", "photo-jpeg-edited"] {
        let dir = root.join(class);
        let mut bytes = Vec::new();
        let t = std::time::Instant::now();
        let (ws, fast) = archive_fast(&dir, &mut bytes, FastOptions::default()).unwrap();
        let secs = t.elapsed().as_secs_f64();
        let mut a = Archive::open(Cursor::new(&bytes[..]), &Resources::default()).unwrap();
        register_full_reader(&mut a);
        let table = a.entry_table().unwrap();
        let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
        let (mut files, mut raw) = (0u64, 0u64);
        for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
            let mut got = Vec::new();
            a.extract(e, &mut got).unwrap();
            let want = std::fs::read(join(&dir, &e.path)).unwrap();
            assert!(got == want, "{class}: {} differs", e.path);
            files += 1;
            raw += want.len() as u64;
        }
        a.verify().unwrap();
        let p = fast.peel;
        println!(
            "{class}: {files} files, {raw} bytes raw, archive {} bytes ({} entries, {} blocks), \
             {secs:.1} s, header minor {}",
            ws.archive_len,
            ws.entries,
            ws.blocks,
            a.header().version.minor
        );
        println!(
            "  peeled {} files ({} bytes in, {} bytes out); stored as-is {} files ({} bytes); \
             mismatches 0",
            p.peeled.files,
            p.peeled.bytes,
            p.peeled_output_bytes,
            p.fallback_total().files,
            p.fallback_total().bytes
        );
        for c in Cause::ALL {
            let n = p.fallback(c);
            if n.files > 0 {
                println!(
                    "  as-is, {}: {} files, {} bytes",
                    c.label(),
                    n.files,
                    n.bytes
                );
            }
        }
        // Which peeled files declare more than revision 1.1's term (the library's need is
        // larger); a report only.
        let mut more = Vec::new();
        for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
            let data = std::fs::read(join(&dir, &e.path)).unwrap();
            let Ok(plan) = lpk_core::JpegPeel::default().peel(&data, 1 << 26) else {
                continue;
            };
            let h =
                lpk_core::peel::jpeg::read_stream_header(&plan.stream, plan.primary_len).unwrap();
            let old = lpk_core::peel::jpeg::memory_bound(&h.frame);
            if plan.memory > old {
                more.push(format!(
                    "{} ({}x{}, SOF {:02X}: {} > {old})",
                    e.path, h.frame.width, h.frame.height, h.frame.marker, plan.memory
                ));
            }
        }
        println!(
            "  declaring more than revision 1.1's term: {} files",
            more.len()
        );
        for m in &more {
            println!("    {m}");
        }
    }
}

/// Design `docs/notes/e2-10-peel-pool-design.md` task 4: every `photo-jpeg` file's Lepton
/// stream is the same at one and at eight processor threads (the pool's workers use one).
#[test]
#[ignore = "needs LPK_CORPUS; run in release by hand"]
fn lepton_streams_are_identical_at_one_and_eight_processor_threads() {
    use lpk_core::{JpegPeel, PeelStage};
    let root = PathBuf::from(std::env::var("LPK_CORPUS").expect("set LPK_CORPUS"));
    let one = JpegPeel {
        processor_threads: 1,
        ..JpegPeel::default()
    };
    let eight = JpegPeel::default();
    let (mut files, mut peeled) = (0u64, 0u64);
    let mut stack = vec![root.join("photo-jpeg")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let data = std::fs::read(&p).unwrap();
            files += 1;
            match (one.peel(&data, 1 << 26), eight.peel(&data, 1 << 26)) {
                (Ok(a), Ok(b)) => {
                    assert!(a.stream == b.stream, "{}", p.display());
                    peeled += 1;
                }
                (Err(a), Err(b)) => assert_eq!(a, b, "{}", p.display()),
                _ => panic!("{}: one peeled, the other not", p.display()),
            }
        }
    }
    println!("photo-jpeg: {files} files, {peeled} peeled, every stream identical");
    assert!(peeled > 0);
}
