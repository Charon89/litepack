//! Reconstruction records: the `Records` frame and the six record types
//! (spec section 12).
//!
//! A record holds the side information one reconstruction primitive needs to
//! rebuild the original bytes of a peeled stream. This crate parses, validates
//! and hashes records; applying them belongs to the full reader.

use crate::error::FormatError;
use crate::primitive::PrimitiveId;
use crate::varint;

const WHAT: &str = "records";
const BODY: &str = "record body";
const HASH_LEN: usize = 32;
/// Smallest bytes per record the count bound assumes: the count of a `Records`
/// payload may not exceed the remaining bytes divided by this.
pub const RECORD_COUNT_BOUND: usize = 5;

/// The kind of a record: the id of the reconstruction primitive it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum RecordKind {
    /// JPEG (primitive 7).
    Jpeg = 7,
    /// Deflate stream (primitive 8).
    Deflate = 8,
    /// PNG filter (primitive 9).
    PngFilter = 9,
    /// Base64 text (primitive 10).
    Base64 = 10,
    /// UTF-16 text (primitive 11).
    Utf16 = 11,
    /// Container (primitive 12).
    Container = 12,
}

impl RecordKind {
    /// Every kind, in id order.
    pub const ALL: [RecordKind; 6] = [
        RecordKind::Jpeg,
        RecordKind::Deflate,
        RecordKind::PngFilter,
        RecordKind::Base64,
        RecordKind::Utf16,
        RecordKind::Container,
    ];

    /// The kind with this raw id, if known.
    pub fn from_u16(kind: u16) -> Option<RecordKind> {
        match kind {
            7 => Some(RecordKind::Jpeg),
            8 => Some(RecordKind::Deflate),
            9 => Some(RecordKind::PngFilter),
            10 => Some(RecordKind::Base64),
            11 => Some(RecordKind::Utf16),
            12 => Some(RecordKind::Container),
            _ => None,
        }
    }

    /// The name used in the spec.
    pub fn name(self) -> &'static str {
        match self {
            RecordKind::Jpeg => "jpeg",
            RecordKind::Deflate => "deflate",
            RecordKind::PngFilter => "png-filter",
            RecordKind::Base64 => "base64",
            RecordKind::Utf16 => "utf16",
            RecordKind::Container => "container",
        }
    }

    /// The reconstruction primitive this kind belongs to.
    pub fn primitive(self) -> PrimitiveId {
        match self {
            RecordKind::Jpeg => PrimitiveId::JpegReconstruct,
            RecordKind::Deflate => PrimitiveId::DeflateReconstruct,
            RecordKind::PngFilter => PrimitiveId::PngFilter,
            RecordKind::Base64 => PrimitiveId::Base64,
            RecordKind::Utf16 => PrimitiveId::Utf16,
            RecordKind::Container => PrimitiveId::ContainerReconstruct,
        }
    }
}

// ---------------------------------------------------------------- byte I/O

/// Cursor over a record body; every failure names the record.
struct Rd<'a> {
    s: &'a [u8],
    record: u64,
}

impl<'a> Rd<'a> {
    fn new(s: &'a [u8], record: u64) -> Self {
        Rd { s, record }
    }

    fn truncated() -> FormatError {
        FormatError::Truncated { what: WHAT }
    }

    fn bad(&self, reason: &'static str) -> FormatError {
        FormatError::BadRecord {
            record: self.record,
            reason,
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], FormatError> {
        let (a, b) = self.s.split_at_checked(n).ok_or_else(Self::truncated)?;
        self.s = b;
        Ok(a)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], FormatError> {
        self.take(N)?.try_into().map_err(|_| Self::truncated())
    }

    fn varint(&mut self) -> Result<u64, FormatError> {
        match varint::read(&mut self.s) {
            Err(FormatError::Truncated { .. }) => Err(Self::truncated()),
            other => other,
        }
    }

    fn u8(&mut self) -> Result<u8, FormatError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, FormatError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, FormatError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, FormatError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn hash(&mut self) -> Result<[u8; HASH_LEN], FormatError> {
        self.array()
    }

    /// A byte string: varint length (bounded by the bytes that remain), then the bytes.
    fn bytes(&mut self) -> Result<Vec<u8>, FormatError> {
        let len = self.varint()?;
        let len = usize::try_from(len)
            .ok()
            .filter(|&l| l <= self.s.len())
            .ok_or_else(Self::truncated)?;
        Ok(self.take(len)?.to_vec())
    }

    /// An item count, bounded by the remaining bytes at `min_item` bytes each.
    fn count(&mut self, min_item: usize) -> Result<usize, FormatError> {
        let n = self.varint()?;
        usize::try_from(n)
            .ok()
            .filter(|&n| n <= self.s.len() / min_item)
            .ok_or_else(Self::truncated)
    }

    fn chunks(&mut self) -> Result<Vec<u64>, FormatError> {
        let n = self.count(1)?;
        (0..n).map(|_| self.varint()).collect()
    }

    /// An enumeration byte that must be at most `max`.
    fn small(&mut self, max: u8, field: &'static str) -> Result<u8, FormatError> {
        let v = self.u8()?;
        if v > max {
            return Err(self.bad(field));
        }
        Ok(v)
    }

    fn finish(&self) -> Result<(), FormatError> {
        if self.s.is_empty() {
            Ok(())
        } else {
            Err(FormatError::TrailingBytes { what: BODY })
        }
    }
}

fn put_varint(out: &mut Vec<u8>, v: u64) {
    // Writing into a Vec cannot fail.
    let _ = varint::write(out, v);
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn put_chunks(out: &mut Vec<u8>, c: &[u64]) {
    put_varint(out, c.len() as u64);
    for &i in c {
        put_varint(out, i);
    }
}

fn hash_matches(original: &[u8], hash: &[u8; HASH_LEN]) -> bool {
    blake3::hash(original).as_bytes() == hash
}

// ------------------------------------------------------------------- jpeg

/// A secondary image (such as a gain map) inside a JPEG file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondaryImage {
    /// Position inside the original file.
    pub offset: u64,
    /// Length in the original file.
    pub len: u64,
    /// The chunks holding the image as stored.
    pub chunks: Vec<u64>,
}

/// Record of a peeled JPEG file (kind 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegRecord {
    /// Byte length of the JPEG file.
    pub original_len: u64,
    /// Bytes of the primary image up to and including its EOI.
    pub primary_len: u64,
    /// Data after the EOI as stored: raw, or empty when peeled as a nested stream.
    pub trailing: Vec<u8>,
    /// The chunks holding the peeled trailing data; empty when `trailing` is raw.
    pub nested_trailing_chunks: Vec<u64>,
    /// Secondary images.
    pub gainmaps: Vec<SecondaryImage>,
    /// The Lepton format revision the stream was written with (0 = lepton_jpeg 0.5).
    pub lepton_version: u8,
    /// BLAKE3 of the whole original file.
    pub original_hash: [u8; 32],
}

impl JpegRecord {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.original_len.to_le_bytes());
        out.extend_from_slice(&self.primary_len.to_le_bytes());
        put_bytes(out, &self.trailing);
        put_chunks(out, &self.nested_trailing_chunks);
        put_varint(out, self.gainmaps.len() as u64);
        for g in &self.gainmaps {
            out.extend_from_slice(&g.offset.to_le_bytes());
            out.extend_from_slice(&g.len.to_le_bytes());
            put_chunks(out, &g.chunks);
        }
        out.push(self.lepton_version);
        out.extend_from_slice(&self.original_hash);
    }

    /// The body bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    fn read(r: &mut Rd<'_>) -> Result<Self, FormatError> {
        let original_len = r.u64()?;
        let primary_len = r.u64()?;
        if primary_len > original_len {
            return Err(r.bad("primary_len"));
        }
        let trailing = r.bytes()?;
        let nested_trailing_chunks = r.chunks()?;
        if !trailing.is_empty() && !nested_trailing_chunks.is_empty() {
            return Err(r.bad("nested_trailing_chunks"));
        }
        let n = r.count(8 + 8 + 1)?;
        let mut gainmaps = Vec::with_capacity(n);
        // Secondary images lie after the primary image, ascending, not overlapping.
        let mut prev_end = primary_len;
        let mut covered = 0u64;
        for _ in 0..n {
            let offset = r.u64()?;
            let len = r.u64()?;
            let end = offset
                .checked_add(len)
                .filter(|&e| e <= original_len && offset >= prev_end)
                .ok_or_else(|| r.bad("gainmaps"))?;
            prev_end = end;
            covered = covered.saturating_add(len);
            gainmaps.push(SecondaryImage {
                offset,
                len,
                chunks: r.chunks()?,
            });
        }
        let after = original_len - primary_len;
        if !trailing.is_empty() {
            // Raw trailing data is everything after the primary image,
            // secondary images included; none is peeled separately.
            if !gainmaps.is_empty() {
                return Err(r.bad("gainmaps"));
            }
            if trailing.len() as u64 != after {
                return Err(r.bad("trailing"));
            }
        } else if nested_trailing_chunks.is_empty() && covered != after {
            // Nothing else holds the bytes after the primary image.
            return Err(r.bad("gainmaps"));
        }
        Ok(JpegRecord {
            original_len,
            primary_len,
            trailing,
            nested_trailing_chunks,
            gainmaps,
            lepton_version: r.u8()?,
            original_hash: r.hash()?,
        })
    }

    /// Parse a body; `record` is its id, for the errors.
    pub fn parse(body: &[u8], record: u64) -> Result<Self, FormatError> {
        let mut r = Rd::new(body, record);
        let v = Self::read(&mut r)?;
        r.finish()?;
        Ok(v)
    }

    /// True when `original` (the whole JPEG file) has the recorded length and hash.
    pub fn verify_hash(&self, original: &[u8]) -> bool {
        original.len() as u64 == self.original_len && hash_matches(original, &self.original_hash)
    }
}

// ---------------------------------------------------------------- deflate

/// Record of a peeled Deflate stream (kind 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeflateRecord {
    /// Compressed byte length of the Deflate stream.
    pub original_len: u64,
    /// Decompressed length.
    pub plain_len: u64,
    /// preflate-rs's correction data.
    pub corrections: Vec<u8>,
    /// 0 = preflate-rs 0.7 format.
    pub library: u8,
    /// BLAKE3 of the original compressed stream.
    pub original_hash: [u8; 32],
}

impl DeflateRecord {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.original_len.to_le_bytes());
        out.extend_from_slice(&self.plain_len.to_le_bytes());
        put_bytes(out, &self.corrections);
        out.push(self.library);
        out.extend_from_slice(&self.original_hash);
    }

    /// The body bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Parse a body; `record` is its id, for the errors.
    pub fn parse(body: &[u8], record: u64) -> Result<Self, FormatError> {
        let mut r = Rd::new(body, record);
        let v = DeflateRecord {
            original_len: r.u64()?,
            plain_len: r.u64()?,
            corrections: r.bytes()?,
            library: r.small(0, "library")?,
            original_hash: r.hash()?,
        };
        r.finish()?;
        Ok(v)
    }

    /// True when `original` (the compressed stream) has the recorded length and hash.
    pub fn verify_hash(&self, original: &[u8]) -> bool {
        original.len() as u64 == self.original_len && hash_matches(original, &self.original_hash)
    }
}

// ------------------------------------------------------------- png-filter

/// Record of a PNG whose filter bytes were peeled (kind 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PngFilterRecord {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// 1, 2, 4, 8 or 16.
    pub bit_depth: u8,
    /// 0, 2, 3, 4 or 6.
    pub color_type: u8,
    /// 0 (none) or 1 (Adam7).
    pub interlace: u8,
    /// One filter byte per scanline, in order (per pass when interlaced).
    pub filters: Vec<u8>,
    /// BLAKE3 of the filtered scanline bytes (the Deflate-decoded IDAT data).
    pub original_hash: [u8; 32],
}

impl PngFilterRecord {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&[self.bit_depth, self.color_type, self.interlace]);
        put_bytes(out, &self.filters);
        out.extend_from_slice(&self.original_hash);
    }

    /// The body bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Parse a body; `record` is its id, for the errors.
    pub fn parse(body: &[u8], record: u64) -> Result<Self, FormatError> {
        let mut r = Rd::new(body, record);
        let width = r.u32()?;
        let height = r.u32()?;
        let bit_depth = r.u8()?;
        if !matches!(bit_depth, 1 | 2 | 4 | 8 | 16) {
            return Err(r.bad("bit_depth"));
        }
        let color_type = r.u8()?;
        if !matches!(color_type, 0 | 2 | 3 | 4 | 6) {
            return Err(r.bad("color_type"));
        }
        let interlace = r.small(1, "interlace")?;
        let filters = r.bytes()?;
        // A non-interlaced image has one filter byte per scanline.
        if interlace == 0 && filters.len() as u64 != u64::from(height) {
            return Err(r.bad("filters"));
        }
        let v = PngFilterRecord {
            width,
            height,
            bit_depth,
            color_type,
            interlace,
            filters,
            original_hash: r.hash()?,
        };
        r.finish()?;
        Ok(v)
    }

    /// True when `original` (the filtered scanline bytes) has the recorded hash.
    pub fn verify_hash(&self, original: &[u8]) -> bool {
        hash_matches(original, &self.original_hash)
    }
}

// ----------------------------------------------------------------- base64

/// Record of a Base64 text (kind 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base64Record {
    /// 0 standard, 1 url-safe.
    pub variant: u8,
    /// Characters per line; 0 when the text has no line breaks.
    pub line_len: u16,
    /// 0 LF, 1 CRLF, 2 none.
    pub line_ending: u8,
    /// 0 none, 1 `=`.
    pub padding: u8,
    /// Length of the encoded text.
    pub original_len: u64,
    /// BLAKE3 of the encoded text.
    pub original_hash: [u8; 32],
}

impl Base64Record {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.variant);
        out.extend_from_slice(&self.line_len.to_le_bytes());
        out.push(self.line_ending);
        out.push(self.padding);
        out.extend_from_slice(&self.original_len.to_le_bytes());
        out.extend_from_slice(&self.original_hash);
    }

    /// The body bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Parse a body; `record` is its id, for the errors.
    pub fn parse(body: &[u8], record: u64) -> Result<Self, FormatError> {
        let mut r = Rd::new(body, record);
        let variant = r.small(1, "variant")?;
        let line_len = r.u16()?;
        let line_ending = r.small(2, "line_ending")?;
        let padding = r.small(1, "padding")?;
        // No line breaks (line_len 0) is line_ending 2, and only that.
        if (line_len == 0) != (line_ending == 2) {
            return Err(r.bad("line_ending"));
        }
        let v = Base64Record {
            variant,
            line_len,
            line_ending,
            padding,
            original_len: r.u64()?,
            original_hash: r.hash()?,
        };
        r.finish()?;
        Ok(v)
    }

    /// True when `original` (the encoded text) has the recorded length and hash.
    pub fn verify_hash(&self, original: &[u8]) -> bool {
        original.len() as u64 == self.original_len && hash_matches(original, &self.original_hash)
    }
}

// ------------------------------------------------------------------ utf16

/// Record of a UTF-16 text (kind 11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utf16Record {
    /// 0 little endian, 1 big endian.
    pub endian: u8,
    /// 0 no byte order mark, 1 present.
    pub bom: u8,
    /// Length of the UTF-16 text in bytes.
    pub original_len: u64,
    /// BLAKE3 of the UTF-16 text.
    pub original_hash: [u8; 32],
}

impl Utf16Record {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.endian);
        out.push(self.bom);
        out.extend_from_slice(&self.original_len.to_le_bytes());
        out.extend_from_slice(&self.original_hash);
    }

    /// The body bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Parse a body; `record` is its id, for the errors.
    pub fn parse(body: &[u8], record: u64) -> Result<Self, FormatError> {
        let mut r = Rd::new(body, record);
        let endian = r.small(1, "endian")?;
        let bom = r.small(1, "bom")?;
        let v = Utf16Record {
            endian,
            bom,
            original_len: r.u64()?,
            original_hash: r.hash()?,
        };
        r.finish()?;
        Ok(v)
    }

    /// True when `original` (the UTF-16 bytes) has the recorded length and hash.
    pub fn verify_hash(&self, original: &[u8]) -> bool {
        original.len() as u64 == self.original_len && hash_matches(original, &self.original_hash)
    }
}

// -------------------------------------------------------------- container

/// One member of a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerMember {
    /// Position in the original.
    pub offset: u64,
    /// Length in the original.
    pub len: u64,
    /// The chunks whose plain bytes are the member's original bytes (a nested peel is undone
    /// when they are decoded), so their plain lengths add up to `len`.
    pub chunks: Vec<u64>,
}

/// Record of a peeled container (kind 12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerRecord {
    /// 0 ZIP, 1 PDF, 2 gzip, 3 TAR.
    pub format: u8,
    /// Byte length of the container.
    pub original_len: u64,
    /// The verbatim bytes that are not member data.
    pub framing: Vec<u8>,
    /// The members: ascending, not overlapping. The original is the framing
    /// bytes with each member's data inserted at its `offset`, so
    /// `framing.len() + sum(member.len) == original_len`.
    pub members: Vec<ContainerMember>,
    /// BLAKE3 of the whole original container.
    pub original_hash: [u8; 32],
}

impl ContainerRecord {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.format);
        out.extend_from_slice(&self.original_len.to_le_bytes());
        put_bytes(out, &self.framing);
        put_varint(out, self.members.len() as u64);
        for m in &self.members {
            out.extend_from_slice(&m.offset.to_le_bytes());
            out.extend_from_slice(&m.len.to_le_bytes());
            put_chunks(out, &m.chunks);
        }
        out.extend_from_slice(&self.original_hash);
    }

    /// The body bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Parse a body; `record` is its id, for the errors.
    pub fn parse(body: &[u8], record: u64) -> Result<Self, FormatError> {
        let mut r = Rd::new(body, record);
        let format = r.small(3, "format")?;
        let original_len = r.u64()?;
        let framing = r.bytes()?;
        let n = r.count(8 + 8 + 1)?;
        let mut members = Vec::with_capacity(n);
        // Members ascend and do not overlap; their lengths and the framing
        // add up to the container.
        let mut prev_end = 0u64;
        let mut total = framing.len() as u64;
        for _ in 0..n {
            let offset = r.u64()?;
            let len = r.u64()?;
            let end = offset
                .checked_add(len)
                .filter(|&e| e <= original_len && offset >= prev_end)
                .ok_or_else(|| r.bad("members"))?;
            prev_end = end;
            total = total.saturating_add(len);
            members.push(ContainerMember {
                offset,
                len,
                chunks: r.chunks()?,
            });
        }
        if total != original_len {
            return Err(r.bad("original_len"));
        }
        let original_hash = r.hash()?;
        r.finish()?;
        Ok(ContainerRecord {
            format,
            original_len,
            framing,
            members,
            original_hash,
        })
    }

    /// True when `original` (the whole container) has the recorded length and hash.
    pub fn verify_hash(&self, original: &[u8]) -> bool {
        original.len() as u64 == self.original_len && hash_matches(original, &self.original_hash)
    }
}

// ----------------------------------------------------------------- record

/// The typed body of a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordBody {
    /// Kind 7.
    Jpeg(JpegRecord),
    /// Kind 8.
    Deflate(DeflateRecord),
    /// Kind 9.
    PngFilter(PngFilterRecord),
    /// Kind 10.
    Base64(Base64Record),
    /// Kind 11.
    Utf16(Utf16Record),
    /// Kind 12.
    Container(ContainerRecord),
}

impl RecordBody {
    /// The kind of this body.
    pub fn kind(&self) -> RecordKind {
        match self {
            RecordBody::Jpeg(_) => RecordKind::Jpeg,
            RecordBody::Deflate(_) => RecordKind::Deflate,
            RecordBody::PngFilter(_) => RecordKind::PngFilter,
            RecordBody::Base64(_) => RecordKind::Base64,
            RecordBody::Utf16(_) => RecordKind::Utf16,
            RecordBody::Container(_) => RecordKind::Container,
        }
    }
}

/// One record: its kind and its parsed body.
///
/// `kind` and `body` agree; [`Record::new`] derives the kind. The frame writer
/// writes the kind of the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The record kind.
    pub kind: RecordKind,
    /// The body.
    pub body: RecordBody,
}

impl Record {
    /// A record for `body`, with the matching kind.
    pub fn new(body: RecordBody) -> Self {
        Record {
            kind: body.kind(),
            body,
        }
    }

    /// The body bytes (without the kind, flags, length and hash).
    pub fn encode(&self) -> Vec<u8> {
        match &self.body {
            RecordBody::Jpeg(b) => b.encode(),
            RecordBody::Deflate(b) => b.encode(),
            RecordBody::PngFilter(b) => b.encode(),
            RecordBody::Base64(b) => b.encode(),
            RecordBody::Utf16(b) => b.encode(),
            RecordBody::Container(b) => b.encode(),
        }
    }

    /// Parse the `body` of a record of `kind`; `record` is its id, for the errors.
    pub fn parse(kind: RecordKind, body: &[u8], record: u64) -> Result<Self, FormatError> {
        let body = match kind {
            RecordKind::Jpeg => RecordBody::Jpeg(JpegRecord::parse(body, record)?),
            RecordKind::Deflate => RecordBody::Deflate(DeflateRecord::parse(body, record)?),
            RecordKind::PngFilter => RecordBody::PngFilter(PngFilterRecord::parse(body, record)?),
            RecordKind::Base64 => RecordBody::Base64(Base64Record::parse(body, record)?),
            RecordKind::Utf16 => RecordBody::Utf16(Utf16Record::parse(body, record)?),
            RecordKind::Container => RecordBody::Container(ContainerRecord::parse(body, record)?),
        };
        Ok(Record { kind, body })
    }

    /// The chunk lists the record references, each with the total `plain_len`
    /// its chunks must add up to: a JPEG's secondary images and (when the
    /// trailing data is peeled) its nested trailing chunks, which hold the
    /// bytes after the primary image that no secondary image covers; a
    /// container's members.
    pub fn chunk_groups(&self) -> Vec<(&[u64], u64)> {
        match &self.body {
            RecordBody::Jpeg(j) => {
                let mut v: Vec<(&[u64], u64)> = j
                    .gainmaps
                    .iter()
                    .map(|g| (g.chunks.as_slice(), g.len))
                    .collect();
                if j.trailing.is_empty() {
                    let covered = j.gainmaps.iter().fold(0u64, |a, g| a.saturating_add(g.len));
                    let rest = j
                        .original_len
                        .saturating_sub(j.primary_len)
                        .saturating_sub(covered);
                    v.push((j.nested_trailing_chunks.as_slice(), rest));
                }
                v
            }
            RecordBody::Container(c) => c
                .members
                .iter()
                .map(|m| (m.chunks.as_slice(), m.len))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Check the original bytes against the record's verification target
    /// (see each body's `verify_hash`).
    pub fn verify_hash(&self, original: &[u8]) -> bool {
        match &self.body {
            RecordBody::Jpeg(b) => b.verify_hash(original),
            RecordBody::Deflate(b) => b.verify_hash(original),
            RecordBody::PngFilter(b) => b.verify_hash(original),
            RecordBody::Base64(b) => b.verify_hash(original),
            RecordBody::Utf16(b) => b.verify_hash(original),
            RecordBody::Container(b) => b.verify_hash(original),
        }
    }
}

// ------------------------------------------------------------- the frame

/// Encoder of the `Records` frame payload.
#[derive(Debug)]
pub struct RecordsWriter;

impl RecordsWriter {
    /// Encode `records`; a record's id is its position.
    pub fn encode(records: &[Record]) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, records.len() as u64);
        for r in records {
            let body = r.encode();
            out.extend_from_slice(&(r.body.kind() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            put_varint(&mut out, body.len() as u64);
            out.extend_from_slice(&body);
            out.extend_from_slice(blake3::hash(&body).as_bytes());
        }
        out
    }
}

/// A parsed `Records` payload: remembers the payload and the count only.
#[derive(Debug, Clone, Copy)]
pub struct RecordsTable<'a> {
    payload: &'a [u8],
    body: usize,
    count: u64,
}

impl<'a> RecordsTable<'a> {
    /// Read the record count; no record is examined until iteration. A count
    /// above the remaining bytes divided by [`RECORD_COUNT_BOUND`] is `Truncated`.
    pub fn parse(payload: &'a [u8]) -> Result<RecordsTable<'a>, FormatError> {
        let mut s = payload;
        let count = match varint::read(&mut s) {
            Err(FormatError::Truncated { .. }) => {
                return Err(FormatError::Truncated { what: WHAT })
            }
            other => other?,
        };
        if count > (s.len() / RECORD_COUNT_BOUND) as u64 {
            return Err(FormatError::Truncated { what: WHAT });
        }
        Ok(RecordsTable {
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

    /// Stream the records, each hash-checked and parsed. After the last one,
    /// leftover bytes yield `TrailingBytes`; any error ends the stream.
    pub fn iter(&self) -> RecordsIter<'a> {
        RecordsIter {
            rest: self.payload.get(self.body..).unwrap_or(&[]),
            remaining: self.count,
            next_id: 0,
            done: false,
        }
    }

    /// The record `id`, or `None` past the end. Walks from the start, so the
    /// first error among records `0..=id` wins, as with the chunk table.
    pub fn get(&self, id: u64) -> Result<Option<Record>, FormatError> {
        if id >= self.count {
            return Ok(None);
        }
        let mut it = self.iter();
        for _ in 0..id {
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

/// Streaming iterator over the records of a [`RecordsTable`].
#[derive(Debug)]
pub struct RecordsIter<'a> {
    rest: &'a [u8],
    remaining: u64,
    next_id: u64,
    done: bool,
}

impl RecordsIter<'_> {
    fn next_record(&mut self) -> Result<Record, FormatError> {
        let id = self.next_id;
        let mut r = Rd::new(self.rest, id);
        let raw = r.u16()?;
        let kind = RecordKind::from_u16(raw).ok_or(FormatError::UnknownRecordKind {
            kind: raw,
            record: id,
        })?;
        let flags = r.u16()?;
        if flags != 0 {
            return Err(FormatError::ReservedRecordBits {
                bits: flags,
                record: id,
            });
        }
        let body_len = r.varint()?;
        let body_len = usize::try_from(body_len)
            .ok()
            .filter(|&l| l <= r.s.len())
            .ok_or_else(Rd::truncated)?;
        let body = r.take(body_len)?;
        let hash = r.hash()?;
        if blake3::hash(body).as_bytes() != &hash {
            return Err(FormatError::RecordHashMismatch { record: id });
        }
        let rec = Record::parse(kind, body, id)?;
        self.rest = r.s;
        Ok(rec)
    }
}

impl Iterator for RecordsIter<'_> {
    type Item = Result<Record, FormatError>;

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
            Ok(_) => {
                self.remaining -= 1;
                self.next_id += 1;
            }
            Err(_) => self.done = true,
        }
        Some(r)
    }
}

// ----------------------------------------------------------------- tables

/// The Markdown table of the record kinds, pasted verbatim into the spec.
pub fn record_kind_table() -> String {
    let mut s = String::from("| Kind | Name | Primitive |\n|---|---|---|\n");
    for k in RecordKind::ALL {
        let p = k.primitive();
        s.push_str(&format!(
            "| {} | `{}` | `{}` (0x{:04X}) |\n",
            k as u16,
            k.name(),
            p.name(),
            p as u16
        ));
    }
    s
}

/// The Markdown tables of the `Records` frame payload, of one record, and of
/// the six record bodies, pasted verbatim into the spec.
pub fn record_layout_tables() -> String {
    "**Records frame payload**\n\n\
| Field | Size | Meaning |\n|---|---|---|\n\
| record_count | varint | number of records; at most the remaining bytes divided by 5 |\n\
| records | variable | per record, in ascending id order (the id is the position, from 0): the fields below |\n\
| kind | u16 LE | the primitive the record belongs to: 7 to 12 (section 12 kind table) |\n\
| flags | u16 LE | reserved, must be 0 |\n\
| body_len | varint | length of `body`; at most the bytes that remain |\n\
| body | body_len bytes | the record body of the kind |\n\
| body_hash | 32 | BLAKE3-256 of `body` |\n\n\
**jpeg body (kind 7)**\n\n\
| Field | Size | Meaning |\n|---|---|---|\n\
| original_len | u64 LE | byte length of the JPEG file |\n\
| primary_len | u64 LE | bytes of the primary image up to and including its EOI; at most `original_len` |\n\
| trailing | bytes | the data after the EOI as stored: the raw bytes, or empty when they were peeled as a nested stream |\n\
| nested_trailing_chunks | chunk list | the chunks holding the peeled trailing data; empty when `trailing` is not empty |\n\
| gainmap_count | varint | number of secondary images |\n\
| offset | u64 LE | per secondary image: position inside the original file |\n\
| len | u64 LE | per secondary image: length; `offset + len` is at most `original_len` |\n\
| chunks | chunk list | per secondary image: the chunks holding it |\n\
| lepton_version | u8 | the Lepton format revision the stream was written with (0 for the version lepton_jpeg 0.5 writes) |\n\
| original_hash | 32 | BLAKE3-256 of the whole original file |\n\n\
**deflate body (kind 8)**\n\n\
| Field | Size | Meaning |\n|---|---|---|\n\
| original_len | u64 LE | compressed byte length of the Deflate stream |\n\
| plain_len | u64 LE | its decompressed length |\n\
| corrections | bytes | preflate-rs's correction data |\n\
| library | u8 | 0 = preflate-rs 0.7 format; no other value |\n\
| original_hash | 32 | BLAKE3-256 of the original compressed stream |\n\n\
**png-filter body (kind 9)**\n\n\
| Field | Size | Meaning |\n|---|---|---|\n\
| width | u32 LE | image width in pixels |\n\
| height | u32 LE | image height in pixels |\n\
| bit_depth | u8 | 1, 2, 4, 8 or 16 |\n\
| color_type | u8 | 0, 2, 3, 4 or 6 |\n\
| interlace | u8 | 0 none, 1 Adam7 |\n\
| filters | bytes | one filter byte per scanline, in order; for interlaced images per pass in PNG's order |\n\
| original_hash | 32 | BLAKE3-256 of the filtered scanline bytes (the Deflate-decoded IDAT data) |\n\n\
**base64 body (kind 10)**\n\n\
| Field | Size | Meaning |\n|---|---|---|\n\
| variant | u8 | 0 standard, 1 url-safe |\n\
| line_len | u16 LE | characters per line; 0 = no line breaks |\n\
| line_ending | u8 | 0 LF, 1 CRLF, 2 none |\n\
| padding | u8 | 0 none, 1 `=` |\n\
| original_len | u64 LE | length of the encoded text |\n\
| original_hash | 32 | BLAKE3-256 of the encoded text |\n\n\
**utf16 body (kind 11)**\n\n\
| Field | Size | Meaning |\n|---|---|---|\n\
| endian | u8 | 0 little endian, 1 big endian |\n\
| bom | u8 | 0 no byte order mark, 1 present |\n\
| original_len | u64 LE | length of the UTF-16 text in bytes |\n\
| original_hash | 32 | BLAKE3-256 of the UTF-16 text |\n\n\
**container body (kind 12)**\n\n\
| Field | Size | Meaning |\n|---|---|---|\n\
| format | u8 | 0 ZIP, 1 PDF, 2 gzip, 3 TAR; other values are reserved |\n\
| original_len | u64 LE | byte length of the container |\n\
| framing | bytes | the verbatim bytes of the container that are not member data (ZIP: every local header, extra field, data descriptor, the central directory and the end record; PDF: object headers and xref; gzip: header and trailer; TAR: the headers) |\n\
| member_count | varint | number of members |\n\
| offset | u64 LE | per member: position in the original |\n\
| len | u64 LE | per member: length in the original; members ascend and do not overlap, and `offset + len` is at most `original_len` |\n\
| chunks | chunk list | per member: the chunks whose plain bytes are the member's original bytes (a nested peel of a member is undone when those chunks are decoded, so their plain lengths add up to `len`) |\n\
| original_hash | 32 | after the last member: BLAKE3-256 of the whole original container |\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    type Case<T> = (fn(&mut T), &'static str);

    fn h(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn jpeg(nested: bool, gainmaps: usize) -> JpegRecord {
        JpegRecord {
            original_len: 5000,
            primary_len: 4000,
            // Raw trailing data is everything after the primary image.
            trailing: if nested { vec![] } else { vec![1; 1000] },
            nested_trailing_chunks: if nested { vec![7, 8] } else { vec![] },
            gainmaps: (0..gainmaps as u64)
                .map(|i| SecondaryImage {
                    offset: 4000 + i * 100,
                    len: 100,
                    chunks: vec![i, i + 1, 300],
                })
                .collect(),
            lepton_version: 0,
            original_hash: h(9),
        }
    }

    fn deflate() -> DeflateRecord {
        DeflateRecord {
            original_len: 10,
            plain_len: 99,
            corrections: vec![5; 17],
            library: 0,
            original_hash: h(1),
        }
    }

    fn png() -> PngFilterRecord {
        PngFilterRecord {
            width: 640,
            height: 3,
            bit_depth: 8,
            color_type: 6,
            interlace: 0,
            filters: vec![0, 1, 4],
            original_hash: h(2),
        }
    }

    fn b64() -> Base64Record {
        Base64Record {
            variant: 1,
            line_len: 76,
            line_ending: 1,
            padding: 1,
            original_len: 1234,
            original_hash: h(3),
        }
    }

    fn utf16() -> Utf16Record {
        Utf16Record {
            endian: 1,
            bom: 1,
            original_len: 40,
            original_hash: h(4),
        }
    }

    fn container(members: usize) -> ContainerRecord {
        // Four framing bytes first, then the members back to back.
        ContainerRecord {
            format: 0,
            original_len: 4 + 50 * members as u64,
            framing: vec![0x50, 0x4B, 3, 4],
            members: (0..members as u64)
                .map(|i| ContainerMember {
                    offset: 4 + 50 * i,
                    len: 50,
                    chunks: vec![i; (i + 1) as usize],
                })
                .collect(),
            original_hash: h(6),
        }
    }

    /// One record of each kind, with every optional part present.
    fn all_kinds() -> Vec<Record> {
        vec![
            Record::new(RecordBody::Jpeg(jpeg(true, 2))),
            Record::new(RecordBody::Deflate(deflate())),
            Record::new(RecordBody::PngFilter(png())),
            Record::new(RecordBody::Base64(b64())),
            Record::new(RecordBody::Utf16(utf16())),
            Record::new(RecordBody::Container(container(3))),
        ]
    }

    #[test]
    fn kinds_and_names() {
        for (i, k) in RecordKind::ALL.iter().enumerate() {
            assert_eq!(*k as u16, 7 + i as u16);
            assert_eq!(RecordKind::from_u16(*k as u16), Some(*k));
            assert_eq!(k.primitive() as u16, *k as u16);
        }
        for bad in [0u16, 6, 13, 0x8000, u16::MAX] {
            assert_eq!(RecordKind::from_u16(bad), None);
        }
        let names: Vec<_> = RecordKind::ALL.iter().map(|k| k.name()).collect();
        assert_eq!(
            names,
            [
                "jpeg",
                "deflate",
                "png-filter",
                "base64",
                "utf16",
                "container"
            ]
        );
        assert!(
            record_kind_table().contains("| 12 | `container` | `container-reconstruct` (0x000C) |")
        );
    }

    macro_rules! round_trip {
        ($t:ident, $v:expr) => {{
            let v: $t = $v;
            let body = v.encode();
            assert_eq!($t::parse(&body, 4).unwrap(), v);
            // A cut at every position is a truncation (or a rejected field),
            // and one more byte is trailing.
            for n in 0..body.len() {
                assert!(
                    $t::parse(&body[..n], 4).is_err(),
                    "{} cut at {n}",
                    stringify!($t)
                );
            }
            let mut more = body.clone();
            more.push(0);
            assert!(matches!(
                $t::parse(&more, 4),
                Err(FormatError::TrailingBytes {
                    what: "record body"
                })
            ));
        }};
    }

    #[test]
    fn every_record_type_round_trips_with_and_without_optional_parts() {
        // Raw trailing data holds the secondary images too; peeled data has them apart.
        round_trip!(JpegRecord, jpeg(false, 0));
        for g in [0, 1, 2] {
            round_trip!(JpegRecord, jpeg(true, g));
        }
        // Nothing after the primary image.
        let mut both_empty = jpeg(false, 0);
        both_empty.trailing.clear();
        both_empty.original_len = both_empty.primary_len;
        round_trip!(JpegRecord, both_empty);
        // Secondary images that cover everything after the primary image.
        let mut covered = jpeg(true, 2);
        covered.nested_trailing_chunks.clear();
        covered.gainmaps[1].len = 1000 - 100;
        round_trip!(JpegRecord, covered);
        round_trip!(DeflateRecord, deflate());
        let mut d = deflate();
        d.corrections.clear();
        round_trip!(DeflateRecord, d);
        round_trip!(PngFilterRecord, png());
        let mut p = png();
        p.filters.clear();
        p.height = 0;
        round_trip!(PngFilterRecord, p);
        round_trip!(Base64Record, b64());
        let mut b = b64();
        (b.variant, b.line_len, b.line_ending, b.padding) = (0, 0, 2, 0);
        round_trip!(Base64Record, b);
        round_trip!(Utf16Record, utf16());
        let mut u = utf16();
        (u.endian, u.bom) = (0, 0);
        round_trip!(Utf16Record, u);
        for m in [0, 1, 3] {
            round_trip!(ContainerRecord, container(m));
        }
        let mut c = container(3);
        c.framing.clear();
        c.format = 3;
        c.original_len = 150;
        for (i, m) in c.members.iter_mut().enumerate() {
            m.offset = 50 * i as u64;
        }
        round_trip!(ContainerRecord, c);
        // Members with framing between them.
        let mut c = container(2);
        c.framing = vec![9; 10];
        c.original_len = 110;
        c.members[1].offset = 4 + 50 + 6;
        round_trip!(ContainerRecord, c);
        // The typed wrapper round-trips too.
        for r in all_kinds() {
            assert_eq!(Record::parse(r.kind, &r.encode(), 0).unwrap(), r);
        }
    }

    fn flip(data: &[u8]) -> Vec<u8> {
        let mut d = data.to_vec();
        let mid = d.len() / 2;
        d[mid] ^= 1;
        d
    }

    #[test]
    fn verify_hash_accepts_the_right_bytes_and_rejects_a_flipped_one() {
        let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let hash = *blake3::hash(&data).as_bytes();
        let j = JpegRecord {
            original_hash: hash,
            ..jpeg(false, 0)
        };
        assert!(j.verify_hash(&data) && !j.verify_hash(&flip(&data)));
        assert!(!j.verify_hash(&data[1..]));
        let d = DeflateRecord {
            original_len: 5000,
            original_hash: hash,
            ..deflate()
        };
        assert!(d.verify_hash(&data) && !d.verify_hash(&flip(&data)));
        let p = PngFilterRecord {
            original_hash: hash,
            ..png()
        };
        assert!(p.verify_hash(&data) && !p.verify_hash(&flip(&data)));
        let b = Base64Record {
            original_len: 5000,
            original_hash: hash,
            ..b64()
        };
        assert!(b.verify_hash(&data) && !b.verify_hash(&flip(&data)));
        let u = Utf16Record {
            original_len: 5000,
            original_hash: hash,
            ..utf16()
        };
        assert!(u.verify_hash(&data) && !u.verify_hash(&flip(&data)));
        // A container: the hash covers the whole file, framing bytes included.
        let c = ContainerRecord {
            original_len: 5000,
            original_hash: hash,
            ..container(3)
        };
        assert!(c.verify_hash(&data));
        for at in [0usize, 2, 4 + 25, 4999] {
            let mut hit = data.clone();
            hit[at] ^= 1;
            assert!(!c.verify_hash(&hit), "flip at {at}");
        }
        assert!(!c.verify_hash(&data[..4999]));
        for r in all_kinds() {
            // The wrapper dispatches; none of the samples hashes `data`.
            assert!(!r.verify_hash(&data));
        }
        assert!(Record::new(RecordBody::Jpeg(j)).verify_hash(&data));
    }

    #[test]
    fn enumerations_out_of_range_are_bad_records_naming_the_field() {
        fn reason(e: FormatError) -> &'static str {
            match e {
                FormatError::BadRecord { record: 4, reason } => reason,
                e => panic!("unexpected {e:?}"),
            }
        }
        let mut d = deflate();
        d.library = 1;
        assert_eq!(
            reason(DeflateRecord::parse(&d.encode(), 4).unwrap_err()),
            "library"
        );
        let png_cases: [Case<PngFilterRecord>; 5] = [
            (|p| p.bit_depth = 3, "bit_depth"),
            (|p| p.bit_depth = 0, "bit_depth"),
            (|p| p.color_type = 1, "color_type"),
            (|p| p.color_type = 5, "color_type"),
            (|p| p.interlace = 2, "interlace"),
        ];
        for (f, name) in png_cases {
            let mut p = png();
            f(&mut p);
            assert_eq!(
                reason(PngFilterRecord::parse(&p.encode(), 4).unwrap_err()),
                name
            );
        }
        for depth in [1, 2, 4, 8, 16] {
            let mut p = png();
            p.bit_depth = depth;
            PngFilterRecord::parse(&p.encode(), 4).unwrap();
        }
        let b64_cases: [Case<Base64Record>; 3] = [
            (|b| b.variant = 2, "variant"),
            (|b| b.line_ending = 3, "line_ending"),
            (|b| b.padding = 2, "padding"),
        ];
        for (f, name) in b64_cases {
            let mut b = b64();
            f(&mut b);
            assert_eq!(
                reason(Base64Record::parse(&b.encode(), 4).unwrap_err()),
                name
            );
        }
        let utf_cases: [Case<Utf16Record>; 2] =
            [(|u| u.endian = 2, "endian"), (|u| u.bom = 2, "bom")];
        for (f, name) in utf_cases {
            let mut u = utf16();
            f(&mut u);
            assert_eq!(
                reason(Utf16Record::parse(&u.encode(), 4).unwrap_err()),
                name
            );
        }
        let mut c = container(1);
        c.format = 4;
        assert_eq!(
            reason(ContainerRecord::parse(&c.encode(), 4).unwrap_err()),
            "format"
        );
        // Consistency rules beyond the enumerations.
        let mut j = jpeg(false, 0);
        j.primary_len = 5001;
        assert_eq!(
            reason(JpegRecord::parse(&j.encode(), 4).unwrap_err()),
            "primary_len"
        );
        let mut j = jpeg(false, 0);
        j.nested_trailing_chunks = vec![1];
        assert_eq!(
            reason(JpegRecord::parse(&j.encode(), 4).unwrap_err()),
            "nested_trailing_chunks"
        );
        let mut j = jpeg(true, 1);
        j.gainmaps[0].len = u64::MAX;
        assert_eq!(
            reason(JpegRecord::parse(&j.encode(), 4).unwrap_err()),
            "gainmaps"
        );
        let mut c = container(1);
        c.members[0].offset = 990;
        assert_eq!(
            reason(ContainerRecord::parse(&c.encode(), 4).unwrap_err()),
            "members"
        );
        // Assembly rules: ascending, not overlapping, lengths add up.
        let mut c = container(2);
        c.members[1].offset = c.members[0].offset + 10;
        assert_eq!(
            reason(ContainerRecord::parse(&c.encode(), 4).unwrap_err()),
            "members"
        );
        let mut c = container(2);
        c.members.swap(0, 1);
        assert_eq!(
            reason(ContainerRecord::parse(&c.encode(), 4).unwrap_err()),
            "members"
        );
        let mut c = container(2);
        c.original_len += 1;
        assert_eq!(
            reason(ContainerRecord::parse(&c.encode(), 4).unwrap_err()),
            "original_len"
        );
        let mut c = container(2);
        c.framing.push(0);
        assert_eq!(
            reason(ContainerRecord::parse(&c.encode(), 4).unwrap_err()),
            "original_len"
        );
        // JPEG: secondary images after the primary image and apart; raw
        // trailing data is everything after it; peeled data is covered.
        let mut j = jpeg(true, 1);
        j.gainmaps[0].offset = 3999;
        assert_eq!(
            reason(JpegRecord::parse(&j.encode(), 4).unwrap_err()),
            "gainmaps"
        );
        let mut j = jpeg(true, 2);
        j.gainmaps[1].offset = 4050;
        assert_eq!(
            reason(JpegRecord::parse(&j.encode(), 4).unwrap_err()),
            "gainmaps"
        );
        let mut j = jpeg(false, 0);
        j.trailing.pop();
        assert_eq!(
            reason(JpegRecord::parse(&j.encode(), 4).unwrap_err()),
            "trailing"
        );
        let mut j = jpeg(false, 0);
        j.gainmaps.push(SecondaryImage {
            offset: 4000,
            len: 10,
            chunks: vec![],
        });
        assert_eq!(
            reason(JpegRecord::parse(&j.encode(), 4).unwrap_err()),
            "gainmaps"
        );
        let mut j = jpeg(true, 1);
        j.nested_trailing_chunks.clear();
        assert_eq!(
            reason(JpegRecord::parse(&j.encode(), 4).unwrap_err()),
            "gainmaps"
        );
        // Contradictory text and image shapes.
        for (ll, le) in [(0u16, 0u8), (0, 1), (76, 2)] {
            let mut b = b64();
            (b.line_len, b.line_ending) = (ll, le);
            assert_eq!(
                reason(Base64Record::parse(&b.encode(), 4).unwrap_err()),
                "line_ending"
            );
        }
        let mut p = png();
        p.filters.pop();
        assert_eq!(
            reason(PngFilterRecord::parse(&p.encode(), 4).unwrap_err()),
            "filters"
        );
        // An interlaced image has more scanlines than its height.
        p.interlace = 1;
        PngFilterRecord::parse(&p.encode(), 4).unwrap();
    }

    #[test]
    fn chunk_groups_list_what_the_records_reference() {
        let j = Record::new(RecordBody::Jpeg(jpeg(true, 2)));
        let g = j.chunk_groups();
        // Two secondary images of 100 bytes, then the rest: 1000 - 200.
        assert_eq!(
            g,
            vec![
                (&[0u64, 1, 300][..], 100),
                (&[1, 2, 300][..], 100),
                (&[7, 8][..], 800)
            ]
        );
        // Raw trailing data references no chunks.
        let j = Record::new(RecordBody::Jpeg(jpeg(false, 0)));
        assert!(j.chunk_groups().is_empty());
        let c = Record::new(RecordBody::Container(container(2)));
        assert_eq!(c.chunk_groups(), vec![(&[0u64][..], 50), (&[1, 1][..], 50)]);
        assert!(Record::new(RecordBody::Utf16(utf16()))
            .chunk_groups()
            .is_empty());
    }

    #[test]
    fn counts_and_lengths_are_bounded_by_the_bytes_that_remain() {
        // A deflate body whose corrections length claims u64::MAX bytes.
        let mut body = vec![0u8; 16];
        varint::write(&mut body, u64::MAX).unwrap();
        assert!(matches!(
            DeflateRecord::parse(&body, 0),
            Err(FormatError::Truncated { what: "records" })
        ));
        // A jpeg body claiming a huge chunk list and a huge gain-map count.
        let mut body = vec![0u8; 16];
        body.push(0); // trailing: empty
        varint::write(&mut body, 1 << 40).unwrap();
        assert!(matches!(
            JpegRecord::parse(&body, 0),
            Err(FormatError::Truncated { what: "records" })
        ));
        let mut body = vec![0u8; 16];
        body.extend_from_slice(&[0, 0]); // trailing and chunk list empty
        varint::write(&mut body, 1 << 40).unwrap();
        assert!(matches!(
            JpegRecord::parse(&body, 0),
            Err(FormatError::Truncated { what: "records" })
        ));
        // A container claiming members it has no bytes for.
        let mut body = vec![0u8];
        body.extend_from_slice(&0u64.to_le_bytes());
        body.push(0);
        body.push(2);
        assert!(matches!(
            ContainerRecord::parse(&body, 0),
            Err(FormatError::Truncated { what: "records" })
        ));
        // Non-canonical varint inside a body.
        let mut body = vec![0u8; 16];
        body.extend_from_slice(&[0x80, 0x00]);
        assert!(matches!(
            DeflateRecord::parse(&body, 0),
            Err(FormatError::NonCanonicalVarint)
        ));
    }

    fn collect(p: &[u8]) -> Result<Vec<Record>, FormatError> {
        RecordsTable::parse(p)?.iter().collect()
    }

    #[test]
    fn frame_round_trip_of_one_record_of_each_kind() {
        let recs = all_kinds();
        let p = RecordsWriter::encode(&recs);
        let t = RecordsTable::parse(&p).unwrap();
        assert_eq!(t.len(), 6);
        assert!(!t.is_empty());
        t.validate().unwrap();
        assert_eq!(collect(&p).unwrap(), recs);
        for (i, r) in recs.iter().enumerate() {
            assert_eq!(t.get(i as u64).unwrap().as_ref(), Some(r));
        }
        assert_eq!(t.get(6).unwrap(), None);
        // The empty frame is a single zero byte.
        let e = RecordsWriter::encode(&[]);
        assert_eq!(e, [0]);
        let t = RecordsTable::parse(&e).unwrap();
        assert!(t.is_empty());
        t.validate().unwrap();
        assert_eq!(t.get(0).unwrap(), None);
    }

    #[test]
    fn the_frame_wire_layout() {
        let r = Record::new(RecordBody::Utf16(utf16()));
        let body = r.encode();
        let p = RecordsWriter::encode(&[r]);
        let mut want = vec![1, 11, 0, 0, 0, body.len() as u8];
        want.extend_from_slice(&body);
        want.extend_from_slice(blake3::hash(&body).as_bytes());
        assert_eq!(p, want);
    }

    #[test]
    fn count_bound() {
        let mut p = Vec::new();
        varint::write(&mut p, u64::MAX).unwrap();
        assert!(matches!(
            RecordsTable::parse(&p),
            Err(FormatError::Truncated { what: "records" })
        ));
        // Count 2 with 9 bytes left: the bound is 9 / 5 = 1.
        let mut p = vec![2u8];
        p.extend_from_slice(&[0; 9]);
        assert!(matches!(
            RecordsTable::parse(&p),
            Err(FormatError::Truncated { what: "records" })
        ));
        p.push(0);
        RecordsTable::parse(&p).unwrap();
        assert!(matches!(
            RecordsTable::parse(&[]),
            Err(FormatError::Truncated { what: "records" })
        ));
    }

    #[test]
    fn trailing_bytes_and_truncation_inside_a_body() {
        let mut p = RecordsWriter::encode(&all_kinds());
        p.push(0);
        assert!(matches!(
            RecordsTable::parse(&p).unwrap().validate(),
            Err(FormatError::TrailingBytes { what: "records" })
        ));
        let full = RecordsWriter::encode(&all_kinds());
        for cut in [1, 10, full.len() / 2, full.len() - 1] {
            let e = collect(&full[..full.len() - cut]);
            assert!(
                matches!(e, Err(FormatError::Truncated { what: "records" })),
                "cut {cut}: {e:?}"
            );
        }
    }

    #[test]
    fn a_flipped_body_byte_is_a_hash_mismatch_of_that_record() {
        let recs = all_kinds();
        let one = RecordsWriter::encode(&recs[..1]);
        let two = RecordsWriter::encode(&recs[..2]);
        // Record 1's first body byte: after record 0 and the 5 header bytes.
        let at = one.len() + 5;
        let mut p = two.clone();
        p[at] ^= 1;
        let t = RecordsTable::parse(&p).unwrap();
        let first = t.get(0).unwrap().unwrap();
        assert_eq!(first, recs[0]);
        for e in [
            t.get(1).unwrap_err(),
            t.iter().nth(1).unwrap().unwrap_err(),
            t.validate().unwrap_err(),
        ] {
            assert!(
                matches!(e, FormatError::RecordHashMismatch { record: 1 }),
                "{e:?}"
            );
        }
        // The iterator ends after the error.
        let mut it = t.iter();
        assert!(it.next().unwrap().is_ok());
        assert!(it.next().unwrap().is_err());
        assert!(it.next().is_none());
    }

    #[test]
    fn unknown_kind_and_reserved_flags() {
        let p = RecordsWriter::encode(&all_kinds());
        // Record 0's kind field is at offset 1.
        for kind in [13u16, 6, 0] {
            let mut q = p.clone();
            q[1..3].copy_from_slice(&kind.to_le_bytes());
            assert!(matches!(
                collect(&q).unwrap_err(),
                FormatError::UnknownRecordKind { kind: k, record: 0 } if k == kind
            ));
        }
        let mut q = p.clone();
        q[3] = 1;
        assert!(matches!(
            collect(&q).unwrap_err(),
            FormatError::ReservedRecordBits { bits: 1, record: 0 }
        ));
        let mut q = p.clone();
        q[4] = 0x80;
        assert!(matches!(
            collect(&q).unwrap_err(),
            FormatError::ReservedRecordBits {
                bits: 0x8000,
                record: 0
            }
        ));
    }

    #[test]
    fn a_bad_enumeration_in_a_hashed_body_is_a_bad_record() {
        let mut d = deflate();
        d.library = 3;
        // Hand-build the frame: the writer encodes any body, the table refuses it.
        let p = RecordsWriter::encode(&[Record::new(RecordBody::Deflate(d))]);
        assert!(matches!(
            collect(&p).unwrap_err(),
            FormatError::BadRecord {
                record: 0,
                reason: "library"
            }
        ));
    }

    #[test]
    fn get_past_a_corrupt_record_reports_that_records_error() {
        let recs = all_kinds();
        let r0 = RecordsWriter::encode(&recs[..1]);
        let mut p = RecordsWriter::encode(&recs[..3]);
        // Corrupt record 1's body hash (the last byte of record 1).
        let r1_end = RecordsWriter::encode(&recs[..2]).len();
        assert!(r1_end > r0.len());
        p[r1_end - 1] ^= 1;
        let t = RecordsTable::parse(&p).unwrap();
        assert!(t.get(0).unwrap().is_some());
        for id in [1, 2] {
            assert!(matches!(
                t.get(id).unwrap_err(),
                FormatError::RecordHashMismatch { record: 1 }
            ));
        }
    }

    fn arb_hash() -> impl Strategy<Value = [u8; 32]> {
        any::<[u8; 32]>()
    }

    fn arb_chunks() -> impl Strategy<Value = Vec<u64>> {
        prop::collection::vec(any::<u64>(), 0..5)
    }

    fn arb_record() -> impl Strategy<Value = Record> {
        let jpeg = (
            0u64..1 << 40,
            any::<Option<u8>>(),
            prop::collection::vec(any::<u64>(), 1..5),
            prop::collection::vec((0u64..=100, arb_chunks()), 0..3),
            any::<u8>(),
            arb_hash(),
        )
            .prop_map(|(primary, raw, nested, maps, lv, hash)| {
                // 200 bytes follow the primary image: raw trailing data, or
                // secondary images at primary and primary + 100 and nested chunks.
                let (trailing, nested, gainmaps) = match raw {
                    Some(fill) => (vec![fill; 200], vec![], vec![]),
                    None => (
                        vec![],
                        nested,
                        maps.into_iter()
                            .enumerate()
                            .map(|(i, (len, chunks))| SecondaryImage {
                                offset: primary + 100 * i as u64,
                                len,
                                chunks,
                            })
                            .collect(),
                    ),
                };
                RecordBody::Jpeg(JpegRecord {
                    original_len: primary + 200,
                    primary_len: primary,
                    trailing,
                    nested_trailing_chunks: nested,
                    gainmaps,
                    lepton_version: lv,
                    original_hash: hash,
                })
            });
        let deflate = (
            any::<u64>(),
            any::<u64>(),
            prop::collection::vec(any::<u8>(), 0..32),
            arb_hash(),
        )
            .prop_map(|(a, b, c, hash)| {
                RecordBody::Deflate(DeflateRecord {
                    original_len: a,
                    plain_len: b,
                    corrections: c,
                    library: 0,
                    original_hash: hash,
                })
            });
        let png = (
            any::<u32>(),
            0u32..16,
            prop::sample::select(vec![1u8, 2, 4, 8, 16]),
            prop::sample::select(vec![0u8, 2, 3, 4, 6]),
            0u8..=1,
            prop::collection::vec(any::<u8>(), 0..16),
            arb_hash(),
        )
            .prop_map(
                |(width, height, bit_depth, color_type, interlace, mut filters, hash)| {
                    if interlace == 0 {
                        filters.resize(height as usize, 0);
                    }
                    RecordBody::PngFilter(PngFilterRecord {
                        width,
                        height,
                        bit_depth,
                        color_type,
                        interlace,
                        filters,
                        original_hash: hash,
                    })
                },
            );
        let b64 = (
            0u8..=1,
            any::<u16>(),
            0u8..=1,
            0u8..=1,
            any::<u64>(),
            arb_hash(),
        )
            .prop_map(|(variant, line_len, crlf, padding, original_len, hash)| {
                // No line breaks is line_ending 2 and only that.
                let line_ending = if line_len == 0 { 2 } else { crlf };
                RecordBody::Base64(Base64Record {
                    variant,
                    line_len,
                    line_ending,
                    padding,
                    original_len,
                    original_hash: hash,
                })
            });
        let utf = (0u8..=1, 0u8..=1, any::<u64>(), arb_hash()).prop_map(|(endian, bom, len, h)| {
            RecordBody::Utf16(Utf16Record {
                endian,
                bom,
                original_len: len,
                original_hash: h,
            })
        });
        let cont = (
            0u8..=3,
            prop::collection::vec(any::<u8>(), 0..16),
            prop::collection::vec((0u64..500, arb_chunks()), 0..4),
            arb_hash(),
        )
            .prop_map(|(format, framing, members, hash)| {
                // Members back to back after the framing.
                let mut at = framing.len() as u64;
                let members: Vec<ContainerMember> = members
                    .into_iter()
                    .map(|(len, chunks)| {
                        let m = ContainerMember {
                            offset: at,
                            len,
                            chunks,
                        };
                        at += len;
                        m
                    })
                    .collect();
                RecordBody::Container(ContainerRecord {
                    format,
                    original_len: at,
                    framing,
                    members,
                    original_hash: hash,
                })
            });
        prop_oneof![jpeg, deflate, png, b64, utf, cont].prop_map(Record::new)
    }

    proptest! {
        #[test]
        fn valid_record_sets_round_trip(recs in prop::collection::vec(arb_record(), 0..6)) {
            let p = RecordsWriter::encode(&recs);
            let t = RecordsTable::parse(&p).unwrap();
            prop_assert_eq!(t.len(), recs.len() as u64);
            prop_assert_eq!(collect(&p).unwrap(), recs.clone());
            for (i, r) in recs.iter().enumerate() {
                let got = t.get(i as u64).unwrap();
                prop_assert_eq!(got.as_ref(), Some(r));
            }
        }

        #[test]
        fn random_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
            if let Ok(t) = RecordsTable::parse(&bytes) {
                for r in t.iter() {
                    let _ = r;
                }
                let _ = t.get(1);
            }
        }

        #[test]
        fn random_bodies_never_panic(kind in 7u16..=12, bytes in prop::collection::vec(any::<u8>(), 0..128)) {
            if let Some(k) = RecordKind::from_u16(kind) {
                let _ = Record::parse(k, &bytes, 0);
            }
        }

        #[test]
        fn mutated_frames_never_panic(recs in prop::collection::vec(arb_record(), 1..4), at in any::<usize>(), bit in 0u8..8) {
            let mut p = RecordsWriter::encode(&recs);
            let n = p.len();
            p[at % n] ^= 1 << bit;
            if let Ok(t) = RecordsTable::parse(&p) {
                for r in t.iter() {
                    let _ = r;
                }
            }
        }
    }
}
