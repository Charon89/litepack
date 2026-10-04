//! The chunk table and chunk verification (spec section 5).

use crate::error::FormatError;
use crate::varint;

const WHAT: &str = "chunk table";
/// Length of a chunk hash in bytes.
const HASH_LEN: usize = 32;
/// Smallest encoded chunk record: a one-byte `plain_len` and the 32-byte hash.
pub const MIN_CHUNK_RECORD_LEN: usize = 1 + HASH_LEN;

/// One chunk: its original length and the BLAKE3-256 of its original bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRecord {
    /// Length of the chunk's original bytes; 0 is allowed.
    pub plain_len: u64,
    /// BLAKE3-256 of the original bytes.
    pub hash: [u8; 32],
}

/// Encoder of the chunk table.
#[derive(Debug)]
pub struct ChunkTableWriter;

impl ChunkTableWriter {
    /// Encode `records` in chunk-list order.
    pub fn encode(records: &[ChunkRecord]) -> Vec<u8> {
        let mut out = Vec::with_capacity(10 + records.len() * (MIN_CHUNK_RECORD_LEN + 2));
        // Writing into a Vec cannot fail.
        let _ = varint::write(&mut out, records.len() as u64);
        for r in records {
            let _ = varint::write(&mut out, r.plain_len);
            out.extend_from_slice(&r.hash);
        }
        out
    }
}

fn read_varint(s: &mut &[u8]) -> Result<u64, FormatError> {
    match varint::read(s) {
        Err(FormatError::Truncated { .. }) => Err(FormatError::Truncated { what: WHAT }),
        other => other,
    }
}

/// A parsed chunk table: remembers the payload and the record count only.
#[derive(Debug, Clone, Copy)]
pub struct ChunkTable<'a> {
    payload: &'a [u8],
    body: usize,
    count: u64,
}

impl<'a> ChunkTable<'a> {
    /// Read the record count; no record is examined until iteration. A count
    /// the remaining bytes cannot hold (`MIN_CHUNK_RECORD_LEN` bytes per record)
    /// is `Truncated`.
    pub fn parse(payload: &'a [u8]) -> Result<ChunkTable<'a>, FormatError> {
        let mut s = payload;
        let count = read_varint(&mut s)?;
        if count > (s.len() / MIN_CHUNK_RECORD_LEN) as u64 {
            return Err(FormatError::Truncated { what: WHAT });
        }
        Ok(ChunkTable {
            payload,
            body: payload.len() - s.len(),
            count,
        })
    }

    /// Number of records declared.
    pub fn len(&self) -> u64 {
        self.count
    }

    /// True when the table declares no records.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Stream the records. After the last one, leftover bytes yield
    /// `TrailingBytes`; any error ends the stream.
    pub fn iter(&self) -> ChunkIter<'a> {
        ChunkIter {
            rest: &self.payload[self.body..],
            remaining: self.count,
            done: false,
        }
    }

    /// The record at `index`, or `None` past the end. Walks the table from
    /// the start, so it is linear in `index`.
    pub fn get(&self, index: u64) -> Result<Option<ChunkRecord>, FormatError> {
        if index >= self.count {
            return Ok(None);
        }
        let skip = usize::try_from(index).map_err(|_| FormatError::Truncated { what: WHAT })?;
        let mut it = self.iter();
        for _ in 0..skip {
            match it.next() {
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e),
                None => return Ok(None),
            }
        }
        it.next().transpose()
    }

    /// Walk every record, including the trailing-bytes check.
    pub fn validate(&self) -> Result<(), FormatError> {
        for r in self.iter() {
            r?;
        }
        Ok(())
    }
}

/// Streaming iterator over the records of a [`ChunkTable`].
#[derive(Debug)]
pub struct ChunkIter<'a> {
    rest: &'a [u8],
    remaining: u64,
    done: bool,
}

impl ChunkIter<'_> {
    fn next_record(&mut self) -> Result<ChunkRecord, FormatError> {
        let mut s = self.rest;
        let plain_len = read_varint(&mut s)?;
        let (hash, tail) = s
            .split_first_chunk::<HASH_LEN>()
            .ok_or(FormatError::Truncated { what: WHAT })?;
        self.rest = tail;
        Ok(ChunkRecord {
            plain_len,
            hash: *hash,
        })
    }
}

impl Iterator for ChunkIter<'_> {
    type Item = Result<ChunkRecord, FormatError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.remaining == 0 {
            self.done = true;
            if !self.rest.is_empty() {
                return Some(Err(FormatError::TrailingBytes { what: WHAT }));
            }
            return None;
        }
        let r = self.next_record();
        match &r {
            Ok(_) => self.remaining -= 1,
            Err(_) => self.done = true,
        }
        Some(r)
    }
}

/// Supplier of original chunk bytes by chunk index. E1-4 and the extractor
/// implement it over `ChunkData` frames.
pub trait ChunkSource {
    /// The original (decoded) bytes of chunk `index`.
    fn chunk(&mut self, index: u64) -> Result<Vec<u8>, FormatError>;
}

/// Resolve every chunk index of a file to its record and check that the
/// lengths add up to `file_len`; a sum that overflows `u64` is reported as
/// `FileSizeMismatch` with `found` = `u64::MAX`.
fn resolve(
    chunks: &[u64],
    file_len: u64,
    table: &ChunkTable<'_>,
) -> Result<Vec<ChunkRecord>, FormatError> {
    let mut recs = Vec::with_capacity(chunks.len().min(1 << 16));
    let mut total = Some(0u64);
    for &c in chunks {
        let r = table.get(c)?.ok_or(FormatError::ChunkIndexOutOfRange {
            chunk: c,
            len: table.len(),
        })?;
        total = total.and_then(|t| t.checked_add(r.plain_len));
        recs.push(r);
    }
    match total {
        Some(t) if t == file_len => Ok(recs),
        found => Err(FormatError::FileSizeMismatch {
            expected: file_len,
            found: found.unwrap_or(u64::MAX),
        }),
    }
}

fn fetch_and_check(
    source: &mut dyn ChunkSource,
    index: u64,
    rec: &ChunkRecord,
) -> Result<(), FormatError> {
    let data = source.chunk(index)?;
    if data.len() as u64 != rec.plain_len || blake3::hash(&data).as_bytes() != &rec.hash {
        return Err(FormatError::ChunkMismatch { chunk: index });
    }
    Ok(())
}

/// Verify a whole file: indices in range, summed `plain_len` equal to
/// `file_len`, then every chunk fetched and compared with its table hash.
pub fn verify_file(
    chunks: &[u64],
    file_len: u64,
    table: &ChunkTable<'_>,
    source: &mut dyn ChunkSource,
) -> Result<(), FormatError> {
    let recs = resolve(chunks, file_len, table)?;
    for (&c, r) in chunks.iter().zip(&recs) {
        fetch_and_check(source, c, r)?;
    }
    Ok(())
}

/// Verify bytes `[offset, offset + len)` of a file by fetching and hashing
/// only the chunks that overlap the range. Check order: range against
/// `file_len`, chunk indices, size sum, then the overlapping chunks in order.
pub fn verify_range(
    chunks: &[u64],
    file_len: u64,
    offset: u64,
    len: u64,
    table: &ChunkTable<'_>,
    source: &mut dyn ChunkSource,
) -> Result<(), FormatError> {
    let end = match offset.checked_add(len) {
        Some(e) if e <= file_len => e,
        _ => {
            return Err(FormatError::RangeOutOfFile {
                offset,
                len,
                file_len,
            })
        }
    };
    let recs = resolve(chunks, file_len, table)?;
    if len == 0 {
        return Ok(());
    }
    let mut start = 0u64;
    for (&c, r) in chunks.iter().zip(&recs) {
        if start >= end {
            break;
        }
        let stop = start + r.plain_len;
        if r.plain_len > 0 && stop > offset {
            fetch_and_check(source, c, r)?;
        }
        start = stop;
    }
    Ok(())
}

/// The Markdown table of one chunk record, pasted verbatim into the spec.
pub fn chunk_record_table() -> String {
    format!(
        "| Field | Size | Meaning |\n|---|---|---|\n\
         | plain_len | varint | length in bytes of the chunk's original data; 0 is allowed |\n\
         | hash | {HASH_LEN} | BLAKE3-256 of the chunk's original data (the bytes extraction returns, not the stored form) |\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    fn rec(plain_len: u64, b: u8) -> ChunkRecord {
        ChunkRecord {
            plain_len,
            hash: [b; 32],
        }
    }

    fn collect(p: &[u8]) -> Result<Vec<ChunkRecord>, FormatError> {
        ChunkTable::parse(p)?.iter().collect()
    }

    #[test]
    fn round_trip() {
        let sets: [Vec<ChunkRecord>; 4] = [
            vec![],
            vec![rec(0, 1)],
            vec![rec(5, 1), rec(0, 2), rec(u64::MAX, 3)],
            vec![rec(u64::MAX, 9)],
        ];
        for recs in sets {
            let p = ChunkTableWriter::encode(&recs);
            let t = ChunkTable::parse(&p).unwrap();
            assert_eq!(t.len(), recs.len() as u64);
            assert_eq!(t.is_empty(), recs.is_empty());
            t.validate().unwrap();
            assert_eq!(collect(&p).unwrap(), recs);
            for (i, r) in recs.iter().enumerate() {
                assert_eq!(t.get(i as u64).unwrap(), Some(*r));
            }
            assert_eq!(t.get(recs.len() as u64).unwrap(), None);
        }
    }

    #[test]
    fn count_bound() {
        // A count of u64::MAX (10 bytes) followed by nothing.
        let mut p = Vec::new();
        varint::write(&mut p, u64::MAX).unwrap();
        assert!(matches!(
            ChunkTable::parse(&p),
            Err(FormatError::Truncated {
                what: "chunk table"
            })
        ));
        // Five bytes: the count 5 and four more bytes.
        let p = [0x05u8, 0, 0, 0, 0];
        assert!(matches!(
            ChunkTable::parse(&p),
            Err(FormatError::Truncated {
                what: "chunk table"
            })
        ));
        assert!(matches!(
            ChunkTable::parse(&[]),
            Err(FormatError::Truncated {
                what: "chunk table"
            })
        ));
    }

    #[test]
    fn trailing_and_truncated() {
        let mut p = ChunkTableWriter::encode(&[rec(3, 1), rec(4, 2)]);
        p.push(0);
        let t = ChunkTable::parse(&p).unwrap();
        assert!(matches!(
            t.validate(),
            Err(FormatError::TrailingBytes {
                what: "chunk table"
            })
        ));
        let mut p = ChunkTableWriter::encode(&[rec(3, 1)]);
        p.pop();
        // The count bound already rejects a short last record.
        assert!(matches!(
            ChunkTable::parse(&p),
            Err(FormatError::Truncated {
                what: "chunk table"
            })
        ));
        // A cut hash that the bound lets through: the first record is long
        // (a ten-byte plain_len) so the bound's slack hides the cut.
        let mut p = ChunkTableWriter::encode(&[rec(u64::MAX, 1), rec(1, 2)]);
        p.truncate(p.len() - 1);
        let t = ChunkTable::parse(&p).unwrap();
        assert!(matches!(
            t.validate(),
            Err(FormatError::Truncated {
                what: "chunk table"
            })
        ));
        assert!(t.get(1).is_err());
    }

    #[test]
    fn non_canonical_plain_len() {
        let mut p = vec![1u8, 0x80, 0x00];
        p.extend_from_slice(&[0u8; 32]);
        let t = ChunkTable::parse(&p).unwrap();
        assert!(matches!(t.validate(), Err(FormatError::NonCanonicalVarint)));
    }

    #[test]
    fn scale_million_records() {
        let recs: Vec<ChunkRecord> = (0..1_000_000u64)
            .map(|i| rec(i % 1000, (i % 251) as u8))
            .collect();
        let p = ChunkTableWriter::encode(&recs);
        let t = ChunkTable::parse(&p).unwrap();
        assert_eq!(t.len(), 1_000_000);
        let mut n = 0u64;
        for r in t.iter() {
            r.unwrap();
            n += 1;
        }
        assert_eq!(n, 1_000_000);
    }

    /// In-memory chunk store that records which chunks were fetched.
    struct Store {
        data: Vec<Vec<u8>>,
        fetched: Vec<u64>,
    }

    impl ChunkSource for Store {
        fn chunk(&mut self, index: u64) -> Result<Vec<u8>, FormatError> {
            self.fetched.push(index);
            self.data
                .get(index as usize)
                .cloned()
                .ok_or(FormatError::Truncated { what: "test store" })
        }
    }

    fn build(data: &[Vec<u8>]) -> (Vec<u8>, Store) {
        let recs: Vec<ChunkRecord> = data
            .iter()
            .map(|d| ChunkRecord {
                plain_len: d.len() as u64,
                hash: *blake3::hash(d).as_bytes(),
            })
            .collect();
        (
            ChunkTableWriter::encode(&recs),
            Store {
                data: data.to_vec(),
                fetched: Vec::new(),
            },
        )
    }

    fn seven() -> Vec<Vec<u8>> {
        (0..7u8).map(|i| vec![i + 1; 10 + i as usize]).collect()
    }

    fn all(n: u64) -> Vec<u64> {
        (0..n).collect()
    }

    #[test]
    fn file_verifies_and_detects() {
        let data = seven();
        let total: u64 = data.iter().map(|d| d.len() as u64).sum();
        let (p, mut s) = build(&data);
        let t = ChunkTable::parse(&p).unwrap();
        verify_file(&all(7), total, &t, &mut s).unwrap();
        assert_eq!(s.fetched.len(), 7);

        s.data[4][3] ^= 1;
        let e = verify_file(&all(7), total, &t, &mut s).unwrap_err();
        assert!(matches!(e, FormatError::ChunkMismatch { chunk: 4 }));

        s.data[4][3] ^= 1;
        s.fetched.clear();
        let e = verify_file(&all(7), total + 1, &t, &mut s).unwrap_err();
        assert!(matches!(
            e,
            FormatError::FileSizeMismatch { expected, found } if expected == total + 1 && found == total
        ));
        assert!(s.fetched.is_empty());

        let e = verify_file(&[0, 7], total, &t, &mut s).unwrap_err();
        assert!(matches!(
            e,
            FormatError::ChunkIndexOutOfRange { chunk: 7, len: 7 }
        ));

        // A stored chunk of the wrong length is a mismatch too.
        s.data[2].push(0);
        let e = verify_file(&all(7), total, &t, &mut s).unwrap_err();
        assert!(matches!(e, FormatError::ChunkMismatch { chunk: 2 }));
    }

    #[test]
    fn overflowing_lengths_are_a_size_mismatch() {
        let recs = [rec(10, 1), rec(u64::MAX, 2)];
        let p = ChunkTableWriter::encode(&recs);
        let t = ChunkTable::parse(&p).unwrap();
        let mut s = Store {
            data: vec![],
            fetched: vec![],
        };
        let e = verify_file(&[0, 1], u64::MAX, &t, &mut s).unwrap_err();
        assert!(matches!(
            e,
            FormatError::FileSizeMismatch {
                expected: u64::MAX,
                found: u64::MAX
            }
        ));
        let e = verify_range(&[0, 1], u64::MAX, 0, 5, &t, &mut s).unwrap_err();
        assert!(matches!(e, FormatError::FileSizeMismatch { .. }));
        assert!(s.fetched.is_empty());
    }

    #[test]
    fn get_reports_a_corrupt_record_before_the_index() {
        // Record 1 has a non-canonical plain_len; records 0 and 2 are fine.
        let mut p = vec![3u8, 1];
        p.extend_from_slice(&[0u8; 32]);
        p.extend_from_slice(&[0x80, 0x00]);
        p.extend_from_slice(&[0u8; 32]);
        p.push(1);
        p.extend_from_slice(&[0u8; 32]);
        let t = ChunkTable::parse(&p).unwrap();
        assert!(t.get(0).unwrap().is_some());
        assert!(matches!(t.get(1), Err(FormatError::NonCanonicalVarint)));
        assert!(matches!(t.get(2), Err(FormatError::NonCanonicalVarint)));
        assert!(t.get(3).unwrap().is_none());
    }

    #[test]
    fn permuted_and_repeated_indices() {
        let data = seven();
        let (p, mut s) = build(&data);
        let t = ChunkTable::parse(&p).unwrap();
        let idx = [3u64, 0, 3, 1];
        let len = |i: u64| data[i as usize].len() as u64;
        let total: u64 = idx.iter().map(|i| len(*i)).sum();
        verify_file(&idx, total, &t, &mut s).unwrap();
        // From the start of list position 1 (chunk 0) one byte into position 2.
        s.fetched.clear();
        verify_range(&idx, total, len(3), len(0) + 1, &t, &mut s).unwrap();
        assert_eq!(s.fetched, vec![0, 3]);
        // Corruption is attributed to the chunk index, wherever it repeats.
        s.data[3][0] ^= 1;
        let e = verify_file(&idx, total, &t, &mut s).unwrap_err();
        assert!(matches!(e, FormatError::ChunkMismatch { chunk: 3 }));
        s.data[3][0] ^= 1;
        s.data[1][0] ^= 1;
        let e = verify_file(&idx, total, &t, &mut s).unwrap_err();
        assert!(matches!(e, FormatError::ChunkMismatch { chunk: 1 }));
    }

    #[test]
    fn empty_file_and_shared_chunks() {
        let data = vec![b"abc".to_vec()];
        let (p, mut s) = build(&data);
        let t = ChunkTable::parse(&p).unwrap();
        verify_file(&[], 0, &t, &mut s).unwrap();
        verify_file(&[0, 0, 0], 9, &t, &mut s).unwrap();
    }

    #[test]
    fn range_fetches_only_overlap() {
        let data = seven();
        let total: u64 = data.iter().map(|d| d.len() as u64).sum();
        let (p, mut s) = build(&data);
        let t = ChunkTable::parse(&p).unwrap();
        // Chunks 2 and 3 span [10+11, 10+11+12+13) = [21, 46).
        verify_range(&all(7), total, 25, 10, &t, &mut s).unwrap();
        assert_eq!(s.fetched, vec![2, 3]);
        s.fetched.clear();
        // Exactly one chunk: [21, 33).
        verify_range(&all(7), total, 21, 12, &t, &mut s).unwrap();
        assert_eq!(s.fetched, vec![2]);
        s.fetched.clear();
        verify_range(&all(7), total, 5, 0, &t, &mut s).unwrap();
        verify_range(&all(7), total, total, 0, &t, &mut s).unwrap();
        assert!(s.fetched.is_empty());
        for (o, l) in [(total, 1), (total - 1, 2), (u64::MAX, 2), (0, u64::MAX)] {
            let e = verify_range(&all(7), total, o, l, &t, &mut s).unwrap_err();
            assert!(matches!(e, FormatError::RangeOutOfFile { .. }), "{o} {l}");
        }
        s.data[3][0] ^= 1;
        let e = verify_range(&all(7), total, 25, 10, &t, &mut s).unwrap_err();
        assert!(matches!(e, FormatError::ChunkMismatch { chunk: 3 }));
        // Corruption outside the range is not looked at.
        s.fetched.clear();
        verify_range(&all(7), total, 0, 5, &t, &mut s).unwrap();
        assert_eq!(s.fetched, vec![0]);
    }

    fn arb_chunks() -> impl Strategy<Value = Vec<Vec<u8>>> {
        prop::collection::vec(prop::collection::vec(any::<u8>(), 0..40), 0..12)
    }

    proptest! {
        #[test]
        fn range_fetch_set_equals_overlap(
            data in arb_chunks(),
            a in any::<prop::sample::Index>(),
            b in any::<prop::sample::Index>(),
        ) {
            let total: u64 = data.iter().map(|d| d.len() as u64).sum();
            let (p, mut s) = build(&data);
            let t = ChunkTable::parse(&p).unwrap();
            let x = a.index(total as usize + 1) as u64;
            let y = b.index(total as usize + 1) as u64;
            let (offset, end) = (x.min(y), x.max(y));
            verify_range(&all(data.len() as u64), total, offset, end - offset, &t, &mut s).unwrap();
            let mut expected = BTreeSet::new();
            let mut start = 0u64;
            for (i, d) in data.iter().enumerate() {
                let stop = start + d.len() as u64;
                if !d.is_empty() && start < end && stop > offset && end > offset {
                    expected.insert(i as u64);
                }
                start = stop;
            }
            let got: BTreeSet<u64> = s.fetched.iter().copied().collect();
            prop_assert_eq!(got, expected);
            prop_assert_eq!(s.fetched.len(), s.fetched.iter().collect::<BTreeSet<_>>().len());
        }

        #[test]
        fn corruption_is_attributed(
            data in arb_chunks().prop_filter("some byte", |d| d.iter().any(|c| !c.is_empty())),
            pick in any::<prop::sample::Index>(),
            bit in 0u8..8,
        ) {
            let total: u64 = data.iter().map(|d| d.len() as u64).sum();
            let (p, mut s) = build(&data);
            let t = ChunkTable::parse(&p).unwrap();
            let mut at = pick.index(total as usize);
            let mut holder = 0usize;
            for (i, d) in data.iter().enumerate() {
                if at < d.len() {
                    holder = i;
                    break;
                }
                at -= d.len();
            }
            s.data[holder][at] ^= 1 << bit;
            let n = data.len() as u64;
            let e = verify_file(&all(n), total, &t, &mut s).unwrap_err();
            let got = match e {
                FormatError::ChunkMismatch { chunk } => Some(chunk),
                _ => None,
            };
            prop_assert_eq!(got, Some(holder as u64));
        }
    }
}
