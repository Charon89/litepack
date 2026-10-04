//! Writing an archive as a stream (spec section 9): header, `ChunkData` blocks,
//! entry table, index, trailer. The writer never seeks and never reads back.

use crate::archive::Archive;
use crate::chunk::{ChunkRecord, ChunkTableWriter};
use crate::crypto::{
    index_sequence, sealing_rule, ArchiveKey, Argon2Params, Credentials, KeySlot, Sealer, Suite,
};
use crate::entry::{check_entry, Entry, EntryFlags, EntryKind, EntryTableWriter};
use crate::envelope::{ArchiveSizes, Envelope};
use crate::error::FormatError;
use crate::frame::{Frame, FrameFlags, FrameKind};
use crate::graph::{BlockHeader, Graph, Step, MAX_STEPS};
use crate::header::{Header, HeaderFlags};
use crate::index::{BlockLocation, FrameLocation, GenerationInfo, Index};
use crate::merkle::merkle_root;
use crate::primitive::{GraphResources, PrimitiveId};
use crate::record::{Record, RecordsWriter};
use crate::recovery::{GroupEncoder, RecoveryFrame, RecoveryOptions};
use crate::trailer::{Trailer, TRAILER_FRAME_LEN};
use crate::varint;
use rand::RngCore;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};

/// Smallest `chunk_size` a writer accepts.
pub const MIN_CHUNK_SIZE: u64 = 4096;
/// Default `chunk_size`: 1 MiB.
pub const DEFAULT_CHUNK_SIZE: u64 = 1 << 20;
/// Default `block_size`: 64 MiB.
pub const DEFAULT_BLOCK_SIZE: u64 = 1 << 26;

/// Cuts the bytes of one file into chunks, as a stream.
///
/// The writer feeds the bytes of a file in consecutive pieces. The chunker
/// answers with cut positions measured in the stream of bytes it has been fed
/// and not yet cut (the held-back tail of earlier calls followed by `bytes`);
/// they must be strictly increasing and at most the length of that stream.
/// The writer keeps the bytes after the last cut and counts them in the next
/// call. Rules the writer enforces (`BadChunk` otherwise): no chunk is longer
/// than `chunk_size`, the held-back tail is at most `chunk_size`, and when
/// `eof` is true every byte is cut. The writer calls [`Chunker::reset`]
/// before each file, so chunks never cross files.
pub trait Chunker {
    /// Take the next `bytes` of the file (`eof` marks the last call, possibly
    /// with no bytes) and return the cut positions.
    fn feed(&mut self, bytes: &[u8], eof: bool) -> Vec<usize>;
    /// Forget everything about the previous file.
    fn reset(&mut self);
}

/// Cuts into pieces of exactly `size` bytes, the last one shorter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedChunker {
    size: usize,
    held: usize,
}

impl FixedChunker {
    /// A chunker with pieces of `size` bytes (at least 1).
    pub fn new(size: usize) -> Self {
        FixedChunker {
            size: size.max(1),
            held: 0,
        }
    }
}

impl Chunker for FixedChunker {
    fn feed(&mut self, bytes: &[u8], eof: bool) -> Vec<usize> {
        let total = self.held + bytes.len();
        let mut cuts: Vec<usize> = (1..=total / self.size).map(|k| k * self.size).collect();
        let last = cuts.last().copied().unwrap_or(0);
        if eof {
            if last < total {
                cuts.push(total);
            }
            self.held = 0;
        } else {
            self.held = total - last;
        }
        cuts
    }

    fn reset(&mut self) {
        self.held = 0;
    }
}

/// One encoded block: the bytes, the decode graph that turns them back into the
/// plain bytes, and the decoder resources that graph needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    /// The decode graph of this block. The writer validates it when it closes
    /// the block (registry, step count, parameters, record range) and records
    /// it in the block's header.
    pub graph: Graph,
    /// The encoded bytes: the graph's decoding gives back the plain bytes.
    pub bytes: Vec<u8>,
    /// The decoder resources `graph` needs, for the envelope.
    pub resources: GraphResources,
}

/// Turns the plain bytes of a block into encoded bytes, choosing the decode
/// graph per block.
pub trait BlockEncoder {
    /// Encode one block's plain bytes with a graph of the encoder's choice.
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError>;
}

/// The identity encoder: a one-step `store` graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreEncoder;

impl BlockEncoder for StoreEncoder {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        Ok(Encoded {
            graph: Graph {
                steps: vec![Step {
                    primitive: PrimitiveId::Store,
                    params: Vec::new(),
                }],
            },
            bytes: plain.to_vec(),
            resources: GraphResources::default(),
        })
    }
}

/// Encryption settings of a [`Writer`] (spec section 14).
#[derive(Debug, Clone)]
pub struct SealOptions {
    /// Cipher suite.
    pub suite: Suite,
    /// Argon2id cost; checked against the bounds.
    pub argon2: Argon2Params,
    /// Write the entry table in clear, so the archive can be listed without
    /// the key.
    pub listable: bool,
    /// The password, and the keyfile when there is one.
    pub credentials: Credentials,
}

/// Settings of a [`Writer`].
pub struct WriterOptions {
    /// Chunk length for the fixed cut; at least [`MIN_CHUNK_SIZE`].
    pub chunk_size: u64,
    /// Most plain bytes in a block; at least `chunk_size`. A block is closed
    /// before the chunk that would exceed it.
    pub block_size: u64,
    /// The archive's identity, written to the header and the trailer.
    pub archive_id: [u8; 16],
    /// Encodes every block and names its decode graph (per block);
    /// [`StoreEncoder`] by default.
    pub encoder: Box<dyn BlockEncoder>,
    /// Reconstruction records, written as one `Records` frame before the
    /// index (none: no frame). A record's id is its position; the encoder's
    /// graph may only name ids below `records.len()`.
    pub records: Vec<Record>,
    /// Reed-Solomon recovery over the body (spec section 13): `percent` 0
    /// (the default) writes no recovery frame.
    pub recovery: RecoveryOptions,
    /// Encrypt the archive (none: plain). The archive key and the Argon2 salt
    /// come from the RNG the writer is given ([`Writer::new_with_rng`]), the
    /// thread's RNG otherwise.
    pub seal: Option<SealOptions>,
}

/// The output with the recovery spool beside it: while the spool is set, every
/// byte written is also fed to it.
struct Tee<W: Write> {
    inner: W,
    spool: Option<GroupEncoder>,
}

impl<W: Write> Write for Tee<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        if let Some(s) = &mut self.spool {
            s.feed(&buf[..n]).map_err(std::io::Error::other)?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl std::fmt::Debug for WriterOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterOptions")
            .field("recovery", &self.recovery)
            .field("seal", &self.seal)
            .field("records", &self.records.len())
            .field("chunk_size", &self.chunk_size)
            .field("block_size", &self.block_size)
            .field("archive_id", &self.archive_id)
            .finish()
    }
}

impl Default for WriterOptions {
    fn default() -> Self {
        WriterOptions {
            chunk_size: DEFAULT_CHUNK_SIZE,
            block_size: DEFAULT_BLOCK_SIZE,
            archive_id: [0; 16],
            encoder: Box::new(StoreEncoder),
            records: Vec::new(),
            recovery: RecoveryOptions::default(),
            seal: None,
        }
    }
}

/// What a finished archive holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterSummary {
    /// Entries written.
    pub entries: u64,
    /// Chunks in the chunk table.
    pub chunks: u64,
    /// `ChunkData` blocks.
    pub blocks: u64,
    /// Total length of the archive in bytes.
    pub archive_len: u64,
    /// Most bytes of recovery shards held at once (one group's; 0 without recovery).
    pub recovery_peak: u64,
    /// The generation written: 0 for [`Writer::new`], one more than the
    /// archive's for [`Writer::append`].
    pub generation: u64,
    /// Chunks this write added to the chunk table (all of them in generation 0).
    pub new_chunks: u64,
    /// Chunks an append found in the old chunk table and referenced instead of
    /// writing (always 0 outside [`Writer::append`]).
    pub reused_chunks: u64,
}

/// What an append starts from: the previous generation, read from the archive.
struct Base {
    generation: u64,
    trailer_offset: u64,
    chunks: u64,
    by_hash: HashMap<[u8; 32], u64>,
    entries: Vec<Entry>,
    records: Option<FrameLocation>,
}

/// Builds an archive in one pass over a stream of entries.
pub struct Writer<W: Write> {
    out: Tee<W>,
    options: WriterOptions,
    chunker: Box<dyn Chunker>,
    pos: u64,
    entries: Vec<Entry>,
    records: Vec<ChunkRecord>,
    blocks: Vec<BlockLocation>,
    priors: BTreeSet<[u8; 32]>,
    /// Records the archive will have: a block's graph may name only those.
    record_count: u64,
    /// Paths of the entries added so far, to refuse one twice.
    seen: HashSet<String>,
    pending: Vec<u8>,
    pending_chunks: u64,
    /// The first I/O error seen; every later call fails with it.
    failed: Option<(std::io::ErrorKind, String)>,
    /// Set after a chunker broke the rules; every later call fails with it.
    bad_chunk: Option<&'static str>,
    /// Locations of the recovery frames written so far.
    recovery_locs: Vec<FrameLocation>,
    /// Offset where the open recovery group starts.
    cover_start: u64,
    /// Seals the payloads (none: plain archive).
    sealer: Option<Sealer>,
    /// Position of the next frame among the archive's frames (section 14).
    seq: u64,
    /// BLAKE3 of the payload of the frame written last, as stored.
    last_hash: [u8; 32],
    /// True when the entry table of an encrypted archive is in clear (section 14).
    listable: bool,
    /// The previous generation (an append), or none.
    base: Option<Base>,
    /// Paths an append removes from the entry table.
    deleted: BTreeSet<String>,
    /// Chunks of this write an append took from the old table.
    reused: u64,
    /// Window and BWT block maxima of the earlier generations' graphs and of
    /// this write's blocks so far.
    graph_res: GraphResources,
    /// This generation's nonce salt (zeros when the archive is not encrypted).
    salt: [u8; 16],
    /// The generation table of the new index: the earlier generations, then this one.
    generations: Vec<GenerationInfo>,
    /// Called before the trailer is written, to put the data on disk (section 15).
    sync: Option<Box<dyn FnMut() -> std::io::Result<()>>>,
}

/// Whole encoded length of a frame with a payload of `payload_len` bytes.
fn frame_len(payload_len: u64) -> u64 {
    4 + varint::len(payload_len) as u64 + payload_len + 32
}

impl<W: Write> std::fmt::Debug for Writer<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writer")
            .field("pos", &self.pos)
            .field("entries", &self.entries.len())
            .field("chunks", &self.records.len())
            .field("blocks", &self.blocks.len())
            .finish()
    }
}

fn bad_options(reason: &'static str) -> FormatError {
    FormatError::BadOptions { reason }
}

/// The checks a reader makes of a block's graph: step count, parameters and
/// the records it names (`record_count` is how many the archive will have).
fn check_graph(graph: &Graph, record_count: u64) -> Result<(), FormatError> {
    if graph.steps.is_empty() || graph.steps.len() > MAX_STEPS {
        return Err(FormatError::BadGraph {
            reason: "step count",
        });
    }
    for s in &graph.steps {
        s.primitive.validate_params(&s.params)?;
    }
    graph.check_records(record_count)
}

/// Check `options` the way a reader's rules need them checked. Returns the
/// recovery spool.
fn check_options(options: &WriterOptions) -> Result<Option<GroupEncoder>, FormatError> {
    if options.chunk_size < MIN_CHUNK_SIZE {
        return Err(bad_options("chunk_size below 4 KiB"));
    }
    if options.block_size < options.chunk_size {
        return Err(bad_options("block_size below chunk_size"));
    }
    // A record the reader would refuse is refused here.
    for (i, r) in options.records.iter().enumerate() {
        if r.kind != r.body.kind() {
            return Err(bad_options("record kind"));
        }
        Record::parse(r.kind, &r.encode(), i as u64)?;
    }
    options.recovery.check()?;
    // A block (plus its frame's overhead) must fit in one group.
    if options.recovery.percent > 0
        && options.block_size.saturating_add(4096)
            > u64::from(options.recovery.group_shards) * u64::from(options.recovery.shard_len)
    {
        return Err(bad_options("group smaller than a block"));
    }
    let spool = if options.recovery.percent > 0 {
        Some(GroupEncoder::new(&options.recovery))
    } else {
        None
    };
    Ok(spool)
}

impl<W: Write> Writer<W> {
    /// Start an archive with the fixed-size chunker: check `options` and write
    /// the header.
    pub fn new(out: W, options: WriterOptions) -> Result<Self, FormatError> {
        let size = usize::try_from(options.chunk_size).map_err(|_| bad_options("chunk_size"))?;
        Self::with_chunker(out, options, Box::new(FixedChunker::new(size)))
    }

    /// Like [`Writer::new`] with another chunker (`chunk_size` is still the
    /// longest chunk and tail the writer accepts from it).
    pub fn with_chunker(
        out: W,
        options: WriterOptions,
        chunker: Box<dyn Chunker>,
    ) -> Result<Self, FormatError> {
        Self::with_chunker_and_rng(out, options, chunker, &mut rand::rng())
    }

    /// Like [`Writer::new`], drawing the archive key and the Argon2 salt of an
    /// encrypted archive from `rng` (a seeded one makes the output
    /// reproducible; use an operating system RNG for real archives).
    pub fn new_with_rng(
        out: W,
        options: WriterOptions,
        rng: &mut dyn RngCore,
    ) -> Result<Self, FormatError> {
        let size = usize::try_from(options.chunk_size).map_err(|_| bad_options("chunk_size"))?;
        Self::with_chunker_and_rng(out, options, Box::new(FixedChunker::new(size)), rng)
    }

    /// [`Writer::with_chunker`] with the RNG of [`Writer::new_with_rng`].
    pub fn with_chunker_and_rng(
        mut out: W,
        options: WriterOptions,
        chunker: Box<dyn Chunker>,
        rng: &mut dyn RngCore,
    ) -> Result<Self, FormatError> {
        let spool = check_options(&options)?;
        let record_count = options.records.len() as u64;
        let mut flags = HeaderFlags::EMPTY;
        let mut sealer = None;
        let mut slot = None;
        if let Some(seal) = &options.seal {
            seal.argon2.validate()?;
            flags = HeaderFlags::ENCRYPTED;
            if seal.listable {
                flags = flags.union(HeaderFlags::LISTABLE);
            }
            let key = ArchiveKey::generate(rng);
            slot = Some(KeySlot::create(
                seal.suite,
                seal.argon2,
                &seal.credentials,
                &options.archive_id,
                flags.bits(),
                &key,
                rng,
            )?);
            sealer = Some(Sealer::new(seal.suite, key, options.archive_id));
        }
        let listable = options.seal.as_ref().is_some_and(|s| s.listable);
        // The salt matters only to sealed frames; an archive that is not
        // encrypted keeps zeros, so its bytes depend on nothing random.
        let mut salt = [0u8; 16];
        if sealer.is_some() {
            rng.fill_bytes(&mut salt);
        }
        Header::new(flags, options.archive_id).write(&mut out)?;
        // The covered range starts right after the header.
        let out = Tee { inner: out, spool };
        let mut w = Writer {
            out,
            options,
            chunker,
            pos: Header::LEN as u64,
            entries: Vec::new(),
            records: Vec::new(),
            blocks: Vec::new(),
            priors: BTreeSet::new(),
            record_count,
            seen: HashSet::new(),
            pending: Vec::new(),
            pending_chunks: 0,
            failed: None,
            bad_chunk: None,
            recovery_locs: Vec::new(),
            cover_start: Header::LEN as u64,
            sealer,
            seq: 0,
            last_hash: [0; 32],
            listable,
            base: None,
            deleted: BTreeSet::new(),
            reused: 0,
            graph_res: GraphResources::default(),
            salt,
            generations: vec![GenerationInfo {
                generation: 0,
                start_offset: Header::LEN as u64,
                first_sequence: 0,
                salt,
            }],
            sync: None,
        };
        if let Some(slot) = slot {
            // The key slot is the first frame, in clear.
            let frame = Frame {
                kind: FrameKind::KeySlot,
                flags: FrameFlags::EMPTY,
                payload: slot.encode(),
            };
            frame.write(&mut w.out)?;
            w.pos += frame.encoded_len();
            w.seq += 1;
        }
        Ok(w)
    }

    /// Start the next generation of `existing` (spec section 15): `out` is the
    /// same file opened for writing and positioned at its end (the archive's
    /// length is where the new frames start). The new frames, index and
    /// trailer are appended after the previous trailer; the new index lists the
    /// whole archive, so [`Writer::finish`] needs nothing from the old file.
    ///
    /// `add_file`, `add_directory` and `add_symlink` add entries (a path that
    /// already exists is replaced; the paths of this call stay in sorted
    /// order among themselves), [`Writer::delete_path`] removes one. A chunk
    /// whose BLAKE3 and length match a chunk of the old table is referenced
    /// instead of written; chunks within the appended data are not deduplicated
    /// against each other. `options.archive_id` and `options.seal` are ignored
    /// (the archive's own are kept); an encrypted archive needs `credentials`
    /// (and `existing` opened with them: [`FormatError::AppendNeedsCredentials`]).
    /// `options.records` replaces the records frame when non-empty (it must keep
    /// the old records at their positions); empty keeps the old frame.
    ///
    /// Memory: the old chunk table's hashes (`HashMap`, 32 bytes of key and 8 of
    /// value per chunk), its records (`ChunkRecord`, 40 bytes per chunk) and
    /// the old entries are held until `finish`.
    pub fn append<R: Read + Seek>(
        existing: Archive<R>,
        out: W,
        options: WriterOptions,
        credentials: Option<&Credentials>,
    ) -> Result<Self, FormatError> {
        let size = usize::try_from(options.chunk_size).map_err(|_| bad_options("chunk_size"))?;
        Self::append_with_chunker(
            existing,
            out,
            options,
            credentials,
            Box::new(FixedChunker::new(size)),
        )
    }

    /// [`Writer::append`] with another chunker.
    pub fn append_with_chunker<R: Read + Seek>(
        existing: Archive<R>,
        out: W,
        options: WriterOptions,
        credentials: Option<&Credentials>,
        chunker: Box<dyn Chunker>,
    ) -> Result<Self, FormatError> {
        Self::append_with_chunker_and_rng(
            existing,
            out,
            options,
            credentials,
            chunker,
            &mut rand::rng(),
        )
    }

    /// [`Writer::append_with_chunker`] drawing the new generation's salt from
    /// `rng` (use an operating system RNG for real archives; a seeded one makes
    /// the output reproducible).
    pub fn append_with_chunker_and_rng<R: Read + Seek>(
        mut existing: Archive<R>,
        out: W,
        mut options: WriterOptions,
        credentials: Option<&Credentials>,
        chunker: Box<dyn Chunker>,
        rng: &mut dyn RngCore,
    ) -> Result<Self, FormatError> {
        if existing.is_keyless() || (existing.is_encrypted() && credentials.is_none()) {
            return Err(FormatError::AppendNeedsCredentials);
        }
        // The key comes from the key slot and the credentials given here, not
        // from the opened archive: a wrong password is `WrongKey`.
        let sealer = match (existing.key_slot(), credentials) {
            (Some(slot), Some(c)) => {
                let needed = u64::from(slot.argon2.m_kib) * 1024;
                if needed > existing.resources().memory {
                    return Err(FormatError::Refused(crate::envelope::Refusal {
                        field: "argon2_m",
                        needed,
                        allowed: existing.resources().memory,
                    }));
                }
                let header = existing.header();
                let key = slot.unwrap(&header.archive_id, header.flags.bits(), c)?;
                Some(Sealer::new(slot.suite, key, header.archive_id))
            }
            _ => None,
        };
        let new_generation =
            existing
                .trailer()
                .generation
                .checked_add(1)
                .ok_or(FormatError::BadTrailer {
                    reason: "generation",
                })?;
        options.archive_id = existing.header().archive_id;
        options.seal = None;
        // Old blocks name records by position: a new records frame must keep
        // every old record as it was.
        if !options.records.is_empty() {
            if let Some(old) = existing.records()? {
                let table = old.table()?;
                for (i, r) in (0u64..).zip(table.iter()) {
                    let r = r?;
                    let same =
                        options
                            .records
                            .get(i as usize)
                            .ok_or(FormatError::RecordOutOfRange {
                                record: i,
                                count: options.records.len() as u64,
                            })?;
                    if same.kind != r.kind || same.encode() != r.encode() {
                        return Err(FormatError::BadRecord {
                            record: i,
                            reason: "changed by an append",
                        });
                    }
                }
            }
        }
        let record_count = if options.records.is_empty() {
            existing.record_count()?
        } else {
            options.records.len() as u64
        };
        let spool = check_options(&options)?;
        let end = existing.raw_reader().seek(SeekFrom::End(0))?;
        let trailer_offset = end
            .checked_sub(TRAILER_FRAME_LEN)
            .ok_or(FormatError::NoTrailer)?;
        let generation = existing.trailer().generation;
        let entries = {
            let t = existing.entry_table()?;
            t.table()?.iter().collect::<Result<Vec<_>, _>>()?
        };
        let table = existing.chunks();
        let mut records = Vec::new();
        for i in 0..table.len() {
            records.push(table.record(i).ok_or(FormatError::ChunkIndexOutOfRange {
                chunk: i,
                len: table.len(),
            })?);
        }
        let by_hash = table.by_hash();
        let index = existing.index();
        // Frames of one archive are numbered by position: the old index and the
        // old trailer follow the last frame the index lists.
        let last = index
            .blocks
            .iter()
            .map(|b| b.sequence)
            .chain([index.entry_table.sequence])
            .chain(index.records.map(|r| r.sequence))
            .chain(index.recovery.iter().map(|f| f.sequence))
            .max()
            .unwrap_or(0);
        let seq = last
            .checked_add(3)
            .ok_or(FormatError::BadOptions { reason: "sequence" })?;
        let mut salt = [0u8; 16];
        if sealer.is_some() {
            rng.fill_bytes(&mut salt);
        }
        let mut generations = index.generations.clone();
        generations.push(GenerationInfo {
            generation: new_generation,
            start_offset: end,
            first_sequence: seq,
            salt,
        });
        let base = Base {
            generation,
            trailer_offset,
            chunks: records.len() as u64,
            by_hash,
            entries,
            records: index.records,
        };
        let w = Writer {
            out: Tee { inner: out, spool },
            chunker,
            pos: end,
            entries: Vec::new(),
            records,
            blocks: index.blocks.clone(),
            priors: index.priors.iter().copied().collect(),
            record_count,
            seen: HashSet::new(),
            pending: Vec::new(),
            pending_chunks: 0,
            failed: None,
            bad_chunk: None,
            recovery_locs: index.recovery.clone(),
            cover_start: end,
            sealer,
            seq,
            last_hash: [0; 32],
            listable: existing.is_listable(),
            base: Some(base),
            deleted: BTreeSet::new(),
            reused: 0,
            graph_res: GraphResources {
                window: index.envelope.max_window,
                bwt_block: index.envelope.max_bwt_block,
            },
            salt,
            generations,
            sync: None,
            options,
        };
        Ok(w)
    }

    /// Call `sync` once before the trailer is written, after everything else of
    /// the generation has been flushed, and once more after it: it must put the
    /// data on disk (for a file, `sync_data`). The trailer commits the generation
    /// (spec section 15), so a writer that can sync should; one over a plain
    /// `Write` cannot.
    pub fn with_sync(mut self, sync: Box<dyn FnMut() -> std::io::Result<()>>) -> Self {
        self.sync = Some(sync);
        self
    }

    /// Bytes sealing adds to a payload (0 for a plain archive).
    fn overhead(&self) -> u64 {
        self.sealer
            .as_ref()
            .map_or(0, |s| s.suite().overhead() as u64)
    }

    /// True when a frame of `kind` is sealed in this archive.
    fn seals(&self, kind: FrameKind) -> bool {
        self.sealer.is_some() && sealing_rule(kind as u16, self.listable()) == Some(true)
    }

    fn listable(&self) -> bool {
        self.listable
    }

    /// Covered bytes the writer holds in memory for recovery right now: the
    /// open group's, never more than `group_shards * shard_len`, whatever the
    /// archive's size; 0 without recovery. Memory rule: those bytes, then the
    /// group's encoder ([`crate::recovery::encoder_work_bytes`]) and its
    /// recovery shards (`recovery_shards * shard_len`) while its frame is
    /// written; independent of the archive's size.
    pub fn recovery_buffered(&self) -> usize {
        self.out.spool.as_ref().map_or(0, GroupEncoder::buffered)
    }

    /// Bytes the open group has taken so far (0 without recovery).
    pub fn recovery_group_bytes(&self) -> u64 {
        self.out.spool.as_ref().map_or(0, GroupEncoder::group_bytes)
    }

    /// Before a data frame of `len` bytes: close the group when the frame
    /// would not fit in it, and refuse a frame no group can hold.
    fn make_room(&mut self, len: u64) -> Result<(), FormatError> {
        let o = self.options.recovery;
        if o.percent == 0 {
            return Ok(());
        }
        let cap = u64::from(o.group_shards) * u64::from(o.shard_len);
        if len > cap {
            return Err(bad_options("group smaller than a block"));
        }
        if self.pos - self.cover_start > 0 && self.pos - self.cover_start + len > cap {
            self.close_group()?;
        }
        Ok(())
    }

    /// Close the open group and write its `Recovery` frame right after the
    /// frames it covers (nothing when the group is empty or there is no
    /// recovery). The frame is written piecewise, outside any group.
    fn close_group(&mut self) -> Result<(), FormatError> {
        let Some(mut enc) = self.out.spool.take() else {
            return Ok(());
        };
        let r = self.write_group_frame(&mut enc);
        self.out.spool = Some(enc);
        r
    }

    fn write_group_frame(&mut self, enc: &mut GroupEncoder) -> Result<(), FormatError> {
        let Some(g) = enc.end_group()? else {
            return Ok(());
        };
        let o = self.options.recovery;
        let frame = RecoveryFrame {
            cover_offset: self.cover_start,
            cover_len: g.cover_len,
            shard_len: o.shard_len,
            data_shards: g.hashes.len() as u32,
            group_shards: g.group_shards,
            recovery_shards: g.recovery_shards,
            shard_hashes: g.hashes,
            recovery: Vec::new(),
        };
        let head = frame.head_bytes();
        let payload_len = head.len() as u64 + g.recovery.len() as u64;
        let at = FrameLocation {
            offset: self.pos,
            len: frame_len(payload_len),
            sequence: self.seq,
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        hasher.update(&g.recovery);
        let w = &mut self.out;
        w.write_all(&(FrameKind::Recovery as u16).to_le_bytes())?;
        w.write_all(&FrameFlags::EMPTY.bits().to_le_bytes())?;
        varint::write(w, payload_len)?;
        w.write_all(&head)?;
        w.write_all(&g.recovery)?;
        w.write_all(hasher.finalize().as_bytes())?;
        self.pos += at.len;
        self.seq += 1;
        self.cover_start = self.pos;
        self.recovery_locs.push(at);
        Ok(())
    }

    fn write_frame(
        &mut self,
        kind: FrameKind,
        payload: Vec<u8>,
    ) -> Result<FrameLocation, FormatError> {
        let sealed = self.seals(kind);
        let wire_len = payload.len() as u64 + if sealed { self.overhead() } else { 0 };
        if matches!(kind, FrameKind::EntryTable | FrameKind::Records) {
            self.make_room(frame_len(wire_len))?;
        }
        // The sequence is read after `make_room`, which may write a recovery frame.
        let sequence = self.seq;
        let (flags, payload) = match (&self.sealer, sealed) {
            (Some(s), true) => (
                FrameFlags::SEALED,
                s.seal(&payload, kind as u16, sequence, &self.salt)?,
            ),
            _ => (FrameFlags::EMPTY, payload),
        };
        self.write_wire_frame(kind, flags, payload)
    }

    /// Write a frame whose payload is already as it goes to disk.
    fn write_wire_frame(
        &mut self,
        kind: FrameKind,
        flags: FrameFlags,
        payload: Vec<u8>,
    ) -> Result<FrameLocation, FormatError> {
        let frame = Frame {
            kind,
            flags,
            payload,
        };
        let at = FrameLocation {
            offset: self.pos,
            len: frame.encoded_len(),
            sequence: self.seq,
        };
        self.last_hash = *blake3::hash(&frame.payload).as_bytes();
        frame.write(&mut self.out)?;
        self.pos += at.len;
        self.seq += 1;
        Ok(at)
    }

    /// Check an entry (without its chunks) on its own and against the paths
    /// added so far: the order of adds is free, a path twice is not.
    fn check(&self, entry: &Entry) -> Result<(), FormatError> {
        check_entry(entry, self.entries.len() as u64, None)?;
        if self.seen.contains(&entry.path) {
            return Err(FormatError::DuplicateEntry {
                path: entry.path.clone(),
            });
        }
        Ok(())
    }

    /// Record a checked entry.
    fn push_entry(&mut self, entry: Entry) {
        self.seen.insert(entry.path.clone());
        self.entries.push(entry);
    }

    /// Fail with the first I/O error this writer saw, if any.
    fn check_alive(&self) -> Result<(), FormatError> {
        if let Some(reason) = self.bad_chunk {
            return Err(FormatError::BadChunk { reason });
        }
        match &self.failed {
            Some((kind, msg)) => Err(FormatError::Io(std::io::Error::new(*kind, msg.clone()))),
            None => Ok(()),
        }
    }

    /// Remember an I/O error so that later calls refuse.
    fn note<T>(&mut self, r: Result<T, FormatError>) -> Result<T, FormatError> {
        match &r {
            Err(FormatError::Io(e)) => self.failed = Some((e.kind(), e.to_string())),
            Err(FormatError::BadChunk { reason }) => self.bad_chunk = Some(reason),
            Err(e @ FormatError::BadOptions { .. }) => {
                self.failed = Some((std::io::ErrorKind::Other, e.to_string()));
            }
            _ => {}
        }
        r
    }

    fn flush_block(&mut self) -> Result<(), FormatError> {
        if self.pending_chunks == 0 {
            return Ok(());
        }
        let plain = std::mem::take(&mut self.pending);
        let Encoded {
            graph,
            bytes: encoded,
            resources,
        } = self.options.encoder.encode(&plain)?;
        if let Err(e) = check_graph(&graph, self.record_count) {
            // The block is gone: the archive is abandoned like after an I/O error.
            self.failed = Some((std::io::ErrorKind::InvalidData, e.to_string()));
            return Err(e);
        }
        self.priors.extend(graph.prior_ids());
        self.graph_res = GraphResources {
            window: self.graph_res.window.max(resources.window),
            bwt_block: self.graph_res.bwt_block.max(resources.bwt_block),
        };
        let header = BlockHeader {
            graph,
            plain_len: plain.len() as u64,
            encoded_len: encoded.len() as u64,
        };
        // The frame is written piecewise, without a second copy of the encoded
        // block (the identity encoder still copies the plain block once).
        let head = header.encode();
        let plain_payload_len = (head.len() + encoded.len()) as u64;
        let payload_len = plain_payload_len + self.overhead();
        self.make_room(frame_len(payload_len))?;
        let at = if let Some(sealer) = &self.sealer {
            let mut payload = head;
            payload.extend_from_slice(&encoded);
            let sealed =
                sealer.seal(&payload, FrameKind::ChunkData as u16, self.seq, &self.salt)?;
            self.write_wire_frame(FrameKind::ChunkData, FrameFlags::SEALED, sealed)?
        } else {
            let at = FrameLocation {
                offset: self.pos,
                len: frame_len(payload_len),
                sequence: self.seq,
            };
            let mut hasher = blake3::Hasher::new();
            hasher.update(&head);
            hasher.update(&encoded);
            let w = &mut self.out;
            w.write_all(&(FrameKind::ChunkData as u16).to_le_bytes())?;
            w.write_all(&FrameFlags::EMPTY.bits().to_le_bytes())?;
            varint::write(w, payload_len)?;
            w.write_all(&head)?;
            w.write_all(&encoded)?;
            w.write_all(hasher.finalize().as_bytes())?;
            self.pos += at.len;
            self.seq += 1;
            at
        };
        self.blocks.push(BlockLocation {
            frame_offset: at.offset,
            frame_len: at.len,
            first_chunk: self.records.len() as u64 - self.pending_chunks,
            chunk_count: self.pending_chunks,
            plain_len: plain.len() as u64,
            sequence: at.sequence,
        });
        self.pending_chunks = 0;
        Ok(())
    }

    /// Close the current block now, even if it has room (nothing when it holds
    /// no chunk): the next chunk starts a new block. The block's graph is
    /// validated here, as when a block closes because it is full.
    pub fn close_block(&mut self) -> Result<(), FormatError> {
        self.check_alive()?;
        let r = self.flush_block();
        self.note(r)
    }

    fn add_chunk(&mut self, data: &[u8]) -> Result<u64, FormatError> {
        let len = data.len() as u64;
        // An append reuses a chunk of the old table with the same hash and length.
        if let Some(base) = &self.base {
            let hash = *blake3::hash(data).as_bytes();
            if let Some(&old) = base.by_hash.get(&hash) {
                let same_len = usize::try_from(old)
                    .ok()
                    .and_then(|i| self.records.get(i))
                    .is_some_and(|r| r.plain_len == len);
                if same_len {
                    self.reused += 1;
                    return Ok(old);
                }
            }
        }
        if self.pending_chunks > 0 && self.pending.len() as u64 + len > self.options.block_size {
            self.flush_block()?;
        }
        let index = self.records.len() as u64;
        self.records.push(ChunkRecord {
            plain_len: len,
            hash: *blake3::hash(data).as_bytes(),
        });
        self.pending.extend_from_slice(data);
        self.pending_chunks += 1;
        Ok(index)
    }

    /// Add a regular file: its bytes are read to the end, cut into chunks and
    /// appended to the current block. After an I/O error (from the input or
    /// the output) or a `BadChunk` the archive is abandoned: chunks already
    /// added have no entry, and the writer refuses every further call with
    /// that error.
    pub fn add_file(
        &mut self,
        path: &str,
        flags: EntryFlags,
        mtime_ns: i64,
        data: &mut dyn Read,
    ) -> Result<(), FormatError> {
        self.check_alive()?;
        let r = self.add_file_inner(path, flags, mtime_ns, data);
        self.note(r)
    }

    fn add_file_inner(
        &mut self,
        path: &str,
        flags: EntryFlags,
        mtime_ns: i64,
        data: &mut dyn Read,
    ) -> Result<(), FormatError> {
        let mut entry = Entry {
            kind: EntryKind::File,
            flags,
            path: path.to_string(),
            mtime_ns,
            size: 0,
            symlink_target: None,
            chunks: Vec::new(),
        };
        self.check(&entry)?;
        let chunk_size = self.options.chunk_size as usize;
        // Read a block's worth at a time: besides the pending block, at most
        // the held-back tail plus one segment is in memory.
        let per_segment = self.options.block_size;
        self.chunker.reset();
        // The held-back tail, then the newly read bytes; grows with the file.
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let before = buf.len();
            let n = Read::take(&mut *data, per_segment).read_to_end(&mut buf)?;
            let eof = (n as u64) < per_segment;
            let cuts = self.chunker.feed(&buf[before..], eof);
            let mut start = 0usize;
            for cut in cuts {
                if cut <= start || cut > buf.len() {
                    return Err(FormatError::BadChunk {
                        reason: "cut positions",
                    });
                }
                if cut - start > chunk_size {
                    return Err(FormatError::BadChunk {
                        reason: "chunk longer than chunk_size",
                    });
                }
                let i = self.add_chunk(&buf[start..cut])?;
                entry.chunks.push(i);
                start = cut;
            }
            if eof && start != buf.len() {
                return Err(FormatError::BadChunk {
                    reason: "bytes left uncut at eof",
                });
            }
            buf.drain(..start);
            if buf.len() > chunk_size {
                return Err(FormatError::BadChunk {
                    reason: "held back more than chunk_size",
                });
            }
            entry.size += n as u64;
            if eof {
                break;
            }
        }
        self.push_entry(entry);
        Ok(())
    }

    /// Add a directory.
    pub fn add_directory(
        &mut self,
        path: &str,
        flags: EntryFlags,
        mtime_ns: i64,
    ) -> Result<(), FormatError> {
        let entry = Entry {
            kind: EntryKind::Directory,
            flags,
            path: path.to_string(),
            mtime_ns,
            size: 0,
            symlink_target: None,
            chunks: Vec::new(),
        };
        self.check(&entry)?;
        self.push_entry(entry);
        Ok(())
    }

    /// Add a symbolic link with the raw `target` bytes.
    pub fn add_symlink(
        &mut self,
        path: &str,
        flags: EntryFlags,
        mtime_ns: i64,
        target: &[u8],
    ) -> Result<(), FormatError> {
        let entry = Entry {
            kind: EntryKind::Symlink,
            flags,
            path: path.to_string(),
            mtime_ns,
            size: target.len() as u64,
            symlink_target: Some(target.to_vec()),
            chunks: Vec::new(),
        };
        self.check(&entry)?;
        self.push_entry(entry);
        Ok(())
    }

    /// Append only: remove `path` from the entry table of the new generation
    /// (an exact path; a directory's children are removed one by one). The
    /// chunks of a removed file stay in the chunk table, unreferenced. A path
    /// that is also added in this call keeps the new entry.
    pub fn delete_path(&mut self, path: &str) -> Result<(), FormatError> {
        self.check_alive()?;
        if self.base.is_none() {
            return Err(bad_options("delete_path outside an append"));
        }
        self.deleted.insert(path.to_string());
        Ok(())
    }

    /// The entries of the new generation: the old ones that were neither
    /// deleted nor replaced, merged with the new ones in path order.
    fn merged_entries(&mut self) -> Vec<Entry> {
        let mut new = std::mem::take(&mut self.entries);
        new.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let Some(base) = &mut self.base else {
            return new;
        };
        let old = std::mem::take(&mut base.entries);
        let mut out = Vec::with_capacity(old.len() + new.len());
        let mut new = new.into_iter().peekable();
        for e in old {
            while let Some(n) = new.next_if(|n| n.path.as_bytes() < e.path.as_bytes()) {
                out.push(n);
            }
            if let Some(n) = new.next_if(|n| n.path == e.path) {
                out.push(n);
            } else if !self.deleted.contains(&e.path) {
                out.push(e);
            }
        }
        out.extend(new);
        out
    }

    /// The last block, the entry table and the records frame: the end of the
    /// range recovery covers.
    fn write_covered_tail(
        &mut self,
    ) -> Result<(FrameLocation, [u8; 32], Option<FrameLocation>), FormatError> {
        self.flush_block()?;
        self.entries = self.merged_entries();
        let table = EntryTableWriter::encode(&self.entries)?;
        let entry_table = self.write_frame(FrameKind::EntryTable, table)?;
        let entry_hash = self.last_hash;
        let records = if self.options.records.is_empty() {
            // An append keeps the old records frame when it has none of its own.
            self.base.as_ref().and_then(|b| b.records)
        } else {
            let payload = RecordsWriter::encode(&self.options.records);
            Some(self.write_frame(FrameKind::Records, payload)?)
        };
        Ok((entry_table, entry_hash, records))
    }

    /// Close the last block and write the entry table, the recovery frames, the
    /// index and the trailer.
    pub fn finish(mut self) -> Result<WriterSummary, FormatError> {
        self.check_alive()?;
        let (entry_table, entry_table_hash, records) = self.write_covered_tail()?;
        let records_len = records.map_or(0, |r| r.len);
        // The last group closes before the index.
        self.close_group()?;
        let recovery_peak = self
            .out
            .spool
            .as_ref()
            .map_or(0, GroupEncoder::peak_recovery);
        let recovery = std::mem::take(&mut self.recovery_locs);
        let recovery_len = recovery.iter().map(|r| r.len).max().unwrap_or(0);

        let recs = &self.records;
        let leaves: Vec<[u8; 32]> = recs.iter().map(|r| r.hash).collect();
        let max_plain = self.blocks.iter().map(|b| b.plain_len).max().unwrap_or(0);
        let graph = self.graph_res;
        let mut index = Index {
            chunk_table: ChunkTableWriter::encode(recs).into(),
            merkle_root: merkle_root(&leaves),
            envelope: Envelope {
                max_window: 0,
                max_bwt_block: 0,
                max_block_plain: 0,
                max_frame_payload: 0,
                decode_memory: 0,
                threads_hint: 0,
            },
            priors: self.priors.iter().copied().collect(),
            blocks: std::mem::take(&mut self.blocks),
            entry_table,
            entry_table_hash,
            records,
            recovery,
            generations: std::mem::take(&mut self.generations),
        };
        // The envelope names the index's own payload length, which depends on
        // the envelope's varints. Starting from an upper bound the length can
        // only fall, so this settles within a few rounds.
        let overhead = self.overhead();
        let mut guess = u64::MAX;
        let payload = loop {
            index.envelope = Envelope::for_archive(
                &index.blocks,
                ArchiveSizes {
                    index_payload_len: guess,
                    entry_table_len: entry_table.len,
                    records_len,
                    recovery_len,
                },
                graph,
                max_plain,
                0,
            );
            let payload = index.encode()?;
            // The envelope names the payload as stored: sealed, when sealing.
            let len = payload.len() as u64 + overhead;
            if index.envelope.max_frame_payload == len.max(frames_max(&index)) {
                break payload;
            }
            guess = len;
        };
        // The index is sealed under a fixed sequence: the trailer cannot name
        // its position. The trailer's hash covers the payload as stored.
        let generation = self
            .base
            .as_ref()
            .map_or(0, |b| b.generation.saturating_add(1));
        let (flags, payload) = match &self.sealer {
            Some(s) => (
                FrameFlags::SEALED,
                s.seal(
                    &payload,
                    FrameKind::Index as u16,
                    index_sequence(generation),
                    &self.salt,
                )?,
            ),
            None => (FrameFlags::EMPTY, payload),
        };
        let index_hash = *blake3::hash(&payload).as_bytes();
        let at = self.write_wire_frame(FrameKind::Index, flags, payload)?;
        let trailer = Trailer {
            index_offset: at.offset,
            index_len: at.len,
            index_hash,
            generation,
            archive_id: self.options.archive_id,
            previous_trailer_offset: self.base.as_ref().map_or(0, |b| b.trailer_offset),
            salt: self.salt,
        };
        // The trailer commits the generation: its data must be on disk first.
        self.out.flush()?;
        if let Some(sync) = &mut self.sync {
            sync()?;
        }
        trailer.write(&mut self.out)?;
        self.pos += TRAILER_FRAME_LEN;
        self.out.flush()?;
        // ... and the commit itself is made durable the same way.
        if let Some(sync) = &mut self.sync {
            sync()?;
        }
        let old_chunks = self.base.as_ref().map_or(0, |b| b.chunks);
        Ok(WriterSummary {
            entries: self.entries.len() as u64,
            chunks: self.records.len() as u64,
            blocks: index.blocks.len() as u64,
            archive_len: self.pos,
            recovery_peak,
            generation,
            new_chunks: self.records.len() as u64 - old_chunks,
            reused_chunks: self.reused,
        })
    }
}

/// `max_frame_payload` as `for_archive` computes it without the index term.
fn frames_max(index: &Index) -> u64 {
    Envelope::for_archive(
        &index.blocks,
        ArchiveSizes {
            index_payload_len: 0,
            entry_table_len: index.entry_table.len,
            records_len: index.records.map_or(0, |r| r.len),
            recovery_len: index.recovery.iter().map(|r| r.len).max().unwrap_or(0),
        },
        crate::primitive::GraphResources::default(),
        0,
        0,
    )
    .max_frame_payload
}

impl Writer<std::fs::File> {
    /// Append a generation to the archive at `path`: the file is opened for
    /// reading to read the old archive, then for appending (every write goes
    /// to its end), and the data is synced before the trailer is written.
    /// [`Writer::append`] with a bare writer trusts the caller that `out` is
    /// positioned at the end of the archive; this does not.
    pub fn append_file(
        path: &std::path::Path,
        options: WriterOptions,
        credentials: Option<&Credentials>,
    ) -> Result<Self, FormatError> {
        let existing = Archive::open_with(
            std::fs::File::open(path)?,
            &crate::envelope::Resources::default(),
            credentials,
        )?;
        let out = std::fs::OpenOptions::new().append(true).open(path)?;
        let sync_handle = out.try_clone()?;
        Ok(Self::append(existing, out, options, credentials)?
            .with_sync(Box::new(move || sync_handle.sync_data())))
    }
}
