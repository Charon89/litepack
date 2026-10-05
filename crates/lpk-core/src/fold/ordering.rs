//! File order inside a cluster (E2-8, D-12: files are ordered, chunks are not).
//!
//! Three orders: [`Ordering::None`] keeps the path order of the clustering (directory locality);
//! [`Ordering::Extension`] sorts by extension (lower-cased), then file name, then path, the order
//! 7-Zip uses for solid archives; [`Ordering::Similarity`] keeps the path order and moves each
//! group of near-duplicate files next to the group's first member.
//!
//! The similarity feature is a content-shingle sketch that works at any file size from 48 bytes:
//! a rolling gear hash over a [`WINDOW`]-byte window is sampled where a multiplicative mix of it
//! has its top [`SAMPLE_BITS`] bits zero (about one position in sixty-four, content-defined, so an
//! insertion changes no sample but those near it); the sampled window hashes enter
//! [`SKETCH_HASHES`] min-hashes under as many seeds; each super-feature hashes
//! [`HASHES_PER_FEATURE`] consecutive minima, [`SUPER_FEATURES`] in all. Four minima per feature
//! need a high overlap of shingles (few false pairs); three features give a near-duplicate three
//! chances to match (few missed pairs). Two files sharing a super-feature value are one group,
//! written together largest first at the place of the group's first member. A file shorter than
//! the window, or with no sampled position, is not sketched.
//!
//! The pass reads every candidate file once more than the writer does; only the twelve minima of
//! the file in hand are kept (the window ring is 48 bytes), the sketches of one cluster live until
//! it is ordered. Ordering never changes what deduplicates: that is content-addressed in the
//! writer.

use std::cmp;
use std::collections::HashMap;
use std::io::Read;
use std::time::{Duration, Instant};

use crate::error::CoreError;
use crate::ingest::Input;
use crate::source::Source;

/// Min-hash seeds per file. Twelve is three super-features of four.
pub const SKETCH_HASHES: usize = 12;
/// Super-features per file.
pub const SUPER_FEATURES: usize = 3;
/// Minima hashed together into one super-feature.
pub const HASHES_PER_FEATURE: usize = SKETCH_HASHES / SUPER_FEATURES;
/// The shingle: bytes under the rolling hash.
pub const WINDOW: usize = 48;
/// A window is sampled when this many top bits of its mixed hash are zero (one in sixty-four).
pub const SAMPLE_BITS: u32 = 6;
/// The bytes read per call while sketching.
const SEGMENT: usize = 64 * 1024;

/// How files are ordered inside a cluster.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Ordering {
    /// Path order (what the clustering gives).
    None,
    /// Extension (lower-cased), then name, then path.
    Extension,
    /// Path order with each near-duplicate group moved next to its first member.
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
    /// Files that got a sketch.
    pub files_sketched: u64,
    /// Bytes the pass read.
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

/// The sketch of the stream `reader`; `None` when it is shorter than the window or no window is
/// sampled. `bytes_read` grows by what was read.
pub fn sketch_stream(
    reader: &mut dyn Read,
    bytes_read: &mut u64,
) -> std::io::Result<Option<Sketch>> {
    let mut gear = [0u64; 256];
    for (i, g) in gear.iter_mut().enumerate() {
        *g = mix(i as u64 + 0x6EA2);
    }
    let seeds: [u64; SKETCH_HASHES] = std::array::from_fn(|i| mix(i as u64 + 1));
    let mut mins = [u64::MAX; SKETCH_HASHES];
    let mut sampled = 0u64;
    let mut ring = [0u8; WINDOW];
    let (mut h, mut pos) = (0u64, 0usize);
    let mut buf = vec![0u8; SEGMENT];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        *bytes_read += n as u64;
        for &b in &buf[..n] {
            // h covers the last WINDOW bytes: the byte of age WINDOW is taken out.
            h = (h << 1).wrapping_add(gear[b as usize]);
            let slot = pos % WINDOW;
            if pos >= WINDOW {
                h = h.wrapping_sub(gear[ring[slot] as usize] << WINDOW);
            }
            ring[slot] = b;
            pos += 1;
            if pos >= WINDOW && h.wrapping_mul(0xFF51_AFD7_ED55_8CCD) >> (64 - SAMPLE_BITS) == 0 {
                let w = mix(h);
                sampled += 1;
                for (m, seed) in mins.iter_mut().zip(&seeds) {
                    let v = mix(w ^ seed);
                    if v < *m {
                        *m = v;
                    }
                }
            }
        }
    }
    if sampled == 0 {
        return Ok(None);
    }
    let mut features = [0u64; SUPER_FEATURES];
    for (j, f) in features.iter_mut().enumerate() {
        let mut acc = mix(j as u64 + 0x5EED);
        for m in &mins[j * HASHES_PER_FEATURE..(j + 1) * HASHES_PER_FEATURE] {
            acc = mix(acc ^ m);
        }
        *f = acc;
    }
    Ok(Some(Sketch { features }))
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
/// order and keeps it for files without a match; a group goes where its first member stood,
/// largest file first (then base order). Returns the new order and the group count.
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
    summary: &mut OrderingSummary,
) -> Result<Vec<Input>, CoreError> {
    let t = Instant::now();
    match mode {
        Ordering::None => return Ok(inputs),
        Ordering::Extension => extension_order(&mut inputs),
        Ordering::Similarity if sketch => {
            let mut sketches = Vec::with_capacity(inputs.len());
            for input in &inputs {
                if input.len < WINDOW as u64 {
                    sketches.push(None);
                    continue;
                }
                let mut r = source.open(input)?;
                let s = sketch_stream(&mut r, &mut summary.bytes_read)
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
        Ordering::Similarity => {}
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

    /// Log-like lines: a fixed vocabulary with a varying number and a user id.
    fn log_lines(seed: u64, lines: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut out = String::new();
        for i in 0..lines {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.push_str(&format!(
                "2026-10-04 12:{:02}:{:02} INFO request {} served by worker-{} in {} ms\n",
                i / 60 % 60,
                i % 60,
                x % 100_000,
                x >> 40 & 7,
                x >> 20 & 255
            ));
        }
        out.into_bytes()
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

    fn sketch(data: &[u8]) -> Option<Sketch> {
        let mut n = 0;
        sketch_stream(&mut Cursor::new(data), &mut n).unwrap()
    }

    fn shared(a: &Sketch, b: &Sketch) -> usize {
        a.features.iter().filter(|f| b.features.contains(f)).count()
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
        let mut v = vec![input("y/f.txt", 1), input("x/f.txt", 1)];
        extension_order(&mut v);
        assert_eq!(paths(&v), ["x/f.txt", "y/f.txt"]);
    }

    #[test]
    fn identical_content_shares_all_features_and_unrelated_none() {
        let a = noise(1, 300_000);
        let s1 = sketch(&a).unwrap();
        assert_eq!(s1, sketch(&a).unwrap());
        assert_eq!(shared(&s1, &sketch(&a).unwrap()), SUPER_FEATURES);
        assert_eq!(shared(&s1, &sketch(&noise(2, 300_000)).unwrap()), 0);
        // the same at a size of a few hundred bytes
        let t = log_lines(1, 8);
        assert_eq!(sketch(&t), sketch(&t));
        assert!(sketch(&noise(5, WINDOW - 1)).is_none());
    }

    #[test]
    fn a_modified_copy_shares_a_super_feature() {
        // a few hundred bytes with one changed line
        let a = log_lines(3, 40);
        let mut lines: Vec<&[u8]> = a.split_inclusive(|b| *b == b'\n').collect();
        let changed = b"2026-10-04 13:00:00 WARN something else entirely happened here\n";
        lines[20] = changed;
        let b: Vec<u8> = lines.concat();
        let (sa, sb) = (sketch(&a).unwrap(), sketch(&b).unwrap());
        assert!(shared(&sa, &sb) >= 1);
        // a larger file with a few changed bytes
        let big = noise(7, 3 << 20);
        let mut big2 = big.clone();
        for x in &mut big2[1_500_000..1_500_100] {
            *x ^= 0xFF;
        }
        assert!(shared(&sketch(&big).unwrap(), &sketch(&big2).unwrap()) >= 1);
    }

    #[test]
    fn a_log_family_groups_and_strangers_do_not() {
        // five rotations of one log: each file drops the first lines and gains new ones
        let base = log_lines(11, 400);
        let lines: Vec<&[u8]> = base.split_inclusive(|b| *b == b'\n').collect();
        let family: Vec<Vec<u8>> = (0..5)
            .map(|k| {
                let mut v = lines[k * 4..].concat();
                v.extend_from_slice(&log_lines(100 + k as u64, 4));
                v
            })
            .collect();
        let sk: Vec<Option<Sketch>> = family.iter().map(|d| sketch(d)).collect();
        let stranger = sketch(&log_lines(999, 400)).unwrap();
        let hits = (1..5)
            .filter(|&i| shared(sk[0].as_ref().unwrap(), sk[i].as_ref().unwrap()) > 0)
            .count();
        assert!(hits >= 3, "{hits}");
        assert_eq!(shared(sk[0].as_ref().unwrap(), &stranger), 0);
    }

    #[test]
    fn group_placement_keeps_path_order_and_moves_the_group() {
        let a = Sketch {
            features: [1, 2, 3],
        };
        let b = Sketch {
            features: [7, 8, 9],
        };
        let c = Sketch {
            features: [3, 4, 5],
        }; // shares 3 with a
        let inputs = vec![
            input("a.bin", 10),
            input("b.bin", 30),
            input("c.bin", 20),
            input("d.bin", 5),
        ];
        let (out, groups) = similarity_order(inputs, &[Some(a), Some(b), Some(c), None]);
        assert_eq!(groups, 1);
        // the group {a, c} at a's place, c (larger) first; b and d keep their order
        assert_eq!(paths(&out), ["c.bin", "a.bin", "b.bin", "d.bin"]);
        // no matches: unchanged
        let inputs = vec![input("a", 1), input("b", 2)];
        let (out, groups) = similarity_order(
            inputs,
            &[
                Some(a),
                Some(Sketch {
                    features: [10, 11, 12],
                }),
            ],
        );
        assert_eq!((paths(&out), groups), (vec!["a", "b"], 0));
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
        std::fs::write(dir.path().join("d.tiny"), b"short").unwrap();
        let inputs =
            crate::ingest::walk(dir.path(), &crate::ingest::IngestOptions::default()).unwrap();
        let mut inputs: Vec<Input> = inputs
            .into_iter()
            .filter(|i| i.kind == EntryKind::File)
            .collect();
        inputs.sort_by(|a, b| a.path.cmp(&b.path));
        let src = Source::new();

        let mut s = OrderingSummary::default();
        let none = order_cluster(inputs.clone(), Ordering::None, true, &src, &mut s).unwrap();
        assert_eq!(s, OrderingSummary::default());
        assert_eq!(paths(&none), ["a.bin", "b.dat", "c.exe", "d.tiny"]);

        let mut s = OrderingSummary::default();
        let ext = order_cluster(inputs.clone(), Ordering::Extension, true, &src, &mut s).unwrap();
        assert_eq!(s.bytes_read, 0);
        assert_eq!(paths(&ext), ["a.bin", "b.dat", "c.exe", "d.tiny"]);

        let mut s = OrderingSummary::default();
        let sim = order_cluster(inputs.clone(), Ordering::Similarity, true, &src, &mut s).unwrap();
        assert_eq!(s.files_sketched, 3);
        assert_eq!(s.bytes_read, 6 << 20);
        assert_eq!(s.groups, 1);
        // c and a are the pair, equal size: base order, so a then c; b follows, d last
        assert_eq!(paths(&sim), ["a.bin", "c.exe", "b.dat", "d.tiny"]);

        let mut s = OrderingSummary::default();
        let off = order_cluster(inputs, Ordering::Similarity, false, &src, &mut s).unwrap();
        assert_eq!(paths(&off), paths(&none));
        assert_eq!(s.bytes_read, 0);
    }
}
