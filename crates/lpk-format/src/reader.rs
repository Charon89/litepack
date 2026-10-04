//! Reading chunk data back: the entry table, the block reader with its
//! one-block cache, extraction and whole-archive verification (spec section 9).

use crate::archive::Archive;
use crate::chunk::{fetch_and_check, resolve, ChunkSource};
use crate::decode::decode_block;
use crate::entry::{Entry, EntryKind, EntryTable};
use crate::envelope::Resources;
use crate::error::FormatError;
use crate::frame::FrameKind;
use crate::graph::{BlockHeader, Graph};
use crate::index::FrameLocation;
use crate::record::RecordsTable;
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
    /// True when every chunk was decoded and compared with the chunk table;
    /// false for an archive opened without credentials, where only the frame
    /// hashes and the recovery frames were checked.
    pub chunks_checked: bool,
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

/// The `Records` frame payload, read and hash-checked; owns its bytes so the
/// archive can be used while records are walked.
#[derive(Debug, Clone)]
pub struct OwnedRecordsTable {
    payload: Vec<u8>,
    count: u64,
}

impl OwnedRecordsTable {
    /// Number of records declared.
    pub fn len(&self) -> u64 {
        self.count
    }

    /// True when there are no records.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The parsed table over the owned bytes (the count was checked when the
    /// table was read, so this does not fail in practice).
    pub fn table(&self) -> Result<RecordsTable<'_>, FormatError> {
        RecordsTable::parse(&self.payload)
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

    /// Make `block` the cached block. A block frame whose hash fails is that
    /// frame's `HashMismatch { kind: 2 }`, for extraction and verification
    /// alike (spec section 9); `ChunkMismatch` is left for an intact frame
    /// whose decoded chunk differs from its record.
    fn load(&mut self, block: usize) -> Result<(), FormatError> {
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
            sequence: loc.sequence,
        };
        let frame = a.read_frame_at(at, FrameKind::ChunkData)?;
        // The records frame is read only for a block whose graph names a record.
        let (graph, graph_len) = Graph::parse(&frame.payload)?;
        let record_count = if graph.uses_records() {
            a.record_count()?
        } else {
            0
        };
        graph.check_records(record_count)?;
        let (header, used) = BlockHeader::from_graph(graph, graph_len, &frame.payload, block)?;
        let need = header.graph.resources();
        let env = &a.index().envelope;
        if need.window > env.max_window {
            return Err(FormatError::EnvelopeMismatch {
                field: "max_window",
            });
        }
        if need.bwt_block > env.max_bwt_block {
            return Err(FormatError::EnvelopeMismatch {
                field: "max_bwt_block",
            });
        }
        if let Some(id) = header
            .graph
            .prior_ids()
            .into_iter()
            .find(|id| a.index().priors.binary_search(id).is_err())
        {
            return Err(FormatError::UnlistedPrior { id });
        }
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
        // Every output of the graph is bounded by the archive's envelope,
        // which open has checked against the reader's own resources.
        let limits = Resources {
            max_block_plain: max,
            ..*a.resources()
        };
        let plain = decode_block(&a.registry, &header, block, &frame.payload[used..], &limits)?;
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
        self.load(place.block)?;
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

impl<R: Read + Seek> Archive<R> {
    /// The `Records` frame, read and hash-checked (`HashMismatch` when
    /// damaged); `None` when the index lists no records frame. The records
    /// themselves are checked when the table is walked.
    pub fn records(&mut self) -> Result<Option<OwnedRecordsTable>, FormatError> {
        let Some(at) = self.index().records else {
            return Ok(None);
        };
        let payload = self.read_frame_at(at, FrameKind::Records)?.payload;
        let count = RecordsTable::parse(&payload)?.len();
        Ok(Some(OwnedRecordsTable { payload, count }))
    }

    /// Walk the `Records` frame (when there is one) and check what needs the
    /// chunk table and the blocks: chunk indices in range, chunk lengths that
    /// add up, every chunk of a record in an earlier block than any block
    /// whose graph names the record, and every record id of a block graph.
    fn verify_records(&mut self) -> Result<(), FormatError> {
        let Some(owned) = self.records()? else {
            return Ok(());
        };
        let records = owned.table()?.iter().collect::<Result<Vec<_>, _>>()?;
        let count = records.len() as u64;
        let chunks = self.chunks_arc();
        for (id, rec) in (0u64..).zip(&records) {
            for (list, want) in rec.chunk_groups() {
                let mut total = 0u64;
                for &c in list {
                    let r = chunks.record(c).ok_or(FormatError::ChunkIndexOutOfRange {
                        chunk: c,
                        len: chunks.len(),
                    })?;
                    total = total.saturating_add(r.plain_len);
                }
                if total != want {
                    return Err(FormatError::BadRecord {
                        record: id,
                        reason: "chunk lengths",
                    });
                }
            }
        }
        let blocks = self.index().blocks.clone();
        for (b, loc) in blocks.iter().enumerate() {
            let at = FrameLocation {
                offset: loc.frame_offset,
                len: loc.frame_len,
                sequence: loc.sequence,
            };
            let frame = self.read_frame_at(at, FrameKind::ChunkData)?;
            let (graph, _) = Graph::parse(&frame.payload)?;
            graph.check_records(count)?;
            for step in &graph.steps {
                let Some(id) = step.primitive.record_id(&step.params) else {
                    continue;
                };
                let rec = usize::try_from(id).ok().and_then(|i| records.get(i));
                for (list, _) in rec.map(|r| r.chunk_groups()).unwrap_or_default() {
                    for &c in list {
                        if chunks.locate(c).is_none_or(|p| p.block >= b) {
                            return Err(FormatError::BadRecord {
                                record: id,
                                reason: "chunk order",
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Number of records (0 without a `Records` frame), cached.
    pub(crate) fn record_count(&mut self) -> Result<u64, FormatError> {
        if let Some(n) = self.record_count {
            return Ok(n);
        }
        let n = self.records()?.map_or(0, |t| t.len());
        self.record_count = Some(n);
        Ok(n)
    }

    /// The entry table, read and hash-checked (`HashMismatch` when damaged).
    pub fn entry_table(&mut self) -> Result<OwnedEntryTable, FormatError> {
        let payload = self.entries()?;
        let count = EntryTable::parse(&payload)?.len();
        Ok(OwnedEntryTable { payload, count })
    }

    /// Write the bytes of a file entry to `sink`, checking every chunk against
    /// the chunk table before it is written; the first mismatch is that
    /// chunk's `ChunkMismatch`, and a block frame that fails its hash is that
    /// frame's `HashMismatch { kind: 2 }` (earlier chunks have already been written, so a
    /// caller writing to a file should discard it on error). Directories and
    /// symlinks write nothing.
    ///
    /// Open item: only the block read last is cached, so a hostile chunk list
    /// that alternates between chunks of two blocks makes every reference
    /// read, hash and decode a whole block. A decode budget is needed before
    /// untrusted archives are extracted.
    pub fn extract(&mut self, entry: &Entry, sink: &mut dyn Write) -> Result<(), FormatError> {
        if entry.kind != EntryKind::File {
            return Ok(());
        }
        self.need_index()?;
        let table = self.chunks_arc();
        let recs = resolve(&entry.chunks, entry.size, &*table)?;
        let mut source = ArchiveChunks::new(self);
        for (&c, r) in entry.chunks.iter().zip(&recs) {
            let data = fetch_and_check(&mut source, c, r)?;
            sink.write_all(&data)?;
        }
        Ok(())
    }

    /// Check the whole archive without writing anything: every block is
    /// decoded and every chunk compared with its table record (so a chunk the
    /// entries do not use is checked too; a block frame that fails its hash is
    /// reported as `HashMismatch`), then every file entry's chunk list and
    /// size are checked against the chunk table, which needs no block reads.
    /// The Merkle root and the index were checked when the archive was opened.
    ///
    /// When the index lists a `Records` frame it is read and every record
    /// checked first (spec section 12): the frame and body hashes, the field
    /// rules, the chunk indices and sums, the block order, and the record ids
    /// of every block graph.
    ///
    /// An encrypted archive opened without credentials ([`Archive::open_with`])
    /// cannot decode anything: `verify` then hashes every frame, checks the
    /// recovery frames' shards and reports `chunks_checked: false`.
    pub fn verify(&mut self) -> Result<VerifySummary, FormatError> {
        if self.is_keyless() {
            return self.verify_frames_only();
        }
        self.verify_records()?;
        let table = self.chunks_arc();
        let blocks = self.index().blocks.clone();
        let mut source = ArchiveChunks::new(self);
        for b in &blocks {
            for c in b.first_chunk..b.first_chunk + b.chunk_count {
                let rec = table.record(c).ok_or(FormatError::ChunkIndexOutOfRange {
                    chunk: c,
                    len: table.len(),
                })?;
                fetch_and_check(&mut source, c, &rec)?;
            }
        }
        let entries = self.entry_table()?;
        for e in entries.table()?.iter() {
            let e = e?;
            if e.kind == EntryKind::File {
                resolve(&e.chunks, e.size, &*table)?;
            }
        }
        Ok(VerifySummary {
            entries: entries.len(),
            chunks: table.len(),
            blocks: blocks.len() as u64,
            chunks_checked: true,
        })
    }

    /// `verify` without a key: every frame's hash, then the recovery frames.
    fn verify_frames_only(&mut self) -> Result<VerifySummary, FormatError> {
        let limits = *self.limits();
        let d = Self::walk(self.raw_reader(), &limits, None);
        if let Some(e) = d.error {
            return Err(e);
        }
        let r = self.scan_recovery_frames()?;
        if r.shards_damaged > 0 || r.frames_unusable > 0 {
            return Err(FormatError::DamageFound {
                damaged: r.shards_damaged,
                unusable: r.frames_unusable,
            });
        }
        // A sealed entry table cannot be counted without the key.
        let entries = if self.is_listable() {
            self.entry_table()?.len()
        } else {
            0
        };
        Ok(VerifySummary {
            entries,
            chunks: 0,
            blocks: 0,
            chunks_checked: false,
        })
    }
}
