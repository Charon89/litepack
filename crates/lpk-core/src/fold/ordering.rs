//! File order inside a cluster (E2-8, D-12: files are ordered, chunks are not).
//!
//! Three orders: [`Ordering::None`] keeps the path order of the clustering; [`Ordering::Extension`]
//! sorts by extension (lower-cased), then file name, then path, the order 7-Zip uses for solid
//! archives; [`Ordering::Similarity`] starts from the extension order and puts near-duplicate
//! files next to each other.
//!
//! The similarity feature is computed from the file's chunk hashes: the Fold chunker cuts the
//! file, BLAKE3 hashes each chunk (the same chunks the writer stores), and the first eight bytes
//! of each digest enter a min-hash sketch. The sketch keeps the smallest value per seed over
//! [`SKETCH_HASHES`] seeds; each super-feature is a hash of [`HASHES_PER_FEATURE`] consecutive
//! minima, [`SUPER_FEATURES`] in all. Grouping several minima makes a match need a high chunk
//! overlap (few false pairs); several super-features give a near-duplicate several chances to
//! match (few missed pairs). Two files that share one super-feature value are one group; the
//! group is written together, largest first, at the place of its first member in extension
//! order. A file with fewer than [`MIN_CHUNKS`] chunks is not sketched: a sketch of one or two
//! chunks is the file's whole hash set, and such small files sort by extension only.
//!
//! The pass reads and chunks each sketched candidate once more than the writer does. Chunk
//! hashes are held per file (one 64-bit minimum per seed is all that is kept, the chunk hashes
//! themselves are dropped as they are made), the sketches of one cluster live until the cluster
//! is ordered. Ordering never changes what deduplicates: that is content-addressed in the writer.

use std::cmp;
use std::collections::HashMap;
use std::io::Read;
use std::time::{Duration, Instant};

use lpk_format::Chunker;

use super::chunker::MIN_CHUNK;
use crate::error::CoreError;
use crate::ingest::Input;
use crate::source::Source;

/// Min-hash seeds per file. Twelve is three super-features of four.
pub const SKETCH_HASHES: usize = 12;
/// Super-features per file.
pub const SUPER_FEATURES: usize = 3;
/// Minima hashed together into one super-feature.
pub const HASHES_PER_FEATURE: usize = SKETCH_HASHES / SUPER_FEATURES;
/// Fewest chunks a file needs to be sketched; below it the file sorts by extension only.
pub const MIN_CHUNKS: usize = 4;
/// Files shorter than this cannot hold [`MIN_CHUNKS`] chunks of the smallest size and are not read.
pub const MIN_SKETCH_LEN: u64 = (MIN_CHUNKS * MIN_CHUNK) as u64;
/// The bytes read per call while sketching.
const SEGMENT: u64 = 1 << 20;

/// How files are ordered inside a cluster.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Ordering {
    /// Path order (what the clustering gives).
    None,
    /// Extension (lower-cased), then name, then path.
    Extension,
    /// The extension order with near-duplicate files adjacent.
    #[default]
    Similarity,
}

impl Ordering {
    /// The name used on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Ordering::None => "none",
            Ordering::Extension => "extension",
            Ordering::Similarity => "similarity",
        }
    }
}

/// What the similarity pass did and cost.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OrderingSummary {
    /// Files that got a sketch (enough chunks).
    pub files_sketched: u64,
    /// Bytes the pass read (every candidate, sketched or not).
    pub bytes_read: u64,
    /// Groups of two or more near-duplicate files placed together.
    pub groups: u64,
    /// Wall time of the whole ordering step.
    pub seconds: f64,
}

/// The super-features of one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sketch {
    /// One value per super-feature.
    pub features: [u64; SUPER_FEATURES],
}

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

struct SketchBuilder {
    mins: [u64; SKETCH_HASHES],
    chunks: usize,
}

impl SketchBuilder {
    fn new() -> Self {
        SketchBuilder {
            mins: [u64::MAX; SKETCH_HASHES],
            chunks: 0,
        }
    }

    fn add(&mut self, digest: &[u8; 32]) {
        let mut b = [0u8; 8];
        b.copy_from_slice(&digest[..8]);
        let h = u64::from_le_bytes(b);
        for (i, m) in self.mins.iter_mut().enumerate() {
            let v = mix(h ^ mix(i as u64 + 1));
            if v < *m {
                *m = v;
            }
        }
        self.chunks += 1;
    }

    fn finish(self) -> Option<Sketch> {
        if self.chunks < MIN_CHUNKS {
            return None;
        }
        let mut features = [0u64; SUPER_FEATURES];
        for (j, f) in features.iter_mut().enumerate() {
            let mut acc = mix(j as u64 + 0x5EED);
            for m in &self.mins[j * HASHES_PER_FEATURE..(j + 1) * HASHES_PER_FEATURE] {
                acc = mix(acc ^ m);
            }
            *f = acc;
        }
        Some(Sketch { features })
    }
}

/// The sketch of the stream `reader`, cut by `chunker`; `None` when it has fewer than
/// [`MIN_CHUNKS`] chunks or the chunker's cuts are not usable. `bytes_read` grows by what was read.
pub fn sketch_stream(
    reader: &mut dyn Read,
    chunker: &mut dyn Chunker,
    bytes_read: &mut u64,
) -> std::io::Result<Option<Sketch>> {
    chunker.reset();
    let mut b = SketchBuilder::new();
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let before = buf.len();
        let n = Read::take(&mut *reader, SEGMENT).read_to_end(&mut buf)?;
        *bytes_read += n as u64;
        let eof = (n as u64) < SEGMENT;
        let cuts = chunker.feed(&buf[before..], eof);
        let mut start = 0usize;
        for cut in cuts {
            if cut <= start || cut > buf.len() {
                return Ok(None);
            }
            b.add(blake3::hash(&buf[start..cut]).as_bytes());
            start = cut;
        }
        buf.drain(..start);
        if eof {
            return Ok(b.finish());
        }
    }
}

/// Sort by extension (lower-cased; none sorts first), then file name (lower-cased), then path.
pub fn extension_order(inputs: &mut [Input]) {
    inputs.sort_by_cached_key(|i| {
        let name = i.path.rsplit('/').next().unwrap_or(&i.path).to_string();
        let ext = match name.rsplit_once('.') {
            Some((stem, e)) if !stem.is_empty() => e.to_ascii_lowercase(),
            _ => String::new(),
        };
        (ext, name.to_lowercase(), i.path.clone())
    });
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

/// Place files whose sketches share a super-feature next to each other. `inputs` is in the base
/// (extension) order and keeps it for files without a match; a group goes where its first member
/// stood, largest file first (then base order). Returns the new order and the group count.
pub fn similarity_order(inputs: Vec<Input>, sketches: &[Option<Sketch>]) -> (Vec<Input>, u64) {
    let n = inputs.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut seen: HashMap<u64, usize> = HashMap::new();
    for (i, s) in sketches.iter().enumerate() {
        let Some(s) = s else { continue };
        for f in s.features {
            match seen.get(&f) {
                Some(&j) => {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    if a != b {
                        parent[a.max(b)] = a.min(b);
                    }
                }
                None => {
                    seen.insert(f, i);
                }
            }
        }
    }
    let roots: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, r) in roots.iter().enumerate() {
        members.entry(*r).or_default().push(i);
    }
    let mut slots: Vec<Option<Input>> = inputs.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(n);
    let mut groups = 0u64;
    for i in 0..n {
        let Some(m) = members.get_mut(&roots[i]) else {
            continue;
        };
        if m.is_empty() {
            continue; // already placed with its group
        }
        if m.len() == 1 {
            out.extend(slots[i].take());
            continue;
        }
        groups += 1;
        let mut m = std::mem::take(m);
        m.sort_by_key(|&k| (cmp::Reverse(slots[k].as_ref().map_or(0, |x| x.len)), k));
        for k in m {
            out.extend(slots[k].take());
        }
    }
    (out, groups)
}

/// Order one cluster's files. `sketch` says whether the similarity pass may read them (it is
/// false for clusters that are stored as they are or peeled).
pub fn order_cluster(
    mut inputs: Vec<Input>,
    mode: Ordering,
    sketch: bool,
    source: &Source,
    chunker: &mut dyn Chunker,
    summary: &mut OrderingSummary,
) -> Result<Vec<Input>, CoreError> {
    if mode == Ordering::None {
        return Ok(inputs);
    }
    let t = Instant::now();
    extension_order(&mut inputs);
    if mode == Ordering::Similarity && sketch {
        let mut sketches = Vec::with_capacity(inputs.len());
        for input in &inputs {
            if input.len < MIN_SKETCH_LEN {
                sketches.push(None);
                continue;
            }
            let mut r = source.open(input)?;
            let s = sketch_stream(&mut r, chunker, &mut summary.bytes_read)
                .map_err(|e| CoreError::io(&input.source, e))?;
            if s.is_some() {
                summary.files_sketched += 1;
            }
            sketches.push(s);
        }
        let (ordered, groups) = similarity_order(inputs, &sketches);
        inputs = ordered;
        summary.groups += groups;
    }
    summary.seconds += t.elapsed().as_secs_f64();
    Ok(inputs)
}

/// The wall time of an ordering step as a duration.
pub fn seconds(s: &OrderingSummary) -> Duration {
    Duration::from_secs_f64(s.seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fold::chunker::FastCdcChunker;
    use lpk_format::{EntryFlags, EntryKind};
    use std::io::Cursor;
    use std::path::PathBuf;

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

    fn input(path: &str, len: u64) -> Input {
        Input {
            path: path.into(),
            kind: EntryKind::File,
            len,
            mtime_ns: 0,
            flags: EntryFlags::default(),
            source: PathBuf::from(path),
            symlink_target: None,
            identity: None,
        }
    }

    fn chunker() -> FastCdcChunker {
        FastCdcChunker::new(MIN_CHUNK, super::super::AVG_CHUNK, super::super::MAX_CHUNK).unwrap()
    }

    fn sketch(data: &[u8]) -> Option<Sketch> {
        let mut c = chunker();
        let mut n = 0;
        sketch_stream(&mut Cursor::new(data), &mut c, &mut n).unwrap()
    }

    fn paths(v: &[Input]) -> Vec<&str> {
        v.iter().map(|i| i.path.as_str()).collect()
    }

    #[test]
    fn extension_order_cases_ties_and_no_extension() {
        let mut v = vec![
            input("z/b.TXT", 1),
            input("a/b.txt", 1),
            input("m/README", 1),
            input("a/a.c", 1),
            input(".hidden", 1),
            input("b/a.txt", 1),
        ];
        extension_order(&mut v);
        assert_eq!(
            paths(&v),
            [".hidden", "m/README", "a/a.c", "b/a.txt", "a/b.txt", "z/b.TXT"]
        );
        // ties on extension and name fall back to the path
        let mut v = vec![input("y/f.txt", 1), input("x/f.txt", 1)];
        extension_order(&mut v);
        assert_eq!(paths(&v), ["x/f.txt", "y/f.txt"]);
    }

    #[test]
    fn sketch_is_deterministic_and_content_only() {
        let a = noise(1, 1 << 20);
        let s1 = sketch(&a).unwrap();
        assert_eq!(s1, sketch(&a).unwrap());
        assert_ne!(s1, sketch(&noise(2, 1 << 20)).unwrap());
    }

    #[test]
    fn near_duplicates_share_a_super_feature_and_a_stranger_does_not() {
        let a = noise(7, 3 << 20);
        let mut b = a.clone();
        for x in &mut b[1_500_000..1_500_100] {
            *x ^= 0xFF;
        }
        let (sa, sb) = (sketch(&a).unwrap(), sketch(&b).unwrap());
        assert!(sa.features.iter().any(|f| sb.features.contains(f)));
        let sc = sketch(&noise(99, 3 << 20)).unwrap();
        assert!(!sa.features.iter().any(|f| sc.features.contains(f)));

        // Extension order a, b, c has the stranger between the pair; similarity closes the gap.
        let inputs = vec![input("a.bin", 10), input("b.bin", 30), input("c.bin", 20)];
        let sk = vec![Some(sa), Some(sc), Some(sb)];
        let (out, groups) = similarity_order(inputs, &sk);
        assert_eq!(groups, 1);
        // a and c are the pair (largest first: c, then a); b keeps its place after them.
        assert_eq!(paths(&out), ["c.bin", "a.bin", "b.bin"]);
    }

    #[test]
    fn small_files_are_not_sketched() {
        assert!(sketch(&noise(3, 10_000)).is_none());
        // Three chunks' worth (about) is still below the threshold: a 12 KiB file at most.
        assert!(sketch(&noise(3, 3 * MIN_CHUNK)).is_none());
        let src = Source::new();
        let mut c = chunker();
        let mut s = OrderingSummary::default();
        let v = vec![input("tiny.bin", MIN_SKETCH_LEN - 1)];
        let out = order_cluster(v, Ordering::Similarity, true, &src, &mut c, &mut s).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(s.bytes_read, 0);
        assert_eq!(s.files_sketched, 0);
    }

    #[test]
    fn order_cluster_counts_and_modes() {
        let dir = tempfile::tempdir().unwrap();
        let a = noise(5, 2 << 20);
        let mut b = a.clone();
        b[1 << 20] ^= 1;
        std::fs::write(dir.path().join("a.bin"), &a).unwrap();
        std::fs::write(dir.path().join("b.dat"), noise(6, 2 << 20)).unwrap();
        std::fs::write(dir.path().join("c.exe"), &b).unwrap();
        let inputs =
            crate::ingest::walk(dir.path(), &crate::ingest::IngestOptions::default()).unwrap();
        let src = Source::new();
        let mut c = chunker();

        let mut s = OrderingSummary::default();
        let none =
            order_cluster(inputs.clone(), Ordering::None, true, &src, &mut c, &mut s).unwrap();
        assert_eq!(s, OrderingSummary::default());
        assert_eq!(none.len(), 3);

        let mut s = OrderingSummary::default();
        let ext = order_cluster(
            inputs.clone(),
            Ordering::Extension,
            true,
            &src,
            &mut c,
            &mut s,
        )
        .unwrap();
        assert_eq!(s.bytes_read, 0);
        assert_eq!(paths(&ext), ["a.bin", "b.dat", "c.exe"]);

        let mut s = OrderingSummary::default();
        let sim = order_cluster(
            inputs.clone(),
            Ordering::Similarity,
            true,
            &src,
            &mut c,
            &mut s,
        )
        .unwrap();
        assert_eq!(s.files_sketched, 3);
        assert_eq!(s.bytes_read, 6 << 20);
        assert_eq!(s.groups, 1);
        assert_eq!(paths(&sim).len(), 3);
        // the pair is adjacent: b.dat is not between a.bin and c.exe
        let pos = |p: &str| paths(&sim).iter().position(|x| *x == p).unwrap();
        assert_eq!(pos("a.bin").abs_diff(pos("c.exe")), 1);

        // not allowed to sketch: the extension order, nothing read
        let mut s = OrderingSummary::default();
        let off = order_cluster(inputs, Ordering::Similarity, false, &src, &mut c, &mut s).unwrap();
        assert_eq!(paths(&off), paths(&ext));
        assert_eq!(s.bytes_read, 0);
    }
}
