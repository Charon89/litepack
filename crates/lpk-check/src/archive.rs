//! Opening an archive (section 6), reading blocks and chunks (section 9), verification.

use std::collections::HashMap;

use crate::block::{self, DecodeCtx};
use crate::crypto::{self, Sealer};
use crate::entries::{parse_entry_table, Entry, EntryKind};
use crate::error::{Error, Result};
use crate::index::{parse_index, Index};
use crate::journal;
use crate::record::{self, bad_record, Body, JpegRecord, Record};
use crate::wire::{
    kind, parse_frame, parse_header, read_recorded, trailer_ending_at, Frame, Header, Trailer,
    HEADER_LEN, MIN_FRAME_LEN, TRAILER_FRAME_LEN,
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
    /// Behave as a revision 1.0 reader: `jpeg-reconstruct` is `UnimplementedPrimitive`.
    pub revision_1_0: bool,
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

/// The result of opening steps 1 to 4 (header, key slot, trailer).
#[derive(Debug, Clone)]
pub struct Head {
    /// The header.
    pub header: Header,
    /// The sealer, when the archive is encrypted and credentials were given.
    pub sealer: Option<Sealer>,
    /// End of the key slot frame (encrypted archives), else the end of the header.
    pub body_start: usize,
    /// Offset of the last trailer frame.
    pub trailer_offset: usize,
    /// The last trailer.
    pub trailer: Trailer,
}

/// The sealing rules of section 14 for a frame met by a walk at position `seq` (0 = first frame
/// after the header): the key slot's place and the SEALED flag of known kinds.
pub fn walk_sealing_rules(f: &Frame<'_>, seq: u64, header: &Header) -> Result<()> {
    let enc = header.encrypted();
    if enc && seq == 0 && f.kind != kind::KEY_SLOT {
        return Err(Error::new(
            "MissingKeySlot",
            "the first frame is not the key slot",
        ));
    }
    if f.kind == kind::KEY_SLOT && (!enc || seq != 0) {
        return Err(Error::new("UnexpectedKeySlot", "unexpected key slot"));
    }
    if crypto::sealed_kind(f.kind, header.listable()).is_some() {
        crypto::check_seal_flag(f.kind, f.sealed(), enc, header.listable())?;
    }
    Ok(())
}

/// The diagnosis walk of section 6 ("Truncated versus corrupt"): returns the error to report.
pub fn diagnose(data: &[u8], limit: u64) -> Error {
    let header = match parse_header(data) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let mut pos = HEADER_LEN;
    let mut seq = 0u64;
    let mut after_trailer = false;
    let mut last_bad_trailer = false;
    loop {
        if pos == data.len() {
            if last_bad_trailer {
                return Error::new("NoTrailer", "the last frame is not a trailer");
            }
            return Error::truncated("trailer");
        }
        let f = match parse_frame(data, pos, limit) {
            Err(e) if e.class == "Truncated" => return Error::truncated("trailer"),
            Err(_) if after_trailer => return Error::trailing("archive"),
            Err(e) => return e,
            Ok(f) => f,
        };
        if !f.hash_ok {
            if after_trailer {
                return Error::trailing("archive");
            }
            return Error::hash_mismatch(f.kind);
        }
        if let Err(e) = walk_sealing_rules(&f, seq, &header) {
            return e;
        }
        after_trailer = f.kind == kind::TRAILER;
        last_bad_trailer = after_trailer && !crate::wire::is_trailer_shape(&f);
        pos = f.end;
        seq += 1;
    }
}

/// Opening steps 1 to 4 of section 6: header, key slot (unwrapped when credentials are given),
/// last trailer.
pub fn open_head(data: &[u8], opts: &Options) -> Result<Head> {
    let limit = opts.resources.max_frame_payload;
    if data.len() < HEADER_LEN + TRAILER_FRAME_LEN {
        return Err(diagnose(data, limit));
    }
    let header = parse_header(data)?;
    let mut sealer = None;
    let mut body_start = HEADER_LEN;
    let first_kind = u16::from_le_bytes([data[HEADER_LEN], data[HEADER_LEN + 1]]);
    if header.encrypted() {
        if first_kind != kind::KEY_SLOT {
            return Err(Error::new(
                "MissingKeySlot",
                "the first frame is not the key slot",
            ));
        }
        let f = parse_frame(data, HEADER_LEN, crypto::KEY_SLOT_LEN as u64).map_err(|e| {
            if e.class == "PayloadTooLarge" {
                Error::new("BadKeySlot", "bad key slot: length")
            } else {
                e
            }
        })?;
        f.check_hash()?;
        if f.sealed() {
            return Err(Error::new(
                "UnexpectedSealedFrame",
                "frame kind 7 is sealed but must not be",
            ));
        }
        let slot = crypto::parse_key_slot(f.payload)?;
        body_start = f.end;
        if let Some(pw) = opts.password.as_deref() {
            crypto::check_argon2_memory(&slot, opts.resources.memory)?;
            let key = crypto::unwrap_key(
                &slot,
                pw,
                opts.keyfile.as_deref(),
                &header.archive_id,
                header.flags,
            )?;
            sealer = Some(Sealer::new(key, slot.suite, header.archive_id));
        }
    } else if first_kind == kind::KEY_SLOT {
        return Err(Error::new(
            "UnexpectedKeySlot",
            "a key slot in an archive that is not encrypted",
        ));
    }
    let Some((trailer_offset, trailer)) = trailer_ending_at(data, data.len()) else {
        return Err(diagnose(data, limit));
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
    if trailer.generation == 0 && trailer.previous_trailer_offset != 0 {
        return Err(Error::new(
            "BadTrailer",
            "bad trailer: previous_trailer_offset",
        ));
    }
    Ok(Head {
        header,
        sealer,
        body_start,
        trailer_offset,
        trailer,
    })
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

impl Archive {
    /// Opens an archive held in memory (section 6 "Opening an archive"). An encrypted archive
    /// without credentials is `PasswordRequired` here; see [`crate::keyless`] for that case.
    pub fn open(data: Vec<u8>, opts: Options) -> Result<Self> {
        let head = open_head(&data, &opts)?;
        let Head {
            header,
            sealer,
            trailer_offset,
            trailer,
            ..
        } = head;
        if header.encrypted() && sealer.is_none() {
            return Err(Error::new("PasswordRequired", "a password is required"));
        }
        // 6. The index location and frame.
        let io = trailer.index_offset;
        let il = trailer.index_len;
        let in_range = io >= HEADER_LEN as u64
            && il >= MIN_FRAME_LEN
            && io.checked_add(il) == Some(trailer_offset as u64);
        if !in_range {
            return Err(Error::bad_location("index"));
        }
        let limit = opts.resources.max_frame_payload;
        let f = read_recorded(&data, io, il, kind::INDEX, "index", limit)?;
        // 7. The trailer's index hash over the payload as stored.
        if blake3::hash(f.payload).as_bytes() != &trailer.index_hash {
            return Err(Error::new("IndexHashMismatch", "index hash mismatch"));
        }
        // 8. Sealing.
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
        // 9. Parse.
        let index = parse_index(&plain, trailer.index_offset)?;
        // 10. The generation table against the trailer.
        let gens = &index.generations;
        let gen_err =
            |r: &str| Error::new("BadGenerationTable", format!("bad generation table: {r}"));
        if gens.len() as u64 != trailer.generation + 1 {
            return Err(gen_err("count"));
        }
        let last = gens.last().ok_or_else(|| gen_err("count"))?;
        if last.salt != trailer.salt {
            return Err(gen_err("salt"));
        }
        if trailer.generation > 0
            && Some(last.start_offset) != trailer.previous_trailer_offset.checked_add(133)
        {
            return Err(gen_err("start_offset"));
        }
        if !header.encrypted() && gens.iter().any(|g| g.salt != [0; 16]) {
            return Err(gen_err("salt not zero"));
        }
        // 11. Refusal rule (section 7).
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

    fn recorded(&self, offset: u64, len: u64, want: u16, what: &str) -> Result<Frame<'_>> {
        read_recorded(&self.data, offset, len, want, what, self.frame_limit())
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

    /// The entry table (section 6 step 12).
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let l = self.index.entry_table;
        let f = self.recorded(l.offset, l.len, kind::ENTRY_TABLE, "entry table")?;
        let plain = self.unseal(&f, l.sequence)?;
        if blake3::hash(f.payload).as_bytes() != &self.index.entry_table_hash {
            return Err(Error::new(
                "EntryTableMismatch",
                "entry table hash mismatch",
            ));
        }
        parse_entry_table(&plain)
    }

    /// The plain payload of the `Records` frame, when the index lists one.
    fn records_payload(&self) -> Result<Option<Vec<u8>>> {
        match self.index.records {
            None => Ok(None),
            Some(l) => {
                let f = self.recorded(l.offset, l.len, kind::RECORDS, "records")?;
                Ok(Some(self.unseal(&f, l.sequence)?))
            }
        }
    }

    /// The number of records (0 without a `Records` frame): the frame's structure and every
    /// `body_hash` are checked.
    pub fn record_count(&self) -> Result<u64> {
        match self.records_payload()? {
            None => Ok(0),
            Some(p) => Ok(record::walk(&p, false, None)?.count),
        }
    }

    /// Every record, every body under its field rules (whole-archive verification).
    pub fn records(&self) -> Result<Vec<Record>> {
        match self.records_payload()? {
            None => Ok(Vec::new()),
            Some(p) => Ok(record::walk(&p, true, None)?.records),
        }
    }

    /// Record `id`, walking from the start (section 12).
    fn record(&self, id: u64) -> Result<Record> {
        let p = self.records_payload()?.unwrap_or_default();
        let w = if p.is_empty() {
            None
        } else {
            Some(record::walk(&p, true, Some(id))?)
        };
        let count = w.as_ref().map_or(0, |w| w.count);
        w.and_then(|mut w| w.records.pop())
            .filter(|_| id < count)
            .ok_or_else(|| {
                Error::new(
                    "RecordOutOfRange",
                    format!("record {id} out of range (count {count})"),
                )
            })
    }

    /// Parses block `b`'s header (order of checks 1 to 4 of section 8) and returns whether its
    /// graph names a reconstruction primitive, and the record ids it names.
    fn block_graph(&self, b: usize) -> Result<(bool, Vec<u64>)> {
        let bl = self.index.blocks[b];
        let f = self.recorded(bl.frame_offset, bl.frame_len, kind::CHUNK_DATA, "ChunkData")?;
        let plain = self.unseal(&f, bl.sequence)?;
        let h = block::parse_block(&plain, &mut || self.record_count())?;
        Ok((h.reconstruction, h.record_ids))
    }

    /// One chunk a record names, read for the reconstruction of block `b` (section 8 check 7):
    /// the chunk order and nesting checks, then the chunk's own checks. `cache` holds the plain
    /// bytes of the block read last.
    fn record_chunk(
        &self,
        id: u64,
        b: usize,
        i: u64,
        cache: &mut Option<(usize, Vec<u8>)>,
    ) -> Result<Vec<u8>> {
        let n = self.index.chunks.len() as u64;
        if i >= n {
            return Err(Error::new(
                "ChunkIndexOutOfRange",
                format!("chunk index {i} out of range ({n})"),
            ));
        }
        let iu = i as usize;
        let cb = self.index.chunk_block[iu];
        if cb >= b {
            return Err(bad_record(id, "chunk order"));
        }
        if cache.as_ref().map(|c| c.0) != Some(cb) {
            let (recon, _) = self.block_graph(cb)?;
            if recon {
                return Err(bad_record(id, "nested record"));
            }
            *cache = Some((cb, self.block_plain(cb)?));
        }
        let rec = self.index.chunks[iu];
        let off = self.index.chunk_offset[iu] as usize;
        let bytes = cache
            .as_ref()
            .and_then(|c| c.1.get(off..off + rec.plain_len as usize))
            .filter(|s| blake3::hash(s).as_bytes() == &rec.hash)
            .ok_or_else(|| Error::new("ChunkMismatch", format!("chunk {i} mismatch")))?;
        Ok(bytes.to_vec())
    }

    /// A `jpeg-reconstruct` step of block `b` (section 8, checks 1 to 8).
    fn jpeg_step(
        &self,
        b: usize,
        id: u64,
        stream: &[u8],
        bound: u64,
        last: bool,
    ) -> Result<Vec<u8>> {
        // (1) and (2).
        let r = self.record(id)?;
        let Body::Jpeg(j) = r.body else {
            return Err(bad_record(id, "kind"));
        };
        // (3).
        if j.lepton_version != 0 {
            return Err(bad_record(id, "lepton_version"));
        }
        // (4).
        if j.primary_len > bound {
            return Err(block::too_large(last, "jpeg-reconstruct"));
        }
        // (5).
        let frame = crate::jpeg::stream_frame(id, stream, j.primary_len)?;
        let env = self.index.envelope;
        crate::jpeg::check_memory(
            frame.memory_term(),
            env.decode_memory,
            self.opts.resources.memory,
            env.max_block_plain,
        )?;
        // (6).
        let primary = crate::jpeg::decode(id, stream, j.primary_len)?;
        // (7) and (8).
        self.assemble_check(b, id, &j, &primary)?;
        Ok(primary)
    }

    /// Check 7 (assembly in file order) and check 8 (`original_len`, `original_hash`).
    fn assemble_check(&self, b: usize, id: u64, j: &JpegRecord, primary: &[u8]) -> Result<()> {
        let mut h = blake3::Hasher::new();
        h.update(primary);
        let mut len = primary.len() as u64;
        if j.trailing.is_empty() {
            let mut cache = None;
            // The nested trailing data, fetched chunk by chunk as it is needed.
            let mut nested = j.nested_trailing_chunks.iter();
            let mut pending: Vec<u8> = Vec::new();
            for g in &j.gainmaps {
                if g.offset < len {
                    return Err(bad_record(id, "gainmaps"));
                }
                while len + (pending.len() as u64) < g.offset {
                    let Some(&c) = nested.next() else {
                        return Err(bad_record(id, "trailing"));
                    };
                    pending.extend(self.record_chunk(id, b, c, &mut cache)?);
                }
                let need = (g.offset - len) as usize;
                h.update(&pending[..need]);
                pending.drain(..need);
                len = g.offset;
                // Bytes of nested trailing data fetched past the offset follow the image.
                let mut glen = 0u64;
                for &c in &g.chunks {
                    let bytes = self.record_chunk(id, b, c, &mut cache)?;
                    glen += bytes.len() as u64;
                    h.update(&bytes);
                }
                if glen != g.len {
                    return Err(bad_record(id, "gainmaps"));
                }
                len += glen;
            }
            h.update(&pending);
            len += pending.len() as u64;
            for &c in nested {
                let bytes = self.record_chunk(id, b, c, &mut cache)?;
                len = len.saturating_add(bytes.len() as u64);
                h.update(&bytes);
            }
        } else {
            h.update(&j.trailing);
            len += j.trailing.len() as u64;
        }
        if len != j.original_len || h.finalize().as_bytes() != &j.original_hash {
            return Err(bad_record(id, "original_hash"));
        }
        Ok(())
    }

    /// The record checks of whole-archive verification (section 12): every record walked, the
    /// chunk indices and sums, the record ids of every block graph and the block order.
    fn verify_records(&self) -> Result<()> {
        if self.index.records.is_none() {
            return Ok(());
        }
        let records = self.records()?;
        let n = self.index.chunks.len() as u64;
        for (id, r) in records.iter().enumerate() {
            for (list, want) in r.chunk_lists() {
                let mut sum = 0u64;
                for &c in list {
                    if c >= n {
                        return Err(Error::new(
                            "ChunkIndexOutOfRange",
                            format!("chunk index {c} out of range ({n})"),
                        ));
                    }
                    sum = sum.saturating_add(self.index.chunks[c as usize].plain_len);
                }
                if sum != want {
                    return Err(bad_record(id as u64, "chunk lengths"));
                }
            }
        }
        // The first block whose graph names each record.
        let mut first = vec![usize::MAX; records.len()];
        for b in 0..self.index.blocks.len() {
            let (_, ids) = self.block_graph(b)?;
            for id in ids {
                if let Some(f) = first.get_mut(id as usize) {
                    *f = (*f).min(b);
                }
            }
        }
        for (id, r) in records.iter().enumerate() {
            for (list, _) in r.chunk_lists() {
                if list
                    .iter()
                    .any(|&c| self.index.chunk_block[c as usize] >= first[id])
                {
                    return Err(bad_record(id as u64, "chunk order"));
                }
            }
        }
        Ok(())
    }

    /// Decodes block `b` to its plain bytes, in the order of checks of section 8.
    pub fn block_plain(&self, b: usize) -> Result<Vec<u8>> {
        let bl = self.index.blocks[b];
        // 1. The frame at its recorded location.
        let f = self.recorded(bl.frame_offset, bl.frame_len, kind::CHUNK_DATA, "ChunkData")?;
        let plain = self.unseal(&f, bl.sequence)?;
        // 2 to 4. The graph, the record ids, plain_len and encoded_len.
        let h = block::parse_block(&plain, &mut || self.record_count())?;
        let env = self.index.envelope;
        // 5. Resources against the envelope.
        if h.steps.iter().any(|s| s.window() > env.max_window) {
            return Err(Error::new(
                "EnvelopeMismatch",
                "envelope field max_window below a block's window",
            ));
        }
        if h.bwt_block > env.max_bwt_block {
            return Err(Error::new(
                "EnvelopeMismatch",
                "envelope field max_bwt_block below a block's BWT block",
            ));
        }
        // 6. Priors listed.
        for s in &h.steps {
            if let block::Prim::Zstd { dictionary, .. } = s {
                if *dictionary != [0; 32] && !self.index.priors.contains(dictionary) {
                    return Err(Error::new("UnlistedPrior", "block names an unlisted prior"));
                }
            }
        }
        // 7. plain_len against the block table and the envelope.
        if h.plain_len != bl.plain_len {
            return Err(Error::new(
                "BlockLengthMismatch",
                format!("block {b} header plain_len differs from the block table"),
            ));
        }
        // 7 (bound) and 8 are checked first thing in `decode`.
        let ctx = DecodeCtx {
            priors: &self.opts.priors,
            max_window: self.opts.resources.max_window,
            max_block_plain: env.max_block_plain,
            jpeg: !self.opts.revision_1_0,
        };
        let mut recon = |id: u64, input: &[u8], bound: u64, last: bool| {
            self.jpeg_step(b, id, input, bound, last)
        };
        block::decode(&h, &ctx, &mut recon)
    }

    /// The bytes of chunk `i`, checked against the chunk table. A block whose frame fails keeps
    /// its own error (`HashMismatch` of kind 2, section 9).
    pub fn chunk(&mut self, i: u64) -> Result<Vec<u8>> {
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
            let plain = self.block_plain(b)?;
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
        for &c in &e.chunks {
            if c >= n {
                return Err(Error::new(
                    "ChunkIndexOutOfRange",
                    format!("chunk index {c} out of range ({n})"),
                ));
            }
        }
        let sum = e
            .chunks
            .iter()
            .try_fold(0u64, |s, &c| {
                s.checked_add(self.index.chunks[c as usize].plain_len)
            })
            .unwrap_or(u64::MAX);
        if sum != e.size {
            return Err(Error::new(
                "FileSizeMismatch",
                format!("file size mismatch: expected {}, found {sum}", e.size),
            ));
        }
        Ok(())
    }

    /// The bytes of a file entry, every chunk verified.
    pub fn read_file(&mut self, e: &Entry) -> Result<Vec<u8>> {
        if e.kind != EntryKind::File {
            return Err(Error::new("NotAFile", format!("{} is not a file", e.path)));
        }
        self.check_file_chunks(e)?;
        let mut out = Vec::with_capacity(e.size as usize);
        for &c in &e.chunks {
            out.extend_from_slice(&self.chunk(c)?);
        }
        Ok(out)
    }

    /// Whole-archive verification (section 9): the records, every block and chunk, every file's
    /// chunk list; recovery is left to `check`. The trailer chain is walked too (an addition of
    /// this decoder; the spec does not list it among `verify`'s checks).
    pub fn verify(&mut self) -> Result<VerifySummary> {
        self.verify_records()?;
        let entries = self.entries()?;
        for b in 0..self.index.blocks.len() {
            let plain = self.block_plain(b)?;
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
        journal::history(&self.data)?;
        Ok(VerifySummary {
            entries: entries.len(),
            chunks: self.index.chunks.len(),
            blocks: self.index.blocks.len(),
        })
    }
}
