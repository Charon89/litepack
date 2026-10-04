//! Opening an archive from its tail, and diagnosing a damaged one (spec section 6).

use crate::chunk::{ChunkIndex, ChunkTableWriter};
use crate::crypto::{
    index_sequence, sealing_rule, Credentials, KeySlot, Sealer, Suite, KEY_SLOT_LEN,
};
use crate::decode::Registry;
use crate::envelope::{Envelope, Refusal, Resources};
use crate::error::FormatError;
use crate::frame::{Frame, FrameFlags, FrameKind, ReadFrame, ReadLimits};
use crate::header::{Header, HeaderFlags};
use crate::index::{FrameLocation, GenerationInfo, Index};
use crate::merkle::merkle_root;
use crate::priors::PriorStore;
use crate::trailer::{Trailer, TRAILER_FRAME_LEN};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
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
    /// Opens sealed frames; `None` for an archive that is not encrypted and for
    /// a listable one opened without credentials.
    sealer: Option<Sealer>,
    /// The key slot of an encrypted archive.
    key_slot: Option<KeySlot>,
    /// True for an encrypted archive opened without credentials: the index is
    /// sealed, so only the frame envelopes, the recovery frames and (when
    /// listable) the entry table are available.
    keyless: bool,
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
    /// Offset just after the last trailer frame read (0 when none was): the
    /// end of the last complete generation, where a rollback truncates.
    pub last_trailer_end: u64,
    /// Why the walk stopped: `Truncated { what: "trailer" }` for a clean
    /// truncation, the frame's own error for corruption, `TrailingBytes {
    /// what: "archive" }` for bytes after a trailer; `None` when the input
    /// ends exactly after a trailer.
    pub error: Option<FormatError>,
}

/// One generation of the trailer chain (spec section 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation {
    /// The generation number: 0 for the first write.
    pub generation: u64,
    /// Absolute offset of this generation's trailer frame.
    pub trailer_offset: u64,
    /// Absolute offset of this generation's index frame.
    pub index_offset: u64,
    /// BLAKE3-256 of the index frame's payload, from the trailer.
    pub index_hash: [u8; 32],
}

impl Generation {
    /// The generation `t` describes, whose trailer frame is at `trailer_offset`.
    pub(crate) fn of(t: &Trailer, trailer_offset: u64) -> Generation {
        Generation {
            generation: t.generation,
            trailer_offset,
            index_offset: t.index_offset,
            index_hash: t.index_hash,
        }
    }

    /// Offset just after this generation's trailer: the archive's length when
    /// it is the latest.
    pub fn end(&self) -> u64 {
        self.trailer_offset + TRAILER_FRAME_LEN
    }
}

/// Walk the trailer chain from `trailer` (the frame at `offset`) back to
/// generation 0, newest first. Every step checks the archive id, that the
/// generation falls by one, that the previous trailer lies before the index
/// and that each index frame ends where its trailer starts.
pub(crate) fn walk_chain<R: Read + Seek>(
    reader: &mut R,
    archive_id: [u8; 16],
    mut trailer: Trailer,
    mut offset: u64,
) -> Result<Vec<Generation>, FormatError> {
    let mut out = Vec::new();
    loop {
        if trailer.index_offset.checked_add(trailer.index_len) != Some(offset) {
            return Err(FormatError::BadFrameLocation { what: "index" });
        }
        out.push(Generation::of(&trailer, offset));
        if trailer.generation == 0 {
            if trailer.previous_trailer_offset != 0 {
                return Err(FormatError::BadTrailer {
                    reason: "previous_trailer_offset",
                });
            }
            return Ok(out);
        }
        let prev = trailer.previous_trailer_offset;
        if prev.saturating_add(TRAILER_FRAME_LEN) > trailer.index_offset {
            return Err(FormatError::BadFrameLocation { what: "trailer" });
        }
        let t = Trailer::read_at(reader, prev)?;
        if t.archive_id != archive_id {
            return Err(FormatError::ArchiveIdMismatch);
        }
        if t.generation != trailer.generation - 1 {
            return Err(FormatError::GenerationMismatch {
                expected: trailer.generation - 1,
                found: t.generation,
            });
        }
        trailer = t;
        offset = prev;
    }
}

/// The generation-table rules that need the trailer (spec section 15), in
/// this order: `g + 1` entries for a trailer of generation `g` (`count`); the
/// last entry's salt is the trailer's (`salt`); in a later generation the last
/// entry starts right after the previous trailer (`start_offset`); in an
/// archive that is not encrypted every salt is zero (`salt not zero`).
/// The rules of the table alone are checked when the index is parsed.
fn check_generation_table(
    generations: &[GenerationInfo],
    trailer: &Trailer,
    encrypted: bool,
) -> Result<(), FormatError> {
    let bad = |reason| Err(FormatError::BadGenerationTable { reason });
    let Some(last) = generations.last() else {
        return bad("count");
    };
    if generations.len() as u64 != trailer.generation.saturating_add(1) {
        return bad("count");
    }
    if last.salt != trailer.salt {
        return bad("salt");
    }
    if trailer.generation > 0
        && Some(last.start_offset)
            != trailer
                .previous_trailer_offset
                .checked_add(TRAILER_FRAME_LEN)
    {
        return bad("start_offset");
    }
    if !encrypted && generations.iter().any(|g| g.salt != [0; 16]) {
        return bad("salt not zero");
    }
    Ok(())
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

/// Check a frame's `SEALED` flag against the archive's sealing rules.
fn check_sealing(encrypted: bool, listable: bool, frame: &Frame) -> Result<(), FormatError> {
    let kind = frame.kind as u16;
    let sealed = frame.flags.contains(FrameFlags::SEALED);
    if frame.kind == FrameKind::KeySlot && !encrypted {
        return Err(FormatError::UnexpectedKeySlot);
    }
    if !encrypted {
        return if sealed {
            Err(FormatError::UnexpectedSealedFrame { kind })
        } else {
            Ok(())
        };
    }
    match sealing_rule(kind, listable) {
        Some(true) if !sealed => Err(FormatError::UnsealedFrame { kind }),
        Some(false) if sealed => Err(FormatError::UnexpectedSealedFrame { kind }),
        _ => Ok(()),
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

/// Open a frame read from disk: check its `SEALED` flag against the rules and
/// open the payload when it is sealed.
fn unseal(
    sealer: Option<&Sealer>,
    encrypted: bool,
    listable: bool,
    frame: Frame,
    sequence: u64,
    salt: &[u8; 16],
) -> Result<Frame, FormatError> {
    check_sealing(encrypted, listable, &frame)?;
    if !frame.flags.contains(FrameFlags::SEALED) {
        return Ok(frame);
    }
    let sealer = sealer.ok_or(FormatError::PasswordRequired)?;
    let payload = sealer.open(
        &frame.payload,
        frame.kind as u16,
        sequence,
        salt,
        frame.payload.len() as u64,
    )?;
    Ok(Frame {
        kind: frame.kind,
        flags: FrameFlags::EMPTY,
        payload,
    })
}

/// Read the key slot frame that must follow the header; returns it parsed and
/// the offset after it. Nothing beyond the key slot is read.
fn read_key_slot<R: Read + Seek>(reader: &mut R) -> Result<(KeySlot, u64), FormatError> {
    reader.seek(SeekFrom::Start(Header::LEN as u64))?;
    let mut kind = [0u8; 2];
    match reader.read_exact(&mut kind) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(FormatError::MissingKeySlot)
        }
        Err(e) => return Err(e.into()),
    }
    if u16::from_le_bytes(kind) != FrameKind::KeySlot as u16 {
        return Err(FormatError::MissingKeySlot);
    }
    reader.seek(SeekFrom::Start(Header::LEN as u64))?;
    let limits = ReadLimits {
        max_payload: KEY_SLOT_LEN as u64,
    };
    let frame = match Frame::read(&mut *reader, &limits) {
        Ok(Some(ReadFrame::Known(f))) => f,
        Ok(_) => return Err(FormatError::MissingKeySlot),
        Err(FormatError::PayloadTooLarge { .. }) => {
            return Err(FormatError::BadKeySlot { reason: "length" })
        }
        Err(e) => return Err(e),
    };
    if frame.flags.contains(FrameFlags::SEALED) {
        return Err(FormatError::UnexpectedSealedFrame {
            kind: FrameKind::KeySlot as u16,
        });
    }
    let end = Header::LEN as u64 + frame.encoded_len();
    Ok((KeySlot::parse(&frame.payload)?, end))
}

/// Walk the frame envelopes from `start` (the first frame after the key slot,
/// sequence 1) to the trailer, skipping payloads by their declared length
/// without reading them. Returns the entry table's location (the first
/// `EntryTable` frame) and the locations of the recovery frames; a frame whose
/// payload is damaged is still skipped, the sequence count continuing. An
/// archive with several generations has several entry tables and trailers: the
/// last entry table is the current one, the walk runs to the last trailer, and
/// the recovery frames of every generation are returned.
/// The entry table's location, the recovery frames and the generation starts.
type EnvelopeWalk = (Option<FrameLocation>, Vec<FrameLocation>, Vec<u64>);

fn walk_envelopes<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    file_len: u64,
) -> Result<EnvelopeWalk, FormatError> {
    let mut pos = start;
    // Where each generation starts: after the header, then after each trailer.
    let mut starts = vec![Header::LEN as u64];
    let mut sequence = 1u64;
    let mut entry = None;
    let mut recovery = Vec::new();
    loop {
        let trunc = FormatError::Truncated { what: "frames" };
        if pos.saturating_add(5) > file_len {
            return Err(trunc);
        }
        reader.seek(SeekFrom::Start(pos))?;
        let mut head = [0u8; 4];
        reader.read_exact(&mut head)?;
        let kind = u16::from_le_bytes([head[0], head[1]]);
        let payload_len = crate::varint::read(&mut *reader)?;
        let frame_len = (4 + crate::varint::len(payload_len) as u64)
            .checked_add(payload_len)
            .and_then(|n| n.checked_add(32))
            .ok_or(FormatError::BadFrameLocation { what: "frames" })?;
        let end = pos.saturating_add(frame_len);
        if end > file_len {
            return Err(trunc);
        }
        match FrameKind::from_u16(kind) {
            Some(FrameKind::EntryTable) => {
                entry = Some(FrameLocation {
                    offset: pos,
                    len: frame_len,
                    sequence,
                });
            }
            Some(FrameKind::Recovery) => recovery.push(FrameLocation {
                offset: pos,
                len: frame_len,
                sequence,
            }),
            Some(FrameKind::KeySlot) => return Err(FormatError::UnexpectedKeySlot),
            // The last trailer ends the file; an earlier one ends a generation.
            Some(FrameKind::Trailer) if end == file_len => return Ok((entry, recovery, starts)),
            Some(FrameKind::Trailer) => starts.push(end),
            _ => {}
        }
        pos = end;
        sequence += 1;
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
    pub fn open(reader: R, resources: &Resources) -> Result<Self, FormatError> {
        Self::open_with(reader, resources, None)
    }

    /// Like [`Archive::open`], with the credentials of an encrypted archive
    /// (spec section 14). For an encrypted archive the key slot is read right
    /// after the header and opened first: a wrong password is `WrongKey` before
    /// any other frame is read, and a key slot whose Argon2 memory exceeds
    /// `resources.memory` is `Refused` (field `argon2_m`) before it is derived.
    /// Without credentials an encrypted archive opens keyless: the frames are
    /// found by walking their envelopes, so recovery (`check_recovery`,
    /// `repair`) and `verify` work on the sealed bytes (`verify` checks every
    /// frame's hash but the recovery frames' only); a listable archive's entry
    /// table can be read; everything that needs the index or a sealed frame
    /// (`extract`, the chunk table, a sealed entry table) is `PasswordRequired`. Credentials given for an
    /// archive that is not encrypted are ignored.
    pub fn open_with(
        mut reader: R,
        resources: &Resources,
        credentials: Option<&Credentials>,
    ) -> Result<Self, FormatError> {
        let mut limits = ReadLimits {
            max_payload: resources.max_frame_payload,
        };
        let len = reader.seek(SeekFrom::End(0))?;
        if len < Header::LEN as u64 + TRAILER_FRAME_LEN {
            return Err(Self::fallback_error(reader, &limits));
        }
        reader.seek(SeekFrom::Start(0))?;
        let header = Header::read(&mut reader)?;
        let encrypted = header.flags.contains(HeaderFlags::ENCRYPTED);
        let listable = header.flags.contains(HeaderFlags::LISTABLE);
        let mut sealer = None;
        let mut key_slot = None;
        let mut keyless = false;
        let mut after_slot = Header::LEN as u64;
        if encrypted {
            let (slot, end) = read_key_slot(&mut reader)?;
            after_slot = end;
            match credentials {
                Some(c) => {
                    let needed = u64::from(slot.argon2.m_kib) * 1024;
                    if needed > resources.memory {
                        return Err(FormatError::Refused(Refusal {
                            field: "argon2_m",
                            needed,
                            allowed: resources.memory,
                        }));
                    }
                    let key = slot.unwrap(&header.archive_id, header.flags.bits(), c)?;
                    sealer = Some(Sealer::new(slot.suite, key, header.archive_id));
                }
                None => keyless = true,
            }
            key_slot = Some(slot);
        } else {
            let mut kind = [0u8; 2];
            reader.seek(SeekFrom::Start(Header::LEN as u64))?;
            if reader.read_exact(&mut kind).is_ok()
                && u16::from_le_bytes(kind) == FrameKind::KeySlot as u16
            {
                return Err(FormatError::UnexpectedKeySlot);
            }
        }
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
        // Every generation holds at least a trailer frame.
        if trailer.generation > len / TRAILER_FRAME_LEN {
            return Err(FormatError::BadTrailer {
                reason: "generation",
            });
        }
        if trailer.generation == 0 && trailer.previous_trailer_offset != 0 {
            return Err(FormatError::BadTrailer {
                reason: "previous_trailer_offset",
            });
        }
        if keyless {
            let (entry_table, recovery, starts) = walk_envelopes(&mut reader, after_slot, len)?;
            // A listable archive's clear entry table must be there; a sealed one
            // cannot be read without the key and its location is not needed.
            let entry_table = match entry_table {
                Some(l) => l,
                None if listable => {
                    return Err(FormatError::BadFrameLocation {
                        what: "entry table",
                    })
                }
                None => FrameLocation {
                    offset: 0,
                    len: 0,
                    sequence: 0,
                },
            };
            // Only the offsets are known without the key: they are what the
            // recovery scan needs.
            let generations = (0u64..)
                .zip(&starts)
                .map(|(generation, &start_offset)| GenerationInfo {
                    generation,
                    start_offset,
                    first_sequence: 0,
                    salt: trailer.salt,
                })
                .collect();
            let index = Index {
                chunk_table: ChunkTableWriter::encode(&[]).into(),
                merkle_root: merkle_root(&[]),
                envelope: Envelope {
                    max_window: 0,
                    max_bwt_block: 0,
                    max_block_plain: 0,
                    max_frame_payload: resources.max_frame_payload,
                    decode_memory: 0,
                    threads_hint: 0,
                },
                priors: Vec::new(),
                blocks: Vec::new(),
                entry_table,
                entry_table_hash: [0; 32],
                records: None,
                recovery,
                generations,
            };
            let chunks = index.chunk_index()?;
            return Ok(Archive {
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
                sealer: None,
                key_slot,
                keyless: true,
            });
        }
        let at = FrameLocation {
            offset: trailer.index_offset,
            len: trailer.index_len,
            sequence: index_sequence(trailer.generation),
        };
        // The index ends exactly where its trailer starts.
        if !at.fits_below(len - TRAILER_FRAME_LEN)
            || at.offset.checked_add(at.len) != Some(len - TRAILER_FRAME_LEN)
        {
            return Err(FormatError::BadFrameLocation { what: "index" });
        }
        let raw = read_frame_at_impl(&mut reader, &limits, at, FrameKind::Index)?;
        // The trailer's hash covers the payload as stored: the sealed bytes.
        if blake3::hash(&raw.payload).as_bytes() != &trailer.index_hash {
            return Err(FormatError::IndexHashMismatch);
        }
        let frame = unseal(
            sealer.as_ref(),
            encrypted,
            listable,
            raw,
            index_sequence(trailer.generation),
            &trailer.salt,
        )?;
        let (index, chunks) = Index::parse_with_chunks_in(
            &frame.payload,
            trailer.index_offset,
            trailer.generation,
            trailer.previous_trailer_offset,
        )?;
        check_generation_table(&index.generations, &trailer, encrypted)?;
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
            sealer,
            key_slot,
            keyless: false,
        })
    }

    /// True when the archive is encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.header.flags.contains(HeaderFlags::ENCRYPTED)
    }

    /// True when the archive's entry table is in clear.
    pub fn is_listable(&self) -> bool {
        self.header.flags.contains(HeaderFlags::LISTABLE)
    }

    /// The cipher suite of an encrypted archive.
    pub fn suite(&self) -> Option<Suite> {
        self.key_slot.as_ref().map(|k| k.suite)
    }

    /// The key slot of an encrypted archive.
    pub fn key_slot(&self) -> Option<&KeySlot> {
        self.key_slot.as_ref()
    }

    /// True for an encrypted archive opened without credentials: the index is
    /// sealed, so only the frame envelopes, the recovery frames and (when
    /// listable) the entry table are available.
    pub fn is_keyless(&self) -> bool {
        self.keyless
    }

    /// Fail with `PasswordRequired` when the archive was opened keyless (no index).
    pub(crate) fn need_index(&self) -> Result<(), FormatError> {
        if self.keyless {
            Err(FormatError::PasswordRequired)
        } else {
            Ok(())
        }
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

    /// The generation this archive was opened at: 0 for the first write, one
    /// more for every append.
    pub fn generation(&self) -> u64 {
        self.trailer.generation
    }

    /// The trailer chain, newest first: this generation, then each earlier one
    /// down to generation 0. The chain is read from the file (one trailer
    /// frame per generation); an earlier trailer that is damaged, names
    /// another archive or breaks the numbering is an error.
    pub fn history(&mut self) -> Result<Vec<Generation>, FormatError> {
        let len = self.reader.seek(SeekFrom::End(0))?;
        let at = len
            .checked_sub(TRAILER_FRAME_LEN)
            .ok_or(FormatError::NoTrailer)?;
        walk_chain(&mut self.reader, self.header.archive_id, self.trailer, at)
    }

    /// The salt of the generation that wrote the frame with this `sequence`.
    pub(crate) fn salt_for(&self, sequence: u64) -> [u8; 16] {
        let g = &self.index.generations;
        let n = g.partition_point(|x| x.first_sequence <= sequence);
        n.checked_sub(1).map_or(self.trailer.salt, |i| g[i].salt)
    }

    /// The index.
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// The underlying reader, for the recovery scan.
    pub(crate) fn raw_reader(&mut self) -> &mut R {
        &mut self.reader
    }

    /// Locations of the `Recovery` frames, as the index lists them.
    pub fn recovery_frames(&self) -> &[FrameLocation] {
        &self.index.recovery
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
        let raw = read_frame_at_impl(&mut self.reader, &self.limits, at, FrameKind::EntryTable)?;
        // The index (when we have it) vouches for the table as stored; a keyless
        // reader has no index and the table is unauthenticated.
        if !self.keyless && blake3::hash(&raw.payload).as_bytes() != &self.index.entry_table_hash {
            return Err(FormatError::EntryTableMismatch);
        }
        let frame = unseal(
            self.sealer.as_ref(),
            self.is_encrypted(),
            self.is_listable(),
            raw,
            at.sequence,
            &self.salt_for(at.sequence),
        )?;
        Ok(frame.payload)
    }

    /// Read and verify the frame at `at`: its kind must be `expected` and its
    /// whole encoded length must be `at.len`. A sealed frame is opened with
    /// the archive key (`at.sequence` names its position); the payload
    /// returned is the plain one.
    pub fn read_frame_at(
        &mut self,
        at: FrameLocation,
        expected: FrameKind,
    ) -> Result<Frame, FormatError> {
        let raw = read_frame_at_impl(&mut self.reader, &self.limits, at, expected)?;
        unseal(
            self.sealer.as_ref(),
            self.is_encrypted(),
            self.is_listable(),
            raw,
            at.sequence,
            &self.salt_for(at.sequence),
        )
    }

    /// Walk the frames from the header forward and report where and why the
    /// walk stopped. A walk that reaches the end of the input, or an
    /// incomplete frame, without having seen a trailer ends in
    /// `Truncated { what: "trailer" }`.
    ///
    /// The walk also applies the sealing rules that need no key: the key slot
    /// is the first frame of an encrypted archive and appears nowhere else,
    /// and every frame's `SEALED` flag is the one the rules give its kind.
    pub fn diagnose(mut reader: R, limits: &ReadLimits) -> Diagnosis {
        Self::walk(&mut reader, limits, None)
    }

    /// [`Archive::diagnose`] over a borrowed reader; the locations of the
    /// recovery frames seen are pushed to `recovery` when it is given.
    pub(crate) fn walk(
        reader: &mut R,
        limits: &ReadLimits,
        recovery: Option<&mut Vec<FrameLocation>>,
    ) -> Diagnosis {
        Self::walk_with(reader, limits, recovery, false)
    }

    /// [`Archive::walk`]; with `skip_recovery_hash` a `Recovery` frame whose
    /// hash fails is passed over like a good frame (recovery is `check`'s
    /// business, not `verify`'s; spec section 9).
    pub(crate) fn walk_with(
        reader: &mut R,
        limits: &ReadLimits,
        mut recovery: Option<&mut Vec<FrameLocation>>,
        skip_recovery_hash: bool,
    ) -> Diagnosis {
        let mut d = Diagnosis {
            frames_ok: 0,
            ends_at: 0,
            trailer_seen: false,
            last_trailer_end: 0,
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
        let header = match Header::read(&mut r) {
            Ok(h) => h,
            Err(e) => {
                d.error = Some(e);
                return d;
            }
        };
        let encrypted = header.flags.contains(HeaderFlags::ENCRYPTED);
        let listable = header.flags.contains(HeaderFlags::LISTABLE);
        d.ends_at = r.n;
        // True for the first frame after a trailer: bytes there that are not a
        // frame are trailing garbage, not an interrupted append.
        let mut first_after_trailer = false;
        loop {
            let start = r.n;
            match Frame::read(&mut r, limits) {
                Ok(None) => {
                    d.error = Some(FormatError::Truncated { what: "trailer" });
                    return d;
                }
                Ok(Some(f)) => {
                    let sequence = d.frames_ok;
                    let rules = match &f {
                        ReadFrame::Known(k) => {
                            let first_ok =
                                !encrypted || sequence > 0 || k.kind == FrameKind::KeySlot;
                            if !first_ok {
                                Err(FormatError::MissingKeySlot)
                            } else if k.kind == FrameKind::KeySlot && sequence > 0 {
                                Err(FormatError::UnexpectedKeySlot)
                            } else {
                                check_sealing(encrypted, listable, k)
                            }
                        }
                        ReadFrame::Unknown { .. } => Ok(()),
                    };
                    if let Err(e) = rules {
                        d.error = Some(e);
                        return d;
                    }
                    if let (Some(list), ReadFrame::Known(k)) = (recovery.as_deref_mut(), &f) {
                        if k.kind == FrameKind::Recovery {
                            list.push(FrameLocation {
                                offset: start,
                                len: r.n - start,
                                sequence,
                            });
                        }
                    }
                    d.frames_ok += 1;
                    d.ends_at = r.n;
                    let is_trailer =
                        matches!(&f, ReadFrame::Known(k) if k.kind == FrameKind::Trailer);
                    first_after_trailer = false;
                    if is_trailer {
                        d.trailer_seen = true;
                        d.last_trailer_end = r.n;
                        // More bytes after a trailer are the next generation.
                        match r.inner.fill_buf() {
                            Ok([]) => return d,
                            Ok(_) => first_after_trailer = true,
                            Err(e) => {
                                d.error = Some(e.into());
                                return d;
                            }
                        }
                    }
                }
                Err(FormatError::Truncated { .. }) => {
                    d.error = Some(FormatError::Truncated { what: "trailer" });
                    return d;
                }
                // The whole frame was read before its hash failed, so the
                // walk goes on after it.
                Err(FormatError::HashMismatch { kind })
                    if skip_recovery_hash && kind == FrameKind::Recovery as u16 =>
                {
                    d.frames_ok += 1;
                    d.ends_at = r.n;
                    first_after_trailer = false;
                }
                Err(_) if first_after_trailer => {
                    d.error = Some(FormatError::TrailingBytes { what: "archive" });
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

/// The Markdown table of the generation rules, pasted verbatim into the spec
/// (section 15).
pub fn generation_rules_table() -> String {
    String::from(
        "| Rule | Value |\n|---|---|\n\
         | generation number | 0 for the first write; an append writes the previous number plus one |\n\
         | trailer length | 133 bytes (payload 96), the same for every generation |\n\
         | previous_trailer_offset | offset of the previous generation's trailer frame; 0 in generation 0 |\n\
         | generation salt | 16 random bytes per generation (zeros when not encrypted), in the trailer and in the index's generation table |\n\
         | index | complete: every chunk, block, recovery frame and prior of every generation, and the generation table |\n\
         | entry table | complete: the entries of the new generation, in sorted order |\n\
         | frame sequence | the position of the frame in the file, counting every frame; a new generation continues after the previous trailer |\n\
         | index sequence | 2^64 - 1 - generation |\n\
         | nonce | derived from the key, the archive id, kind, sequence and the salt of the generation that wrote the frame |\n\
         | deduplication | a new chunk with the BLAKE3 and length of a chunk of the old table is referenced, not written |\n\
         | recovery | a generation's recovery frames cover only its own data frames |\n\
         | commit | the append is committed when the last byte of the new trailer is written, after the data is synced |\n\
         | rollback | truncate the file to the end of an earlier generation's trailer |\n",
    )
}

/// Roll the archive in `file` back to `generation`: truncate the file to the
/// end of that generation's trailer (spec section 15). No credentials are
/// needed, nothing is decrypted. The file's last complete trailer is found
/// from the tail, or by walking the frames when the tail is not a trailer (an
/// append that was cut short), so rolling back to the latest generation
/// repairs a half-appended file. The chain is checked on the way: every
/// trailer on it must verify, name this archive and number the generations
/// down by one. Asking for a generation above the latest is
/// `NoSuchGeneration`. Returns the generation the file now ends with.
pub fn rollback(file: &mut std::fs::File, generation: u64) -> Result<Generation, FormatError> {
    let len = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(0))?;
    let header = Header::read(&mut *file)?;
    let (latest, at) = match Trailer::read_tail(&mut *file, len) {
        Ok(t) => (t, len - TRAILER_FRAME_LEN),
        Err(
            FormatError::NoTrailer
            | FormatError::HashMismatch { .. }
            | FormatError::Truncated { .. },
        ) => {
            let mut r = &mut *file;
            // No frame is longer than the file.
            let limits = ReadLimits { max_payload: len };
            let d = Archive::<&mut std::fs::File>::walk(&mut r, &limits, None);
            if d.last_trailer_end < TRAILER_FRAME_LEN {
                return Err(d.error.unwrap_or(FormatError::NoTrailer));
            }
            let at = d.last_trailer_end - TRAILER_FRAME_LEN;
            (Trailer::read_at(&mut *file, at)?, at)
        }
        Err(e) => return Err(e),
    };
    if latest.archive_id != header.archive_id {
        return Err(FormatError::ArchiveIdMismatch);
    }
    let chain = walk_chain(&mut *file, header.archive_id, latest, at)?;
    let target = chain
        .iter()
        .find(|g| g.generation == generation)
        .copied()
        .ok_or(FormatError::NoSuchGeneration {
            requested: generation,
            latest: latest.generation,
        })?;
    file.set_len(target.end())?;
    file.sync_all()?;
    Ok(target)
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
        let entry_hash = *blake3::hash(&entry_payload).as_bytes();
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
                sequence: 0,
            },
            BlockLocation {
                frame_offset: chunk2_off,
                frame_len: c2.len() as u64,
                first_chunk: 2,
                chunk_count: 2,
                plain_len: 40,
                sequence: 0,
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
                sequence: 0,
            },
            entry_table_hash: entry_hash,
            records: None,
            recovery: vec![],
            generations: vec![crate::index::GenerationInfo {
                generation: 0,
                start_offset: 32,
                first_sequence: 0,
                salt: [0; 16],
            }],
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
            previous_trailer_offset: 0,
            salt: [0; 16],
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
                sequence: 0,
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
                    sequence: 0,
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
                    len: at.len + 1,
                    sequence: 0,
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
                    len: at.len,
                    sequence: 0,
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
        // An index that does not end where the trailer starts is a bad
        // location (spec section 6), checked before the kind.
        let bytes = with_trailer(&b, |t| t.index_offset = b.chunk1_off);
        assert!(matches!(
            open(bytes),
            Err(FormatError::BadFrameLocation { what: "index" })
        ));
        let bytes = with_trailer(&b, |t| {
            t.index_offset = b.chunk1_off;
            t.index_len = b.trailer_off - b.chunk1_off;
        });
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

    /// Spec section 3 (E1-14d ruling 5): MUST_UNDERSTAND on a known kind is
    /// accepted and ignored; the trailer alone must have empty flags.
    #[test]
    fn must_understand_on_a_known_kind_is_ignored() {
        let b = build();
        let mut bytes = b.bytes.clone();
        bytes[b.entry_off as usize + 2] |= 1;
        let mut a = open(bytes).unwrap();
        assert!(a.entries().is_ok());
        let clean = open(b.bytes.clone()).unwrap().verify().map(|_| ());
        let r = a.verify().map(|_| ());
        assert_eq!(format!("{r:?}"), format!("{clean:?}"));
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
