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
use crate::primitive::PrimitiveId;
use crate::trailer::Trailer;
use std::io::{Read, Write};

/// Smallest `chunk_size` a writer accepts.
pub const MIN_CHUNK_SIZE: u64 = 4096;
/// Default `chunk_size`: 1 MiB.
pub const DEFAULT_CHUNK_SIZE: u64 = 1 << 20;
/// Default `block_size`: 64 MiB.
pub const DEFAULT_BLOCK_SIZE: u64 = 1 << 26;

/// Cuts a run of file bytes into chunks.
///
/// The writer hands the chunker consecutive segments of one file, each a
/// whole number of `chunk_size` bytes long except the last (which ends the
/// file); the returned slices must tile the segment, in order, without gaps
/// or empty pieces.
pub trait Chunker {
    /// Cut `data` into consecutive non-empty slices that cover it exactly.
    fn chunks<'a>(&mut self, data: &'a [u8]) -> Vec<&'a [u8]>;
}

/// Cuts into pieces of exactly `size` bytes, the last one shorter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedChunker {
    /// Piece length in bytes; at least 1.
    pub size: usize,
}

impl Chunker for FixedChunker {
    fn chunks<'a>(&mut self, data: &'a [u8]) -> Vec<&'a [u8]> {
        data.chunks(self.size.max(1)).collect()
    }
}

/// Settings of a [`Writer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterOptions {
    /// Chunk length for the fixed cut; at least [`MIN_CHUNK_SIZE`].
    pub chunk_size: u64,
    /// Most plain bytes in a block; at least `chunk_size`. A block is closed
    /// before the chunk that would exceed it.
    pub block_size: u64,
    /// The archive's identity, written to the header and the trailer.
    pub archive_id: [u8; 16],
    /// The decode graph of every block. This writer has no encoder but the
    /// identity, so every step must be `store`.
    pub graph: Graph,
}

impl Default for WriterOptions {
    fn default() -> Self {
        WriterOptions {
            chunk_size: DEFAULT_CHUNK_SIZE,
            block_size: DEFAULT_BLOCK_SIZE,
            archive_id: [0; 16],
            graph: Graph {
                steps: vec![Step {
                    primitive: PrimitiveId::Store,
                    params: Vec::new(),
                }],
            },
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
    pending: Vec<u8>,
    pending_chunks: u64,
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
        Self::with_chunker(out, options, Box::new(FixedChunker { size }))
    }

    /// Like [`Writer::new`] with another chunker (the `chunk_size` option still
    /// sets the segment length the chunker is fed).
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
        let steps = &options.graph.steps;
        if steps.is_empty() || steps.len() > MAX_STEPS {
            return Err(FormatError::BadGraph {
                reason: "step count",
            });
        }
        for s in steps {
            s.primitive.validate_params(&s.params)?;
            if s.primitive != PrimitiveId::Store {
                return Err(bad_options(
                    "graph needs an encoder; only store is available",
                ));
            }
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
            pending: Vec::new(),
            pending_chunks: 0,
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

    fn flush_block(&mut self) -> Result<(), FormatError> {
        if self.pending_chunks == 0 {
            return Ok(());
        }
        let plain = std::mem::take(&mut self.pending);
        let header = BlockHeader {
            graph: self.options.graph.clone(),
            plain_len: plain.len() as u64,
            encoded_len: plain.len() as u64,
        };
        // The graph is all `store` (checked at construction): encoding is the identity.
        let mut payload = header.encode();
        payload.extend_from_slice(&plain);
        let at = self.write_frame(FrameKind::ChunkData, payload)?;
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
    /// appended to the current block. A failed read leaves the chunks read so
    /// far in the archive without an entry; the caller should then abandon the
    /// writer.
    pub fn add_file(
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
        // Whole chunks per segment, so a fixed cut never splits a chunk across reads.
        let per_segment =
            (self.options.block_size / self.options.chunk_size) * self.options.chunk_size;
        // The buffer grows with the file: a small file does not pay for a whole block.
        let mut buf: Vec<u8> = Vec::new();
        loop {
            buf.clear();
            let n = Read::take(&mut *data, per_segment).read_to_end(&mut buf)?;
            if n == 0 {
                break;
            }
            let segment = &buf[..n];
            let cuts = self.chunker.chunks(segment);
            if cuts.iter().map(|c| c.len()).sum::<usize>() != n || cuts.iter().any(|c| c.is_empty())
            {
                return Err(bad_options("chunker did not tile its input"));
            }
            for piece in cuts {
                let i = self.add_chunk(piece)?;
                entry.chunks.push(i);
            }
            entry.size += n as u64;
            if (n as u64) < per_segment {
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
        self.flush_block()?;
        let table = EntryTableWriter::encode(&self.entries)?;
        let entry_table = self.write_frame(FrameKind::EntryTable, table)?;

        let recs = &self.records;
        let leaves: Vec<[u8; 32]> = recs.iter().map(|r| r.hash).collect();
        let max_plain = self.blocks.iter().map(|b| b.plain_len).max().unwrap_or(0);
        let graph = self.options.graph.resources();
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
