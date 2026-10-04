//! Opening an archive (section 6), reading blocks and chunks (section 9), verification.

use std::collections::HashMap;

use crate::block::{self, DecodeCtx};
use crate::crypto::{self, Sealer};
use crate::entries::{parse_entry_table, Entry, EntryKind};
use crate::error::{Error, Result};
use crate::index::{parse_index, Index, Loc};
use crate::journal;
use crate::recovery;
use crate::wire::{
    kind, parse_frame, parse_header, trailer_ending_at, Frame, Header, Trailer, HEADER_LEN,
    MIN_FRAME_LEN, TRAILER_FRAME_LEN,
};

/// The reader's resources (section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resources {
    /// Largest window.
    pub max_window: u64,
    /// Largest BWT block.
    pub max_bwt_block: u64,
    /// Largest block plain length.
    pub max_block_plain: u64,
    /// Largest frame payload.
    pub max_frame_payload: u64,
    /// Memory.
    pub memory: u64,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            max_window: 268_435_456,
            max_bwt_block: 67_108_864,
            max_block_plain: 1_073_741_824,
            max_frame_payload: 1_073_741_824,
            memory: 2_147_483_648,
        }
    }
}

/// What the caller gives the reader.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Password bytes.
    pub password: Option<Vec<u8>>,
    /// Keyfile bytes.
    pub keyfile: Option<Vec<u8>>,
    /// Prior store by BLAKE3 id.
    pub priors: HashMap<[u8; 32], Vec<u8>>,
    /// Resources.
    pub resources: Resources,
}

impl Options {
    /// Adds a prior; its id is its BLAKE3.
    pub fn add_prior(&mut self, bytes: Vec<u8>) {
        self.priors.insert(*blake3::hash(&bytes).as_bytes(), bytes);
    }
}

/// The counts of a clean verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifySummary {
    /// Entries.
    pub entries: usize,
    /// Chunks.
    pub chunks: usize,
    /// Blocks.
    pub blocks: usize,
}

/// An opened archive.
#[derive(Debug)]
pub struct Archive {
    data: Vec<u8>,
    /// The header.
    pub header: Header,
    /// The trailer.
    pub trailer: Trailer,
    /// Offset of the trailer frame.
    pub trailer_offset: usize,
    /// The index.
    pub index: Index,
    sealer: Option<Sealer>,
    opts: Options,
    cache: Option<(usize, Vec<u8>)>,
}

/// The diagnosis walk of section 6 ("Truncated versus corrupt"): returns the error to report.
pub fn diagnose(data: &[u8], limit: u64) -> Error {
    if let Err(e) = parse_header(data) {
        return e;
    }
    let mut pos = HEADER_LEN;
    let mut after_trailer = false;
    let mut last_bad_trailer = false;
    loop {
        if pos == data.len() {
            if last_bad_trailer {
                return Error::new("NoTrailer", "the last frame is not a trailer");
            }
            return Error::truncated("trailer");
        }
        match parse_frame(data, pos, limit) {
            Err(e) if e.class == "Truncated" => return Error::truncated("trailer"),
            Err(e) => {
                if after_trailer {
                    return Error::trailing("archive");
                }
                return e;
            }
            Ok(f) => {
                if !f.hash_ok {
                    return Error::hash_mismatch(f.kind);
                }
                if f.kind > kind::KEY_SLOT && f.flags & crate::wire::FF_MUST_UNDERSTAND != 0 {
                    return Error::new(
                        "UnknownFrameKind",
                        format!("unknown frame kind {} must be understood", f.kind),
                    );
                }
                after_trailer = f.kind == kind::TRAILER;
                last_bad_trailer = after_trailer && !crate::wire::is_trailer_shape(&f);
                pos = f.end;
            }
        }
    }
}

impl Archive {
    /// Opens an archive held in memory (section 6 "Opening an archive", section 14 order).
    pub fn open(data: Vec<u8>, opts: Options) -> Result<Self> {
        let limit = opts.resources.max_frame_payload;
        if data.len() < HEADER_LEN + TRAILER_FRAME_LEN {
            return Err(diagnose(&data, limit));
        }
        let header = parse_header(&data)?;
        let mut sealer = None;
        if header.encrypted() {
            let f = parse_frame(&data, HEADER_LEN, limit)?;
            if f.kind != kind::KEY_SLOT {
                return Err(Error::new(
                    "MissingKeySlot",
                    "the first frame is not the key slot",
                ));
            }
            f.check_hash()?;
            crypto::check_seal_flag(f.kind, f.sealed(), true, header.listable())?;
            let slot = crypto::parse_key_slot(f.payload, opts.resources.memory)?;
            let Some(pw) = opts.password.as_deref() else {
                return Err(Error::new("PasswordRequired", "a password is required"));
            };
            let key = crypto::unwrap_key(
                &slot,
                pw,
                opts.keyfile.as_deref(),
                &header.archive_id,
                header.flags,
            )?;
            sealer = Some(Sealer::new(key, slot.suite, header.archive_id));
        }
        let Some((trailer_offset, trailer)) = trailer_ending_at(&data, data.len()) else {
            return Err(diagnose(&data, limit));
        };
        if trailer.archive_id != header.archive_id {
            return Err(Error::new(
                "ArchiveIdMismatch",
                "trailer archive id differs",
            ));
        }
        if trailer.generation > (data.len() / TRAILER_FRAME_LEN) as u64 {
            return Err(Error::new("BadTrailer", "bad trailer: generation"));
        }
        let io = trailer.index_offset;
        let il = trailer.index_len;
        let in_range = io >= HEADER_LEN as u64
            && il >= MIN_FRAME_LEN
            && io
                .checked_add(il)
                .is_some_and(|e| e <= trailer_offset as u64);
        if !in_range {
            return Err(Error::bad_location("index"));
        }
        let (io, il) = (io as usize, il as usize);
        let f = parse_frame(&data[..io + il], io, limit).map_err(|e| {
            if e.class == "Truncated" {
                Error::bad_location("index")
            } else {
                e
            }
        })?;
        if f.kind != kind::INDEX {
            return Err(Error::new(
                "WrongFrameKind",
                format!("expected frame kind 5, found {}", f.kind),
            ));
        }
        f.check_hash()?;
        if f.len() != il {
            return Err(Error::bad_location("index"));
        }
        if blake3::hash(f.payload).as_bytes() != &trailer.index_hash {
            return Err(Error::new("IndexHashMismatch", "index hash mismatch"));
        }
        crypto::check_seal_flag(f.kind, f.sealed(), header.encrypted(), header.listable())?;
        let plain = match &sealer {
            Some(s) => s.open(
                kind::INDEX,
                u64::MAX - trailer.generation,
                &trailer.salt,
                f.payload,
            )?,
            None => f.payload.to_vec(),
        };
        let index = parse_index(&plain, trailer.index_offset)?;
        // Generation table against the trailer (section 15).
        let gens = &index.generations;
        let gen_ok = gens.len() as u64 == trailer.generation + 1
            && gens
                .last()
                .is_some_and(|g| g.generation == trailer.generation && g.salt == trailer.salt);
        if !gen_ok {
            return Err(Error::new(
                "BadGenerationTable",
                "generation table does not match the trailer",
            ));
        }
        // Refusal rule (section 7).
        let env = index.envelope;
        let r = opts.resources;
        for (name, need, allow) in [
            ("max_window", env.max_window, r.max_window),
            ("max_bwt_block", env.max_bwt_block, r.max_bwt_block),
            ("max_block_plain", env.max_block_plain, r.max_block_plain),
            (
                "max_frame_payload",
                env.max_frame_payload,
                r.max_frame_payload,
            ),
            ("decode_memory", env.decode_memory, r.memory),
        ] {
            if need > allow {
                return Err(Error::new(
                    "Refused",
                    format!("the archive needs {name} of {need} bytes; this reader allows {allow}"),
                ));
            }
        }
        Ok(Self {
            data,
            header,
            trailer,
            trailer_offset,
            index,
            sealer,
            opts,
            cache: None,
        })
    }

    /// The raw archive bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    fn frame_limit(&self) -> u64 {
        self.index
            .envelope
            .max_frame_payload
            .min(self.opts.resources.max_frame_payload)
    }

    /// Reads a recorded frame bounded by its location: kind, hash and length checked.
    fn read_recorded(&self, l: &Loc, want: u16, what: &str) -> Result<Frame<'_>> {
        let end = l
            .offset
            .checked_add(l.len)
            .filter(|e| *e <= self.data.len() as u64)
            .ok_or_else(|| Error::bad_location(what))? as usize;
        let f =
            parse_frame(&self.data[..end], l.offset as usize, self.frame_limit()).map_err(|e| {
                if e.class == "Truncated" {
                    Error::bad_location(what)
                } else {
                    e
                }
            })?;
        if f.kind != want {
            return Err(Error::new(
                "WrongFrameKind",
                format!("expected frame kind {want}, found {}", f.kind),
            ));
        }
        f.check_hash()?;
        if f.end != end {
            return Err(Error::bad_location(what));
        }
        Ok(f)
    }

    /// The salt of the generation that wrote the frame of a sequence.
    fn salt_for(&self, sequence: u64) -> [u8; 16] {
        self.index
            .generations
            .iter()
            .rev()
            .find(|g| g.first_sequence <= sequence)
            .map(|g| g.salt)
            .unwrap_or([0; 16])
    }

    /// The plain payload of a frame (opened when sealed).
    fn unseal(&self, f: &Frame<'_>, sequence: u64) -> Result<Vec<u8>> {
        crypto::check_seal_flag(
            f.kind,
            f.sealed(),
            self.header.encrypted(),
            self.header.listable(),
        )?;
        match (&self.sealer, f.sealed()) {
            (Some(s), true) => s.open(f.kind, sequence, &self.salt_for(sequence), f.payload),
            (None, true) => Err(Error::new("PasswordRequired", "a password is required")),
            _ => Ok(f.payload.to_vec()),
        }
    }

    /// The entry table, checked against the index's hash.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let l = self.index.entry_table;
        let f = self.read_recorded(&l, kind::ENTRY_TABLE, "entry table")?;
        if blake3::hash(f.payload).as_bytes() != &self.index.entry_table_hash {
            return Err(Error::new(
                "EntryTableMismatch",
                "entry table hash mismatch",
            ));
        }
        let plain = self.unseal(&f, l.sequence)?;
        parse_entry_table(&plain)
    }

    /// The number of records (0 without a `Records` frame).
    pub fn record_count(&self) -> Result<u64> {
        match self.index.records {
            None => Ok(0),
            Some(l) => {
                let f = self.read_recorded(&l, kind::RECORDS, "records")?;
                let plain = self.unseal(&f, l.sequence)?;
                Ok(parse_records(&plain)? as u64)
            }
        }
    }

    /// Decodes block `b` to its plain bytes.
    pub fn block_plain(&self, b: usize, record_count: u64) -> Result<Vec<u8>> {
        let bl = self.index.blocks[b];
        let l = Loc {
            offset: bl.frame_offset,
            len: bl.frame_len,
            sequence: bl.sequence,
        };
        let f = self.read_recorded(&l, kind::CHUNK_DATA, "block")?;
        let plain = self.unseal(&f, bl.sequence)?;
        let h = block::parse_block(&plain, record_count)?;
        if h.plain_len != bl.plain_len {
            return Err(Error::new(
                "BlockLengthMismatch",
                format!("block {b} header plain_len differs from the block table"),
            ));
        }
        let env = self.index.envelope;
        for s in &h.steps {
            if let block::Prim::Zstd { dictionary, .. } = s {
                if *dictionary != [0; 32] && !self.index.priors.contains(dictionary) {
                    return Err(Error::new("UnlistedPrior", "block names an unlisted prior"));
                }
            }
            if s.window() > env.max_window {
                return Err(Error::new(
                    "EnvelopeMismatch",
                    "envelope field max_window below a block's window",
                ));
            }
        }
        if h.bwt_block > env.max_bwt_block {
            return Err(Error::new(
                "EnvelopeMismatch",
                "envelope field max_bwt_block below a block's BWT block",
            ));
        }
        let ctx = DecodeCtx {
            priors: &self.opts.priors,
            max_window: self.opts.resources.max_window,
            max_block_plain: self.opts.resources.max_block_plain,
        };
        block::decode(&h, &ctx)
    }

    /// The bytes of chunk `i`, checked against the chunk table. Errors of the block are
    /// reported as `ChunkMismatch` of the chunk (section 9).
    pub fn chunk(&mut self, i: u64, record_count: u64) -> Result<Vec<u8>> {
        let n = self.index.chunks.len() as u64;
        if i >= n {
            return Err(Error::new(
                "ChunkIndexOutOfRange",
                format!("chunk index {i} out of range ({n})"),
            ));
        }
        let iu = i as usize;
        let b = self.index.chunk_block[iu];
        if self.cache.as_ref().map(|c| c.0) != Some(b) {
            let plain = self
                .block_plain(b, record_count)
                .map_err(|e| Error::new("ChunkMismatch", format!("chunk {i} mismatch: {e}")))?;
            self.cache = Some((b, plain));
        }
        let rec = self.index.chunks[iu];
        let off = self.index.chunk_offset[iu] as usize;
        let bytes = self
            .cache
            .as_ref()
            .and_then(|c| c.1.get(off..off + rec.plain_len as usize))
            .ok_or_else(|| Error::new("ChunkMismatch", format!("chunk {i} mismatch")))?;
        if blake3::hash(bytes).as_bytes() != &rec.hash {
            return Err(Error::new("ChunkMismatch", format!("chunk {i} mismatch")));
        }
        Ok(bytes.to_vec())
    }

    /// Checks a file entry's chunk list against the table (section 5, no block read).
    pub fn check_file_chunks(&self, e: &Entry) -> Result<()> {
        let n = self.index.chunks.len() as u64;
        let mut sum = 0u64;
        let mut overflow = false;
        for &c in &e.chunks {
            if c >= n {
                return Err(Error::new(
                    "ChunkIndexOutOfRange",
                    format!("chunk index {c} out of range ({n})"),
                ));
            }
        }
        for &c in &e.chunks {
            match sum.checked_add(self.index.chunks[c as usize].plain_len) {
                Some(s) => sum = s,
                None => overflow = true,
            }
        }
        if overflow {
            sum = u64::MAX;
        }
        if sum != e.size {
            return Err(Error::new(
                "FileSizeMismatch",
                format!("file size mismatch: expected {}, found {sum}", e.size),
            ));
        }
        Ok(())
    }

    /// The bytes of a file entry, every chunk verified.
    pub fn read_file(&mut self, e: &Entry, record_count: u64) -> Result<Vec<u8>> {
        if e.kind != EntryKind::File {
            return Err(Error::new("NotAFile", format!("{} is not a file", e.path)));
        }
        self.check_file_chunks(e)?;
        let mut out = Vec::with_capacity(e.size as usize);
        for &c in &e.chunks {
            out.extend_from_slice(&self.chunk(c, record_count)?);
        }
        Ok(out)
    }

    /// Whole-archive verification (section 9 and the gate of CONFORMANCE.md).
    pub fn verify(&mut self) -> Result<VerifySummary> {
        let record_count = self.record_count()?;
        let entries = self.entries()?;
        for b in 0..self.index.blocks.len() {
            let plain = self.block_plain(b, record_count)?;
            let bl = self.index.blocks[b];
            for i in bl.first_chunk..bl.first_chunk + bl.chunk_count {
                let iu = i as usize;
                let rec = self.index.chunks[iu];
                let off = self.index.chunk_offset[iu] as usize;
                let ok = plain
                    .get(off..off + rec.plain_len as usize)
                    .is_some_and(|s| blake3::hash(s).as_bytes() == &rec.hash);
                if !ok {
                    return Err(Error::new("ChunkMismatch", format!("chunk {i} mismatch")));
                }
            }
        }
        for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
            self.check_file_chunks(e)?;
        }
        let rep = recovery::scan(
            &self.data,
            &self.index.recovery,
            &self.index.generations,
            self.trailer.index_offset,
            None,
            self.opts.resources.memory,
        )?;
        if rep.damaged > 0 || rep.unusable > 0 {
            return Err(Error::new(
                "DamageFound",
                format!(
                    "damage found: {} shards damaged, {} recovery frames unusable",
                    rep.damaged, rep.unusable
                ),
            ));
        }
        journal::history(&self.data)?;
        Ok(VerifySummary {
            entries: entries.len(),
            chunks: self.index.chunks.len(),
            blocks: self.index.blocks.len(),
        })
    }
}

/// Parses a `Records` payload (section 12): structure and every `body_hash`; returns the count.
/// Body fields are not interpreted (no reconstruction primitive runs in this decoder).
pub fn parse_records(payload: &[u8]) -> Result<usize> {
    let mut c = crate::wire::Cursor::new(payload, "records");
    let n = c.count(5)?;
    for id in 0..n {
        let k = c.u16()?;
        if !(7..=12).contains(&k) {
            return Err(Error::new(
                "UnknownRecordKind",
                format!("record {id} has unknown kind {k}"),
            ));
        }
        if c.u16()? != 0 {
            return Err(Error::new(
                "ReservedRecordBits",
                format!("record {id} flags"),
            ));
        }
        let len = c.varint()?;
        if len > c.remaining() as u64 {
            return Err(Error::truncated("records"));
        }
        let body = c.bytes(len as usize)?;
        let h: [u8; 32] = c.array()?;
        if blake3::hash(body).as_bytes() != &h {
            return Err(Error::new(
                "RecordHashMismatch",
                format!("record {id} body hash mismatch"),
            ));
        }
    }
    if c.remaining() != 0 {
        return Err(Error::trailing("records"));
    }
    Ok(n as usize)
}
