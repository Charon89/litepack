//! Reading chunk data back: the entry table, the block reader with its
//! one-block cache, extraction and whole-archive verification (spec section 9).

use crate::archive::Archive;
use crate::chunk::{fetch_and_check, resolve, ChunkSource};
use crate::decode::{decode_block, decode_block_in, DecodeContext};
use crate::entry::{Entry, EntryKind, EntryTable};
use crate::envelope::Resources;
use crate::error::FormatError;
use crate::frame::FrameKind;
use crate::graph::{BlockHeader, Graph};
use crate::index::FrameLocation;
use crate::record::{Record, RecordsTable};
use std::io::{Read, Seek, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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

/// The records cache of an opened archive, shared by its forks: the
/// `Records` frame read, hash-checked and walked once. The lock is held
/// while the frame is read and walked, so forks that start together wait
/// for one parse instead of each making their own.
#[derive(Debug, Default)]
pub(crate) struct RecordsShared {
    cell: Mutex<Option<Arc<ParsedRecords>>>,
    /// Times the frame was read for the cache (the test hook).
    parses: AtomicU64,
}

/// The `Records` frame after one walk: the verified payload and where each
/// record starts; a lookup parses its one record from there.
#[derive(Debug)]
struct ParsedRecords {
    /// The frame (`None` when the index lists none).
    table: Option<OwnedRecordsTable>,
    /// The payload offset of records `0..offsets.len()`, each of which the
    /// walk hash-checked and parsed; the walk stopped at the first bad
    /// record (id `offsets.len()`) or at the end.
    offsets: Vec<usize>,
}

impl ParsedRecords {
    fn count(&self) -> u64 {
        self.table.as_ref().map_or(0, OwnedRecordsTable::len)
    }
}

/// A [`ChunkSource`] over an archive's `ChunkData` blocks. The archive keeps
/// the plain bytes of the block read last, so chunks read in order decode each
/// block once. A chunk's allocation is bounded by its `plain_len`, and a
/// block's by its `plain_len` (at most `max_block_plain`).
#[derive(Debug)]
pub struct ArchiveChunks<'a, R: Read + Seek> {
    archive: &'a mut Archive<R>,
    /// Set while a reconstruction step reads the chunks of this record: a block
    /// whose graph names a reconstruction primitive is then refused.
    nested_for: Option<u64>,
}

impl<'a, R: Read + Seek> ArchiveChunks<'a, R> {
    /// A source over `archive`.
    pub fn new(archive: &'a mut Archive<R>) -> Self {
        ArchiveChunks {
            archive,
            nested_for: None,
        }
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
        // Nesting (spec section 12): a chunk a record names lies in a block whose
        // graph names no reconstruction primitive. (The cache never holds such a
        // block here: loading one clears the cache before its step runs.)
        if let Some(record) = self.nested_for {
            if graph.uses_records() {
                return Err(FormatError::BadRecord {
                    record,
                    reason: "nested record",
                });
            }
        }
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
        // which open has checked against the reader's own resources; so is
        // the working memory a decoder may use (`decode_memory`, never more
        // than the reader's resource).
        let limits = Resources {
            max_block_plain: max,
            memory: a.index().envelope.decode_memory.min(a.resources().memory),
            ..*a.resources()
        };
        let encoded = &frame.payload[used..];
        a.decodes += 1;
        let plain = if header.graph.uses_records() {
            // The decoder may read the chunks its record names, from blocks
            // with lower indices, through the archive itself; those blocks
            // are decoded with the 1.0 registry (one level), so the archive's
            // own registry is set aside meanwhile and always put back.
            let nested = a.registry.nested();
            let registry = std::mem::replace(&mut a.registry, nested);
            let mut ctx = ArchiveContext {
                archive: &mut *a,
                block,
            };
            let r = decode_block_in(&registry, &header, block, encoded, &limits, &mut ctx);
            a.registry = registry;
            a.cache = None;
            r?
        } else {
            decode_block(&a.registry, &header, block, encoded, &limits)?
        };
        a.cache = Some((block, plain));
        Ok(())
    }
}

/// The [`DecodeContext`] of a block of an archive: records from its
/// `Records` frame, chunks through its blocks.
struct ArchiveContext<'a, R: Read + Seek> {
    archive: &'a mut Archive<R>,
    block: usize,
}

impl<R: Read + Seek> DecodeContext for ArchiveContext<'_, R> {
    fn block(&self) -> usize {
        self.block
    }

    fn record(&mut self, id: u64) -> Result<Record, FormatError> {
        self.archive.record(id)
    }

    fn memory_budget(&self) -> Option<u64> {
        Some(self.archive.resources().memory)
    }

    fn chunk(&mut self, record: u64, index: u64) -> Result<Vec<u8>, FormatError> {
        let table = self.archive.chunks_arc();
        let place = table
            .locate(index)
            .ok_or(FormatError::ChunkIndexOutOfRange {
                chunk: index,
                len: table.len(),
            })?;
        if place.block >= self.block {
            return Err(FormatError::BadRecord {
                record,
                reason: "chunk order",
            });
        }
        let rec = table
            .record(index)
            .ok_or(FormatError::ChunkIndexOutOfRange {
                chunk: index,
                len: table.len(),
            })?;
        let mut source = ArchiveChunks {
            archive: &mut *self.archive,
            nested_for: Some(record),
        };
        fetch_and_check(&mut source, index, &rec)
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

    /// The `Records` frame parsed once for this archive and its forks: read
    /// and hash-checked on the first call that succeeds (a failed read is not
    /// kept, so the next call reads again, as before the cache).
    fn parsed_records(&mut self) -> Result<Arc<ParsedRecords>, FormatError> {
        let shared = Arc::clone(&self.records_cache);
        // A panic while parsing leaves `None` behind: nothing half-built.
        let mut cell = shared
            .cell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(parsed) = &*cell {
            return Ok(Arc::clone(parsed));
        }
        let table = self.records()?;
        shared.parses.fetch_add(1, Ordering::Relaxed);
        let mut offsets = Vec::new();
        if let Some(owned) = &table {
            let payload_len = owned.payload.len();
            let mut it = owned.table()?.iter();
            // Stop at the first bad record; a lookup at or past it walks the
            // table again for that record's error. (After the last record the
            // walk only checks trailing bytes, which no lookup reports.)
            loop {
                let start = payload_len - it.rest_len();
                match it.next() {
                    Some(Ok(_)) => offsets.push(start),
                    _ => break,
                }
            }
        }
        let parsed = Arc::new(ParsedRecords { table, offsets });
        *cell = Some(Arc::clone(&parsed));
        Ok(parsed)
    }

    /// Number of records (0 without a `Records` frame), from the parsed
    /// records.
    pub(crate) fn record_count(&mut self) -> Result<u64, FormatError> {
        Ok(self.parsed_records()?.count())
    }

    /// The record `id` of the `Records` frame, hash-checked and parsed:
    /// `RecordOutOfRange` when there is no such record, and otherwise the
    /// first error among records `0..=id` (the frame is parsed once per
    /// archive and shared with its forks).
    pub(crate) fn record(&mut self, id: u64) -> Result<Record, FormatError> {
        let parsed = self.parsed_records()?;
        let count = parsed.count();
        let out_of_range = FormatError::RecordOutOfRange { record: id, count };
        let Some(owned) = parsed.table.as_ref().filter(|_| id < count) else {
            return Err(out_of_range);
        };
        if let Some(&at) = usize::try_from(id).ok().and_then(|i| parsed.offsets.get(i)) {
            // A record the walk found good: parse it alone (the same checks).
            let rest = owned.payload.get(at..).unwrap_or(&[]);
            return crate::record::RecordsIter::one(rest, id)
                .next()
                .unwrap_or(Err(out_of_range));
        }
        // At or past the first bad record: the walk reports its error.
        owned.table()?.get(id)?.ok_or(out_of_range)
    }

    /// How many times the `Records` frame has been read and parsed for the
    /// record cache of this archive and its forks (a test hook: once per
    /// opened archive when nothing fails).
    #[cfg(test)]
    pub(crate) fn records_parse_count(&self) -> u64 {
        self.records_cache.parses.load(Ordering::Relaxed)
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

    /// Decode block `block` and return its plain bytes, every chunk of the
    /// block checked against the chunk table first: the first chunk whose
    /// length or BLAKE3 differs is that chunk's `ChunkMismatch`, a block frame
    /// that fails its hash is `HashMismatch { kind: 2 }`. The checks and the
    /// decode are exactly those of [`Archive::extract`] (the envelope, the
    /// priors, the records and the nesting rule of revision 1.1, the chunks a
    /// record names read through this archive); the plain bytes are handed to
    /// the caller and not kept in the one-block cache. A chunk `c` of the
    /// block lies at its `ChunkIndex::locate(c).offset_in_block`.
    pub fn decode_block_checked(&mut self, block: usize) -> Result<Vec<u8>, FormatError> {
        self.need_index()?;
        let range = self
            .block_chunks(block)
            .ok_or(FormatError::BlockCoverage { block })?;
        ArchiveChunks::new(self).load(block)?;
        let plain = match self.cache.take() {
            Some((b, plain)) if b == block => plain,
            _ => return Err(FormatError::BlockLengthMismatch { block }),
        };
        let table = self.chunks_arc();
        for c in range {
            let len = table.len();
            let rec = table
                .record(c)
                .ok_or(FormatError::ChunkIndexOutOfRange { chunk: c, len })?;
            let place = table
                .locate(c)
                .ok_or(FormatError::ChunkIndexOutOfRange { chunk: c, len })?;
            let data = usize::try_from(place.offset_in_block)
                .ok()
                .and_then(|s| Some(s..s.checked_add(usize::try_from(rec.plain_len).ok()?)?))
                .and_then(|r| plain.get(r))
                .ok_or(FormatError::BlockLengthMismatch { block })?;
            if blake3::hash(data).as_bytes() != &rec.hash {
                return Err(FormatError::ChunkMismatch { chunk: c });
            }
        }
        Ok(plain)
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
    /// cannot decode anything: `verify` then hashes every frame but the
    /// recovery frames and reports `chunks_checked: false`. In neither mode
    /// does `verify` read a recovery payload; `check` and `repair` do.
    pub fn verify(&mut self) -> Result<VerifySummary, FormatError> {
        self.verify_with(&mut crate::hook::no_hook)
            .map(|s| s.expect("no_hook never stops the walk"))
    }

    /// [`Archive::verify`] with a per-block hook ([`crate::hook`]): called with 0 of the
    /// block count before the first block and after each block's chunks are checked; a
    /// `Break` stops the walk and returns `Ok(None)`. A keyless walk (frame hashes only) calls
    /// it before and after.
    pub fn verify_with(
        &mut self,
        hook: &mut crate::hook::BlockHook<'_>,
    ) -> Result<Option<VerifySummary>, FormatError> {
        if self.is_keyless() {
            if crate::hook::stop(hook, 0, 1) {
                return Ok(None);
            }
            let s = self.verify_frames_only()?;
            return Ok((!crate::hook::stop(hook, 1, 1)).then_some(s));
        }
        self.verify_records()?;
        let table = self.chunks_arc();
        let blocks = self.index().blocks.clone();
        let total = blocks.len() as u64;
        if crate::hook::stop(hook, 0, total) {
            return Ok(None);
        }
        let mut source = ArchiveChunks::new(self);
        for (done, b) in blocks.iter().enumerate() {
            for c in b.first_chunk..b.first_chunk + b.chunk_count {
                let rec = table.record(c).ok_or(FormatError::ChunkIndexOutOfRange {
                    chunk: c,
                    len: table.len(),
                })?;
                fetch_and_check(&mut source, c, &rec)?;
            }
            if crate::hook::stop(hook, done as u64 + 1, total) {
                return Ok(None);
            }
        }
        let entries = self.entry_table()?;
        for e in entries.table()?.iter() {
            let e = e?;
            if e.kind == EntryKind::File {
                resolve(&e.chunks, e.size, &*table)?;
            }
        }
        Ok(Some(VerifySummary {
            entries: entries.len(),
            chunks: table.len(),
            blocks: blocks.len() as u64,
            chunks_checked: true,
        }))
    }

    /// `verify` without a key: the hash of every frame except the recovery
    /// frames, and the sealing rules (the key slot first and once). Recovery
    /// is left to `check` in every mode (spec section 9): no recovery payload
    /// is parsed and a damaged recovery frame does not fail.
    fn verify_frames_only(&mut self) -> Result<VerifySummary, FormatError> {
        let limits = *self.limits();
        let d = Self::walk_with(self.raw_reader(), &limits, None, true);
        if let Some(e) = d.error {
            return Err(e);
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

#[cfg(test)]
#[path = "records_cache_tests.rs"]
mod records_cache_tests;
