//! Reading chunk data back: the entry table, the block reader with its
//! one-block cache, extraction and whole-archive verification (spec section 9).

use crate::archive::Archive;
use crate::chunk::{verify_file, ChunkRecord, ChunkSource};
use crate::decode::decode_block;
use crate::entry::{Entry, EntryKind, EntryTable};
use crate::error::FormatError;
use crate::frame::FrameKind;
use crate::graph::BlockHeader;
use crate::index::FrameLocation;
use std::io::{Read, Seek, Write};

/// What [`Archive::verify`] checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifySummary {
    /// Entries in the entry table.
    pub entries: u64,
    /// Chunks in the chunk table.
    pub chunks: u64,
    /// `ChunkData` blocks.
    pub blocks: u64,
}

/// The entry table payload, read and hash-checked; owns its bytes so the
/// archive can be used while entries are walked.
#[derive(Debug, Clone)]
pub struct OwnedEntryTable {
    payload: Vec<u8>,
    count: u64,
}

impl OwnedEntryTable {
    /// Number of entries declared.
    pub fn len(&self) -> u64 {
        self.count
    }

    /// True when there are no entries.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The parsed table over the owned bytes (the parse was checked when the
    /// table was read, so this does not fail in practice).
    pub fn table(&self) -> Result<EntryTable<'_>, FormatError> {
        EntryTable::parse(&self.payload)
    }
}

/// A [`ChunkSource`] over an archive's `ChunkData` blocks. The archive keeps
/// the plain bytes of the block read last, so chunks read in order decode each
/// block once. A chunk's allocation is bounded by its `plain_len`, and a
/// block's by its `plain_len` (at most `max_block_plain`).
#[derive(Debug)]
pub struct ArchiveChunks<'a, R: Read + Seek> {
    archive: &'a mut Archive<R>,
}

impl<'a, R: Read + Seek> ArchiveChunks<'a, R> {
    /// A source over `archive`.
    pub fn new(archive: &'a mut Archive<R>) -> Self {
        ArchiveChunks { archive }
    }

    /// Make `block` the cached block. A block frame whose hash fails is
    /// reported as a mismatch of the requested chunk: its bytes cannot vouch
    /// for any chunk they hold.
    fn load(&mut self, block: usize, chunk: u64) -> Result<(), FormatError> {
        let a = &mut *self.archive;
        if a.cache.as_ref().is_some_and(|(b, _)| *b == block) {
            return Ok(());
        }
        a.cache = None;
        let loc = *a
            .index()
            .blocks
            .get(block)
            .ok_or(FormatError::BlockCoverage { block })?;
        let at = FrameLocation {
            offset: loc.frame_offset,
            len: loc.frame_len,
        };
        let frame = match a.read_frame_at(at, FrameKind::ChunkData) {
            Err(FormatError::HashMismatch { .. }) => {
                return Err(FormatError::ChunkMismatch { chunk })
            }
            other => other?,
        };
        let (header, used) = BlockHeader::parse(&frame.payload, block)?;
        if header.plain_len != loc.plain_len {
            return Err(FormatError::BlockLengthMismatch { block });
        }
        let max = a.index().envelope.max_block_plain;
        if header.plain_len > max {
            return Err(FormatError::PayloadTooLarge {
                len: header.plain_len,
                max,
            });
        }
        let plain = decode_block(
            &a.registry,
            &header,
            block,
            &frame.payload[used..],
            a.resources(),
        )?;
        a.cache = Some((block, plain));
        Ok(())
    }
}

impl<R: Read + Seek> ChunkSource for ArchiveChunks<'_, R> {
    fn chunk(&mut self, index: u64) -> Result<Vec<u8>, FormatError> {
        let place =
            self.archive
                .chunks()
                .locate(index)
                .ok_or(FormatError::ChunkIndexOutOfRange {
                    chunk: index,
                    len: self.archive.chunks().len(),
                })?;
        self.load(place.block, index)?;
        let bad = FormatError::BlockLengthMismatch { block: place.block };
        let Some((_, plain)) = &self.archive.cache else {
            return Err(bad);
        };
        let start = usize::try_from(place.offset_in_block)
            .map_err(|_| FormatError::BlockLengthMismatch { block: place.block })?;
        let end = usize::try_from(place.plain_len)
            .ok()
            .and_then(|l| start.checked_add(l))
            .ok_or(FormatError::BlockLengthMismatch { block: place.block })?;
        plain.get(start..end).map(<[u8]>::to_vec).ok_or(bad)
    }
}

fn check_chunk(data: &[u8], index: u64, rec: &ChunkRecord) -> Result<(), FormatError> {
    if data.len() as u64 != rec.plain_len || blake3::hash(data).as_bytes() != &rec.hash {
        return Err(FormatError::ChunkMismatch { chunk: index });
    }
    Ok(())
}

impl<R: Read + Seek> Archive<R> {
    /// The entry table, read and hash-checked (`HashMismatch` when damaged).
    pub fn entry_table(&mut self) -> Result<OwnedEntryTable, FormatError> {
        let payload = self.entries()?;
        let count = EntryTable::parse(&payload)?.len();
        Ok(OwnedEntryTable { payload, count })
    }

    /// Write the bytes of a file entry to `sink`, checking every chunk against
    /// the chunk table before it is written; the first mismatch is that
    /// chunk's `ChunkMismatch` (earlier chunks have already been written).
    /// Directories and symlinks write nothing.
    pub fn extract(&mut self, entry: &Entry, sink: &mut dyn Write) -> Result<(), FormatError> {
        if entry.kind != EntryKind::File {
            return Ok(());
        }
        let table = self.chunks_arc();
        let mut recs = Vec::with_capacity(entry.chunks.len().min(1 << 16));
        let mut total = Some(0u64);
        for &c in &entry.chunks {
            let r = table.record(c).ok_or(FormatError::ChunkIndexOutOfRange {
                chunk: c,
                len: table.len(),
            })?;
            total = total.and_then(|t| t.checked_add(r.plain_len));
            recs.push(r);
        }
        if total != Some(entry.size) {
            return Err(FormatError::FileSizeMismatch {
                expected: entry.size,
                found: total.unwrap_or(u64::MAX),
            });
        }
        let mut source = ArchiveChunks::new(self);
        for (&c, r) in entry.chunks.iter().zip(&recs) {
            let data = source.chunk(c)?;
            check_chunk(&data, c, r)?;
            sink.write_all(&data)?;
        }
        Ok(())
    }

    /// Check the whole archive without writing anything: every block is
    /// decoded and every chunk compared with its table record (so a chunk the
    /// entries do not use is checked too), then every entry's chunk list and
    /// sizes are verified with [`verify_file`]. The Merkle root and the index
    /// were checked when the archive was opened.
    pub fn verify(&mut self) -> Result<VerifySummary, FormatError> {
        let table = self.chunks_arc();
        let blocks = self.index().blocks.clone();
        let mut source = ArchiveChunks::new(self);
        for b in &blocks {
            for c in b.first_chunk..b.first_chunk + b.chunk_count {
                let rec = table.record(c).ok_or(FormatError::ChunkIndexOutOfRange {
                    chunk: c,
                    len: table.len(),
                })?;
                let data = source.chunk(c)?;
                check_chunk(&data, c, &rec)?;
            }
        }
        let entries = self.entry_table()?;
        let mut source = ArchiveChunks::new(self);
        for e in entries.table()?.iter() {
            let e = e?;
            if e.kind == EntryKind::File {
                verify_file(&e.chunks, e.size, &*table, &mut source)?;
            }
        }
        Ok(VerifySummary {
            entries: entries.len(),
            chunks: table.len(),
            blocks: blocks.len() as u64,
        })
    }
}
