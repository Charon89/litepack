//! Writing an archive as a stream (spec section 9): header, `ChunkData` blocks,
//! entry table, index, trailer. The writer never seeks and never reads back.

use crate::archive::Archive;
use crate::chunk::{ChunkRecord, ChunkTableWriter};
pub use crate::chunkhash::MAX_HASH_THREADS;
use crate::chunkhash::{hash_and_copy, hash_ranges, hash_two, HashFn, Two};
use crate::crypto::{
    index_sequence, sealing_rule, ArchiveKey, Argon2Params, Credentials, KeySlot, Sealer, Suite,
};
use crate::entry::{check_entry, Entry, EntryFlags, EntryKind, EntryTableWriter};
use crate::envelope::{ArchiveSizes, Envelope};
use crate::error::FormatError;
use crate::frame::{Frame, FrameFlags, FrameKind};
use crate::graph::{BlockHeader, Graph, Step, MAX_STEPS};
use crate::header::{FormatVersion, Header, HeaderFlags};
use crate::index::{BlockLocation, FrameLocation, GenerationInfo, Index};
use crate::merkle::merkle_root;
use crate::parhash::frame_payload_hash;
use crate::prefetch::{SegmentError, Segments};
use crate::primitive::{GraphResources, PrimitiveId};
use crate::profile;
use crate::record::{Record, RecordBody, RecordsWriter};
use crate::recovery::{GroupEncoder, RecoveryFrame, RecoveryOptions};
use crate::sink::Sink;
pub use crate::sink::WRITE_DEPTH;
use crate::trailer::{Trailer, TRAILER_FRAME_LEN};
use crate::varint;
use rand::RngCore;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::thread::JoinHandle;
use std::time::Instant;

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
    /// The decoder resources `graph` needs, for the envelope. The writer takes
    /// the maximum of this and `graph.resources()`, so an encoder that reports
    /// too little cannot make the envelope too small.
    pub resources: GraphResources,
    /// Decoder working memory this block needs beyond its plain buffer (a
    /// zstd window, an LZMA dictionary, a reconstruction's working set), for
    /// the envelope: `decode_memory` is `max_block_plain` plus the largest
    /// such figure of the archive's blocks (and of the `memory` given to
    /// [`Writer::add_part_encoded`]). 0 declares none.
    pub memory: u64,
}

/// What a finished [`EncodeJob`] hands back besides the encoding, for its
/// encoder to take back through [`BlockEncoder::absorb`]: the counts the job
/// made and any state it borrowed (a compressor instance). Opaque to the
/// writer, which passes it on untouched.
pub type JobState = Box<dyn std::any::Any + Send>;

/// One block's encoding as a job the writer runs on another thread
/// (`WriterOptions::encode_threads`). The job captures, when
/// [`BlockEncoder::job`] makes it, everything the encoder would use for the
/// block at that moment (its steering: class, dictionary kind), so the
/// encoding is the one [`BlockEncoder::encode`] would make then.
pub trait EncodeJob: Send {
    /// Encode `plain`. The state comes back to the encoder through
    /// [`BlockEncoder::absorb`] when the writer takes the block, in block
    /// order.
    fn run(self: Box<Self>, plain: &[u8]) -> Result<(Encoded, Option<JobState>), FormatError>;

    /// Encode `plain`, owning it; the writer calls this one. Returns the result of
    /// [`EncodeJob::run`] and the plain buffer when the encoding did not take it (the
    /// writer reuses it for a later block). The default runs `run` and hands the buffer back;
    /// a job that stores the block overrides it to move the buffer into the encoding instead
    /// of copying it.
    fn run_owned(self: Box<Self>, plain: Vec<u8>) -> (JobResult, Option<Vec<u8>>) {
        let r = self.run(&plain);
        (r, Some(plain))
    }
}

/// Turns the plain bytes of a block into encoded bytes, choosing the decode
/// graph per block.
///
/// The two methods with defaults are the parallel extension: a writer with
/// `encode_threads` above 1 asks for a [`BlockEncoder::job`] per block and
/// runs it on a pool thread; an encoder that keeps the default (`None`) is
/// run sequentially, on the writer's thread, through `encode`, whatever the
/// thread count. A job's graph may not name a reconstruction record (the
/// writer refuses it: `BadRecord`, `pool-encoded block names a record`); a
/// block that names one goes through [`Writer::add_part_encoded`].
pub trait BlockEncoder {
    /// Encode one block's plain bytes with a graph of the encoder's choice.
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError>;

    /// Encode, owning the plain bytes; the writer calls this one when it encodes on its own
    /// thread. Returns the result of [`BlockEncoder::encode`] and the plain buffer when the
    /// encoding did not take it (the writer reuses it). The default calls `encode` and hands
    /// the buffer back; an encoder that stores the block overrides it to move the buffer into
    /// the encoding instead of copying it.
    fn encode_owned(&mut self, plain: Vec<u8>) -> (Result<Encoded, FormatError>, Option<Vec<u8>>) {
        let r = self.encode(&plain);
        (r, Some(plain))
    }

    /// A job for the next block, carrying the encoder's steering as it is
    /// now, to run on another thread. `None` (the default) means the writer
    /// calls `encode` on its own thread instead.
    fn job(&mut self) -> Option<Box<dyn EncodeJob>> {
        None
    }

    /// Take back what a finished job returned (its counts, borrowed state).
    /// The writer calls this in block order, when it writes the block. The
    /// default drops the state.
    fn absorb(&mut self, state: JobState) {
        drop(state);
    }
}

/// The identity encoder: a one-step `store` graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreEncoder;

impl StoreEncoder {
    /// The `store` encoding of a block whose plain bytes are `plain`, taking the buffer as
    /// the encoded bytes (no copy).
    pub fn wrap(plain: Vec<u8>) -> Encoded {
        Encoded {
            graph: Graph {
                steps: vec![Step {
                    primitive: PrimitiveId::Store,
                    params: Vec::new(),
                }],
            },
            bytes: plain,
            resources: GraphResources::default(),
            memory: 0,
        }
    }
}

impl BlockEncoder for StoreEncoder {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        Ok(StoreEncoder::wrap(plain.to_vec()))
    }

    fn encode_owned(&mut self, plain: Vec<u8>) -> (Result<Encoded, FormatError>, Option<Vec<u8>>) {
        (Ok(StoreEncoder::wrap(plain)), None)
    }

    fn job(&mut self) -> Option<Box<dyn EncodeJob>> {
        Some(Box::new(StoreEncoder))
    }
}

impl EncodeJob for StoreEncoder {
    fn run(self: Box<Self>, plain: &[u8]) -> Result<(Encoded, Option<JobState>), FormatError> {
        Ok((StoreEncoder.encode(plain)?, None))
    }

    fn run_owned(self: Box<Self>, plain: Vec<u8>) -> (JobResult, Option<Vec<u8>>) {
        (Ok((StoreEncoder::wrap(plain), None)), None)
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
    /// index (none: no frame). A record's id is its position; each block's
    /// graph is checked when the block closes and may only name ids below
    /// `records.len()` plus the records [`Writer::add_record`] added. In an
    /// append, empty means the old records (they are loaded, and a record
    /// added later follows them); non-empty must restate every old record at
    /// its position and is written as the new frame.
    pub records: Vec<Record>,
    /// Reed-Solomon recovery over the body (spec section 13): `percent` 0
    /// (the default) writes no recovery frame.
    pub recovery: RecoveryOptions,
    /// Encrypt the archive (none: plain). The archive key and the Argon2 salt
    /// come from the RNG the writer is given ([`Writer::new_with_rng`]), the
    /// thread's RNG otherwise.
    pub seal: Option<SealOptions>,
    /// Deduplicate chunks within the archive: a chunk whose BLAKE3 and length
    /// equal those of a chunk this writer already stored is referenced by its
    /// index instead of being written (`false` by default: every chunk is
    /// written, as in the earlier revisions of this writer; an append always
    /// reuses the old table's chunks, whatever this says). The writer keeps
    /// one hash-map entry per stored chunk (32 bytes of key, 8 of value, plus
    /// the map's own overhead) for the life of the archive.
    pub dedup: bool,
    /// Blocks encoded at once. 1 (the default; 0 counts as 1) encodes every
    /// block on the calling thread when it closes, as the earlier revisions
    /// of this writer did. Above 1, a closing block is handed to a pool
    /// thread (one per block in flight, through [`BlockEncoder::job`]) and
    /// the writer keeps taking chunks; at most `encode_threads` blocks are
    /// in flight, the oldest is awaited and written when one more must be
    /// handed over, and blocks are written in order, so the archive is byte
    /// for byte the one a single thread writes. Memory: `encode_threads`
    /// times (`block_size` plus the encoded size) for the blocks in flight,
    /// plus the encoders' own working memory, one per block in flight; the
    /// block buffer pool and the read buffer, as [`Writer::new`] describes.
    /// Threads: up to `encode_threads` encode jobs, plus a write thread when
    /// [`Writer::with_write_thread`] was called and a reader thread when the
    /// caller reads ahead ([`crate::prefetch`]); the two I/O threads mostly
    /// wait on I/O and come on top of the encode jobs. Above 1, the BLAKE3
    /// of the chunks of a segment given to [`Writer::add_file_segments`] is
    /// computed on up to [`MAX_HASH_THREADS`] threads, the feeding thread
    /// included, scoped to that segment and joined before its chunks are
    /// looked up and added in order (`crate::chunkhash`); the same threads
    /// copy the segment's leading chunks into the pending block in the same
    /// pass (the copy stands when the lookups store them all, as on
    /// incompressible data; from the first chunk already stored the rest are
    /// copied in order, E2-24), and a chunker may cut on as many threads
    /// (LitePack's FastCDC does). They hold no memory beyond 32 bytes per
    /// chunk of the segment. LitePack's pipeline starts them only above one
    /// block in flight, so one block in flight is one thread doing everything.
    pub encode_threads: usize,
    /// The most blocks the archive tells a reader it may decode at once: the
    /// envelope's `threads_hint` is the smaller of this and the number of
    /// blocks the archive has. 0 (the default) writes 0, no hint. Kept apart
    /// from `encode_threads` so that an archive's bytes never depend on the
    /// threads (or the memory budget) used to write it (D-60).
    pub threads_hint: u32,
}

/// The output with the recovery spool beside it: while the spool is set, every
/// byte written is also fed to it.
struct Tee<W: Write> {
    inner: Sink<W>,
    spool: Option<GroupEncoder>,
}

impl<W: Write> Write for Tee<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write_all(buf)?;
        if let Some(s) = &mut self.spool {
            s.feed(buf).map_err(std::io::Error::other)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Tee<W> {
    /// Write `buf` whole, by value (moved to the write thread when there is
    /// one). A block buffer (`back`) comes back for the pool: here at once when
    /// written on this thread, else through the write thread; any other buffer
    /// is dropped once written.
    fn write_owned(&mut self, buf: Vec<u8>, back: bool) -> std::io::Result<Option<Vec<u8>>> {
        if let Some(s) = &mut self.spool {
            s.feed(&buf).map_err(std::io::Error::other)?;
        }
        self.inner.write_owned(buf, back)
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
            .field("dedup", &self.dedup)
            .field("encode_threads", &self.encode_threads)
            .field("threads_hint", &self.threads_hint)
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
            dedup: false,
            encode_threads: 1,
            threads_hint: 0,
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
    /// Chunk references this write made to chunks it had already stored
    /// (`WriterOptions::dedup`; 0 without it). Not counted in `reused_chunks`.
    pub deduped_chunks: u64,
    /// The bytes of those chunks.
    pub deduped_bytes: u64,
}

/// A file entry being written in parts (see [`Writer::begin_entry`]).
struct OpenEntry {
    entry: Entry,
    /// Per part: its offset in the file, its length and its chunks.
    parts: Vec<(u64, u64, Vec<u64>)>,
}

/// What an append starts from: the previous generation, read from the archive.
struct Base {
    generation: u64,
    trailer_offset: u64,
    chunks: u64,
    by_hash: HashMap<[u8; 32], u64>,
    entries: Vec<Entry>,
    records: Option<FrameLocation>,
    /// The old archive, kept open to read an old block's graph when a record
    /// part would reference one of its chunks (spec section 12, nesting).
    graphs: Box<dyn OldGraphs>,
    /// Old block index to whether its graph names a reconstruction primitive:
    /// one entry per old block read, read once.
    recon: HashMap<usize, bool>,
}

/// Reads the graph of a block of the archive an append extends, the way the
/// reader opens a block (its frame hash-checked, unsealed with the archive key).
trait OldGraphs {
    /// Whether the graph of the `ChunkData` frame at `at` names a
    /// reconstruction primitive.
    fn uses_records(&mut self, at: FrameLocation) -> Result<bool, FormatError>;
}

impl<R: Read + Seek> OldGraphs for Archive<R> {
    fn uses_records(&mut self, at: FrameLocation) -> Result<bool, FormatError> {
        let frame = self.read_frame_at(at, FrameKind::ChunkData)?;
        let (graph, _) = Graph::parse(&frame.payload)?;
        Ok(graph.uses_records())
    }
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
    /// True when `finish` writes a `Records` frame from `options.records`:
    /// records given in the options, or added by `add_record`. An append
    /// with neither keeps the old frame's location in its index.
    write_records: bool,
    /// Paths of the entries added so far, to refuse one twice (a second copy
    /// of the paths: a known cost for the memory task).
    seen: HashSet<String>,
    /// The block being filled. Its length may exceed the fill
    /// (`pending_len`): a buffer from the pool keeps its full capacity,
    /// initialised, so chunks are copied into place — by the hash threads, in
    /// parallel, on the segment path — without a resize on the feeding thread;
    /// the bytes past the fill are stale. It is truncated to the fill when
    /// the block closes.
    pending: Vec<u8>,
    /// Bytes of the pending block filled so far.
    pending_len: usize,
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
    /// Hash to index of the chunks this write stored (only with `options.dedup`).
    own: HashMap<[u8; 32], u64>,
    /// Chunk references to chunks this write already stored, and their bytes.
    deduped_chunks: u64,
    deduped_bytes: u64,
    /// Window and BWT block maxima of the earlier generations' graphs and of
    /// this write's blocks so far.
    graph_res: GraphResources,
    /// This generation's nonce salt (zeros when the archive is not encrypted).
    salt: [u8; 16],
    /// The generation table of the new index: the earlier generations, then this one.
    generations: Vec<GenerationInfo>,
    /// Called before the trailer is written, to put the data on disk (section 15).
    sync: Option<Box<dyn FnMut() -> std::io::Result<()>>>,
    /// The header's `version_minor`: a block's graph may name only primitives
    /// of this revision or an earlier one.
    version_minor: u16,
    /// Largest decoder working memory beyond the block buffer that a block
    /// written with [`Writer::add_part_encoded`] declared.
    extra_memory: u64,
    /// The file entry being written in parts, if any.
    open: Option<OpenEntry>,
    /// `(first_chunk, chunk_count)` of the blocks this writer closed whose graph
    /// names a reconstruction primitive, in order: a record may not name their
    /// chunks (spec section 12, nesting).
    recon_blocks: Vec<(u64, u64)>,
    /// Blocks handed over and not yet written, in block order (empty when
    /// `encode_threads` is 1: a block is then written as it closes).
    queue: VecDeque<InFlight>,
    /// The block buffer pool: plain or encoded block buffers kept after their block
    /// was written (or a job handed its plain buffer back), cleared, for the next
    /// pending blocks (their pages already touched). A buffer enters it only once
    /// nothing references it any more (ownership moves back here); at most
    /// [`Writer::pool_cap`] are kept.
    spares: Vec<Vec<u8>>,
    /// The read buffer of `chunk_stream`, kept across files.
    read_buf: Vec<u8>,
    /// When the stage profile is on ([`crate::profile`]): when this writer
    /// started, and the plain bytes it was fed.
    profile: Option<(Instant, u64)>,
    /// Tests only: skip the record chunk checks at block close, to build the
    /// archives the reader must refuse.
    #[cfg(test)]
    unchecked_records: bool,
    /// Tests only: the pool threads hand back a wrong frame hash.
    #[cfg(test)]
    corrupt_pool_hash: bool,
    /// Tests only: every buffer entering the pool is filled with [`POISON`] first,
    /// so a written frame holding that pattern shows a buffer reused too early.
    #[cfg(test)]
    poison_pool: bool,
}

/// Tests only: the byte a poisoned pool fills returned buffers with.
#[cfg(test)]
const POISON: u8 = 0xA5;

/// What an encode job returns: the encoding and the state its encoder takes back.
pub type JobResult = Result<(Encoded, Option<JobState>), FormatError>;

/// What a pool thread hands back for a block: the job's result, the plain buffer when the
/// encoding did not take it (reused for a later block), and the frame hash it computed over
/// exactly the payload the writer will write (none when the writer hashes the frame itself:
/// an encrypted archive, whose payload is sealed at the write step).
struct PoolOutput {
    result: JobResult,
    plain: Option<Vec<u8>>,
    frame_hash: Option<[u8; 32]>,
}

/// Buffers smaller than this are not kept for reuse.
const RECYCLE_MIN: usize = 1 << 20;

/// Computes a pool-encoded block's frame hash on the pool thread, over the
/// payload the writer will write: the block header (encoded from the graph,
/// `plain_len` and the encoded length, as [`Writer`] encodes it at the write
/// step), then the encoded bytes.
struct PoolHasher {
    plain_len: u64,
    threads: usize,
    /// Tests only: hand back a wrong hash, to show the writer refuses it.
    #[cfg(test)]
    corrupt: bool,
}

impl PoolHasher {
    fn hash(&self, e: &Encoded) -> [u8; 32] {
        let head = BlockHeader {
            graph: e.graph.clone(),
            plain_len: self.plain_len,
            encoded_len: e.bytes.len() as u64,
        }
        .encode();
        #[allow(unused_mut)]
        let mut h = frame_payload_hash(&head, &e.bytes, self.threads);
        #[cfg(test)]
        if self.corrupt {
            h[0] ^= 1;
        }
        h
    }
}

/// A block handed over and not yet written.
struct InFlight {
    first_chunk: u64,
    chunk_count: u64,
    plain_len: u64,
    /// How the block is checked when it is written.
    commit: Commit,
    work: Work,
}

/// The checks a block gets when it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Commit {
    /// Checked as the earlier revisions of this writer did, at the write: the
    /// graph and the chunks of the records it names (sequential mode, given or
    /// encoded).
    Unchecked,
    /// A pool-encoded block: the graph is checked at the write and may not
    /// name a record.
    Pool,
    /// A given block whose graph and record chunks were checked, and whose
    /// reconstruction status was recorded, at hand-over.
    Checked,
}

enum Work {
    Done(PoolOutput),
    Running(JoinHandle<PoolOutput>),
}

/// A chunk found stored already: `(index, from_old_table, len)`.
type KnownChunk = (u64, bool, u64);

/// Whole encoded length of a frame with a payload of `payload_len` bytes.
fn frame_len(payload_len: u64) -> u64 {
    4 + varint::len(payload_len) as u64 + payload_len + 32
}

impl<W: Write> Drop for Writer<W> {
    /// Every job still running is joined (its result, or its panic payload,
    /// dropped), so a writer dropped after an error, a panic or without
    /// `finish` leaves no thread behind. A job that never completes holds
    /// the drop, as it would hold `finish`.
    fn drop(&mut self) {
        for item in self.queue.drain(..) {
            if let Work::Running(handle) = item.work {
                let _ = handle.join();
            }
        }
    }
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
/// the records it names (`record_count` is how many the archive will have);
/// then the writer's own: no primitive that no revision decodes (`bcj-x86`,
/// `bcj-arm64`, `delta`, `base64`, `utf16`,
/// `container-reconstruct`; `BadGraph`, `undecoded primitive`, at every
/// minor, spec section 8), `bwt` only last or directly before a last
/// `deflate-reconstruct` (`BadGraph`, `bwt position`, revision 1.3),
/// `png-filter` only directly before a last `deflate-reconstruct`
/// (`BadGraph`, `png-filter not before deflate-reconstruct`, revision 1.4), and
/// every primitive belongs to a revision no later than the archive's
/// `version_minor` (`BadOptions`, `version_minor`).
fn check_graph(graph: &Graph, record_count: u64, version_minor: u16) -> Result<(), FormatError> {
    if graph.steps.is_empty() || graph.steps.len() > MAX_STEPS {
        return Err(FormatError::BadGraph {
            reason: "step count",
        });
    }
    for s in &graph.steps {
        s.primitive.validate_params(&s.params)?;
    }
    graph.check_records(record_count)?;
    if graph.steps.iter().any(|s| !s.primitive.is_decoded()) {
        return Err(FormatError::BadGraph {
            reason: "undecoded primitive",
        });
    }
    check_bwt_steps(graph)?;
    check_png_position(graph)?;
    if graph
        .steps
        .iter()
        .any(|s| s.primitive.revision() > version_minor)
    {
        return Err(bad_options("version_minor"));
    }
    Ok(())
}

/// Revision 1.3's position rule for `bwt`: a writer puts it last, or
/// directly before a last `deflate-reconstruct` (the look-ahead of the
/// reader's bounds goes one step, so only there is the step before `bwt`
/// bounded by the inflated length); anywhere else is `BadGraph`,
/// `bwt position`. A reader decodes `bwt` in any position (spec section 8,
/// "Decoding `bwt`").
fn check_bwt_steps(graph: &Graph) -> Result<(), FormatError> {
    let last = graph.steps.len().saturating_sub(1);
    for (i, s) in graph.steps.iter().enumerate() {
        if s.primitive != PrimitiveId::Bwt {
            continue;
        }
        let allowed = i == last
            || (i + 1 == last && graph.steps[i + 1].primitive == PrimitiveId::DeflateReconstruct);
        if !allowed {
            return Err(FormatError::BadGraph {
                reason: "bwt position",
            });
        }
    }
    Ok(())
}

/// Revision 1.4's position rule for `png-filter`: a writer puts it directly
/// before a last `deflate-reconstruct` (its output is that step's input, the
/// inflated text, and the look-ahead bounds the step before it by the
/// image's filtered length); anywhere else is `BadGraph`,
/// `png-filter not before deflate-reconstruct`. A reader decodes it in any
/// position (spec section 8, "Decoding `png-filter`").
fn check_png_position(graph: &Graph) -> Result<(), FormatError> {
    let last = graph.steps.len().saturating_sub(1);
    for (i, s) in graph.steps.iter().enumerate() {
        if s.primitive != PrimitiveId::PngFilter {
            continue;
        }
        if !(i + 1 == last && graph.steps[last].primitive == PrimitiveId::DeflateReconstruct) {
            return Err(FormatError::BadGraph {
                reason: "png-filter not before deflate-reconstruct",
            });
        }
    }
    Ok(())
}

/// A pool-encoded block may not name a record: every block in flight is then
/// a plain block, so a chunk's reconstruction status is known at hand-over.
fn pool_record_rule(graph: &Graph) -> Result<(), FormatError> {
    match graph
        .steps
        .iter()
        .find_map(|s| s.primitive.record_id(&s.params))
    {
        Some(record) => Err(FormatError::BadRecord {
            record,
            reason: "pool-encoded block names a record",
        }),
        None => Ok(()),
    }
}

/// A record the reader would refuse is refused here (`record` is its id).
fn check_record(r: &Record, record: u64) -> Result<(), FormatError> {
    if r.kind != r.body.kind() {
        return Err(bad_options("record kind"));
    }
    Record::parse(r.kind, &r.encode(), record)?;
    Ok(())
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
    for (i, r) in options.records.iter().enumerate() {
        check_record(r, i as u64)?;
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
    ///
    /// Memory: the pending block (up to `block_size` of plain bytes) and, while
    /// a block is encoded, its encoding; with `encode_threads` above 1, up to
    /// `encode_threads` blocks in flight at once, each its plain bytes plus its
    /// encoding — `encode_threads` times (`block_size` plus the encoded size)
    /// — plus the encoders' own working memory, one encoder per block in
    /// flight; the block buffer pool: buffers of written blocks (plain or
    /// encoded, those under 1 MiB or over `block_size` dropped) are kept,
    /// cleared, for the next pending blocks, at most `encode_threads` plus 3
    /// of them ([`WRITE_DEPTH`] plus 2: the pending block's, the frames queued
    /// for the write thread and the one it writes), and never more than were
    /// alive at once, for the writer's life; the read buffer of the file being
    /// added by [`Writer::add_file`] (up to `block_size` plus `chunk_size`),
    /// kept for the writer's life; the chunk table (40 bytes per chunk) and the block table for
    /// the life of the archive; the dedup map when `dedup` is set; the
    /// recovery group's bytes when `recovery` is set ([`Writer::recovery_buffered`]);
    /// with `encode_threads` above 1, the hashes of one read-ahead segment's
    /// chunks (32 bytes each) while up to [`MAX_HASH_THREADS`] scoped threads
    /// compute them.
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
        out: W,
        options: WriterOptions,
        chunker: Box<dyn Chunker>,
        rng: &mut dyn RngCore,
    ) -> Result<Self, FormatError> {
        Self::start(out, options, chunker, rng, FormatVersion::CURRENT.minor)
    }

    /// Like [`Writer::new`], writing `version_minor` into the header: 0 (what
    /// every other constructor writes) or 1, which lets blocks name the
    /// primitives of revision 1.1 (`jpeg-reconstruct`). The minor is the
    /// revision the writer writes under, not a fact about the blocks (spec
    /// section 2): an archive declared 1 may use no 1.1 primitive at all.
    ///
    /// The caller's obligation: the header is written first and never
    /// rewritten, so the caller declares, before any block exists, a revision
    /// no earlier than that of every primitive it will use. A block whose graph
    /// names a primitive of a later revision than declared is refused
    /// (`BadOptions`, `version_minor`, abandoning the archive), and so is a
    /// value above [`FormatVersion::LATEST`]'s minor.
    pub fn new_revision(
        out: W,
        options: WriterOptions,
        version_minor: u16,
    ) -> Result<Self, FormatError> {
        let size = usize::try_from(options.chunk_size).map_err(|_| bad_options("chunk_size"))?;
        Self::start(
            out,
            options,
            Box::new(FixedChunker::new(size)),
            &mut rand::rng(),
            version_minor,
        )
    }

    /// [`Writer::new_revision`] with another chunker (`chunk_size` is still the
    /// longest chunk and tail the writer accepts from it).
    pub fn new_revision_with_chunker(
        out: W,
        options: WriterOptions,
        version_minor: u16,
        chunker: Box<dyn Chunker>,
    ) -> Result<Self, FormatError> {
        Self::start(out, options, chunker, &mut rand::rng(), version_minor)
    }

    fn start(
        mut out: W,
        options: WriterOptions,
        chunker: Box<dyn Chunker>,
        rng: &mut dyn RngCore,
        version_minor: u16,
    ) -> Result<Self, FormatError> {
        if version_minor > FormatVersion::LATEST.minor {
            return Err(bad_options("version_minor"));
        }
        let spool = check_options(&options)?;
        let record_count = options.records.len() as u64;
        let write_records = record_count > 0;
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
        Header::with_minor(flags, options.archive_id, version_minor).write(&mut out)?;
        // The covered range starts right after the header.
        let out = Tee {
            inner: Sink::Direct(out),
            spool,
        };
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
            write_records,
            seen: HashSet::new(),
            pending: Vec::new(),
            pending_len: 0,
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
            own: HashMap::new(),
            deduped_chunks: 0,
            deduped_bytes: 0,
            graph_res: GraphResources::default(),
            salt,
            generations: vec![GenerationInfo {
                generation: 0,
                start_offset: Header::LEN as u64,
                first_sequence: 0,
                salt,
            }],
            sync: None,
            version_minor,
            extra_memory: 0,
            open: None,
            recon_blocks: Vec::new(),
            queue: VecDeque::new(),
            spares: Vec::new(),
            read_buf: Vec::new(),
            profile: profile::on().then(|| {
                profile::reset();
                (Instant::now(), 0)
            }),
            #[cfg(test)]
            unchecked_records: false,
            #[cfg(test)]
            corrupt_pool_hash: false,
            #[cfg(test)]
            poison_pool: false,
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
    /// Records: empty `options.records` loads the old records, so a record
    /// [`Writer::add_record`] adds follows them; the new `Records` frame
    /// (written only when a record was added) restates the old records at
    /// their positions, as the spec requires. Non-empty `options.records`
    /// must restate every old record unchanged (`BadRecord`, `changed by an
    /// append`; `RecordOutOfRange` when one is missing) and is written as the
    /// new frame. An append that adds no record keeps the old frame's location.
    /// Loading the old records parses the old `Records` table here: an archive
    /// whose records frame is damaged, or holds a record this writer cannot
    /// represent, is refused by this call rather than at `finish`.
    ///
    /// `existing` stays open and is read lazily between writes (see Memory),
    /// so its reader must not share a file position with `out`: a handle of
    /// its own on the same file (as [`Writer::append_file`] opens) is fine, a
    /// clone of `out`'s handle is not.
    ///
    /// Memory: the old chunk table's hashes (`HashMap`, 32 bytes of key and 8 of
    /// value per chunk), its records (`ChunkRecord`, 40 bytes per chunk), the
    /// old entries and the old `Records` table (parsed) are held until
    /// `finish`; so is `existing` itself (boxed), whose `Index` and chunk index
    /// stay resident next to them. It is read when a record part would
    /// reference an old chunk, or a closing block's graph names a record with
    /// an old chunk: that chunk's old block frame is read to learn whether the
    /// block's graph names a reconstruction primitive (spec section 12); the
    /// answer is cached per old block (`HashMap`, one entry per block read), so
    /// no old block is read twice and none is read when no record is involved.
    pub fn append<R: Read + Seek + 'static>(
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
    pub fn append_with_chunker<R: Read + Seek + 'static>(
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
    pub fn append_with_chunker_and_rng<R: Read + Seek + 'static>(
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
        // The new frame is written only for records given here or added later;
        // without either the index keeps the old frame's location.
        let write_records = !options.records.is_empty();
        if options.records.is_empty() {
            // Load the old records: a record added later follows them, and the
            // frame written then restates them at their positions.
            if let Some(old) = existing.records()? {
                options.records = old.table()?.iter().collect::<Result<Vec<_>, _>>()?;
            }
        }
        let record_count = options.records.len() as u64;
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
        let version_minor = existing.header().version.minor;
        let extra_memory = index
            .envelope
            .decode_memory
            .saturating_sub(index.envelope.max_block_plain);
        let old_records = index.records;
        let blocks = index.blocks.clone();
        let priors = index.priors.iter().copied().collect();
        let recovery_locs = index.recovery.clone();
        let graph_res = GraphResources {
            window: index.envelope.max_window,
            bwt_block: index.envelope.max_bwt_block,
        };
        let listable = existing.is_listable();
        let base = Base {
            generation,
            trailer_offset,
            chunks: records.len() as u64,
            by_hash,
            entries,
            records: old_records,
            graphs: Box::new(existing),
            recon: HashMap::new(),
        };
        let w = Writer {
            out: Tee {
                inner: Sink::Direct(out),
                spool,
            },
            chunker,
            pos: end,
            entries: Vec::new(),
            records,
            blocks,
            priors,
            record_count,
            write_records,
            seen: HashSet::new(),
            pending: Vec::new(),
            pending_len: 0,
            pending_chunks: 0,
            failed: None,
            bad_chunk: None,
            recovery_locs,
            cover_start: end,
            sealer,
            seq,
            last_hash: [0; 32],
            listable,
            base: Some(base),
            deleted: BTreeSet::new(),
            reused: 0,
            own: HashMap::new(),
            deduped_chunks: 0,
            deduped_bytes: 0,
            graph_res,
            salt,
            generations,
            sync: None,
            version_minor,
            extra_memory,
            open: None,
            recon_blocks: Vec::new(),
            queue: VecDeque::new(),
            spares: Vec::new(),
            read_buf: Vec::new(),
            profile: profile::on().then(|| {
                profile::reset();
                (Instant::now(), 0)
            }),
            #[cfg(test)]
            unchecked_records: false,
            #[cfg(test)]
            corrupt_pool_hash: false,
            #[cfg(test)]
            poison_pool: false,
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
        // Moved, not copied, to the write thread when there is one; dropped once written.
        w.write_owned(g.recovery, false)?;
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
        let at = FrameLocation {
            offset: self.pos,
            len: frame_len(payload.len() as u64),
            sequence: self.seq,
        };
        // As `Frame::write` lays it out, the payload moved rather than copied.
        self.last_hash = *blake3::hash(&payload).as_bytes();
        let w = &mut self.out;
        w.write_all(&(kind as u16).to_le_bytes())?;
        w.write_all(&flags.bits().to_le_bytes())?;
        varint::write(w, payload.len() as u64)?;
        // Not a block buffer (a table, the index, a sealed block): dropped once written.
        w.write_owned(payload, false)?;
        self.out.write_all(&self.last_hash)?;
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
        // A write that failed on the write thread fails every later call.
        if let Some(e) = self.out.inner.error() {
            return Err(FormatError::Io(e));
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
        self.flush_block_with(None)
    }

    /// Blocks in flight at most (1: sequential).
    fn threads(&self) -> usize {
        self.options.encode_threads.max(1)
    }

    /// Close the pending block; `given` is its encoding when the caller made
    /// it ([`Writer::add_part_encoded`]), else the encoder is asked.
    ///
    /// Sequential (`encode_threads` 1): the block is encoded, checked and
    /// written here. Otherwise it is handed over: a given block joins the
    /// ordered queue checked; an encoder's block becomes a job on a pool
    /// thread (or is encoded here when the encoder offers no job); and when
    /// the queue is full the oldest block is awaited and written first.
    fn flush_block_with(&mut self, given: Option<Encoded>) -> Result<(), FormatError> {
        if self.pending_chunks == 0 {
            return Ok(());
        }
        let mut closing = profile::Timer::start(profile::Stage::BlockClose);
        closing.bytes = self.pending_len as u64;
        let next = self.take_spare();
        let mut plain = std::mem::replace(&mut self.pending, next);
        plain.truncate(self.pending_len);
        self.pending_len = 0;
        let chunk_count = std::mem::take(&mut self.pending_chunks);
        let first_chunk = self.records.len() as u64 - chunk_count;
        let plain_len = plain.len() as u64;
        let threads = self.threads();
        if threads == 1 {
            let encoded = match given {
                Some(e) => {
                    self.recycle(plain);
                    Ok(e)
                }
                None => {
                    let (r, back) = self.options.encoder.encode_owned(plain);
                    if let Some(b) = back {
                        self.recycle(b);
                    }
                    r
                }
            };
            let encoded = self.abandon_on_error(encoded)?;
            return self.commit_block(
                encoded,
                first_chunk,
                chunk_count,
                plain_len,
                Commit::Unchecked,
                None,
            );
        }
        // Room first: the bound counts the encoders' cores too, and a job
        // borrows one when it is made.
        while self.queue.len() >= threads {
            self.write_oldest()?;
        }
        let item = match given {
            Some(e) => {
                self.recycle(plain);
                let checked = check_graph(&e.graph, self.record_count, self.version_minor)
                    .and_then(|()| self.check_record_chunks(&e.graph, first_chunk))
                    .and_then(|()| self.check_deflate_steps(&e.graph, plain_len));
                self.abandon_on_error(checked)?;
                // Classified now: a later chunk's dedup lookup must know this
                // block is a reconstruction block before it is written.
                if e.graph.uses_records() {
                    self.recon_blocks.push((first_chunk, chunk_count));
                }
                InFlight {
                    first_chunk,
                    chunk_count,
                    plain_len,
                    commit: Commit::Checked,
                    work: Work::Done(PoolOutput {
                        result: Ok((e, None)),
                        plain: None,
                        frame_hash: None,
                    }),
                }
            }
            None => {
                let work = match self.options.encoder.job() {
                    Some(job) => {
                        let hasher = self.pool_hasher(plain_len, threads);
                        let spawned = std::thread::Builder::new()
                            .name("lpk-encode".to_string())
                            .spawn(move || {
                                let mut encoding = profile::Timer::start(profile::Stage::Encode);
                                encoding.bytes = plain_len;
                                let (result, plain) = job.run_owned(plain);
                                drop(encoding);
                                let mut hashing = profile::Timer::start(profile::Stage::FrameHash);
                                hashing.bytes = plain_len;
                                let frame_hash = match (&result, hasher) {
                                    (Ok((e, _)), Some(h)) => Some(h.hash(e)),
                                    _ => None,
                                };
                                drop(hashing);
                                PoolOutput {
                                    result,
                                    plain,
                                    frame_hash,
                                }
                            });
                        match spawned {
                            Ok(handle) => Work::Running(handle),
                            Err(e) => return self.abandon_on_error(Err(FormatError::Io(e))),
                        }
                    }
                    None => {
                        // No job: this encoder runs sequentially, on this thread.
                        let (r, back) = self.options.encoder.encode_owned(plain);
                        let encoded = self.abandon_on_error(r)?;
                        Work::Done(PoolOutput {
                            result: Ok((encoded, None)),
                            plain: back,
                            frame_hash: None,
                        })
                    }
                };
                InFlight {
                    first_chunk,
                    chunk_count,
                    plain_len,
                    commit: Commit::Pool,
                    work,
                }
            }
        };
        self.queue.push_back(item);
        self.write_finished()
    }

    /// An encoder or check error at block close abandons the archive (the
    /// block's chunks are gone) like an I/O error: every later call fails.
    fn abandon_on_error<T>(&mut self, r: Result<T, FormatError>) -> Result<T, FormatError> {
        if let Err(e) = &r {
            self.failed = Some((std::io::ErrorKind::InvalidData, e.to_string()));
        }
        r
    }

    /// Write the blocks at the front of the queue whose encoding is complete.
    fn write_finished(&mut self) -> Result<(), FormatError> {
        while self.queue.front().is_some_and(|b| match &b.work {
            Work::Done(_) => true,
            Work::Running(h) => h.is_finished(),
        }) {
            self.write_oldest()?;
        }
        Ok(())
    }

    /// Write every block in flight, in order.
    fn drain(&mut self) -> Result<(), FormatError> {
        while !self.queue.is_empty() {
            self.write_oldest()?;
        }
        Ok(())
    }

    /// Await the oldest block in flight and write it. A job's error abandons
    /// the archive and is returned; a job's panic is re-raised once the writer
    /// is marked failed.
    fn write_oldest(&mut self) -> Result<(), FormatError> {
        let Some(InFlight {
            first_chunk,
            chunk_count,
            plain_len,
            commit,
            work,
        }) = self.queue.pop_front()
        else {
            return Ok(());
        };
        let mut waiting = profile::Timer::start(profile::Stage::BlockWait);
        waiting.bytes = plain_len;
        let output = match work {
            Work::Done(r) => r,
            Work::Running(handle) => match handle.join() {
                Ok(r) => r,
                Err(panic) => {
                    self.failed = Some((
                        std::io::ErrorKind::InvalidData,
                        "an encode job panicked".to_string(),
                    ));
                    std::panic::resume_unwind(panic);
                }
            },
        };
        let PoolOutput {
            result,
            plain,
            frame_hash,
        } = output;
        if let Some(p) = plain {
            self.recycle(p);
        }
        let (encoded, state) = self.abandon_on_error(result)?;
        if let Some(state) = state {
            self.options.encoder.absorb(state);
        }
        self.commit_block(
            encoded,
            first_chunk,
            chunk_count,
            plain_len,
            commit,
            frame_hash,
        )
    }

    /// Block buffers the pool keeps at most: one per block in flight, plus the
    /// pending block's and the ones queued for or being written by the write
    /// thread ([`WRITE_DEPTH`] plus one). Never more than were alive at once.
    fn pool_cap(&self) -> usize {
        self.threads() + WRITE_DEPTH + 2
    }

    /// Return `buf` to the pool (cleared): it becomes the pending block when that
    /// holds nothing yet and is smaller, else a spare. Buffers under 1 MiB or
    /// larger than a block are dropped; a full pool drops its smallest buffer.
    fn recycle(&mut self, mut buf: Vec<u8>) {
        if !self.pool_sized(buf.capacity()) {
            return;
        }
        #[cfg(test)]
        if self.poison_pool {
            buf.clear();
            buf.resize(buf.capacity(), POISON);
        }
        // Cleared, never filled: a block it later holds grows it only as far
        // as that block's bytes go (`put`, the segment path's resize), so no
        // thread touches more of it than is written.
        buf.clear();
        if self.pending_len == 0 && self.pending.capacity() < buf.capacity() {
            buf = std::mem::replace(&mut self.pending, buf);
            if buf.capacity() < RECYCLE_MIN {
                return;
            }
        }
        if self.spares.len() < self.pool_cap() {
            self.spares.push(buf);
            return;
        }
        if let Some(small) = self
            .spares
            .iter_mut()
            .min_by_key(|s| s.capacity())
            .filter(|s| s.capacity() < buf.capacity())
        {
            *small = buf;
        }
    }

    /// Whether a buffer of `capacity` bytes belongs in the pool: at least
    /// [`RECYCLE_MIN`] and at most a block (a compressor's output above a
    /// block is dropped once written, and never sent back).
    fn pool_sized(&self, capacity: usize) -> bool {
        capacity >= RECYCLE_MIN && capacity as u64 <= self.options.block_size
    }

    /// The largest buffer of the pool, for the next pending block (empty when
    /// the pool has none).
    fn take_spare(&mut self) -> Vec<u8> {
        for b in self.out.inner.returned() {
            self.recycle(b);
        }
        match (0..self.spares.len()).max_by_key(|&i| self.spares[i].capacity()) {
            Some(i) => self.spares.swap_remove(i),
            None => Vec::new(),
        }
    }

    /// What computes a pool-encoded block's frame hash on the pool thread:
    /// none for an encrypted archive (its payload is sealed at the write step,
    /// with the frame's sequence, and the writer hashes the sealed payload).
    fn pool_hasher(&self, plain_len: u64, threads: usize) -> Option<PoolHasher> {
        if self.sealer.is_some() {
            return None;
        }
        Some(PoolHasher {
            plain_len,
            threads,
            #[cfg(test)]
            corrupt: self.corrupt_pool_hash,
        })
    }

    /// The in-order write step of a closed block: the checks `commit` asks
    /// for, priors, the resource maxima, the frame (sealed or plain), the
    /// block table and the reconstruction blocks. `frame_hash` is the hash a
    /// pool thread computed over the frame's payload (the block header the
    /// writer encodes here, then the encoded bytes); without one the writer
    /// hashes the payload itself. Debug builds recompute a given hash and
    /// refuse a wrong one (`BadOptions`, `pool frame hash`), abandoning the
    /// archive before the frame is written.
    fn commit_block(
        &mut self,
        encoded: Encoded,
        first_chunk: u64,
        chunk_count: u64,
        plain_len: u64,
        commit: Commit,
        frame_hash: Option<[u8; 32]>,
    ) -> Result<(), FormatError> {
        let Encoded {
            graph,
            bytes: encoded,
            resources,
            memory,
        } = encoded;
        let checked = match commit {
            Commit::Unchecked => check_graph(&graph, self.record_count, self.version_minor)
                .and_then(|()| self.check_record_chunks(&graph, first_chunk))
                .and_then(|()| self.check_deflate_steps(&graph, plain_len)),
            Commit::Pool => check_graph(&graph, self.record_count, self.version_minor)
                .and_then(|()| pool_record_rule(&graph)),
            Commit::Checked => Ok(()),
        };
        // The block is gone: the archive is abandoned like after an I/O error.
        self.abandon_on_error(checked)?;
        let recon = graph.uses_records();
        self.priors.extend(graph.prior_ids());
        self.extra_memory = self.extra_memory.max(memory);
        let own = graph.resources();
        self.graph_res = GraphResources {
            window: self.graph_res.window.max(resources.window).max(own.window),
            bwt_block: self
                .graph_res
                .bwt_block
                .max(resources.bwt_block)
                .max(own.bwt_block),
        };
        let header = BlockHeader {
            graph,
            plain_len,
            encoded_len: encoded.len() as u64,
        };
        // The frame is written piecewise, without a second copy of the encoded
        // block (a stored block's encoded bytes are its plain buffer, moved).
        let head = header.encode();
        let plain_payload_len = (head.len() + encoded.len()) as u64;
        let payload_len = plain_payload_len + self.overhead();
        let hash = match frame_hash {
            Some(h) if self.sealer.is_none() => {
                if cfg!(debug_assertions) && h != frame_payload_hash(&head, &encoded, 1) {
                    return self.abandon_on_error(Err(bad_options("pool frame hash")));
                }
                Some(h)
            }
            _ => None,
        };
        self.make_room(frame_len(payload_len))?;
        let at = if let Some(sealer) = &self.sealer {
            let mut payload = head;
            payload.extend_from_slice(&encoded);
            let sealed =
                sealer.seal(&payload, FrameKind::ChunkData as u16, self.seq, &self.salt)?;
            self.recycle(encoded);
            self.write_wire_frame(FrameKind::ChunkData, FrameFlags::SEALED, sealed)?
        } else {
            let at = FrameLocation {
                offset: self.pos,
                len: frame_len(payload_len),
                sequence: self.seq,
            };
            let hash = hash.unwrap_or_else(|| frame_payload_hash(&head, &encoded, 1));
            // The block buffer moves to the write thread, if any, and comes
            // back to the pool once written when the pool would keep it (a
            // compressor's output, larger than a block, is dropped there).
            let back = self.pool_sized(encoded.capacity());
            let w = &mut self.out;
            w.write_all(&(FrameKind::ChunkData as u16).to_le_bytes())?;
            w.write_all(&FrameFlags::EMPTY.bits().to_le_bytes())?;
            varint::write(w, payload_len)?;
            w.write_all(&head)?;
            if let Some(b) = w.write_owned(encoded, back)? {
                self.recycle(b);
            }
            self.out.write_all(&hash)?;
            self.pos += at.len;
            self.seq += 1;
            at
        };
        self.blocks.push(BlockLocation {
            frame_offset: at.offset,
            frame_len: at.len,
            first_chunk,
            chunk_count,
            plain_len,
            sequence: at.sequence,
        });
        if recon && commit != Commit::Checked {
            self.recon_blocks.push((first_chunk, chunk_count));
        }
        Ok(())
    }

    /// The reader's rules for the chunks of the records a closing block's
    /// graph names (spec section 12), for the records this writer holds (in an
    /// append, the old records included): every chunk lies in an earlier
    /// block (`BadRecord`, `chunk order`) whose graph names no reconstruction
    /// primitive (`BadRecord`, `nested record`). For a chunk of an old
    /// generation the old block's graph is read (once per block, cached).
    fn check_record_chunks(&mut self, graph: &Graph, first_chunk: u64) -> Result<(), FormatError> {
        #[cfg(test)]
        if self.unchecked_records {
            return Ok(());
        }
        for s in &graph.steps {
            let Some(record) = s.primitive.record_id(&s.params) else {
                continue;
            };
            let Some(r) = usize::try_from(record)
                .ok()
                .and_then(|i| self.options.records.get(i))
            else {
                continue;
            };
            // The record's chunk lists, copied: the block lookup may read an
            // old block through `self`.
            let chunks: Vec<u64> = r
                .chunk_groups()
                .iter()
                .flat_map(|(c, _)| c.iter().copied())
                .collect();
            for c in chunks {
                if c >= first_chunk {
                    return Err(FormatError::BadRecord {
                        record,
                        reason: "chunk order",
                    });
                }
                if self.in_recon_block(c)? {
                    return Err(FormatError::BadRecord {
                        record,
                        reason: "nested record",
                    });
                }
            }
        }
        Ok(())
    }

    /// Revision 1.2's record checks of `deflate-reconstruct` (spec section 8,
    /// checks 2 to 4) for the steps of a closing block's graph, for the records
    /// this writer holds: the record is a `deflate` record (`BadRecord`,
    /// `kind`); as the last step, its `original_len` is the block's
    /// `plain_len` (`BlockLengthMismatch`); its `corrections` are no longer
    /// than `original_len` (`BadRecord`, `corrections`). Before those, a
    /// `deflate-reconstruct` step that is not the graph's last is refused
    /// (`BadGraph`, `deflate-reconstruct not last`): a writer writes it last
    /// (spec section 8), so its output is the block's and never has to meet
    /// `max_block_plain`, which is known only when the archive is finished. A
    /// reader still decodes the step in any position.
    fn check_deflate_steps(&self, graph: &Graph, plain_len: u64) -> Result<(), FormatError> {
        #[cfg(test)]
        if self.unchecked_records {
            return Ok(());
        }
        let last = graph.steps.len().saturating_sub(1);
        for (i, s) in graph.steps.iter().enumerate() {
            if s.primitive != PrimitiveId::DeflateReconstruct {
                continue;
            }
            if i != last {
                return Err(FormatError::BadGraph {
                    reason: "deflate-reconstruct not last",
                });
            }
            let Some(record) = s.primitive.record_id(&s.params) else {
                continue;
            };
            let Some(r) = usize::try_from(record)
                .ok()
                .and_then(|i| self.options.records.get(i))
            else {
                continue;
            };
            let RecordBody::Deflate(d) = &r.body else {
                return Err(FormatError::BadRecord {
                    record,
                    reason: "kind",
                });
            };
            if d.original_len != plain_len {
                return Err(FormatError::BlockLengthMismatch {
                    block: self.blocks.len() + self.queue.len(),
                });
            }
            if d.corrections.len() as u64 > d.original_len {
                return Err(FormatError::BadRecord {
                    record,
                    reason: "corrections",
                });
            }
            if i > 0 {
                self.check_png_step(&graph.steps[i - 1], d.plain_len)?;
            }
        }
        Ok(())
    }

    /// Revision 1.4's record checks of a `png-filter` step directly before a
    /// last `deflate-reconstruct` whose record's `plain_len` is `plain_len`,
    /// for the records this writer holds: a `png-filter` record (`BadRecord`,
    /// `kind`), check (3) of "Decoding `png-filter`" (`width`, `height`,
    /// `bit_depth`, `filters`), and its filtered length equal to `plain_len`
    /// (`BadRecord`, `filtered length`; the writer's own reason: a reader meets
    /// a longer one as `PayloadTooLarge` and a shorter one as the deflate
    /// step's `plain_len`).
    fn check_png_step(&self, step: &Step, plain_len: u64) -> Result<(), FormatError> {
        if step.primitive != PrimitiveId::PngFilter {
            return Ok(());
        }
        let Some(record) = step.primitive.record_id(&step.params) else {
            return Ok(());
        };
        let Some(r) = usize::try_from(record)
            .ok()
            .and_then(|i| self.options.records.get(i))
        else {
            return Ok(());
        };
        let RecordBody::PngFilter(p) = &r.body else {
            return Err(FormatError::BadRecord {
                record,
                reason: "kind",
            });
        };
        p.check_image(record)?;
        if p.filtered_len() != Some(plain_len) {
            return Err(FormatError::BadRecord {
                record,
                reason: "filtered length",
            });
        }
        Ok(())
    }

    /// Close the current block now, even if it has room (nothing when it holds
    /// no chunk): the next chunk starts a new block. The block's graph is
    /// validated here, as when a block closes because it is full.
    ///
    /// With `encode_threads` above 1 the block is handed to the pool and this
    /// call returns without waiting on it: it waits only when `encode_threads`
    /// blocks are already in flight, for the oldest, which it writes first;
    /// blocks whose encoding has completed are written in order before it
    /// returns. The checks and the frame write of the block happen at that
    /// later, in-order step, so an error of an earlier block (its encoder's,
    /// its graph check's, the output's) may surface here rather than in the
    /// call that closed it.
    ///
    /// Errors: the call that fails (an encoder error, a graph that fails its
    /// check, an I/O error) returns the error's own class and abandons the
    /// archive; every later call reports `Io` (`InvalidData`) with the same
    /// message. The same holds for the `add_*` calls (which close a full
    /// block) and for `finish` (which closes the last one).
    pub fn close_block(&mut self) -> Result<(), FormatError> {
        self.check_alive()?;
        let r = self.flush_block();
        self.note(r)
    }

    /// The index of a chunk this write may reference instead of storing:
    /// `(index, from_old_table)`. A chunk of a block whose graph names a
    /// reconstruction primitive is skipped when `allow_recon` is false (a
    /// record's chunks may not lie in such a block, spec section 12): for a
    /// chunk of the old table that means reading the old block's graph (once
    /// per block), and a chunk found there is stored anew instead. Errors:
    /// the old block frame could not be read or is damaged.
    fn find_known(
        &mut self,
        hash: &[u8; 32],
        len: u64,
        allow_recon: bool,
    ) -> Result<Option<(u64, bool)>, FormatError> {
        let same_len = |records: &[ChunkRecord], i: u64| {
            usize::try_from(i)
                .ok()
                .and_then(|i| records.get(i))
                .is_some_and(|r| r.plain_len == len)
        };
        let old = self
            .base
            .as_ref()
            .and_then(|b| b.by_hash.get(hash).copied());
        if let Some(old) = old {
            if same_len(&self.records, old) && (allow_recon || !self.in_recon_block(old)?) {
                return Ok(Some((old, true)));
            }
        }
        if self.options.dedup {
            if let Some(&i) = self.own.get(hash) {
                if same_len(&self.records, i) && (allow_recon || !self.in_recon_block(i)?) {
                    return Ok(Some((i, false)));
                }
            }
        }
        Ok(None)
    }

    /// Whether chunk `c` lies in a block whose graph names a reconstruction
    /// primitive: a block this writer closed is known; a block of an old
    /// generation (an append) has its graph read from the archive the first
    /// time one of its chunks is asked about, the answer cached per block.
    fn in_recon_block(&mut self, c: u64) -> Result<bool, FormatError> {
        let i = self.recon_blocks.partition_point(|&(f, _)| f <= c);
        if i > 0 && c - self.recon_blocks[i - 1].0 < self.recon_blocks[i - 1].1 {
            return Ok(true);
        }
        let Some(base) = &mut self.base else {
            return Ok(false);
        };
        if c >= base.chunks {
            return Ok(false);
        }
        // The block whose range starts at or before `c`: the last one when the
        // table runs short (reported, as the reader does, as the block that
        // fails to cover the chunk).
        let block = self
            .blocks
            .partition_point(|b| b.first_chunk <= c)
            .saturating_sub(1);
        let Some(loc) = self
            .blocks
            .get(block)
            .filter(|loc| c >= loc.first_chunk && c - loc.first_chunk < loc.chunk_count)
        else {
            return Err(FormatError::BlockCoverage { block });
        };
        if let Some(&known) = base.recon.get(&block) {
            return Ok(known);
        }
        let recon = base.graphs.uses_records(FrameLocation {
            offset: loc.frame_offset,
            len: loc.frame_len,
            sequence: loc.sequence,
        })?;
        base.recon.insert(block, recon);
        Ok(recon)
    }

    fn note_known(&mut self, len: u64, old_table: bool) {
        if old_table {
            self.reused += 1;
        } else {
            self.deduped_chunks += 1;
            self.deduped_bytes += len;
        }
    }

    fn add_chunk(&mut self, data: &[u8], allow_recon: bool) -> Result<u64, FormatError> {
        let hash = *blake3::hash(data).as_bytes();
        self.add_chunk_hashed(data, &[], hash, allow_recon)
    }

    /// [`Writer::add_chunk`] for a chunk made of `head` then `tail`, whose
    /// BLAKE3 `hash` is already known.
    fn add_chunk_hashed(
        &mut self,
        head: &[u8],
        tail: &[u8],
        hash: [u8; 32],
        allow_recon: bool,
    ) -> Result<u64, FormatError> {
        let len = (head.len() + tail.len()) as u64;
        // An append reuses a chunk of the old table with the same hash and length;
        // with `dedup`, so does any chunk this write stored before.
        if let Some((i, old)) = self.find_known(&hash, len, allow_recon)? {
            self.note_known(len, old);
            return Ok(i);
        }
        if self.pending_chunks > 0 && self.pending_len as u64 + len > self.options.block_size {
            self.flush_block()?;
        }
        let index = self.note_record(hash, len)?;
        self.put(head);
        self.put(tail);
        Ok(index)
    }

    /// Record a chunk of `len` bytes that goes to the pending block (its
    /// record and, with dedup, its index); the caller puts its bytes.
    fn note_record(&mut self, hash: [u8; 32], len: u64) -> Result<u64, FormatError> {
        let index = self.records.len() as u64;
        self.records.push(ChunkRecord {
            plain_len: len,
            hash,
        });
        if self.options.dedup {
            // Prefer an index in a plain block: a chunk first seen in a reconstruction
            // block is replaced by this plain copy, which records may name as well.
            match self.own.get(&hash).copied() {
                None => {
                    self.own.insert(hash, index);
                }
                // A chunk of this write: its block is known without a read.
                Some(i) if self.in_recon_block(i)? => {
                    self.own.insert(hash, index);
                }
                Some(_) => {}
            }
        }
        self.pending_chunks += 1;
        Ok(index)
    }

    /// Append `src` to the pending block at its fill: in place when the
    /// buffer is long enough (a pool buffer), else by growing it.
    fn put(&mut self, src: &[u8]) {
        let at = self.pending_len;
        let end = at + src.len();
        if self.pending.len() >= end {
            self.pending[at..end].copy_from_slice(src);
        } else {
            self.pending.truncate(at);
            self.pending.extend_from_slice(src);
        }
        self.pending_len = end;
    }

    /// Cut `plain` with the chunker into `(start, end)` ranges, checked as the
    /// writer checks a stream's.
    fn cut_all(&mut self, plain: &[u8]) -> Result<Vec<(usize, usize)>, FormatError> {
        let chunk_size = self.options.chunk_size as usize;
        self.chunker.reset();
        let cuts = self.chunker.feed(plain, true);
        let mut start = 0usize;
        let mut out = Vec::with_capacity(cuts.len());
        for cut in cuts {
            if cut <= start || cut > plain.len() {
                return Err(FormatError::BadChunk {
                    reason: "cut positions",
                });
            }
            if cut - start > chunk_size {
                return Err(FormatError::BadChunk {
                    reason: "chunk longer than chunk_size",
                });
            }
            out.push((start, cut));
            start = cut;
        }
        if start != plain.len() {
            return Err(FormatError::BadChunk {
                reason: "bytes left uncut at eof",
            });
        }
        Ok(out)
    }

    /// Add the part of the open entry that starts at `offset` when every chunk
    /// of `plain` (cut by the chunker) is already stored in this archive (needs
    /// `WriterOptions::dedup`): the part then references them, nothing is
    /// written, and the chunk indices are returned. Otherwise nothing happens
    /// and `None` comes back (an empty `plain` is `None` too). Chunks in
    /// reconstruction blocks count, so the returned indices must never be named
    /// by a reconstruction record (spec section 12, nesting): use this for a
    /// part the entry lists plainly, without a record. A part a record names
    /// goes through [`Writer::add_part`], which never references such chunks.
    pub fn add_part_known(
        &mut self,
        offset: u64,
        plain: &[u8],
    ) -> Result<Option<Vec<u64>>, FormatError> {
        self.check_alive()?;
        let r = self.add_part_known_inner(offset, plain);
        self.note(r)
    }

    /// Add a regular file whose chunks (cut by the chunker) are all stored
    /// already in this archive (needs `WriterOptions::dedup`): the entry
    /// references them and nothing is written, and `true` comes back.
    /// Otherwise nothing happens and the result is `false` (also for an empty
    /// file). The path is checked as in [`Writer::add_file`].
    pub fn add_file_if_known(
        &mut self,
        path: &str,
        flags: EntryFlags,
        mtime_ns: i64,
        data: &[u8],
    ) -> Result<bool, FormatError> {
        self.check_alive()?;
        self.no_open_entry()?;
        let r = self.add_file_if_known_inner(path, flags, mtime_ns, data);
        self.note(r)
    }

    fn add_file_if_known_inner(
        &mut self,
        path: &str,
        flags: EntryFlags,
        mtime_ns: i64,
        data: &[u8],
    ) -> Result<bool, FormatError> {
        let mut entry = Entry {
            kind: EntryKind::File,
            flags,
            path: path.to_string(),
            mtime_ns,
            size: data.len() as u64,
            symlink_target: None,
            chunks: Vec::new(),
        };
        self.check(&entry)?;
        if !self.options.dedup || data.is_empty() {
            return Ok(false);
        }
        let mut found = Vec::new();
        for (a, b) in self.cut_all(data)? {
            let part = &data[a..b];
            let hash = *blake3::hash(part).as_bytes();
            match self.find_known(&hash, part.len() as u64, true)? {
                Some((i, old)) => found.push((i, old, part.len() as u64)),
                None => return Ok(false),
            }
        }
        for (i, old, len) in found {
            self.note_known(len, old);
            entry.chunks.push(i);
        }
        self.push_entry(entry);
        Ok(true)
    }

    /// Whether [`Writer::add_part_known`] would reference `plain` as a part
    /// at the current position, and with which chunks, without committing
    /// anything: `Some(indices)` when `WriterOptions::dedup` is on, `plain`
    /// is not empty and every chunk the chunker cuts from `plain` (cut from
    /// its start, exactly as `add_part_known` cuts it) is stored already with
    /// the same hash and length (in this write, or in the old table of an
    /// append; chunks in reconstruction blocks count, as for
    /// `add_part_known`); `None` otherwise. The indices are those
    /// `add_part_known` returns for the same `plain` if nothing is added in
    /// between. Nothing of the writer changes (no entry is needed, no counter
    /// moves, a failure does not abandon the archive); `BadChunk` comes back
    /// for a chunker that cuts badly.
    pub fn part_known(&mut self, plain: &[u8]) -> Result<Option<Vec<u64>>, FormatError> {
        self.check_alive()?;
        Ok(self
            .known_chunks(plain)?
            .map(|found| found.into_iter().map(|(i, _, _)| i).collect()))
    }

    /// The `(index, from_old_table, len)` of every chunk of `plain` when all
    /// are stored already (see [`Writer::part_known`]); `None` otherwise.
    /// Commits nothing: the chunker is reset before it cuts, as at every use.
    fn known_chunks(&mut self, plain: &[u8]) -> Result<Option<Vec<KnownChunk>>, FormatError> {
        if !self.options.dedup || plain.is_empty() {
            return Ok(None);
        }
        let mut found = Vec::new();
        for (a, b) in self.cut_all(plain)? {
            let data = &plain[a..b];
            let hash = *blake3::hash(data).as_bytes();
            match self.find_known(&hash, data.len() as u64, true)? {
                Some((i, old)) => found.push((i, old, data.len() as u64)),
                None => return Ok(None),
            }
        }
        Ok(Some(found))
    }

    fn add_part_known_inner(
        &mut self,
        offset: u64,
        plain: &[u8],
    ) -> Result<Option<Vec<u64>>, FormatError> {
        if self.open.is_none() {
            return Err(bad_options("no entry is open"));
        }
        let Some(found) = self.known_chunks(plain)? else {
            return Ok(None);
        };
        let mut chunks = Vec::with_capacity(found.len());
        for (i, old, len) in found {
            self.note_known(len, old);
            chunks.push(i);
        }
        if let Some(o) = &mut self.open {
            o.parts.push((offset, plain.len() as u64, chunks.clone()));
        }
        Ok(Some(chunks))
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
        self.no_open_entry()?;
        let r = self.add_file_inner(path, flags, mtime_ns, data);
        self.note(r)
    }

    /// Refuse a call that needs no entry to be open.
    fn no_open_entry(&self) -> Result<(), FormatError> {
        if self.open.is_some() {
            return Err(bad_options("an entry is open"));
        }
        Ok(())
    }

    /// Start a file entry written in parts: [`Writer::add_part`] and
    /// [`Writer::add_part_encoded`] add its bytes part by part, in any order
    /// (each part names its offset in the file), with blocks closing between
    /// them as they would for any chunks; [`Writer::end_entry`] lists the
    /// chunks in file order. The path is checked here as in
    /// [`Writer::add_file`]. Until `end_entry`, the other `add_*` calls and
    /// `finish` are refused (`BadOptions`, `an entry is open`).
    pub fn begin_entry(
        &mut self,
        path: &str,
        flags: EntryFlags,
        mtime_ns: i64,
    ) -> Result<(), FormatError> {
        self.check_alive()?;
        self.no_open_entry()?;
        let entry = Entry {
            kind: EntryKind::File,
            flags,
            path: path.to_string(),
            mtime_ns,
            size: 0,
            symlink_target: None,
            chunks: Vec::new(),
        };
        self.check(&entry)?;
        self.open = Some(OpenEntry {
            entry,
            parts: Vec::new(),
        });
        Ok(())
    }

    /// Add the part of the open entry that starts at `offset` in the file: its
    /// bytes are read to the end, cut into chunks and appended to the current
    /// block, like a file's. Returns the part's chunk indices in order (in an
    /// append, a chunk of the old table may be referenced instead of written).
    pub fn add_part(&mut self, offset: u64, data: &mut dyn Read) -> Result<Vec<u64>, FormatError> {
        self.check_alive()?;
        let r = self.add_part_inner(offset, data);
        self.note(r)
    }

    fn add_part_inner(
        &mut self,
        offset: u64,
        data: &mut dyn Read,
    ) -> Result<Vec<u64>, FormatError> {
        if self.open.is_none() {
            return Err(bad_options("no entry is open"));
        }
        let mut chunks = Vec::new();
        let len = self.chunk_stream(data, &mut chunks, false)?;
        if let Some(o) = &mut self.open {
            o.parts.push((offset, len, chunks.clone()));
        }
        Ok(chunks)
    }

    /// Add the part of the open entry that starts at `offset` as a block of
    /// its own whose encoding the caller made: the current block is closed,
    /// `plain` is cut into chunks (never deduplicated against an old table),
    /// and those chunks form one block written with `encoded`, whose graph is
    /// checked as an encoder's would be (including the records added so far
    /// and the archive's `version_minor`). `memory` is the decoder working
    /// memory the block needs beyond its plain buffer; the envelope's
    /// `decode_memory` is `max_block_plain` plus the largest such figure.
    /// `plain` must not be empty and at most `block_size` long (`BadOptions`,
    /// `encoded part length`). Returns the part's chunk indices.
    pub fn add_part_encoded(
        &mut self,
        offset: u64,
        plain: &[u8],
        encoded: Encoded,
        memory: u64,
    ) -> Result<Vec<u64>, FormatError> {
        self.check_alive()?;
        let r =
            self.add_parts_encoded_inner(&[(offset, plain.len() as u64)], plain, encoded, memory);
        self.note(r)
    }

    /// [`Writer::add_part_encoded`] for a block whose plain bytes lie in
    /// several parts of the open entry (a PNG's IDAT payloads): `spans` lists
    /// each part's `(offset, len)` in the file, sorted by offset, disjoint and
    /// none empty; `plain` is the concatenation of those parts' bytes, in that
    /// order. The chunker cuts each span on its own, so no chunk crosses a
    /// span boundary; the chunks of every span form one block written with
    /// `encoded`, checked and accounted exactly as in `add_part_encoded`
    /// (which is the one-span case). Each span becomes a part of the entry at
    /// its offset, so parts added before or after (the framing between the
    /// spans) interleave with them at [`Writer::end_entry`]. Refused
    /// (`BadOptions`): `plain` empty or longer than `block_size` (`encoded
    /// part length`); spans unsorted, overlapping, empty, or whose lengths do
    /// not add up to `plain.len()` (`encoded part spans`). Returns the
    /// block's chunk indices, span after span. Dedup of a multi-span stream
    /// is all or nothing: a later copy can be referenced (span by span, with
    /// [`Writer::part_known`] and [`Writer::add_part_known`]) only when every
    /// span's chunks are stored already; otherwise the whole stream is written
    /// again as a block of its own.
    pub fn add_parts_encoded(
        &mut self,
        spans: &[(u64, u64)],
        plain: &[u8],
        encoded: Encoded,
        memory: u64,
    ) -> Result<Vec<u64>, FormatError> {
        self.check_alive()?;
        let r = self.add_parts_encoded_inner(spans, plain, encoded, memory);
        self.note(r)
    }

    /// The spans of [`Writer::add_parts_encoded`] as ranges of `plain`.
    fn span_ranges(spans: &[(u64, u64)], plain_len: usize) -> Option<Vec<(usize, usize)>> {
        let mut out = Vec::with_capacity(spans.len());
        let mut start = 0usize;
        let mut end_of_last: Option<u64> = None;
        for &(offset, len) in spans {
            if len == 0 || end_of_last.is_some_and(|e| offset < e) {
                return None;
            }
            end_of_last = Some(offset.checked_add(len)?);
            let end = start.checked_add(usize::try_from(len).ok()?)?;
            out.push((start, end));
            start = end;
        }
        (!out.is_empty() && start == plain_len).then_some(out)
    }

    fn add_parts_encoded_inner(
        &mut self,
        spans: &[(u64, u64)],
        plain: &[u8],
        encoded: Encoded,
        memory: u64,
    ) -> Result<Vec<u64>, FormatError> {
        if self.open.is_none() {
            return Err(bad_options("no entry is open"));
        }
        if plain.is_empty() || plain.len() as u64 > self.options.block_size {
            return Err(bad_options("encoded part length"));
        }
        let ranges =
            Self::span_ranges(spans, plain.len()).ok_or(bad_options("encoded part spans"))?;
        self.flush_block()?;
        let mut chunks = Vec::new();
        let mut per_span = Vec::with_capacity(ranges.len());
        for &(s, e) in &ranges {
            let span = &plain[s..e];
            let first = chunks.len();
            for (a, b) in self.cut_all(span)? {
                let data = &span[a..b];
                chunks.push(self.records.len() as u64);
                self.records.push(ChunkRecord {
                    plain_len: data.len() as u64,
                    hash: *blake3::hash(data).as_bytes(),
                });
                self.put(data);
                self.pending_chunks += 1;
            }
            per_span.push(first..chunks.len());
        }
        self.flush_block_with(Some(encoded))?;
        if self.options.dedup {
            // Later files may reference these chunks (never a record's part: `find_known`
            // skips a reconstruction block's chunks for one).
            for &c in &chunks {
                if let Some(r) = usize::try_from(c).ok().and_then(|i| self.records.get(i)) {
                    self.own.entry(r.hash).or_insert(c);
                }
            }
        }
        self.extra_memory = self.extra_memory.max(memory);
        if let Some(o) = &mut self.open {
            for (&(offset, len), r) in spans.iter().zip(per_span) {
                o.parts.push((offset, len, chunks[r].to_vec()));
            }
        }
        Ok(chunks)
    }

    /// Finish the open entry: its parts, sorted by offset, must cover the file
    /// from 0 without a gap or an overlap (`BadOptions`, `entry parts`); the
    /// entry's chunk list is theirs in that order and its size their sum.
    pub fn end_entry(&mut self) -> Result<(), FormatError> {
        self.check_alive()?;
        let r = self.end_entry_inner();
        self.note(r)
    }

    fn end_entry_inner(&mut self) -> Result<(), FormatError> {
        let Some(OpenEntry {
            mut entry,
            mut parts,
        }) = self.open.take()
        else {
            return Err(bad_options("no entry is open"));
        };
        parts.sort_by_key(|p| p.0);
        let mut at = 0u64;
        for (offset, len, chunks) in parts {
            if offset != at {
                return Err(bad_options("entry parts"));
            }
            at = at.checked_add(len).ok_or(bad_options("entry parts"))?;
            entry.chunks.extend(chunks);
        }
        entry.size = at;
        self.push_entry(entry);
        Ok(())
    }

    /// Add a reconstruction record while writing; returns its id, its
    /// position in the `Records` frame after the records of
    /// `WriterOptions::records` (in an append, after the old records: the
    /// frame this generation writes restates them at their positions, as old
    /// blocks name records by position). A block closed from now on may name
    /// it. The record is checked as `WriterOptions::records` are (`BadOptions`
    /// `record kind`, or the record's parse error).
    pub fn add_record(&mut self, record: Record) -> Result<u64, FormatError> {
        self.check_alive()?;
        let id = self.options.records.len() as u64;
        check_record(&record, id)?;
        self.options.records.push(record);
        self.record_count = id + 1;
        self.write_records = true;
        Ok(id)
    }

    /// The header's `version_minor` of this archive.
    pub fn version_minor(&self) -> u16 {
        self.version_minor
    }

    /// The archive's position: the bytes written (or handed to the write thread) so far.
    /// A figure for progress reports; it changes nothing.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Blocks closed so far, the blocks in flight included. A figure for progress reports;
    /// it changes nothing.
    pub fn blocks_closed(&self) -> u64 {
        (self.blocks.len() + self.queue.len()) as u64
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
        let mut chunks = Vec::new();
        entry.size = self.chunk_stream(data, &mut chunks, true)?;
        entry.chunks = chunks;
        self.push_entry(entry);
        Ok(())
    }

    /// Read `data` to the end and cut it into chunks appended to the current
    /// block (their indices pushed to `chunks`); returns the bytes read.
    fn chunk_stream(
        &mut self,
        data: &mut dyn Read,
        chunks: &mut Vec<u64>,
        allow_recon: bool,
    ) -> Result<u64, FormatError> {
        let chunk_size = self.options.chunk_size as usize;
        let mut size = 0u64;
        // Read a block's worth at a time: besides the pending block, at most
        // the held-back tail plus one segment is in memory.
        let per_segment = self.options.block_size;
        self.chunker.reset();
        // The held-back tail, then the newly read bytes; kept across files so
        // its pages are touched once per writer, not once per file.
        let mut buf: Vec<u8> = std::mem::take(&mut self.read_buf);
        buf.clear();
        loop {
            let before = buf.len();
            let n = Read::take(&mut *data, per_segment).read_to_end(&mut buf)?;
            let eof = (n as u64) < per_segment;
            if !eof && self.pending.capacity() < self.options.block_size as usize {
                // A whole segment read: the input fills blocks, so the pending
                // block takes its full size at once instead of growing by
                // doubling (each step a copy into untouched memory).
                self.pending.truncate(self.pending_len);
                let want = self.options.block_size as usize - self.pending_len;
                self.pending.reserve_exact(want);
            }
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
                let i = self.add_chunk(&buf[start..cut], allow_recon)?;
                chunks.push(i);
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
            size += n as u64;
            if eof {
                break;
            }
        }
        self.read_buf = buf;
        Ok(size)
    }

    /// Add a regular file whose bytes come from `data` segment by segment (a
    /// read-ahead, [`crate::prefetch::Prefetch`]): cut, hashed and appended to
    /// the current block exactly as [`Writer::add_file`] does with the same
    /// bytes (the chunker's cuts do not depend on how the bytes are split;
    /// see [`Chunker`]), so the archive is the same byte for byte. `len_hint`
    /// is the file's expected length (it only sizes the pending block). Every
    /// segment goes back to `data` once its bytes are copied. Errors as in
    /// [`Writer::add_file`]; a reader thread's panic marks the writer failed
    /// and is re-raised.
    pub fn add_file_segments(
        &mut self,
        path: &str,
        flags: EntryFlags,
        mtime_ns: i64,
        data: &mut dyn Segments,
        len_hint: u64,
    ) -> Result<(), FormatError> {
        self.check_alive()?;
        self.no_open_entry()?;
        let r = self.add_file_segments_inner(path, flags, mtime_ns, data, len_hint);
        self.note(r)
    }

    fn add_file_segments_inner(
        &mut self,
        path: &str,
        flags: EntryFlags,
        mtime_ns: i64,
        data: &mut dyn Segments,
        len_hint: u64,
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
        let mut chunks = Vec::new();
        entry.size = self.chunk_segments(data, &mut chunks, len_hint)?;
        entry.chunks = chunks;
        self.push_entry(entry);
        Ok(())
    }

    /// [`Writer::chunk_stream`] over segments: the chunker is fed each segment
    /// in place; its held-back tail (at most `chunk_size`) is the only copy
    /// besides the pending block, and a chunk that spans it and the next
    /// segment is hashed and appended in two pieces.
    fn chunk_segments(
        &mut self,
        data: &mut dyn Segments,
        chunks: &mut Vec<u64>,
        len_hint: u64,
    ) -> Result<u64, FormatError> {
        let chunk_size = self.options.chunk_size as usize;
        let block_size = self.options.block_size as usize;
        if self.pending.capacity() < block_size
            && len_hint >= (block_size - self.pending_len).min(RECYCLE_MIN) as u64
        {
            // A file of a block's remainder, or of at least a pool buffer's
            // size: the pending block takes its full size at once instead of
            // growing by doubling (each step a copy into untouched memory). A
            // zeroed allocation: its pages are first touched by the copies
            // into it, on the hash threads.
            let mut full = vec![0u8; block_size];
            full[..self.pending_len].copy_from_slice(&self.pending[..self.pending_len]);
            self.pending = full;
        }
        self.chunker.reset();
        let mut carry: Vec<u8> = std::mem::take(&mut self.read_buf);
        carry.clear();
        let mut size = 0u64;
        loop {
            let waiting = profile::Timer::start(profile::Stage::FeedWait);
            let seg = match data.next_segment() {
                Ok(s) => s,
                Err(SegmentError::Io(e)) => return Err(FormatError::Io(e)),
                Err(SegmentError::Panicked(p)) => {
                    self.failed = Some((
                        std::io::ErrorKind::Other,
                        "the read-ahead thread panicked".to_string(),
                    ));
                    std::panic::resume_unwind(p);
                }
            };
            drop(waiting);
            // The end is fed with the last bytes when the reader knew them to be
            // the last (as `chunk_stream` does after a short read), else alone.
            // Only a buffer the reader handed out goes back to it.
            let from_reader = seg.is_some();
            let (seg, eof) = seg.unwrap_or((Vec::new(), true));
            size += seg.len() as u64;
            let mut cutting = profile::Timer::start(profile::Stage::Cut);
            cutting.bytes = seg.len() as u64;
            let cuts = self.chunker.feed(&seg, eof);
            drop(cutting);
            let held = carry.len();
            let total = held + seg.len();
            let mut ranges = Vec::with_capacity(cuts.len());
            let mut start = 0usize;
            for cut in cuts {
                if cut <= start || cut > total {
                    return Err(FormatError::BadChunk {
                        reason: "cut positions",
                    });
                }
                if cut - start > chunk_size {
                    return Err(FormatError::BadChunk {
                        reason: "chunk longer than chunk_size",
                    });
                }
                ranges.push((start, cut));
                start = cut;
            }
            if eof && start != total {
                return Err(FormatError::BadChunk {
                    reason: "bytes left uncut at eof",
                });
            }
            if total - start > chunk_size {
                return Err(FormatError::BadChunk {
                    reason: "held back more than chunk_size",
                });
            }
            // The two pieces of each chunk: in the carry, then in the segment.
            let bytes = Two {
                carry: &carry,
                seg: &seg,
            };
            let cut_bytes = start as u64;
            // With blocks in flight, the leading chunks that fit the current
            // block whatever the lookups say are copied into place by the hash
            // threads along with their hashes (one pass over the bytes, off
            // this thread); the lookups then confirm each in order, and from
            // the first chunk already stored (then the copies are stale) the
            // rest take the in-order path, which overwrites them.
            let threads = self.threads();
            let room = block_size - self.pending_len;
            let ahead = if threads > 1 {
                ranges.iter().take_while(|&&(_, b)| b <= room).count()
            } else {
                0
            };
            let ahead_end = ahead.checked_sub(1).map_or(0, |i| ranges[i].1);
            let mut hashing = profile::Timer::start(profile::Stage::Hash);
            hashing.bytes = cut_bytes;
            let hashed = if ahead_end > 0 {
                let fill = self.pending_len;
                if self.pending.len() < fill + ahead_end {
                    if self.pending.capacity() < block_size && fill + ahead_end >= RECYCLE_MIN {
                        // Not a pool buffer (none had come back when the last
                        // block closed): a full block at once, zeroed by the
                        // allocator, rather than grown by doubling — each
                        // step a copy and a fill of untouched memory.
                        let mut full = vec![0u8; block_size];
                        full[..fill].copy_from_slice(&self.pending[..fill]);
                        self.pending = full;
                    } else {
                        // A pool buffer (cleared when recycled): only what
                        // this segment writes, never the whole capacity.
                        self.pending.resize(fill + ahead_end, 0);
                    }
                }
                let dst = &mut self.pending[fill..fill + ahead_end];
                hash_and_copy(bytes, &ranges, threads, hash_fn(), dst)
            } else {
                hash_ranges(bytes, &ranges, threads, hash_fn())
            };
            let hashes = self.hashed(hashed);
            drop(hashing);
            let mut adding = profile::Timer::start(profile::Stage::Add);
            adding.bytes = cut_bytes;
            let mut in_place = ahead;
            for (j, (&(a, b), hash)) in ranges.iter().zip(hashes).enumerate() {
                if j < in_place {
                    let len = (b - a) as u64;
                    match self.find_known(&hash, len, true)? {
                        Some((i, old)) => {
                            self.note_known(len, old);
                            chunks.push(i);
                            in_place = 0;
                        }
                        None => {
                            // Fits by construction (`room`): no block close here.
                            let i = self.note_record(hash, len)?;
                            self.pending_len += b - a;
                            chunks.push(i);
                        }
                    }
                    continue;
                }
                let (head, tail) = bytes.piece(a, b);
                let i = self.add_chunk_hashed(head, tail, hash, true)?;
                chunks.push(i);
            }
            drop(adding);
            if let Some((_, fed)) = &mut self.profile {
                *fed += seg.len() as u64;
            }
            // The uncut rest becomes the carry.
            if start >= held {
                carry.clear();
                carry.extend_from_slice(&seg[start - held..]);
            } else {
                carry.drain(..start);
                carry.extend_from_slice(&seg);
            }
            if from_reader {
                data.give_back(seg);
            }
            if eof {
                break;
            }
        }
        // The end of the file after its last segment.
        if !matches!(data.next_segment(), Ok(None)) {
            return Err(FormatError::Io(std::io::Error::other(
                "read-ahead: bytes after the last segment",
            )));
        }
        self.read_buf = carry;
        Ok(size)
    }

    /// The BLAKE3 of each chunk `ranges` names in `bytes`, in order: on this
    /// thread with one block in flight, else on up to
    /// [`MAX_HASH_THREADS`] scoped threads that are joined
    /// before this returns. A hashing thread's panic marks the writer failed
    /// and is re-raised here.
    /// The hashes a segment's hash threads computed; a thread's panic marks
    /// the writer failed and is re-raised here.
    fn hashed(&mut self, r: Result<Vec<[u8; 32]>, crate::chunkhash::Panic>) -> Vec<[u8; 32]> {
        match r {
            Ok(h) => h,
            Err(p) => {
                self.failed = Some((
                    std::io::ErrorKind::Other,
                    "a chunk hashing thread panicked".to_string(),
                ));
                std::panic::resume_unwind(p)
            }
        }
    }

    /// Add a directory.
    pub fn add_directory(
        &mut self,
        path: &str,
        flags: EntryFlags,
        mtime_ns: i64,
    ) -> Result<(), FormatError> {
        self.no_open_entry()?;
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
        self.no_open_entry()?;
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
        self.drain()?;
        self.entries = self.merged_entries();
        let table = EntryTableWriter::encode(&self.entries)?;
        let entry_table = self.write_frame(FrameKind::EntryTable, table)?;
        let entry_hash = self.last_hash;
        let records = if self.write_records {
            let payload = RecordsWriter::encode(&self.options.records);
            Some(self.write_frame(FrameKind::Records, payload)?)
        } else {
            // An append that added no record keeps the old records frame.
            self.base.as_ref().and_then(|b| b.records)
        };
        Ok((entry_table, entry_hash, records))
    }

    /// Close the last block and write the entry table, the recovery frames, the
    /// index and the trailer.
    pub fn finish(mut self) -> Result<WriterSummary, FormatError> {
        let r = self.finish_inner();
        if let Some((started, fed)) = self.profile {
            eprint!("{}", profile::table(started.elapsed(), fed));
        }
        r
    }

    fn finish_inner(&mut self) -> Result<WriterSummary, FormatError> {
        let _finishing = profile::Timer::start(profile::Stage::Finish);
        self.check_alive()?;
        self.no_open_entry()?;
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
        let threads_hint = u32::try_from(self.blocks.len())
            .unwrap_or(u32::MAX)
            .min(self.options.threads_hint);
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
                max_plain.saturating_add(self.extra_memory),
                threads_hint,
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
            deduped_chunks: self.deduped_chunks,
            deduped_bytes: self.deduped_bytes,
        })
    }
}

/// The chunk hash function of the segment path: [`hash_two`] outside tests.
fn hash_fn() -> &'static HashFn<'static> {
    #[cfg(test)]
    if crate::chunkhash::HASH_FAULT.with(std::cell::Cell::get) {
        return &crate::chunkhash::faulty_hash;
    }
    &hash_two
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

impl<W: Write + Send + 'static> Writer<W> {
    /// Move the output onto a write thread, so writing overlaps the cut: the
    /// writer decides every frame's position, sequence number and hash in order
    /// as before and hands the frame's bytes over (a block buffer by value)
    /// through a bounded queue ([`WRITE_DEPTH`] frames beyond the one being
    /// written); the archive is the same byte for byte. A failed write fails
    /// the next call of this writer and abandons the archive, as a failed write
    /// on this thread does; [`Writer::finish`] waits for every byte to be
    /// written and flushed before it syncs (D-59) and writes the trailer. The
    /// thread is joined when the writer is dropped; its panic is re-raised by
    /// the call that finds it, once the writer is marked failed. When the
    /// thread cannot be started the writer writes on this thread.
    pub fn with_write_thread(mut self) -> Self {
        self.out.inner.start_thread();
        self
    }
}

impl<W: Write> Writer<W> {
    /// True when a write thread writes the output ([`Writer::with_write_thread`]).
    pub fn has_write_thread(&self) -> bool {
        matches!(self.out.inner, Sink::Thread(_))
    }
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

#[cfg(test)]
mod record_rule_tests {
    //! The reader-side refusals of the record chunk rules (spec section 12),
    //! through archives the writer refuses unless told not to check.
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::decode::{DecodeContext, PrimitiveDecoder};
    use crate::envelope::Resources;
    use crate::record::{JpegRecord, RecordBody};
    use std::io::Cursor;

    /// A stand-in `jpeg-reconstruct`: the stream is the primary reversed; it
    /// reads every nested trailing chunk through the context.
    struct Reverse;

    impl PrimitiveDecoder for Reverse {
        fn decode(
            &self,
            _: &[u8],
            _: &[u8],
            _: u64,
            _: &Resources,
        ) -> Result<Vec<u8>, FormatError> {
            Err(FormatError::UnimplementedPrimitive { id: 7 })
        }

        fn decode_in(
            &self,
            params: &[u8],
            input: &[u8],
            _: u64,
            _: bool,
            _: &Resources,
            ctx: &mut dyn DecodeContext,
        ) -> Result<Vec<u8>, FormatError> {
            let id = u64::from(params[0]);
            let RecordBody::Jpeg(j) = ctx.record(id)?.body else {
                return Err(FormatError::UnimplementedPrimitive { id: 7 });
            };
            for &c in &j.nested_trailing_chunks {
                ctx.chunk(id, c)?;
            }
            Ok(input.iter().rev().copied().collect())
        }
    }

    fn options() -> WriterOptions {
        WriterOptions {
            chunk_size: 4096,
            block_size: 16384,
            ..WriterOptions::default()
        }
    }

    fn record(original_len: u64, primary_len: u64, chunks: Vec<u64>) -> Record {
        Record::new(RecordBody::Jpeg(JpegRecord {
            original_len,
            primary_len,
            trailing: Vec::new(),
            nested_trailing_chunks: chunks,
            gainmaps: Vec::new(),
            lepton_version: 0,
            original_hash: [0; 32],
        }))
    }

    fn encoded(record: u8, primary: &[u8]) -> Encoded {
        Encoded {
            graph: Graph {
                steps: vec![Step {
                    primitive: PrimitiveId::JpegReconstruct,
                    params: vec![record],
                }],
            },
            bytes: primary.iter().rev().copied().collect(),
            resources: GraphResources::default(),
            memory: 0,
        }
    }

    /// Two peeled files; the second's record names `second_chunks`. File 1's
    /// primary is chunk 0 (block 0, a reconstruction block); file 2's primary
    /// is chunk 1 (block 1); a trailing part after it is chunk 2 (block 2).
    fn archive(second_chunks: Vec<u64>, checked: bool) -> Result<Vec<u8>, FormatError> {
        let p = vec![7u8; 1000];
        let mut out = Vec::new();
        let mut w = Writer::new_revision(&mut out, options(), 1)?;
        w.unchecked_records = !checked;
        w.begin_entry("a", EntryFlags::EMPTY, 0)?;
        let id = w.add_record(record(1000, 1000, Vec::new()))?;
        w.add_part_encoded(0, &p, encoded(id as u8, &p), 0)?;
        w.end_entry()?;
        w.begin_entry("b", EntryFlags::EMPTY, 0)?;
        let rest = if second_chunks == [0] { 1000 } else { 100 };
        let id = w.add_record(record(1000 + rest, 1000, second_chunks))?;
        w.add_part_encoded(0, &p, encoded(id as u8, &p), 0)?;
        w.add_part(1000, &mut &[1u8; 100][..])?;
        w.end_entry()?;
        w.finish()?;
        Ok(out)
    }

    fn reader_error(bytes: Vec<u8>) -> FormatError {
        let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        a.registry_mut()
            .register(PrimitiveId::JpegReconstruct, Box::new(Reverse));
        a.verify().unwrap_err()
    }

    #[test]
    fn a_later_chunk_is_refused_by_the_writer_and_the_reader() {
        assert!(matches!(
            archive(vec![2], true),
            Err(FormatError::BadRecord {
                record: 1,
                reason: "chunk order"
            })
        ));
        assert!(matches!(
            reader_error(archive(vec![2], false).unwrap()),
            FormatError::BadRecord {
                record: 1,
                reason: "chunk order"
            }
        ));
    }

    #[test]
    fn a_chunk_in_a_reconstruction_block_is_refused_by_the_writer_and_the_reader() {
        assert!(matches!(
            archive(vec![0], true),
            Err(FormatError::BadRecord {
                record: 1,
                reason: "nested record"
            })
        ));
        assert!(matches!(
            reader_error(archive(vec![0], false).unwrap()),
            FormatError::BadRecord {
                record: 1,
                reason: "nested record"
            }
        ));
    }

    fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut v = Vec::with_capacity(len + 8);
        while v.len() < len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.truncate(len);
        v
    }

    /// The store path over incompressible files, a repeated (deduplicated) one and a part
    /// whose encoding the caller gives, with blocks large enough for the pool's frame hash
    /// to split across threads.
    fn store_path(threads: usize, corrupt: bool) -> Result<Vec<u8>, FormatError> {
        store_path_with(threads, corrupt, 0)
    }

    /// [`store_path`] with unsealed recovery frames interleaved (`recovery` percent).
    fn store_path_with(
        threads: usize,
        corrupt: bool,
        recovery: u8,
    ) -> Result<Vec<u8>, FormatError> {
        let mut out = Vec::new();
        let opts = WriterOptions {
            chunk_size: 1 << 20,
            block_size: 12 << 20,
            dedup: true,
            encode_threads: threads,
            recovery: RecoveryOptions {
                percent: recovery,
                ..RecoveryOptions::default()
            },
            ..WriterOptions::default()
        };
        let mut w = Writer::new(&mut out, opts)?;
        w.corrupt_pool_hash = corrupt;
        let big = noise(1, 30 << 20);
        w.add_file("a.bin", EntryFlags::default(), 0, &mut &big[..])?;
        w.add_file(
            "b.bin",
            EntryFlags::default(),
            0,
            &mut &noise(2, 3 << 20)[..],
        )?;
        w.add_file("c.bin", EntryFlags::default(), 0, &mut &big[..5 << 20])?;
        w.close_block()?;
        w.add_file("d.txt", EntryFlags::default(), 0, &mut &[7u8; 100_000][..])?;
        // A given block, written in order with the pool's and hashed by the writer.
        let given = noise(4, 2 << 20);
        w.begin_entry("e.bin", EntryFlags::default(), 0)?;
        w.add_part_encoded(0, &given, StoreEncoder::wrap(given.clone()), 0)?;
        w.end_entry()?;
        w.add_file(
            "f.bin",
            EntryFlags::default(),
            0,
            &mut &noise(5, 9 << 20)[..],
        )?;
        w.finish()?;
        Ok(out)
    }

    /// Byte identity of the store path at one thread (the writer hashes every frame) and
    /// at N (the pool threads hash them, in parallel subtrees): the same archive, and in
    /// debug builds the writer re-checks every handed-back hash against its own.
    #[test]
    fn the_store_path_writes_the_same_bytes_at_one_thread_and_n() {
        let one = store_path(1, false).unwrap();
        for threads in [2usize, 4, 24] {
            assert!(
                one == store_path(threads, false).unwrap(),
                "threads {threads}"
            );
        }
        let mut a = Archive::open(Cursor::new(one), &Resources::default()).unwrap();
        a.verify().unwrap();
        // Unsealed recovery frames interleaved: the pool hash still applies.
        let one = store_path_with(1, false, 5).unwrap();
        for threads in [4usize, 24] {
            assert!(
                one == store_path_with(threads, false, 5).unwrap(),
                "recovery, threads {threads}"
            );
        }
        let mut a = Archive::open(Cursor::new(one), &Resources::default()).unwrap();
        a.verify().unwrap();
    }

    /// A pool thread that hands back a wrong frame hash: the writer refuses it before the
    /// frame is written and abandons the archive (debug builds check every handed-back hash).
    #[test]
    #[cfg(debug_assertions)]
    fn a_wrong_pool_frame_hash_is_refused() {
        let e = store_path(4, true).unwrap_err();
        assert!(
            matches!(
                e,
                FormatError::BadOptions {
                    reason: "pool frame hash"
                }
            ),
            "{e:?}"
        );
        // Without a pool (one thread) the writer hashes itself: nothing to refuse.
        store_path(1, true).unwrap();
    }

    /// The identity encoder moves the plain buffer into the encoding (no copy) on its own
    /// thread and as a job.
    #[test]
    fn the_identity_encoder_takes_the_plain_buffer() {
        let plain = noise(3, 4096);
        let at = plain.as_ptr();
        let copy = plain.clone();
        let copy_at = copy.as_ptr();
        let (r, back) = StoreEncoder.encode_owned(copy);
        assert!(back.is_none());
        let bytes = r.unwrap().bytes;
        assert_eq!(bytes.as_ptr(), copy_at);
        assert_eq!(bytes, plain);
        let (r, back) = Box::new(StoreEncoder).run_owned(plain);
        assert!(back.is_none());
        let bytes = r.unwrap().0.bytes;
        assert_eq!(bytes.as_ptr(), at);
    }
}

#[cfg(test)]
mod deflate_rule_tests {
    //! Revision 1.2's writer rules for `deflate-reconstruct`: the minor, and
    //! at block close what the reader's checks 2 to 4 refuse.
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::envelope::Resources;
    use crate::record::{DeflateRecord, JpegRecord};
    use std::io::Cursor;

    fn options() -> WriterOptions {
        WriterOptions {
            chunk_size: 4096,
            block_size: 16384,
            ..WriterOptions::default()
        }
    }

    fn deflate(original_len: u64, corrections: usize) -> Record {
        Record::new(RecordBody::Deflate(DeflateRecord {
            original_len,
            plain_len: 10,
            corrections: vec![1; corrections],
            library: 0,
            original_hash: [0; 32],
        }))
    }

    fn jpeg() -> Record {
        Record::new(RecordBody::Jpeg(JpegRecord {
            original_len: 1000,
            primary_len: 1000,
            trailing: Vec::new(),
            nested_trailing_chunks: Vec::new(),
            gainmaps: Vec::new(),
            lepton_version: 0,
            original_hash: [0; 32],
        }))
    }

    /// A one-step graph `[deflate-reconstruct {id}]` over ten input bytes.
    fn encoded(id: u64) -> Encoded {
        let mut params = Vec::new();
        varint::write(&mut params, id).unwrap();
        Encoded {
            graph: Graph {
                steps: vec![Step {
                    primitive: PrimitiveId::DeflateReconstruct,
                    params,
                }],
            },
            bytes: vec![0; 10],
            resources: GraphResources::default(),
            memory: 0,
        }
    }

    /// An archive of minor `minor` with one 1000-byte part through `record`.
    fn write(minor: u16, record: Record) -> Result<Vec<u8>, FormatError> {
        let p = vec![7u8; 1000];
        let mut out = Vec::new();
        let mut w = Writer::new_revision(&mut out, options(), minor)?;
        w.begin_entry("a", EntryFlags::EMPTY, 0)?;
        let id = w.add_record(record)?;
        w.add_part_encoded(0, &p, encoded(id), 0)?;
        w.end_entry()?;
        w.add_file("readme.txt", EntryFlags::EMPTY, 0, &mut &b"plain"[..])?;
        w.finish()?;
        Ok(out)
    }

    #[test]
    fn deflate_reconstruct_is_revision_2_and_bwt_revision_3() {
        assert_eq!(PrimitiveId::DeflateReconstruct.revision(), 2);
        assert_eq!(PrimitiveId::Bwt.revision(), 3);
        assert_eq!(FormatVersion::LATEST.minor, 4);
    }

    #[test]
    fn minor_below_2_refuses_the_primitive() {
        for minor in [0, 1] {
            assert!(
                matches!(
                    write(minor, deflate(1000, 4)),
                    Err(FormatError::BadOptions {
                        reason: "version_minor"
                    })
                ),
                "minor {minor}"
            );
        }
        assert!(write(2, deflate(1000, 4)).is_ok());
    }

    #[test]
    fn block_close_refuses_what_checks_2_to_4_refuse() {
        assert!(matches!(
            write(2, jpeg()),
            Err(FormatError::BadRecord {
                record: 0,
                reason: "kind"
            })
        ));
        assert!(matches!(
            write(2, deflate(999, 4)),
            Err(FormatError::BlockLengthMismatch { block: 0 })
        ));
        assert!(matches!(
            write(2, deflate(1000, 1001)),
            Err(FormatError::BadRecord {
                record: 0,
                reason: "corrections"
            })
        ));
        // Corrections exactly `original_len` long are allowed.
        assert!(write(2, deflate(1000, 1000)).is_ok());
    }

    #[test]
    fn block_close_refuses_deflate_reconstruct_before_the_last_step() {
        let p = vec![7u8; 1000];
        let mut out = Vec::new();
        let mut w = Writer::new_revision(&mut out, options(), 2).unwrap();
        w.begin_entry("a", EntryFlags::EMPTY, 0).unwrap();
        let id = w.add_record(deflate(1000, 4)).unwrap();
        let mut e = encoded(id);
        e.graph.steps.push(Step {
            primitive: PrimitiveId::Store,
            params: Vec::new(),
        });
        let r = w.add_part_encoded(0, &p, e, 0).and_then(|_| w.end_entry());
        let r = r.and_then(|()| w.close_block());
        assert!(
            matches!(
                r,
                Err(FormatError::BadGraph {
                    reason: "deflate-reconstruct not last"
                })
            ),
            "{r:?}"
        );
    }

    /// lpk-format alone (the minimal reader) does not decode the primitive: it
    /// opens the minor-2 archive and its verification reports
    /// `UnimplementedPrimitive` (8) for the block that names it.
    #[test]
    fn the_minimal_reader_reports_unimplemented_primitive_8() {
        let bytes = write(2, deflate(1000, 4)).unwrap();
        let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        assert_eq!(a.header().version.minor, 2);
        assert!(matches!(
            a.verify().unwrap_err(),
            FormatError::UnimplementedPrimitive { id: 8 }
        ));
    }
}

#[cfg(test)]
mod undecoded_rule_tests {
    //! The writer refuses, at every minor, a primitive no revision decodes
    //! (spec section 8: a writer MUST NOT emit them).
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::record::JpegRecord;

    fn options() -> WriterOptions {
        WriterOptions {
            chunk_size: 4096,
            block_size: 16384,
            // Record 0, so a reconstruction step naming it passes the record
            // check and reaches the rule.
            records: vec![Record::new(RecordBody::Jpeg(JpegRecord {
                original_len: 1000,
                primary_len: 1000,
                trailing: Vec::new(),
                nested_trailing_chunks: Vec::new(),
                gainmaps: Vec::new(),
                lepton_version: 0,
                original_hash: [0; 32],
            }))],
            ..WriterOptions::default()
        }
    }

    /// The primitives no revision decodes, each with valid parameters.
    fn undecoded() -> Vec<Step> {
        vec![
            Step {
                primitive: PrimitiveId::BcjX86,
                params: Vec::new(),
            },
            Step {
                primitive: PrimitiveId::BcjArm64,
                params: Vec::new(),
            },
            Step {
                primitive: PrimitiveId::Delta,
                params: vec![0; 9],
            },
            Step {
                primitive: PrimitiveId::Base64,
                params: vec![0],
            },
            Step {
                primitive: PrimitiveId::Utf16,
                params: vec![0],
            },
            Step {
                primitive: PrimitiveId::ContainerReconstruct,
                params: vec![0],
            },
        ]
    }

    /// An archive of minor `minor` with one 1000-byte part encoded by the
    /// graph `steps` (an identity: the bytes are the part).
    fn write(minor: u16, steps: Vec<Step>) -> Result<Vec<u8>, FormatError> {
        let p = vec![7u8; 1000];
        let mut out = Vec::new();
        let mut w = Writer::new_revision(&mut out, options(), minor)?;
        w.begin_entry("a", EntryFlags::EMPTY, 0)?;
        let e = Encoded {
            graph: Graph { steps },
            bytes: p.clone(),
            resources: GraphResources::default(),
            memory: 0,
        };
        w.add_part_encoded(0, &p, e, 0)?;
        w.end_entry()?;
        w.finish()?;
        Ok(out)
    }

    #[test]
    fn the_rule_names_every_undecoded_primitive_and_no_other() {
        for p in PrimitiveId::ALL {
            let listed = undecoded().iter().any(|s| s.primitive == p);
            assert_eq!(!p.is_decoded(), listed, "{p:?}");
        }
    }

    #[test]
    fn each_undecoded_primitive_is_refused_at_minors_0_to_4() {
        for minor in [0u16, 1, 2, 3, 4] {
            for step in undecoded() {
                let p = step.primitive;
                // Alone, and after a decoded step.
                for steps in [
                    vec![step.clone()],
                    vec![
                        Step {
                            primitive: PrimitiveId::Store,
                            params: Vec::new(),
                        },
                        step.clone(),
                    ],
                ] {
                    let r = write(minor, steps);
                    assert!(
                        matches!(
                            r,
                            Err(FormatError::BadGraph {
                                reason: "undecoded primitive"
                            })
                        ),
                        "{p:?} at minor {minor}: {r:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_decoded_graph_still_writes() {
        let store = Step {
            primitive: PrimitiveId::Store,
            params: Vec::new(),
        };
        for minor in [0u16, 1, 2, 3, 4] {
            assert!(write(minor, vec![store.clone()]).is_ok(), "minor {minor}");
        }
    }

    #[test]
    fn the_record_check_comes_before_the_rule() {
        // A missing record is reported first (`RecordOutOfRange`), as the
        // reader would report it.
        let r = write(
            2,
            vec![Step {
                primitive: PrimitiveId::Utf16,
                params: vec![5],
            }],
        );
        assert!(
            matches!(
                r,
                Err(FormatError::RecordOutOfRange {
                    record: 5,
                    count: 1
                })
            ),
            "{r:?}"
        );
    }
}

#[cfg(test)]
mod bwt_rule_tests {
    //! Revision 1.3's position rule for `bwt`, checked on the function itself
    //! and through the writer's graph check.
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn st(p: PrimitiveId) -> Step {
        let params = match p {
            PrimitiveId::Bwt => vec![0, 0, 1, 0],
            PrimitiveId::Lzma => vec![0, 0, 1, 0, 3, 0, 2],
            PrimitiveId::DeflateReconstruct | PrimitiveId::PngFilter => vec![0],
            _ => Vec::new(),
        };
        Step {
            primitive: p,
            params,
        }
    }

    fn graph(ps: &[PrimitiveId]) -> Graph {
        Graph {
            steps: ps.iter().map(|&p| st(p)).collect(),
        }
    }

    /// Revision 1.4: `png-filter` only directly before a last
    /// `deflate-reconstruct`, and only under minor 4.
    #[test]
    fn png_filter_position_and_minor() {
        use PrimitiveId::*;
        for ps in [
            &[PngFilter][..],
            &[Lzma, PngFilter],
            &[PngFilter, Lzma, DeflateReconstruct],
            &[Lzma, PngFilter, Store],
            &[Lzma, PngFilter, DeflateReconstruct, Store],
        ] {
            assert!(
                matches!(
                    check_graph(&graph(ps), 1, 4),
                    Err(FormatError::BadGraph {
                        reason: "png-filter not before deflate-reconstruct"
                    })
                ),
                "{ps:?}"
            );
        }
        for ps in [
            &[PngFilter, DeflateReconstruct][..],
            &[Lzma, PngFilter, DeflateReconstruct],
        ] {
            for minor in [0u16, 1, 2, 3] {
                assert!(matches!(
                    check_graph(&graph(ps), 1, minor),
                    Err(FormatError::BadOptions {
                        reason: "version_minor"
                    })
                ));
            }
            assert!(check_graph(&graph(ps), 1, 4).is_ok(), "{ps:?}");
        }
    }

    #[test]
    fn bwt_last_or_before_a_last_deflate_reconstruct_passes() {
        use PrimitiveId::*;
        for ps in [
            &[Bwt][..],
            &[Lzma, Bwt],
            &[Store, Lzma, Bwt],
            &[Bwt, DeflateReconstruct],
            &[Lzma, Bwt, DeflateReconstruct],
            // No bwt at all.
            &[Lzma],
            &[Lzma, DeflateReconstruct],
        ] {
            assert!(check_bwt_steps(&graph(ps)).is_ok(), "{ps:?}");
        }
    }

    #[test]
    fn bwt_anywhere_else_is_bwt_position() {
        use PrimitiveId::*;
        for ps in [
            &[Bwt, Lzma][..],
            &[Bwt, Store],
            &[Lzma, Bwt, Store],
            &[Bwt, DeflateReconstruct, Store],
            &[Bwt, Lzma, DeflateReconstruct],
            &[Bwt, Bwt],
            &[Bwt, Store, DeflateReconstruct],
        ] {
            assert!(
                matches!(
                    check_bwt_steps(&graph(ps)),
                    Err(FormatError::BadGraph {
                        reason: "bwt position"
                    })
                ),
                "{ps:?}"
            );
        }
    }

    #[test]
    fn through_the_graph_check_bwt_needs_minor_3_and_its_position() {
        use PrimitiveId::*;
        // A misplaced `bwt` is `bwt position` at the latest minor.
        for ps in [&[Bwt, Lzma][..], &[Bwt, Store], &[Lzma, Bwt, Store]] {
            assert!(matches!(
                check_graph(&graph(ps), 1, FormatVersion::LATEST.minor),
                Err(FormatError::BadGraph {
                    reason: "bwt position"
                })
            ));
        }
        // Well placed: refused under minors 0 to 2 (`version_minor`), written under 3.
        for ps in [&[Bwt][..], &[Lzma, Bwt], &[Lzma, Bwt, DeflateReconstruct]] {
            for minor in [0u16, 1, 2] {
                assert!(
                    matches!(
                        check_graph(&graph(ps), 1, minor),
                        Err(FormatError::BadOptions {
                            reason: "version_minor"
                        })
                    ),
                    "{ps:?} at minor {minor}"
                );
            }
            assert!(check_graph(&graph(ps), 1, 3).is_ok(), "{ps:?}");
        }
        // The position rule comes before the minor rule.
        assert!(matches!(
            check_graph(&graph(&[Bwt, Lzma]), 1, 0),
            Err(FormatError::BadGraph {
                reason: "bwt position"
            })
        ));
    }
}

#[cfg(test)]
#[path = "pipe_tests.rs"]
mod pipe_tests;
