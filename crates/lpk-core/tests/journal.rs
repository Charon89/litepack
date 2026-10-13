//! The journal through the pipeline (E2-14): a tree's versions as appended generations, each a
//! snapshot of the directory; the appended archive next to the one-shot archive of all the
//! versions; rollback to an earlier generation; a half-written append recovered; a failed
//! append leaving the archive at its old generation; credentials; cross-generation dedup
//! counted.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use lpk_core::{
    extract_file, CoreError, DefaultPolicy, ExtractOptions, FastOptions, FoldStage, Pipeline,
    RunSummary,
};
use lpk_format::{
    Archive, Argon2Params, Chunker, Credentials, FormatError, Resources, SealOptions, Suite,
    WriterOptions, TRAILER_FRAME_LEN,
};

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

fn prose(seed: u64, len: usize) -> Vec<u8> {
    const WORDS: [&str; 8] = ["the", "king", "of", "France", "and", "army", "sea", "road"];
    let mut out = Vec::new();
    let r = noise(seed, len);
    for b in r {
        out.extend_from_slice(WORDS[(b % 8) as usize].as_bytes());
        out.push(b' ');
        if out.len() >= len {
            break;
        }
    }
    out.truncate(len);
    out
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

/// Version 1: prose, noise (one file duplicated), an empty file, a nested directory, a JPEG.
fn make_v1(root: &Path) {
    std::fs::create_dir_all(root.join("docs/deep")).unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("a.txt"), prose(1, 300_000)).unwrap();
    std::fs::write(root.join("docs/b.md"), prose(2, 120_000)).unwrap();
    let big = noise(7, 400_000);
    std::fs::write(root.join("docs/deep/r.bin"), &big).unwrap();
    std::fs::write(root.join("docs/deep/r-again.bin"), &big).unwrap();
    std::fs::write(root.join("gone-in-v2.bin"), noise(9, 50_000)).unwrap();
    std::fs::write(root.join("zero"), b"").unwrap();
    std::fs::write(root.join("p.jpg"), fixture("baseline.jpg")).unwrap();
}

/// Version 2: one file deleted (and the directory `empty` too), one file changed in its tail
/// (its first chunks stay), one new file in a new directory, the rest unchanged.
///
/// The bytes that change between versions are noise, so the comparison with the one-shot
/// archive measures the dedup and the frames, not the coder: a changed chunk of compressible
/// content costs more in an appended generation, whose zstd window holds nothing of the earlier
/// generations (the corpus test shows that case).
fn make_v2(root: &Path) {
    make_v1(root);
    std::fs::remove_file(root.join("gone-in-v2.bin")).unwrap();
    std::fs::remove_dir(root.join("empty")).unwrap();
    let mut r = noise(7, 300_000);
    r.extend_from_slice(&noise(11, 100_000));
    std::fs::write(root.join("docs/deep/r.bin"), r).unwrap();
    std::fs::create_dir_all(root.join("new")).unwrap();
    std::fs::write(root.join("new/c.bin"), noise(3, 80_000)).unwrap();
}

/// Version 3: the new file grown, another file deleted, a second copy of the JPEG.
fn make_v3(root: &Path) {
    make_v2(root);
    std::fs::remove_file(root.join("docs/deep/r-again.bin")).unwrap();
    let mut c = noise(3, 80_000);
    c.extend_from_slice(&noise(12, 30_000));
    std::fs::write(root.join("new/c.bin"), c).unwrap();
    std::fs::write(root.join("p-copy.jpg"), fixture("baseline.jpg")).unwrap();
}

/// Relative path -> BLAKE3 of every file under `root` (directories as `None`).
fn tree(root: &Path) -> BTreeMap<String, Option<[u8; 32]>> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, Option<[u8; 32]>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                out.insert(rel, None);
                walk(&p, base, out);
            } else {
                out.insert(
                    rel,
                    Some(*blake3::hash(&std::fs::read(&p).unwrap()).as_bytes()),
                );
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, root, &mut m);
    m
}

/// The three versions as sibling directories `v1`, `v2`, `v3` under one root.
fn versions() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    make_v1(&d.path().join("v1"));
    make_v2(&d.path().join("v2"));
    make_v3(&d.path().join("v3"));
    d
}

fn fast() -> Pipeline {
    Pipeline::fast(FastOptions::default())
}

fn open(path: &Path) -> Archive<File> {
    Archive::open(File::open(path).unwrap(), &Resources::default()).unwrap()
}

fn entry_paths(a: &mut Archive<File>) -> Vec<String> {
    a.entry_table()
        .unwrap()
        .table()
        .unwrap()
        .iter()
        .map(|e| e.unwrap().path)
        .collect()
}

fn extract_to(archive: &Path, dir: &Path, credentials: Option<&Credentials>) {
    extract_file(
        archive,
        dir,
        &[],
        credentials,
        &DefaultPolicy,
        &ExtractOptions::default(),
    )
    .unwrap();
}

fn open_rw(path: &Path) -> File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

/// Generation 0 from `v1`, then appends of `v2` and `v3`: the summaries, in order.
fn three_generations(root: &Path, archive: &Path) -> Vec<RunSummary> {
    let mut out = vec![fast().run_file(&root.join("v1"), archive).unwrap()];
    for v in ["v2", "v3"] {
        out.push(fast().append_file(&root.join(v), archive, None).unwrap());
    }
    out
}

/// The frames a generation adds on top of its data, as the spec sizes them: its index frame,
/// its trailer frame, its entry table frame, and its `Records` frame when it wrote one. Read
/// from a copy of `archive` rolled back to `generation`.
fn generation_overhead(archive: &Path, generation: u64, scratch: &Path) -> u64 {
    let copy = scratch.join(format!("gen-{generation}.lpk"));
    std::fs::copy(archive, &copy).unwrap();
    let mut f = open_rw(&copy);
    let g = lpk_format::rollback(&mut f, generation).unwrap();
    assert_eq!(g.generation, generation);
    drop(f);
    let a = open(&copy);
    assert_eq!(a.generation(), generation);
    let t = *a.trailer();
    let index = a.index();
    let start = index
        .generations
        .iter()
        .find(|g| g.generation == generation)
        .unwrap()
        .start_offset;
    let mut n = t.index_len + TRAILER_FRAME_LEN + index.entry_table.len;
    assert!(index.entry_table.offset >= start);
    if let Some(r) = index.records {
        if r.offset >= start {
            n += r.len;
        }
    }
    n
}

/// The chunk table's length and the plain bytes of every block of `archive`.
fn chunks_and_plain(archive: &Path) -> (u64, u64) {
    let a = open(archive);
    (
        a.chunks().len(),
        a.index().blocks.iter().map(|b| b.plain_len).sum(),
    )
}

#[test]
fn appends_are_snapshots_within_the_frame_overhead_of_the_one_shot() {
    let v = versions();
    let out = tempfile::tempdir().unwrap();
    let appended = out.path().join("appended.lpk");
    let s = three_generations(v.path(), &appended);
    assert_eq!(s[0].writer.generation, 0);
    assert_eq!(s[1].writer.generation, 1);
    assert_eq!(s[2].writer.generation, 2);
    assert_eq!(s[0].writer.reused_chunks, 0);
    // Cross-generation dedup: the unchanged files of v2 and v3 reference old chunks.
    assert!(s[1].writer.reused_chunks > 0, "{:?}", s[1].writer);
    assert!(s[2].writer.reused_chunks > 0, "{:?}", s[2].writer);
    assert!(s[1].writer.new_chunks > 0 && s[2].writer.new_chunks > 0);
    // The peel ran in the appended generations: v2's unchanged `p.jpg` was deduplicated whole
    // against the old chunks and never peeled; in v3 its new copy too.
    assert_eq!(s[0].peel.peeled.files, 1, "{:?}", s[0].peel);
    assert_eq!(s[1].peel.deduplicated.files, 1, "{:?}", s[1].peel);
    assert_eq!(s[2].peel.deduplicated.files, 2, "{:?}", s[2].peel);

    // Snapshot semantics: the entry table is v3's tree, nothing of v1 or v2 that v3 lacks.
    let mut a = open(&appended);
    assert_eq!(a.generation(), 2);
    assert_eq!(a.history().unwrap().len(), 3);
    let paths = entry_paths(&mut a);
    let expected: Vec<String> = tree(&v.path().join("v3")).into_keys().collect();
    assert_eq!(paths, expected);
    drop(a);
    let x3 = out.path().join("x3");
    extract_to(&appended, &x3, None);
    assert_eq!(tree(&x3), tree(&v.path().join("v3")));

    // The one-shot archive of the three versions side by side, dedup on.
    let one_shot = out.path().join("one-shot.lpk");
    let o = fast().run_file(v.path(), &one_shot).unwrap();
    assert_eq!(o.writer.generation, 0);
    let appended_len = std::fs::metadata(&appended).unwrap().len();
    let one_shot_len = std::fs::metadata(&one_shot).unwrap().len();
    assert_eq!(appended_len, s[2].writer.archive_len);
    assert_eq!(one_shot_len, o.writer.archive_len);
    // The dedup across generations found what the one-shot's found: the same chunk table
    // size and the same plain bytes in blocks.
    let (ac, ap) = chunks_and_plain(&appended);
    let (oc, op) = chunks_and_plain(&one_shot);
    assert_eq!((ac, ap), (oc, op));
    let overhead: u64 = (1..=2)
        .map(|g| generation_overhead(&appended, g, out.path()))
        .sum();
    eprintln!(
        "appended {appended_len} bytes, one-shot {one_shot_len} bytes, overhead of generations 1 and 2: {overhead} bytes"
    );
    assert!(
        appended_len <= one_shot_len + overhead,
        "appended {appended_len} > one-shot {one_shot_len} + overhead {overhead}"
    );
}

#[test]
fn rollback_to_generation_0_reproduces_v1_bit_exactly() {
    let v = versions();
    let out = tempfile::tempdir().unwrap();
    let archive = out.path().join("a.lpk");
    three_generations(v.path(), &archive);
    let mut f = open_rw(&archive);
    // Above the latest: refused, nothing changed.
    assert!(matches!(
        lpk_format::rollback(&mut f, 3),
        Err(FormatError::NoSuchGeneration { .. })
    ));
    let g = lpk_format::rollback(&mut f, 0).unwrap();
    assert_eq!(g.generation, 0);
    drop(f);
    assert_eq!(std::fs::metadata(&archive).unwrap().len(), g.end());
    let mut a = open(&archive);
    assert_eq!(a.generation(), 0);
    assert_eq!(a.history().unwrap().len(), 1);
    let paths = entry_paths(&mut a);
    assert!(paths.iter().any(|p| p == "gone-in-v2.bin"));
    assert!(!paths.iter().any(|p| p == "new/c.txt"));
    drop(a);
    let x = out.path().join("x1");
    extract_to(&archive, &x, None);
    assert_eq!(tree(&x), tree(&v.path().join("v1")));
}

#[test]
fn a_half_written_append_is_recovered_by_rollback() {
    let v = versions();
    let out = tempfile::tempdir().unwrap();
    let archive = out.path().join("a.lpk");
    let s = three_generations(v.path(), &archive);
    let end1 = s[1].writer.archive_len;
    let end2 = s[2].writer.archive_len;
    assert!(end1 < end2);
    // Cut the third generation short: between the old trailer and the new one.
    let cut = end1 + (end2 - end1) / 2;
    let f = open_rw(&archive);
    f.set_len(cut).unwrap();
    drop(f);
    assert!(Archive::open(File::open(&archive).unwrap(), &Resources::default()).is_err());
    let mut f = open_rw(&archive);
    let g = lpk_format::rollback(&mut f, 1).unwrap();
    assert_eq!(g.generation, 1);
    drop(f);
    assert_eq!(std::fs::metadata(&archive).unwrap().len(), end1);
    let mut a = open(&archive);
    assert_eq!(a.generation(), 1);
    assert_eq!(a.history().unwrap().len(), 2);
    let paths = entry_paths(&mut a);
    let expected: Vec<String> = tree(&v.path().join("v2")).into_keys().collect();
    assert_eq!(paths, expected);
    drop(a);
    let x = out.path().join("x2");
    extract_to(&archive, &x, None);
    assert_eq!(tree(&x), tree(&v.path().join("v2")));
    // And the archive appends again from there.
    let s3 = fast()
        .append_file(&v.path().join("v3"), &archive, None)
        .unwrap();
    assert_eq!(s3.writer.generation, 2);
    let x = out.path().join("x3");
    extract_to(&archive, &x, None);
    assert_eq!(tree(&x), tree(&v.path().join("v3")));
}

/// A fold stage whose chunker cuts fixed pieces until it has seen `fail_after` bytes, then
/// returns an invalid cut: the writer fails part-way through the generation.
#[derive(Debug)]
struct FailingFold {
    fail_after: usize,
}

struct FailingChunker {
    size: usize,
    fed: usize,
    fail_after: usize,
}

impl Chunker for FailingChunker {
    fn feed(&mut self, bytes: &[u8], eof: bool) -> Vec<usize> {
        self.fed += bytes.len();
        if self.fed > self.fail_after {
            return vec![0];
        }
        let mut cuts: Vec<usize> = (1..=bytes.len() / self.size)
            .map(|i| i * self.size)
            .collect();
        if eof && cuts.last().copied().unwrap_or(0) < bytes.len() {
            cuts.push(bytes.len());
        }
        cuts
    }

    fn reset(&mut self) {}
}

impl FoldStage for FailingFold {
    fn name(&self) -> &'static str {
        "failing"
    }

    fn install(&self, options: &mut WriterOptions) -> Result<Box<dyn Chunker>, CoreError> {
        options.chunk_size = 65_536;
        options.dedup = true;
        Ok(Box::new(FailingChunker {
            size: 65_536,
            fed: 0,
            fail_after: self.fail_after,
        }))
    }
}

#[test]
fn a_failed_append_leaves_the_archive_at_its_old_generation() {
    let v = versions();
    let out = tempfile::tempdir().unwrap();
    let archive = out.path().join("a.lpk");
    let s0 = fast().run_file(&v.path().join("v1"), &archive).unwrap();
    let before = std::fs::read(&archive).unwrap();
    assert_eq!(before.len() as u64, s0.writer.archive_len);
    let mut p = fast();
    p.fold = Some(Box::new(FailingFold {
        fail_after: 100_000,
    }));
    let e = p
        .append_file(&v.path().join("v2"), &archive, None)
        .unwrap_err();
    assert!(
        matches!(e, CoreError::Format(FormatError::BadChunk { .. })),
        "{e:?}"
    );
    // Truncated back: the bytes are the old archive's, nothing past the old trailer.
    assert_eq!(std::fs::read(&archive).unwrap(), before);
    let mut a = open(&archive);
    assert_eq!(a.generation(), 0);
    assert_eq!(a.history().unwrap().len(), 1);
    drop(a);
    // A sound append goes through afterwards.
    let s1 = fast()
        .append_file(&v.path().join("v2"), &archive, None)
        .unwrap();
    assert_eq!(s1.writer.generation, 1);
    let x = out.path().join("x2");
    extract_to(&archive, &x, None);
    assert_eq!(tree(&x), tree(&v.path().join("v2")));
}

#[test]
fn an_encrypted_archive_appends_with_its_credentials_only() {
    let v = versions();
    let out = tempfile::tempdir().unwrap();
    let archive = out.path().join("a.lpk");
    let creds = Credentials {
        password: b"journal test password".to_vec(),
        keyfile: None,
    };
    let wrong = Credentials {
        password: b"not it".to_vec(),
        keyfile: None,
    };
    let mut p = fast();
    p.seal.writer.seal = Some(SealOptions {
        suite: Suite::AesGcm,
        argon2: Argon2Params {
            t: 1,
            m_kib: 8192,
            p: 1,
        },
        listable: false,
        credentials: creds.clone(),
    });
    let s0 = p.run_file(&v.path().join("v1"), &archive).unwrap();
    let before = std::fs::read(&archive).unwrap();
    assert_eq!(before.len() as u64, s0.writer.archive_len);
    // Keyless: refused before the file is opened for writing.
    let e = fast()
        .append_file(&v.path().join("v2"), &archive, None)
        .unwrap_err();
    assert!(
        matches!(e, CoreError::Format(FormatError::AppendNeedsCredentials)),
        "{e:?}"
    );
    assert_eq!(std::fs::read(&archive).unwrap(), before);
    // A wrong password: refused at the key slot.
    let e = fast()
        .append_file(&v.path().join("v2"), &archive, Some(&wrong))
        .unwrap_err();
    assert!(
        matches!(e, CoreError::Format(FormatError::WrongKey)),
        "{e:?}"
    );
    assert_eq!(std::fs::read(&archive).unwrap(), before);
    // The right one: the generation is sealed with the archive's key.
    let s1 = fast()
        .append_file(&v.path().join("v2"), &archive, Some(&creds))
        .unwrap();
    assert_eq!(s1.writer.generation, 1);
    assert!(s1.writer.reused_chunks > 0);
    let x = out.path().join("x2");
    assert!(matches!(
        extract_file(
            &archive,
            &x,
            &[],
            None,
            &DefaultPolicy,
            &ExtractOptions::default()
        ),
        Err(CoreError::Format(FormatError::PasswordRequired))
    ));
    extract_to(&archive, &x, Some(&creds));
    assert_eq!(tree(&x), tree(&v.path().join("v2")));
    // Rollback needs no key.
    let mut f = open_rw(&archive);
    lpk_format::rollback(&mut f, 0).unwrap();
    drop(f);
    assert_eq!(std::fs::read(&archive).unwrap(), before);
    let x = out.path().join("x1");
    extract_to(&archive, &x, Some(&creds));
    assert_eq!(tree(&x), tree(&v.path().join("v1")));
}

#[test]
fn the_archive_inside_its_root_is_left_out_of_the_append() {
    let v = versions();
    let root = v.path().join("v1");
    let archive = root.join("self.lpk");
    fast().run_file(&root, &archive).unwrap();
    let s = fast().append_file(&root, &archive, None).unwrap();
    assert_eq!(s.writer.generation, 1);
    let mut a = open(&archive);
    let paths = entry_paths(&mut a);
    assert!(!paths.iter().any(|p| p == "self.lpk"), "{paths:?}");
    // Everything unchanged: every chunk reused, none new.
    assert_eq!(s.writer.new_chunks, 0, "{:?}", s.writer);
}
