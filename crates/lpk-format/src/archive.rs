//! Opening an archive from its tail, and diagnosing a damaged one (spec section 6).

use crate::chunk::ChunkIndex;
use crate::decode::Registry;
use crate::envelope::Resources;
use crate::error::FormatError;
use crate::frame::{Frame, FrameKind, ReadFrame, ReadLimits};
use crate::header::Header;
use crate::index::{FrameLocation, Index};
use crate::priors::PriorStore;
use crate::trailer::{Trailer, TRAILER_FRAME_LEN};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::sync::Arc;

/// An opened archive: header, trailer, index and the chunk index are in
/// memory; the body is read only on request.
#[derive(Debug)]
pub struct Archive<R: Read + Seek> {
    reader: R,
    limits: ReadLimits,
    resources: Resources,
    header: Header,
    trailer: Trailer,
    index: Index,
    chunks: Arc<ChunkIndex>,
    pub(crate) registry: Registry,
    /// The most recently decoded block: its index and plain bytes.
    pub(crate) cache: Option<(usize, Vec<u8>)>,
    /// Number of records, read from the `Records` frame the first time a
    /// block names a record (0 without a frame).
    pub(crate) record_count: Option<u64>,
}

/// What a forward walk over the frames found.
#[derive(Debug)]
pub struct Diagnosis {
    /// Frames (of any kind) read with a valid hash.
    pub frames_ok: u64,
    /// Offset just after the last valid frame (the header counts as valid
    /// when it parsed); where the first bad or missing frame would start.
    pub ends_at: u64,
    /// True when a trailer frame was read.
    pub trailer_seen: bool,
    /// Why the walk stopped: `Truncated { what: "trailer" }` for a clean
    /// truncation, the frame's own error for corruption, `TrailingBytes {
    /// what: "archive" }` for bytes after a trailer; `None` when the input
    /// ends exactly after a trailer.
    pub error: Option<FormatError>,
}

/// Counts the bytes handed out, which is the logical read position.
struct Counting<R> {
    inner: R,
    n: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.n += k as u64;
        Ok(k)
    }
}

fn read_frame_at_impl<R: Read + Seek>(
    reader: &mut R,
    limits: &ReadLimits,
    at: FrameLocation,
    expected: FrameKind,
) -> Result<Frame, FormatError> {
    let what = frame_what(expected);
    let bad = FormatError::BadFrameLocation { what };
    reader.seek(SeekFrom::Start(at.offset))?;
    // Nothing past the recorded length is read, whatever the frame claims.
    let mut limited = BufReader::new(&mut *reader).take(at.len);
    let mut kind = [0u8; 2];
    match limited.read_exact(&mut kind) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(bad),
        Err(e) => return Err(e.into()),
    }
    let found = u16::from_le_bytes(kind);
    if found != expected as u16 {
        return Err(FormatError::WrongFrameKind {
            expected: expected as u16,
            found,
        });
    }
    let mut rest = (&kind[..]).chain(&mut limited);
    match Frame::read(&mut rest, limits) {
        Ok(Some(ReadFrame::Known(f))) if f.encoded_len() == at.len => Ok(f),
        Ok(_) => Err(bad),
        Err(FormatError::Truncated { .. }) => Err(bad),
        Err(e) => Err(e),
    }
}

/// The name a frame of this kind goes by in `BadFrameLocation`.
fn frame_what(kind: FrameKind) -> &'static str {
    match kind {
        FrameKind::Index => "index",
        FrameKind::EntryTable => "entry table",
        FrameKind::Records => "records",
        other => other.name(),
    }
}

impl<R: Read + Seek> Archive<R> {
    /// Open from the head and the tail: read the header, then the trailer at
    /// the end, then the index it points to; the body is not read. When the
    /// tail is not a valid trailer the frames are walked from the start
    /// ([`Archive::diagnose`]) and the error says whether the archive is cut
    /// short or damaged.
    ///
    /// `resources` is what this machine allows a decoder: until the index is
    /// read the frame payload limit is `resources.max_frame_payload`; once it
    /// is read, the declared envelope is compared with `resources` and an
    /// archive that needs more is `Refused` before any block is read. After
    /// that the limit is the smaller of the envelope's `max_frame_payload` and
    /// `resources.max_frame_payload`.
    pub fn open(mut reader: R, resources: &Resources) -> Result<Self, FormatError> {
        let mut limits = ReadLimits {
            max_payload: resources.max_frame_payload,
        };
        let len = reader.seek(SeekFrom::End(0))?;
        if len < Header::LEN as u64 + TRAILER_FRAME_LEN {
            return Err(Self::fallback_error(reader, &limits));
        }
        reader.seek(SeekFrom::Start(0))?;
        let header = Header::read(&mut reader)?;
        let trailer = match Trailer::read_tail(&mut reader, len) {
            Ok(t) => t,
            Err(
                FormatError::NoTrailer
                | FormatError::HashMismatch { .. }
                | FormatError::Truncated { .. },
            ) => return Err(Self::fallback_error(reader, &limits)),
            Err(e) => return Err(e),
        };
        if trailer.archive_id != header.archive_id {
            return Err(FormatError::ArchiveIdMismatch);
        }
        let at = FrameLocation {
            offset: trailer.index_offset,
            len: trailer.index_len,
        };
        if !at.fits_below(len - TRAILER_FRAME_LEN) {
            return Err(FormatError::BadFrameLocation { what: "index" });
        }
        let frame = read_frame_at_impl(&mut reader, &limits, at, FrameKind::Index)?;
        if blake3::hash(&frame.payload).as_bytes() != &trailer.index_hash {
            return Err(FormatError::IndexHashMismatch);
        }
        let (index, chunks) = Index::parse_with_chunks(&frame.payload, trailer.index_offset)?;
        index
            .envelope
            .check(resources)
            .map_err(FormatError::Refused)?;
        limits.max_payload = index
            .envelope
            .max_frame_payload
            .min(resources.max_frame_payload);
        Ok(Archive {
            reader,
            limits,
            resources: *resources,
            header,
            trailer,
            index,
            chunks: Arc::new(chunks),
            registry: Registry::v1(),
            cache: None,
            record_count: None,
        })
    }

    fn fallback_error(reader: R, limits: &ReadLimits) -> FormatError {
        Self::diagnose(reader, limits)
            .error
            .unwrap_or(FormatError::NoTrailer)
    }

    /// The resources this reader was opened with.
    pub fn resources(&self) -> &Resources {
        &self.resources
    }

    /// The frame payload limit in force for reads after the index.
    pub fn limits(&self) -> &ReadLimits {
        &self.limits
    }

    /// The header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The trailer.
    pub fn trailer(&self) -> &Trailer {
        &self.trailer
    }

    /// The index.
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Constant-time access to the chunk table.
    pub fn chunks(&self) -> &ChunkIndex {
        &self.chunks
    }

    /// The chunk index, shared (for readers that also borrow the archive mutably).
    pub(crate) fn chunks_arc(&self) -> Arc<ChunkIndex> {
        Arc::clone(&self.chunks)
    }

    /// The registry of primitive decoders blocks are decoded with; register
    /// further decoders here before reading.
    pub fn registry_mut(&mut self) -> &mut Registry {
        self.cache = None;
        &mut self.registry
    }

    /// Use `store` for the priors blocks name (the reader never fetches any
    /// itself). Decoders registered earlier are kept, except the zstd decoder,
    /// which is replaced (register a custom zstd decoder after this call).
    pub fn set_priors(&mut self, store: Box<dyn PriorStore>) {
        self.cache = None;
        let old = std::mem::replace(&mut self.registry, Registry::v1());
        self.registry = old.with_priors(store);
    }

    /// The IDs of the priors the archive's blocks need, from the index:
    /// ascending and unique. A tool can say which are needed before decoding.
    pub fn priors(&self) -> &[[u8; 32]] {
        &self.index.priors
    }

    /// The `EntryTable` payload, read and verified on demand; callers parse it
    /// with `EntryTable::parse`.
    pub fn entries(&mut self) -> Result<Vec<u8>, FormatError> {
        let at = self.index.entry_table;
        Ok(self.read_frame_at(at, FrameKind::EntryTable)?.payload)
    }

    /// Read and verify the frame at `at`: its kind must be `expected` and its
    /// whole encoded length must be `at.len`.
    pub fn read_frame_at(
        &mut self,
        at: FrameLocation,
        expected: FrameKind,
    ) -> Result<Frame, FormatError> {
        read_frame_at_impl(&mut self.reader, &self.limits, at, expected)
    }

    /// Walk the frames from the header forward and report where and why the
    /// walk stopped. A walk that reaches the end of the input, or an
    /// incomplete frame, without having seen a trailer ends in
    /// `Truncated { what: "trailer" }`.
    pub fn diagnose(mut reader: R, limits: &ReadLimits) -> Diagnosis {
        let mut d = Diagnosis {
            frames_ok: 0,
            ends_at: 0,
            trailer_seen: false,
            error: None,
        };
        if let Err(e) = reader.seek(SeekFrom::Start(0)) {
            d.error = Some(e.into());
            return d;
        }
        let mut r = Counting {
            inner: BufReader::new(reader),
            n: 0,
        };
        if let Err(e) = Header::read(&mut r) {
            d.error = Some(e);
            return d;
        }
        d.ends_at = r.n;
        loop {
            match Frame::read(&mut r, limits) {
                Ok(None) => {
                    d.error = Some(FormatError::Truncated { what: "trailer" });
                    return d;
                }
                Ok(Some(f)) => {
                    d.frames_ok += 1;
                    d.ends_at = r.n;
                    let is_trailer =
                        matches!(&f, ReadFrame::Known(k) if k.kind == FrameKind::Trailer);
                    if is_trailer {
                        d.trailer_seen = true;
                        let mut one = [0u8; 1];
                        loop {
                            match r.read(&mut one) {
                                Ok(0) => break,
                                Ok(_) => {
                                    d.error = Some(FormatError::TrailingBytes { what: "archive" });
                                    break;
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                                Err(e) => {
                                    d.error = Some(e.into());
                                    break;
                                }
                            }
                        }
                        return d;
                    }
                }
                Err(FormatError::Truncated { .. }) => {
                    d.error = Some(FormatError::Truncated { what: "trailer" });
                    return d;
                }
                Err(e) => {
                    d.error = Some(e);
                    return d;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkRecord, ChunkTableWriter};
    use crate::envelope::Envelope;
    use crate::frame::FrameFlags;
    use crate::header::HeaderFlags;
    use crate::index::BlockLocation;
    use crate::merkle::merkle_root;
    use std::cell::Cell;
    use std::io::Cursor;
    use std::rc::Rc;

    /// A frame of a raw (possibly unknown) kind.
    fn write_raw(out: &mut Vec<u8>, kind: u16, payload: &[u8]) {
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        crate::varint::write(out, payload.len() as u64).unwrap();
        out.extend_from_slice(payload);
        out.extend_from_slice(blake3::hash(payload).as_bytes());
    }

    const ID: [u8; 16] = [0x42; 16];
    const BLOCK_PAYLOAD: usize = 100_000;
    const ENTRY_PAYLOAD: &[u8] = b"placeholder entry table payload";

    struct Built {
        bytes: Vec<u8>,
        entry_off: u64,
        chunk1_off: u64,
        chunk2_off: u64,
        index_off: u64,
        trailer_off: u64,
        index: Index,
        trailer: Trailer,
    }

    fn frame_bytes(kind: FrameKind, payload: Vec<u8>) -> Vec<u8> {
        let mut v = Vec::new();
        Frame {
            kind,
            flags: FrameFlags::EMPTY,
            payload,
        }
        .write(&mut v)
        .unwrap();
        v
    }

    fn build() -> Built {
        build_with(ENTRY_PAYLOAD.to_vec())
    }

    fn build_with(entry_payload: Vec<u8>) -> Built {
        let mut bytes = Vec::new();
        Header::new(HeaderFlags::EMPTY, ID)
            .write(&mut bytes)
            .unwrap();
        let entry_off = bytes.len() as u64;
        let e = frame_bytes(FrameKind::EntryTable, entry_payload);
        bytes.extend_from_slice(&e);
        let placeholder = |seed: u8| -> Vec<u8> {
            (0..BLOCK_PAYLOAD)
                .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
                .collect()
        };
        let chunk1_off = bytes.len() as u64;
        let c1 = frame_bytes(FrameKind::ChunkData, placeholder(1));
        bytes.extend_from_slice(&c1);
        let chunk2_off = bytes.len() as u64;
        let c2 = frame_bytes(FrameKind::ChunkData, placeholder(2));
        bytes.extend_from_slice(&c2);
        let index_off = bytes.len() as u64;

        let recs = [
            ChunkRecord {
                plain_len: 10,
                hash: [1; 32],
            },
            ChunkRecord {
                plain_len: 20,
                hash: [2; 32],
            },
            ChunkRecord {
                plain_len: 0,
                hash: [3; 32],
            },
            ChunkRecord {
                plain_len: 40,
                hash: [4; 32],
            },
        ];
        let leaves: Vec<[u8; 32]> = recs.iter().map(|r| r.hash).collect();
        let blocks = vec![
            BlockLocation {
                frame_offset: chunk1_off,
                frame_len: c1.len() as u64,
                first_chunk: 0,
                chunk_count: 2,
                plain_len: 30,
            },
            BlockLocation {
                frame_offset: chunk2_off,
                frame_len: c2.len() as u64,
                first_chunk: 2,
                chunk_count: 2,
                plain_len: 40,
            },
        ];
        let index = Index {
            chunk_table: ChunkTableWriter::encode(&recs).into(),
            merkle_root: merkle_root(&leaves),
            envelope: Envelope::for_archive(
                &blocks,
                crate::envelope::ArchiveSizes {
                    index_payload_len: 1000,
                    entry_table_len: e.len() as u64,
                    records_len: 0,
                    recovery_len: 0,
                },
                crate::primitive::GraphResources {
                    window: 1 << 20,
                    bwt_block: 1 << 16,
                },
                1 << 24,
                2,
            ),
            priors: vec![],
            blocks,
            entry_table: FrameLocation {
                offset: entry_off,
                len: e.len() as u64,
            },
            records: None,
            recovery: vec![],
        };
        let payload = index.encode().unwrap();
        let index_hash = *blake3::hash(&payload).as_bytes();
        let idx_frame = frame_bytes(FrameKind::Index, payload);
        bytes.extend_from_slice(&idx_frame);
        let trailer_off = bytes.len() as u64;
        let trailer = Trailer {
            index_offset: index_off,
            index_len: idx_frame.len() as u64,
            index_hash,
            generation: 0,
            archive_id: ID,
        };
        trailer.write(&mut bytes).unwrap();
        Built {
            bytes,
            entry_off,
            chunk1_off,
            chunk2_off,
            index_off,
            trailer_off,
            index,
            trailer,
        }
    }

    fn open(bytes: Vec<u8>) -> Result<Archive<Cursor<Vec<u8>>>, FormatError> {
        Archive::open(Cursor::new(bytes), &Resources::default())
    }

    fn diag(bytes: Vec<u8>) -> Diagnosis {
        Archive::diagnose(Cursor::new(bytes), &ReadLimits::default())
    }

    #[derive(Debug)]
    struct CountRead {
        inner: Cursor<Vec<u8>>,
        read: Rc<Cell<u64>>,
    }

    impl Read for CountRead {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let k = self.inner.read(buf)?;
            self.read.set(self.read.get() + k as u64);
            Ok(k)
        }
    }

    impl Seek for CountRead {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    #[test]
    fn opens_without_reading_the_body() {
        let b = build();
        let total = b.bytes.len() as u64;
        let read = Rc::new(Cell::new(0));
        let r = CountRead {
            inner: Cursor::new(b.bytes.clone()),
            read: Rc::clone(&read),
        };
        let a = Archive::open(r, &Resources::default()).unwrap();
        let body = 2 * BLOCK_PAYLOAD as u64;
        assert!(read.get() < body / 10, "read {} of {total}", read.get());
        assert_eq!(a.header().archive_id, ID);
        assert_eq!(a.trailer(), &b.trailer);
        assert_eq!(a.index(), &b.index);
        assert_eq!(a.chunks().len(), 4);
        let p = a.chunks().locate(3).unwrap();
        assert_eq!((p.block, p.offset_in_block, p.plain_len), (1, 0, 40));
        let p = a.chunks().locate(1).unwrap();
        assert_eq!((p.block, p.offset_in_block, p.plain_len), (0, 10, 20));
    }

    fn counted(bytes: &[u8]) -> (CountRead, Rc<Cell<u64>>) {
        let read = Rc::new(Cell::new(0));
        let r = CountRead {
            inner: Cursor::new(bytes.to_vec()),
            read: Rc::clone(&read),
        };
        (r, read)
    }

    #[test]
    fn an_envelope_above_the_local_resources_is_refused_before_any_block_is_read() {
        let b = build();
        let body = 2 * BLOCK_PAYLOAD as u64;
        let env = b.index.envelope;
        type Tweak = fn(&mut Resources, &Envelope);
        let cases: [(Tweak, &str, u64, u64); 5] = [
            (
                |r, e| r.max_window = e.max_window - 1,
                "max_window",
                1 << 20,
                (1 << 20) - 1,
            ),
            (
                |r, e| r.max_bwt_block = e.max_bwt_block - 1,
                "max_bwt_block",
                1 << 16,
                (1 << 16) - 1,
            ),
            (
                |r, e| r.max_block_plain = e.max_block_plain - 1,
                "max_block_plain",
                40,
                39,
            ),
            (
                |r, e| r.max_frame_payload = e.max_frame_payload - 1,
                "max_frame_payload",
                BLOCK_PAYLOAD as u64,
                BLOCK_PAYLOAD as u64 - 1,
            ),
            (
                |r, e| r.memory = e.decode_memory - 1,
                "decode_memory",
                1 << 24,
                (1 << 24) - 1,
            ),
        ];
        for (tweak, field, needed, allowed) in cases {
            let mut res = Resources::default();
            tweak(&mut res, &env);
            let (r, read) = counted(&b.bytes);
            let e = Archive::open(r, &res).unwrap_err();
            match e {
                FormatError::Refused(ref f) => {
                    assert_eq!((f.field, f.needed, f.allowed), (field, needed, allowed));
                }
                other => panic!("{field}: {other:?}"),
            }
            assert!(e.to_string().contains(field), "{e}");
            assert!(read.get() < body / 10, "{field}: read {}", read.get());
        }
    }

    #[test]
    fn larger_resources_open_it_and_the_limits_follow_the_minimum_rule() {
        let b = build();
        let env = b.index.envelope;
        assert_eq!(env.max_frame_payload, BLOCK_PAYLOAD as u64);
        // Exactly the declared values are enough.
        let exact = Resources {
            max_window: env.max_window,
            max_bwt_block: env.max_bwt_block,
            max_block_plain: env.max_block_plain,
            max_frame_payload: env.max_frame_payload,
            memory: env.decode_memory,
        };
        let a = Archive::open(Cursor::new(b.bytes.clone()), &exact).unwrap();
        assert_eq!(a.resources(), &exact);
        assert_eq!(a.limits().max_payload, BLOCK_PAYLOAD as u64);
        // Generous resources: the limit drops to the envelope's value.
        let mut a = open(b.bytes.clone()).unwrap();
        assert_eq!(a.resources(), &Resources::default());
        assert_eq!(
            a.limits().max_payload,
            env.max_frame_payload
                .min(Resources::default().max_frame_payload)
        );
        assert_eq!(a.limits().max_payload, BLOCK_PAYLOAD as u64);
        // Blocks read with the derived limit.
        let blk = a.index().blocks[0];
        a.read_frame_at(
            FrameLocation {
                offset: blk.frame_offset,
                len: blk.frame_len,
            },
            FrameKind::ChunkData,
        )
        .unwrap();
    }

    #[test]
    fn an_entry_table_larger_than_every_block_is_readable_after_open() {
        let big = 3 * BLOCK_PAYLOAD;
        let b = build_with(vec![7u8; big]);
        assert_eq!(b.index.envelope.max_frame_payload, big as u64);
        let mut a = open(b.bytes.clone()).unwrap();
        assert_eq!(a.limits().max_payload, big as u64);
        assert_eq!(a.entries().unwrap().len(), big);
    }

    #[test]
    fn the_frame_limit_while_reading_the_index_is_the_local_one() {
        let b = build();
        let idx_payload = b.index.encode().unwrap().len() as u64;
        let res = Resources {
            max_frame_payload: idx_payload - 1,
            ..Resources::default()
        };
        let e = Archive::open(Cursor::new(b.bytes.clone()), &res).unwrap_err();
        assert!(matches!(e, FormatError::PayloadTooLarge { .. }), "{e:?}");
        let res = Resources {
            max_frame_payload: idx_payload,
            ..Resources::default()
        };
        // The index fits but the blocks do not: refused by the envelope.
        let e = Archive::open(Cursor::new(b.bytes.clone()), &res).unwrap_err();
        assert!(
            matches!(e, FormatError::Refused(ref f) if f.field == "max_frame_payload"),
            "{e:?}"
        );
    }

    #[test]
    fn entries_and_read_frame_at() {
        let b = build();
        let mut a = open(b.bytes.clone()).unwrap();
        assert_eq!(a.entries().unwrap(), ENTRY_PAYLOAD);
        let at = a.index().entry_table;
        assert!(matches!(
            a.read_frame_at(at, FrameKind::ChunkData),
            Err(FormatError::WrongFrameKind {
                expected: 2,
                found: 1
            })
        ));
        let blk = a.index().blocks[1];
        let f = a
            .read_frame_at(
                FrameLocation {
                    offset: blk.frame_offset,
                    len: blk.frame_len,
                },
                FrameKind::ChunkData,
            )
            .unwrap();
        assert_eq!(f.payload.len(), BLOCK_PAYLOAD);
        // A wrong recorded length is a bad location.
        assert!(matches!(
            a.read_frame_at(
                FrameLocation {
                    offset: at.offset,
                    len: at.len + 1
                },
                FrameKind::EntryTable
            ),
            Err(FormatError::BadFrameLocation { .. })
        ));
        // An offset that is not a frame start fails on kind, flags or hash.
        assert!(a
            .read_frame_at(
                FrameLocation {
                    offset: at.offset + 1,
                    len: at.len
                },
                FrameKind::EntryTable
            )
            .is_err());
    }

    #[test]
    fn corrupt_entry_table_is_found_when_asked() {
        let b = build();
        let mut bytes = b.bytes.clone();
        bytes[b.entry_off as usize + 6] ^= 1;
        let mut a = open(bytes).unwrap();
        assert!(matches!(
            a.entries(),
            Err(FormatError::HashMismatch { kind: 1 })
        ));
    }

    fn with_trailer(b: &Built, f: impl FnOnce(&mut Trailer)) -> Vec<u8> {
        let mut t = b.trailer;
        f(&mut t);
        let mut bytes = b.bytes[..b.trailer_off as usize].to_vec();
        t.write(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn trailer_checks() {
        let b = build();
        let bytes = with_trailer(&b, |t| t.index_hash[0] ^= 1);
        assert!(matches!(open(bytes), Err(FormatError::IndexHashMismatch)));
        let bytes = with_trailer(&b, |t| t.archive_id[0] ^= 1);
        assert!(matches!(open(bytes), Err(FormatError::ArchiveIdMismatch)));
        let bytes = with_trailer(&b, |t| t.index_offset = 0);
        assert!(matches!(
            open(bytes),
            Err(FormatError::BadFrameLocation { what: "index" })
        ));
        let bytes = with_trailer(&b, |t| t.index_len += 1);
        assert!(matches!(
            open(bytes),
            Err(FormatError::BadFrameLocation { .. })
        ));
        let bytes = with_trailer(&b, |t| t.index_offset = b.chunk1_off);
        assert!(matches!(
            open(bytes),
            Err(FormatError::WrongFrameKind {
                expected: 5,
                found: 2
            })
        ));
        for (offset, len) in [
            (u64::MAX, b.trailer.index_len),
            (b.trailer.index_offset, u64::MAX),
            (u64::MAX - 10, 20),
            (b.trailer_off, b.trailer.index_len),
        ] {
            let bytes = with_trailer(&b, |t| {
                t.index_offset = offset;
                t.index_len = len;
            });
            assert!(
                matches!(
                    open(bytes),
                    Err(FormatError::BadFrameLocation { what: "index" })
                ),
                "{offset} {len}"
            );
        }
        // A flipped byte inside the index frame is a frame hash failure.
        let mut bytes = b.bytes.clone();
        bytes[b.index_off as usize + 8] ^= 1;
        assert!(matches!(
            open(bytes),
            Err(FormatError::HashMismatch { kind: 5 })
        ));
    }

    #[test]
    fn a_damaged_length_field_is_not_a_truncation() {
        let b = build();
        for bit in [0u8, 1, 3] {
            let mut bytes = b.bytes.clone();
            bytes[b.index_off as usize + 4] ^= 1 << bit;
            let e = open(bytes).unwrap_err();
            assert!(
                !matches!(e, FormatError::Truncated { .. }),
                "bit {bit}: {e:?}"
            );
        }
        // A frame that claims more than its recorded length is a bad location.
        let mut bytes = b.bytes.clone();
        bytes[b.entry_off as usize + 4] = 100;
        let mut a = open(bytes).unwrap();
        assert!(matches!(
            a.entries(),
            Err(FormatError::BadFrameLocation {
                what: "entry table"
            })
        ));
    }

    #[test]
    fn a_trailer_of_the_wrong_shape_is_no_trailer() {
        let b = build();
        let mut bytes = b.bytes[..b.trailer_off as usize].to_vec();
        Frame {
            kind: FrameKind::Trailer,
            flags: FrameFlags::MUST_UNDERSTAND,
            payload: vec![0; 72],
        }
        .write(&mut bytes)
        .unwrap();
        assert!(matches!(open(bytes), Err(FormatError::NoTrailer)));
        let mut bytes = b.bytes[..b.trailer_off as usize].to_vec();
        Frame {
            kind: FrameKind::Trailer,
            flags: FrameFlags::EMPTY,
            payload: vec![0; 70],
        }
        .write(&mut bytes)
        .unwrap();
        assert!(matches!(open(bytes), Err(FormatError::NoTrailer)));
    }

    #[test]
    fn bad_header_is_reported() {
        let b = build();
        let mut bytes = b.bytes.clone();
        bytes[0] ^= 1;
        assert!(matches!(open(bytes), Err(FormatError::BadMagic)));
    }

    #[test]
    fn cut_before_trailer() {
        let b = build();
        let cut = b.bytes[..b.trailer_off as usize].to_vec();
        assert!(matches!(
            open(cut.clone()),
            Err(FormatError::Truncated { what: "trailer" })
        ));
        let d = diag(cut);
        assert_eq!(
            (d.frames_ok, d.ends_at, d.trailer_seen),
            (4, b.trailer_off, false)
        );
        assert!(matches!(
            d.error,
            Some(FormatError::Truncated { what: "trailer" })
        ));
    }

    #[test]
    fn cut_inside_index() {
        let b = build();
        let cut = b.bytes[..b.index_off as usize + 10].to_vec();
        assert!(matches!(
            open(cut.clone()),
            Err(FormatError::Truncated { what: "trailer" })
        ));
        let d = diag(cut);
        assert_eq!((d.frames_ok, d.ends_at), (3, b.index_off));
        assert!(!d.trailer_seen);
    }

    #[test]
    fn cut_inside_chunk_data() {
        let b = build();
        let cut = b.bytes[..b.chunk2_off as usize + 500].to_vec();
        assert!(matches!(
            open(cut.clone()),
            Err(FormatError::Truncated { what: "trailer" })
        ));
        let d = diag(cut);
        assert_eq!((d.frames_ok, d.ends_at), (2, b.chunk2_off));
    }

    #[test]
    fn short_inputs() {
        let b = build();
        // Header only.
        let d = diag(b.bytes[..32].to_vec());
        assert_eq!((d.frames_ok, d.ends_at), (0, 32));
        assert!(matches!(
            open(b.bytes[..32].to_vec()),
            Err(FormatError::Truncated { what: "trailer" })
        ));
        // Cut inside the header.
        assert!(matches!(
            open(b.bytes[..20].to_vec()),
            Err(FormatError::Truncated { .. })
        ));
        assert!(open(Vec::new()).is_err());
    }

    #[test]
    fn flipped_byte_plus_cut_tail_is_corruption() {
        let b = build();
        let mut cut = b.bytes[..b.trailer_off as usize].to_vec();
        cut[b.chunk1_off as usize + 50] ^= 1;
        assert!(matches!(
            open(cut.clone()),
            Err(FormatError::HashMismatch { kind: 2 })
        ));
        let d = diag(cut);
        assert_eq!((d.frames_ok, d.ends_at), (1, b.chunk1_off));
    }

    #[test]
    fn bytes_after_the_trailer() {
        let b = build();
        let mut bytes = b.bytes.clone();
        bytes.extend_from_slice(&[0xEE; 10]);
        assert!(matches!(
            open(bytes.clone()),
            Err(FormatError::TrailingBytes { what: "archive" })
        ));
        let d = diag(bytes);
        assert!(d.trailer_seen);
        assert_eq!((d.frames_ok, d.ends_at), (5, b.bytes.len() as u64));
    }

    #[test]
    fn diagnose_a_good_archive() {
        let b = build();
        let d = diag(b.bytes.clone());
        assert!(d.error.is_none() && d.trailer_seen);
        assert_eq!((d.frames_ok, d.ends_at), (5, b.bytes.len() as u64));
    }

    #[test]
    fn diagnose_skips_unknown_kinds() {
        let b = build();
        let mut bytes = b.bytes[..b.index_off as usize].to_vec();
        let mut unknown = Vec::new();
        write_raw(&mut unknown, 0x9000, b"future");
        bytes.extend_from_slice(&unknown);
        let d = diag(bytes);
        assert_eq!(d.frames_ok, 4);
        assert!(matches!(
            d.error,
            Some(FormatError::Truncated { what: "trailer" })
        ));
    }

    #[test]
    fn corrupt_frame_midway_is_reported_with_position() {
        let b = build();
        let mut bytes = b.bytes[..b.trailer_off as usize - 3].to_vec();
        bytes[b.entry_off as usize + 6] ^= 1;
        let d = diag(bytes);
        assert_eq!((d.frames_ok, d.ends_at), (0, b.entry_off));
        assert!(matches!(
            d.error,
            Some(FormatError::HashMismatch { kind: 1 })
        ));
    }
}
