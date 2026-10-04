//! Writing an archive as a stream (spec section 9): header, `ChunkData` blocks,
//! entry table, index, trailer. The writer never seeks and never reads back.

use crate::chunk::{ChunkRecord, ChunkTableWriter};
use crate::entry::{check_entry, Entry, EntryFlags, EntryKind, EntryTableWriter};
use crate::envelope::{ArchiveSizes, Envelope};
use crate::error::FormatError;
use crate::frame::{Frame, FrameFlags, FrameKind};
use crate::graph::{BlockHeader, Graph, Step, MAX_STEPS};
use crate::header::{Header, HeaderFlags};
use crate::index::{BlockLocation, FrameLocation, Index};
use crate::merkle::merkle_root;
use crate::primitive::{GraphResources, PrimitiveId};
use crate::trailer::Trailer;
use crate::varint;
use std::collections::BTreeSet;
use std::io::{Read, Write};

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

/// Turns the plain bytes of a block into encoded bytes. The decode graph it
/// reports is what a reader runs on them: `encode` followed by the graph's
/// decoding must give back the plain bytes.
pub trait BlockEncoder {
    /// The decode graph of the blocks this encoder produces. It is fixed for
    /// the writer's lifetime: the writer reads it once, validates it (step
    /// count, parameters) and records it in every block.
    fn graph(&self) -> Graph;
    /// Encode one block's plain bytes.
    fn encode(&mut self, plain: &[u8]) -> Result<Vec<u8>, FormatError>;
    /// The decoder resources the graph needs, for the envelope.
    fn resources(&self) -> GraphResources;
}

/// The identity encoder: a one-step `store` graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreEncoder;

impl BlockEncoder for StoreEncoder {
    fn graph(&self) -> Graph {
        Graph {
            steps: vec![Step {
                primitive: PrimitiveId::Store,
                params: Vec::new(),
            }],
        }
    }

    fn encode(&mut self, plain: &[u8]) -> Result<Vec<u8>, FormatError> {
        Ok(plain.to_vec())
    }

    fn resources(&self) -> GraphResources {
        GraphResources::default()
    }
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
    /// Encodes every block and names its decode graph; [`StoreEncoder`] by default.
    pub encoder: Box<dyn BlockEncoder>,
}

impl std::fmt::Debug for WriterOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterOptions")
            .field("chunk_size", &self.chunk_size)
            .field("block_size", &self.block_size)
            .field("archive_id", &self.archive_id)
            .field("graph", &self.encoder.graph())
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
}

/// Builds an archive in one pass over a stream of entries.
pub struct Writer<W: Write> {
    out: W,
    options: WriterOptions,
    chunker: Box<dyn Chunker>,
    pos: u64,
    entries: Vec<Entry>,
    records: Vec<ChunkRecord>,
    blocks: Vec<BlockLocation>,
    priors: BTreeSet<[u8; 32]>,
    /// The encoder's graph, read once at construction.
    graph: Graph,
    pending: Vec<u8>,
    pending_chunks: u64,
    /// The first I/O error seen; every later call fails with it.
    failed: Option<(std::io::ErrorKind, String)>,
    /// Set after a chunker broke the rules; every later call fails with it.
    bad_chunk: Option<&'static str>,
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
        mut out: W,
        options: WriterOptions,
        chunker: Box<dyn Chunker>,
    ) -> Result<Self, FormatError> {
        if options.chunk_size < MIN_CHUNK_SIZE {
            return Err(bad_options("chunk_size below 4 KiB"));
        }
        if options.block_size < options.chunk_size {
            return Err(bad_options("block_size below chunk_size"));
        }
        let graph = options.encoder.graph();
        if graph.steps.is_empty() || graph.steps.len() > MAX_STEPS {
            return Err(FormatError::BadGraph {
                reason: "step count",
            });
        }
        for s in &graph.steps {
            s.primitive.validate_params(&s.params)?;
        }
        Header::new(HeaderFlags::EMPTY, options.archive_id).write(&mut out)?;
        Ok(Writer {
            out,
            options,
            chunker,
            pos: Header::LEN as u64,
            entries: Vec::new(),
            records: Vec::new(),
            blocks: Vec::new(),
            priors: BTreeSet::new(),
            graph,
            pending: Vec::new(),
            pending_chunks: 0,
            failed: None,
            bad_chunk: None,
        })
    }

    fn write_frame(
        &mut self,
        kind: FrameKind,
        payload: Vec<u8>,
    ) -> Result<FrameLocation, FormatError> {
        let frame = Frame {
            kind,
            flags: FrameFlags::EMPTY,
            payload,
        };
        let at = FrameLocation {
            offset: self.pos,
            len: frame.encoded_len(),
        };
        frame.write(&mut self.out)?;
        self.pos += at.len;
        Ok(at)
    }

    /// Check an entry (without its chunks) against the entries so far.
    fn check(&self, entry: &Entry) -> Result<(), FormatError> {
        check_entry(
            entry,
            self.entries.len() as u64,
            self.entries.last().map(|e| e.path.as_bytes()),
        )
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
            _ => {}
        }
        r
    }

    fn flush_block(&mut self) -> Result<(), FormatError> {
        if self.pending_chunks == 0 {
            return Ok(());
        }
        let plain = std::mem::take(&mut self.pending);
        let encoded = self.options.encoder.encode(&plain)?;
        let graph = self.graph.clone();
        self.priors.extend(graph.prior_ids());
        let header = BlockHeader {
            graph,
            plain_len: plain.len() as u64,
            encoded_len: encoded.len() as u64,
        };
        // The frame is written piecewise, without a second copy of the encoded
        // block (the identity encoder still copies the plain block once).
        let head = header.encode();
        let payload_len = (head.len() + encoded.len()) as u64;
        let at = FrameLocation {
            offset: self.pos,
            len: 4 + varint::len(payload_len) as u64 + payload_len + 32,
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
        self.blocks.push(BlockLocation {
            frame_offset: at.offset,
            frame_len: at.len,
            first_chunk: self.records.len() as u64 - self.pending_chunks,
            chunk_count: self.pending_chunks,
            plain_len: plain.len() as u64,
        });
        self.pending_chunks = 0;
        Ok(())
    }

    fn add_chunk(&mut self, data: &[u8]) -> Result<u64, FormatError> {
        let len = data.len() as u64;
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
        self.entries.push(entry);
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
        self.entries.push(entry);
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
        self.entries.push(entry);
        Ok(())
    }

    /// Close the last block and write the entry table, the index and the trailer.
    pub fn finish(mut self) -> Result<WriterSummary, FormatError> {
        self.check_alive()?;
        self.flush_block()?;
        let table = EntryTableWriter::encode(&self.entries)?;
        let entry_table = self.write_frame(FrameKind::EntryTable, table)?;

        let recs = &self.records;
        let leaves: Vec<[u8; 32]> = recs.iter().map(|r| r.hash).collect();
        let max_plain = self.blocks.iter().map(|b| b.plain_len).max().unwrap_or(0);
        let (g, e) = (self.graph.resources(), self.options.encoder.resources());
        let graph = GraphResources {
            window: g.window.max(e.window),
            bwt_block: g.bwt_block.max(e.bwt_block),
        };
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
            records: None,
        };
        // The envelope names the index's own payload length, which depends on
        // the envelope's varints. Starting from an upper bound the length can
        // only fall, so this settles within a few rounds.
        let mut guess = u64::MAX;
        let payload = loop {
            index.envelope = Envelope::for_archive(
                &index.blocks,
                ArchiveSizes {
                    index_payload_len: guess,
                    entry_table_len: entry_table.len,
                    records_len: 0,
                },
                graph,
                max_plain,
                0,
            );
            let payload = index.encode()?;
            let len = payload.len() as u64;
            if index.envelope.max_frame_payload == len.max(frames_max(&index)) {
                break payload;
            }
            guess = len;
        };
        let index_hash = *blake3::hash(&payload).as_bytes();
        let at = self.write_frame(FrameKind::Index, payload)?;
        Trailer {
            index_offset: at.offset,
            index_len: at.len,
            index_hash,
            generation: 0,
            archive_id: self.options.archive_id,
        }
        .write(&mut self.out)?;
        self.pos += crate::trailer::TRAILER_FRAME_LEN;
        self.out.flush()?;
        Ok(WriterSummary {
            entries: self.entries.len() as u64,
            chunks: self.records.len() as u64,
            blocks: index.blocks.len() as u64,
            archive_len: self.pos,
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
            records_len: 0,
        },
        crate::primitive::GraphResources::default(),
        0,
        0,
    )
    .max_frame_payload
}
