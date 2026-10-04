//! The index frame payload: chunk table, Merkle root, decode envelope, block table and
//! the locations of the entry table and records frames (spec sections 6 and 7).

use crate::chunk::{ChunkIndex, ChunkTable};
use crate::envelope::Envelope;
use crate::error::FormatError;
use crate::header::Header;
use crate::merkle::merkle_root;
use crate::varint;
use std::sync::Arc;

const WHAT: &str = "index";
/// Smallest encoded frame: kind, flags, a one-byte length and the hash.
const MIN_FRAME_LEN: u64 = 4 + 1 + 32;
/// Smallest encoded block record: six one-byte varints.
const MIN_BLOCK_LEN: usize = 6;
/// Smallest encoded recovery location: three one-byte varints.
const MIN_LOCATION_LEN: usize = 3;
/// Smallest generation table entry: three one-byte varints and the salt.
const MIN_GENERATION_LEN: usize = 3 + 16;

/// Where one `ChunkData` block is and which chunks it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockLocation {
    /// Absolute offset of the block's frame (its `kind` field).
    pub frame_offset: u64,
    /// Whole encoded length of the block's frame.
    pub frame_len: u64,
    /// Index of the first chunk the block holds.
    pub first_chunk: u64,
    /// Number of chunks the block holds.
    pub chunk_count: u64,
    /// Sum of the `plain_len` of the block's chunks.
    pub plain_len: u64,
    /// Position of the block's frame among the archive's frames (section 14).
    pub sequence: u64,
}

/// Where a frame is: absolute offset and whole encoded length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLocation {
    /// Absolute offset of the frame's `kind` field.
    pub offset: u64,
    /// Whole encoded length of the frame.
    pub len: u64,
    /// Position of the frame among the archive's frames, counted from the key
    /// slot or the first frame (section 14).
    pub sequence: u64,
}

impl FrameLocation {
    /// True when the frame lies at or after the header and ends no later than `limit`.
    pub(crate) fn fits_below(&self, limit: u64) -> bool {
        self.offset >= Header::LEN as u64
            && self.len >= MIN_FRAME_LEN
            && self
                .offset
                .checked_add(self.len)
                .is_some_and(|e| e <= limit)
    }
}

/// The decoded index payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// The chunk table payload, exactly as encoded (section 5).
    pub chunk_table: Arc<[u8]>,
    /// Merkle root over the chunk table's hashes.
    pub merkle_root: [u8; 32],
    /// The resources a decoder needs (section 7).
    pub envelope: Envelope,
    /// The IDs of the priors the blocks' graphs name: ascending, unique, never
    /// all zeros (section 10).
    pub priors: Vec<[u8; 32]>,
    /// The blocks, ascending by `first_chunk`, partitioning the chunk table.
    pub blocks: Vec<BlockLocation>,
    /// Location of the `EntryTable` frame.
    pub entry_table: FrameLocation,
    /// BLAKE3-256 of the entry table's payload as stored (the sealed bytes when
    /// the table is sealed): the index authenticates the table.
    pub entry_table_hash: [u8; 32],
    /// Location of the `Records` frame, if any.
    pub records: Option<FrameLocation>,
    /// Locations of the `Recovery` frames, in the order written (section 13).
    pub recovery: Vec<FrameLocation>,
    /// One entry per generation, ascending (section 15): where the generation
    /// starts and the salt its sealed frames' nonces use.
    pub generations: Vec<GenerationInfo>,
}

/// A generation as the index lists it (section 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationInfo {
    /// The generation number; the table counts 0, 1, 2, ... in order.
    pub generation: u64,
    /// Absolute offset of the generation's first frame (the end of the header
    /// for generation 0, else the end of the previous trailer).
    pub start_offset: u64,
    /// Position among the archive's frames of that first frame; the frames of
    /// the generation have sequences from here to the next generation's.
    pub first_sequence: u64,
    /// The generation's nonce salt (zeros when the archive is not encrypted).
    pub salt: [u8; 16],
}

/// Cursor that assigns chunks, in order, to blocks and checks each block's
/// chunk count and `plain_len` as the walk leaves it.
pub(crate) struct BlockWalk<'a> {
    blocks: &'a [BlockLocation],
    block: usize,
    seen: u64,
    acc: u64,
}

impl<'a> BlockWalk<'a> {
    pub(crate) fn new(blocks: &'a [BlockLocation]) -> Self {
        BlockWalk {
            blocks,
            block: 0,
            seen: 0,
            acc: 0,
        }
    }

    fn close(&mut self) -> Result<(), FormatError> {
        if let Some(b) = self.blocks.get(self.block) {
            if self.seen != b.chunk_count {
                return Err(FormatError::BlockCoverage { block: self.block });
            }
            if self.acc != b.plain_len {
                return Err(FormatError::BlockLengthMismatch { block: self.block });
            }
        }
        self.block += 1;
        self.seen = 0;
        self.acc = 0;
        Ok(())
    }

    /// Account for chunk number `chunk`; returns its block and the sum of the
    /// `plain_len` of the chunks before it in that block.
    pub(crate) fn advance(
        &mut self,
        chunk: u64,
        plain_len: u64,
    ) -> Result<(usize, u64), FormatError> {
        while self
            .blocks
            .get(self.block)
            .is_some_and(|b| chunk >= b.first_chunk.saturating_add(b.chunk_count))
        {
            self.close()?;
        }
        let block = self.block;
        let b = self
            .blocks
            .get(block)
            .ok_or(FormatError::BlockCoverage { block })?;
        if chunk < b.first_chunk {
            return Err(FormatError::BlockCoverage { block });
        }
        let at = self.acc;
        self.acc = at
            .checked_add(plain_len)
            .ok_or(FormatError::BlockLengthMismatch { block })?;
        self.seen += 1;
        Ok((block, at))
    }

    /// Close the blocks after the last chunk.
    pub(crate) fn finish(mut self) -> Result<(), FormatError> {
        while self.block < self.blocks.len() {
            self.close()?;
        }
        Ok(())
    }
}

fn rv(s: &mut &[u8]) -> Result<u64, FormatError> {
    match varint::read(s) {
        Err(FormatError::Truncated { .. }) => Err(FormatError::Truncated { what: WHAT }),
        other => other,
    }
}

/// Read the prior list: a count bounded by the bytes left, then the IDs, which
/// must ascend strictly and may not be all zeros.
fn read_priors(s: &mut &[u8]) -> Result<Vec<[u8; 32]>, FormatError> {
    let count = rv(s)?;
    if count > (s.len() / 32) as u64 {
        return Err(FormatError::Truncated { what: WHAT });
    }
    let mut priors: Vec<[u8; 32]> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (id, rest) = s
            .split_first_chunk::<32>()
            .ok_or(FormatError::Truncated { what: WHAT })?;
        *s = rest;
        if id.iter().all(|&b| b == 0) {
            return Err(FormatError::BadPriorList { reason: "zero id" });
        }
        if priors.last().is_some_and(|prev| prev >= id) {
            return Err(FormatError::BadPriorList {
                reason: "not ascending and unique",
            });
        }
        priors.push(*id);
    }
    Ok(priors)
}

/// Check that the blocks partition `0..table_len` and lie in the body.
fn validate_blocks(
    blocks: &[BlockLocation],
    table_len: u64,
    index_offset: u64,
) -> Result<(), FormatError> {
    let mut next = 0u64;
    let mut prev_end = 0u64;
    for (i, b) in blocks.iter().enumerate() {
        let loc = FrameLocation {
            offset: b.frame_offset,
            len: b.frame_len,
            sequence: b.sequence,
        };
        if !loc.fits_below(index_offset) {
            return Err(FormatError::BlockOutOfRange { block: i });
        }
        // Frames ascend and do not overlap; an empty block is the only block.
        if b.frame_offset < prev_end || (table_len == 0 && i > 0) {
            return Err(FormatError::BlockCoverage { block: i });
        }
        prev_end = b.frame_offset + b.frame_len;
        if b.first_chunk != next || (b.chunk_count == 0 && table_len > 0) {
            return Err(FormatError::BlockCoverage { block: i });
        }
        next = next
            .checked_add(b.chunk_count)
            .filter(|n| *n <= table_len)
            .ok_or(FormatError::BlockCoverage { block: i })?;
    }
    if next != table_len {
        return Err(FormatError::BlockCoverage {
            block: blocks.len(),
        });
    }
    Ok(())
}

fn check_location(
    loc: FrameLocation,
    index_offset: u64,
    what: &'static str,
) -> Result<FrameLocation, FormatError> {
    if loc.fits_below(index_offset) {
        Ok(loc)
    } else {
        Err(FormatError::BadFrameLocation { what })
    }
}

fn overlaps(a: FrameLocation, b: FrameLocation) -> bool {
    a.offset < b.offset.saturating_add(b.len) && b.offset < a.offset.saturating_add(a.len)
}

/// The recovery frames overlap no block, the entry table, the records frame
/// and each other.
fn check_recovery_disjoint(
    blocks: &[BlockLocation],
    entry: FrameLocation,
    records: Option<FrameLocation>,
    recovery: &[FrameLocation],
) -> Result<(), FormatError> {
    let bad = FormatError::BadFrameLocation { what: "recovery" };
    for (i, r) in recovery.iter().enumerate() {
        let hits_block = blocks.iter().any(|b| {
            overlaps(
                *r,
                FrameLocation {
                    offset: b.frame_offset,
                    len: b.frame_len,
                    sequence: b.sequence,
                },
            )
        });
        if hits_block
            || overlaps(*r, entry)
            || records.is_some_and(|x| overlaps(*r, x))
            || recovery[..i].iter().any(|o| overlaps(*r, *o))
        {
            return Err(bad);
        }
    }
    Ok(())
}

/// The entry-table and records frames overlap no block and each other.
fn check_disjoint(
    blocks: &[BlockLocation],
    entry: FrameLocation,
    records: Option<FrameLocation>,
) -> Result<(), FormatError> {
    for b in blocks {
        let loc = FrameLocation {
            offset: b.frame_offset,
            len: b.frame_len,
            sequence: b.sequence,
        };
        if overlaps(loc, entry) {
            return Err(FormatError::BadFrameLocation {
                what: "entry table",
            });
        }
        if records.is_some_and(|r| overlaps(loc, r)) {
            return Err(FormatError::BadFrameLocation { what: "records" });
        }
    }
    if records.is_some_and(|r| overlaps(entry, r)) {
        return Err(FormatError::BadFrameLocation { what: "records" });
    }
    Ok(())
}

impl Index {
    /// Encode the payload. The rules a reader applies are checked first
    /// (except the upper bound on frame offsets, which depends on where the
    /// index is written), so an `Index` that encodes also parses.
    pub fn encode(&self) -> Result<Vec<u8>, FormatError> {
        ChunkTable::parse(&self.chunk_table)?.validate()?;
        let mut out = Vec::with_capacity(self.chunk_table.len() + 32 + 16 + self.blocks.len() * 20);
        out.extend_from_slice(&self.chunk_table);
        out.extend_from_slice(&self.merkle_root);
        self.envelope.write(&mut out)?;
        varint::write(&mut out, self.priors.len() as u64)?;
        for id in &self.priors {
            out.extend_from_slice(id);
        }
        varint::write(&mut out, self.blocks.len() as u64)?;
        for b in &self.blocks {
            for v in [
                b.frame_offset,
                b.frame_len,
                b.first_chunk,
                b.chunk_count,
                b.plain_len,
                b.sequence,
            ] {
                varint::write(&mut out, v)?;
            }
        }
        varint::write(&mut out, self.entry_table.offset)?;
        varint::write(&mut out, self.entry_table.len)?;
        varint::write(&mut out, self.entry_table.sequence)?;
        out.extend_from_slice(&self.entry_table_hash);
        let rec = self.records.unwrap_or(FrameLocation {
            offset: 0,
            len: 0,
            sequence: 0,
        });
        varint::write(&mut out, rec.offset)?;
        varint::write(&mut out, rec.len)?;
        varint::write(&mut out, rec.sequence)?;
        varint::write(&mut out, self.recovery.len() as u64)?;
        for f in &self.recovery {
            varint::write(&mut out, f.offset)?;
            varint::write(&mut out, f.len)?;
            varint::write(&mut out, f.sequence)?;
        }
        varint::write(&mut out, self.generations.len() as u64)?;
        for g in &self.generations {
            varint::write(&mut out, g.generation)?;
            varint::write(&mut out, g.start_offset)?;
            varint::write(&mut out, g.first_sequence)?;
            out.extend_from_slice(&g.salt);
        }
        Self::parse(&out, u64::MAX)?;
        Ok(out)
    }

    /// Parse and validate an index payload of generation 0 found at `index_offset`: the block
    /// count is bounded by the bytes left, the blocks must partition the chunk
    /// table, lie between the header and the index, and carry the right
    /// lengths, the envelope must agree with the block table and the payload
    /// length, and the stored Merkle root must match the table.
    pub fn parse(payload: &[u8], index_offset: u64) -> Result<Index, FormatError> {
        Self::parse_with_chunks(payload, index_offset).map(|(index, _)| index)
    }

    /// Like [`Index::parse`], and also return the chunk index. After the
    /// table's end has been found, one pass over the records checks the block
    /// lengths, collects the hashes for the Merkle root and builds the chunk
    /// index.
    pub fn parse_with_chunks(
        payload: &[u8],
        index_offset: u64,
    ) -> Result<(Index, ChunkIndex), FormatError> {
        Self::parse_with_chunks_in(payload, index_offset, 0, 0)
    }

    /// [`Index::parse_with_chunks`] for the index of generation `generation`
    /// whose trailer names `previous_trailer_offset`. The last recovery frame
    /// ends where the index starts; in a later generation that wrote no
    /// recovery frame it is an earlier generation's, ending at or before the
    /// previous trailer.
    pub fn parse_with_chunks_in(
        payload: &[u8],
        index_offset: u64,
        generation: u64,
        previous_trailer_offset: u64,
    ) -> Result<(Index, ChunkIndex), FormatError> {
        let (table, used) = ChunkTable::parse_prefix(payload)?;
        let mut s = &payload[used..];
        let (root, rest) = s
            .split_first_chunk::<32>()
            .ok_or(FormatError::Truncated { what: WHAT })?;
        let merkle_root_stored = *root;
        s = rest;
        let envelope = Envelope::read(&mut s)?;
        let priors = read_priors(&mut s)?;
        let count = rv(&mut s)?;
        if count > (s.len() / MIN_BLOCK_LEN) as u64 {
            return Err(FormatError::Truncated { what: WHAT });
        }
        let mut blocks = Vec::with_capacity(count as usize);
        for _ in 0..count {
            blocks.push(BlockLocation {
                frame_offset: rv(&mut s)?,
                frame_len: rv(&mut s)?,
                first_chunk: rv(&mut s)?,
                chunk_count: rv(&mut s)?,
                plain_len: rv(&mut s)?,
                sequence: rv(&mut s)?,
            });
        }
        let entry_table = FrameLocation {
            offset: rv(&mut s)?,
            len: rv(&mut s)?,
            sequence: rv(&mut s)?,
        };
        let (hash, rest) = s
            .split_first_chunk::<32>()
            .ok_or(FormatError::Truncated { what: WHAT })?;
        let entry_table_hash = *hash;
        s = rest;
        let rec = FrameLocation {
            offset: rv(&mut s)?,
            len: rv(&mut s)?,
            sequence: rv(&mut s)?,
        };
        let recovery_count = rv(&mut s)?;
        if recovery_count > (s.len() / MIN_LOCATION_LEN) as u64 {
            return Err(FormatError::Truncated { what: WHAT });
        }
        let mut recovery = Vec::with_capacity(recovery_count as usize);
        for _ in 0..recovery_count {
            recovery.push(FrameLocation {
                offset: rv(&mut s)?,
                len: rv(&mut s)?,
                sequence: rv(&mut s)?,
            });
        }
        let gen_count = rv(&mut s)?;
        if gen_count > (s.len() / MIN_GENERATION_LEN) as u64 {
            return Err(FormatError::Truncated { what: WHAT });
        }
        let mut generations: Vec<GenerationInfo> = Vec::with_capacity(gen_count as usize);
        for i in 0..gen_count {
            let g = rv(&mut s)?;
            let start_offset = rv(&mut s)?;
            let first_sequence = rv(&mut s)?;
            let (salt, rest) = s
                .split_first_chunk::<16>()
                .ok_or(FormatError::Truncated { what: WHAT })?;
            s = rest;
            let ordered = generations
                .last()
                .is_none_or(|p| start_offset > p.start_offset && first_sequence > p.first_sequence);
            if g != i || !ordered {
                return Err(FormatError::BadGenerationTable);
            }
            generations.push(GenerationInfo {
                generation: g,
                start_offset,
                first_sequence,
                salt: *salt,
            });
        }
        if !s.is_empty() {
            return Err(FormatError::TrailingBytes { what: WHAT });
        }

        validate_blocks(&blocks, table.len(), index_offset)?;
        let entry_table = check_location(entry_table, index_offset, "entry table")?;
        let records = if rec.offset == 0 && rec.len == 0 && rec.sequence == 0 {
            None
        } else {
            Some(check_location(rec, index_offset, "records")?)
        };

        check_disjoint(&blocks, entry_table, records)?;
        for r in &recovery {
            check_location(*r, index_offset, "recovery")?;
        }
        check_recovery_disjoint(&blocks, entry_table, records, &recovery)?;
        // Recovery frames ascend, and the last one ends where the index starts
        // (the last group closes before the index). The end is checked only
        // where the index's offset is known (`encode` passes `u64::MAX`).
        let ascending = recovery.windows(2).all(|w| w[0].offset < w[1].offset);
        let latest_start = generations.last().map(|g| g.start_offset);
        let ends_at_index = index_offset == u64::MAX
            || recovery.last().is_none_or(|l| {
                let end = l.offset.saturating_add(l.len);
                end == index_offset
                    || (generation > 0
                        && latest_start.is_some_and(|s| l.offset < s)
                        && end <= previous_trailer_offset)
            });
        if !(ascending && ends_at_index) {
            return Err(FormatError::BadFrameLocation { what: "recovery" });
        }

        let chunk_table: Arc<[u8]> = Arc::from(&payload[..used]);
        let (chunks, leaves) = ChunkIndex::build_with_leaves(Arc::clone(&chunk_table), &blocks)?;
        if merkle_root(&leaves) != merkle_root_stored {
            return Err(FormatError::MerkleRootMismatch);
        }
        envelope.validate(&blocks, entry_table, records, payload.len() as u64)?;
        envelope.validate_recovery(&recovery)?;
        Ok((
            Index {
                chunk_table,
                merkle_root: merkle_root_stored,
                envelope,
                priors,
                blocks,
                entry_table,
                entry_table_hash,
                records,
                recovery,
                generations,
            },
            chunks,
        ))
    }

    /// Build the random-access chunk index in one pass over the table.
    ///
    /// Memory: see [`ChunkIndex`]; the result shares the table bytes with this
    /// `Index` (an `Arc`), so it adds only the chunk index's 16 bytes per
    /// chunk. `parse_with_chunks` additionally holds a temporary 32 bytes per
    /// chunk for the Merkle leaves.
    pub fn chunk_index(&self) -> Result<ChunkIndex, FormatError> {
        let n = ChunkTable::parse(&self.chunk_table)?.len();
        validate_blocks(&self.blocks, n, u64::MAX)?;
        ChunkIndex::build(Arc::clone(&self.chunk_table), &self.blocks)
    }
}

/// The Markdown table of the index payload, pasted verbatim into the spec.
pub fn index_layout_table() -> String {
    format!(
        "| Field | Size | Meaning |\n|---|---|---|\n\
         | chunk_table | variable | the chunk table of section 5 (count, then the records) |\n\
         | merkle_root | 32 | the Merkle root over the chunk table's hashes |\n\
         | max_window | varint | the decode envelope of section 7: largest match-finder window, in bytes |\n\
         | max_bwt_block | varint | envelope: largest BWT block, in bytes (0 when none) |\n\
         | max_block_plain | varint | envelope: largest block `plain_len`; must equal the maximum over the block table |\n\
         | max_frame_payload | varint | envelope: largest frame payload; must admit the index's own payload and every recorded frame (section 7) |\n\
         | decode_memory | varint | envelope: the writer's estimate of peak decoder memory per thread, in bytes |\n\
         | threads_hint | varint | envelope: independent blocks decodable at once; 0 = no hint |\n\
         | prior_list | variable | the prior list of section 10 (`prior_count`, then that many 32-byte IDs); follows the envelope |\n\
         | block_count | varint | number of blocks; at most the bytes left after it divided by {MIN_BLOCK_LEN} |\n\
         | frame_offset | varint | per block: absolute offset of the block's `ChunkData` frame |\n\
         | frame_len | varint | per block: whole encoded length of that frame |\n\
         | first_chunk | varint | per block: index of the first chunk the block holds |\n\
         | chunk_count | varint | per block: number of chunks the block holds |\n\
         | plain_len | varint | per block: sum of the `plain_len` of its chunks |\n\
         | sequence | varint | per block: position of the block's frame among the archive's frames (section 14) |\n\
         | entry_table_offset | varint | absolute offset of the `EntryTable` frame |\n\
         | entry_table_len | varint | whole encoded length of that frame |\n\
         | entry_table_sequence | varint | position of that frame among the archive's frames |\n\
         | entry_table_hash | 32 | BLAKE3-256 of the entry table's payload as stored (the sealed bytes when sealed) |\n\
         | records_offset | varint | absolute offset of the `Records` frame; 0 when there is none |\n\
         | records_len | varint | whole encoded length of that frame; 0 when there is none |\n\
         | records_sequence | varint | position of that frame among the archive's frames; 0 when there is none |\n\
         | recovery_count | varint | number of `Recovery` frames (section 13); at most the bytes left after it divided by {MIN_LOCATION_LEN} |\n\
         | recovery_offset | varint | per recovery frame: absolute offset of the frame |\n\
         | recovery_len | varint | per recovery frame: whole encoded length of that frame |\n\
         | recovery_sequence | varint | per recovery frame: position of that frame among the archive's frames |\n\
         | generation_count | varint | number of generations (section 15); at most the bytes left after it divided by {MIN_GENERATION_LEN} |\n\
         | generation | varint | per generation: its number; the entries count 0, 1, 2, ... |\n\
         | start_offset | varint | per generation: absolute offset of its first frame; strictly ascending |\n\
         | first_sequence | varint | per generation: sequence of that first frame; strictly ascending |\n\
         | salt | 16 | per generation: the salt of its sealed frames' nonces (zeros when not encrypted) |\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkRecord, ChunkTableWriter};

    fn rec(plain_len: u64, b: u8) -> ChunkRecord {
        ChunkRecord {
            plain_len,
            hash: [b; 32],
        }
    }

    fn table_of(recs: &[ChunkRecord]) -> Vec<u8> {
        ChunkTableWriter::encode(recs)
    }

    /// An index over `recs` split into blocks of the given chunk counts.
    fn make(recs: &[ChunkRecord], counts: &[u64]) -> Index {
        let mut blocks = Vec::new();
        let mut first = 0u64;
        for (i, &c) in counts.iter().enumerate() {
            let plain: u64 = recs[first as usize..(first + c) as usize]
                .iter()
                .map(|r| r.plain_len)
                .sum();
            blocks.push(BlockLocation {
                frame_offset: 100 + i as u64 * 100,
                frame_len: 80,
                first_chunk: first,
                chunk_count: c,
                plain_len: plain,
                sequence: 0,
            });
            first += c;
        }
        let leaves: Vec<[u8; 32]> = recs.iter().map(|r| r.hash).collect();
        let mut idx = Index {
            chunk_table: table_of(recs).into(),
            merkle_root: merkle_root(&leaves),
            envelope: Envelope::for_archive(
                &blocks,
                crate::envelope::ArchiveSizes {
                    index_payload_len: 0,
                    entry_table_len: 60,
                    records_len: 0,
                    recovery_len: 0,
                },
                crate::primitive::GraphResources {
                    window: 1 << 20,
                    bwt_block: 0,
                },
                1 << 24,
                0,
            ),
            priors: vec![],
            blocks,
            entry_table: FrameLocation {
                offset: 32,
                len: 60,
                sequence: 0,
            },
            entry_table_hash: [0; 32],
            records: None,
            recovery: vec![],
            generations: vec![],
        };
        fit_envelope(&mut idx);
        idx
    }

    /// Set the envelope the way a writer does: `max_frame_payload` has to cover the
    /// index payload, which contains it, so iterate until it is stable.
    fn fit_envelope(idx: &mut Index) {
        for _ in 0..6 {
            let len = encode_unchecked(idx).len() as u64;
            let e = Envelope::for_archive(
                &idx.blocks,
                crate::envelope::ArchiveSizes {
                    index_payload_len: len,
                    entry_table_len: idx.entry_table.len,
                    records_len: idx.records.map_or(0, |r| r.len),
                    recovery_len: idx.recovery.iter().map(|r| r.len).max().unwrap_or(0),
                },
                crate::primitive::GraphResources {
                    window: idx.envelope.max_window,
                    bwt_block: idx.envelope.max_bwt_block,
                },
                idx.envelope.decode_memory,
                idx.envelope.threads_hint,
            );
            if e == idx.envelope {
                return;
            }
            idx.envelope = e;
        }
        panic!("envelope did not settle");
    }

    const IDX_AT: u64 = 10_000;

    fn three() -> (Vec<ChunkRecord>, Index) {
        let recs = vec![rec(5, 1), rec(0, 2), rec(7, 3), rec(9, 4), rec(1, 5)];
        let idx = make(&recs, &[2, 2, 1]);
        (recs, idx)
    }

    #[test]
    fn round_trip_zero_one_three_blocks() {
        let cases: [(Vec<ChunkRecord>, Vec<u64>); 4] = [
            (vec![], vec![]),
            (vec![], vec![0]),
            (vec![rec(3, 1), rec(4, 2)], vec![2]),
            (
                vec![rec(5, 1), rec(0, 2), rec(7, 3), rec(9, 4)],
                vec![1, 2, 1],
            ),
        ];
        for (recs, counts) in cases {
            let mut idx = make(&recs, &counts);
            for records in [
                None,
                Some(FrameLocation {
                    offset: 500,
                    len: 50,
                    sequence: 0,
                }),
            ] {
                idx.records = records;
                fit_envelope(&mut idx);
                let p = idx.encode().unwrap();
                assert_eq!(Index::parse(&p, IDX_AT).unwrap(), idx);
            }
        }
    }

    #[test]
    fn recovery_locations_round_trip_and_rules() {
        let (_, mut idx) = three();
        idx.recovery = vec![
            FrameLocation {
                offset: 600,
                len: 90,
                sequence: 0,
            },
            FrameLocation {
                offset: 9910,
                len: 90,
                sequence: 0,
            },
        ];
        fit_envelope(&mut idx);
        let p = idx.encode().unwrap();
        assert_eq!(Index::parse(&p, IDX_AT).unwrap(), idx);
        // Overlapping a block (block 0 is at 100..180) or each other.
        let mut o = idx.clone();
        o.recovery[0].offset = 150;
        assert!(matches!(
            parse_err(&o),
            FormatError::BadFrameLocation { what: "recovery" }
        ));
        let mut o = idx.clone();
        o.recovery[1].offset = 650;
        assert!(matches!(
            parse_err(&o),
            FormatError::BadFrameLocation { what: "recovery" }
        ));
        // Past the index, or before the header.
        let mut o = idx.clone();
        o.recovery[1].offset = IDX_AT;
        assert!(matches!(
            parse_err(&o),
            FormatError::BadFrameLocation { what: "recovery" }
        ));
        // Too long for max_frame_payload.
        let mut o = idx.clone();
        o.recovery[1].len = 5000;
        o.recovery[1].offset = 5000;
        assert!(matches!(
            parse_err(&o),
            FormatError::EnvelopeMismatch {
                field: "max_frame_payload"
            }
        ));
        // The last frame must end where the index starts, and the frames ascend.
        let mut o = idx.clone();
        o.recovery[1].offset = 9000;
        assert!(matches!(
            parse_err(&o),
            FormatError::BadFrameLocation { what: "recovery" }
        ));
        let mut o = idx.clone();
        o.recovery.swap(0, 1);
        assert!(matches!(
            parse_err(&o),
            FormatError::BadFrameLocation { what: "recovery" }
        ));
        // A count larger than the bytes left.
        let mut bytes = encode_unchecked(&idx);
        let n = bytes.len();
        bytes.truncate(n - 4);
        bytes.extend_from_slice(&[9, 1]);
        assert!(Index::parse(&bytes, IDX_AT).is_err());
    }

    fn encode_unchecked(idx: &Index) -> Vec<u8> {
        // Same layout as `encode`, without validation.
        let mut out = idx.chunk_table.to_vec();
        out.extend_from_slice(&idx.merkle_root);
        idx.envelope.write(&mut out).unwrap();
        varint::write(&mut out, idx.priors.len() as u64).unwrap();
        for id in &idx.priors {
            out.extend_from_slice(id);
        }
        varint::write(&mut out, idx.blocks.len() as u64).unwrap();
        for b in &idx.blocks {
            for v in [
                b.frame_offset,
                b.frame_len,
                b.first_chunk,
                b.chunk_count,
                b.plain_len,
                b.sequence,
            ] {
                varint::write(&mut out, v).unwrap();
            }
        }
        for v in [
            idx.entry_table.offset,
            idx.entry_table.len,
            idx.entry_table.sequence,
        ] {
            varint::write(&mut out, v).unwrap();
        }
        out.extend_from_slice(&idx.entry_table_hash);
        let r = idx.records.unwrap_or(FrameLocation {
            offset: 0,
            len: 0,
            sequence: 0,
        });
        varint::write(&mut out, r.offset).unwrap();
        varint::write(&mut out, r.len).unwrap();
        varint::write(&mut out, r.sequence).unwrap();
        varint::write(&mut out, idx.recovery.len() as u64).unwrap();
        for f in &idx.recovery {
            varint::write(&mut out, f.offset).unwrap();
            varint::write(&mut out, f.len).unwrap();
            varint::write(&mut out, f.sequence).unwrap();
        }
        varint::write(&mut out, 0).unwrap();
        out
    }

    fn parse_err(idx: &Index) -> FormatError {
        Index::parse(&encode_unchecked(idx), IDX_AT).unwrap_err()
    }

    #[test]
    fn length_mismatch() {
        let (_, mut idx) = three();
        idx.blocks[1].plain_len += 1;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockLengthMismatch { block: 1 }
        ));
        assert!(matches!(
            idx.encode(),
            Err(FormatError::BlockLengthMismatch { block: 1 })
        ));
        assert!(matches!(
            idx.chunk_index(),
            Err(FormatError::BlockLengthMismatch { block: 1 })
        ));
    }

    #[test]
    fn gap_and_overlap() {
        let (_, mut idx) = three();
        // Gap: block 1 starts one chunk late, block 2 shrinks to still end at the table's end.
        idx.blocks[1].first_chunk = 3;
        idx.blocks[1].chunk_count = 1;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockCoverage { block: 1 }
        ));
        // Overlap: block 2 starts inside block 1.
        let (_, mut idx) = three();
        idx.blocks[2].first_chunk = 3;
        idx.blocks[2].chunk_count = 2;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockCoverage { block: 2 }
        ));
        // Blocks run short of the table.
        let (_, mut idx) = three();
        idx.blocks.pop();
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockCoverage { block: 2 }
        ));
        // Blocks run past the table.
        let (_, mut idx) = three();
        idx.blocks[2].chunk_count = 2;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockCoverage { block: 2 }
        ));
        // No blocks at all although chunks exist.
        let (_, mut idx) = three();
        idx.blocks.clear();
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockCoverage { block: 0 }
        ));
    }

    #[test]
    fn empty_block_with_chunks_present() {
        let (recs, _) = three();
        let mut idx = make(&recs, &[2, 2, 1]);
        idx.blocks[1].chunk_count = 0;
        idx.blocks[1].plain_len = 0;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockCoverage { block: 1 }
        ));
    }

    #[test]
    fn frame_offset_bounds() {
        let (_, mut idx) = three();
        idx.blocks[0].frame_offset = Header::LEN as u64 - 1;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockOutOfRange { block: 0 }
        ));
        let (_, mut idx) = three();
        idx.blocks[2].frame_offset = IDX_AT - 79;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockOutOfRange { block: 2 }
        ));
        // Exactly touching the index is fine; overflow is out of range.
        idx.blocks[2].frame_offset = IDX_AT - 80;
        Index::parse(&encode_unchecked(&idx), IDX_AT).unwrap();
        idx.blocks[2].frame_offset = u64::MAX;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockOutOfRange { block: 2 }
        ));
        // A frame shorter than the smallest frame cannot be a frame.
        let (_, mut idx) = three();
        idx.blocks[1].frame_len = MIN_FRAME_LEN - 1;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockOutOfRange { block: 1 }
        ));
    }

    #[test]
    fn entry_and_records_locations() {
        let (_, mut idx) = three();
        idx.entry_table.offset = 0;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BadFrameLocation {
                what: "entry table"
            }
        ));
        let (_, mut idx) = three();
        idx.records = Some(FrameLocation {
            offset: IDX_AT,
            len: 40,
            sequence: 0,
        });
        assert!(matches!(
            parse_err(&idx),
            FormatError::BadFrameLocation { what: "records" }
        ));
        idx.records = Some(FrameLocation {
            offset: 0,
            len: 40,
            sequence: 0,
        });
        assert!(matches!(
            parse_err(&idx),
            FormatError::BadFrameLocation { what: "records" }
        ));
    }

    #[test]
    fn frames_ascend_and_do_not_overlap() {
        // Block 1 starts inside block 0's frame.
        let (_, mut idx) = three();
        idx.blocks[1].frame_offset = idx.blocks[0].frame_offset + 10;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockCoverage { block: 1 }
        ));
        // Blocks out of order.
        let (_, mut idx) = three();
        idx.blocks.swap(0, 2);
        idx.blocks[0].first_chunk = 0;
        idx.blocks[2].first_chunk = 4;
        let e = parse_err(&idx);
        assert!(
            matches!(e, FormatError::BlockCoverage { block: 1 }),
            "{e:?}"
        );
        // The entry table over a block, records over the entry table.
        let (_, mut idx) = three();
        idx.entry_table.offset = idx.blocks[1].frame_offset + 5;
        assert!(matches!(
            parse_err(&idx),
            FormatError::BadFrameLocation {
                what: "entry table"
            }
        ));
        let (_, mut idx) = three();
        idx.records = Some(idx.entry_table);
        assert!(matches!(
            parse_err(&idx),
            FormatError::BadFrameLocation { what: "records" }
        ));
        idx.records = Some(idx.entry_table);
        idx.records = Some(FrameLocation {
            offset: idx.blocks[2].frame_offset,
            len: 80,
            sequence: 0,
        });
        assert!(matches!(
            parse_err(&idx),
            FormatError::BadFrameLocation { what: "records" }
        ));
    }

    #[test]
    fn at_most_one_empty_block_and_only_without_chunks() {
        let mut idx = make(&[], &[0, 0]);
        assert!(matches!(
            parse_err(&idx),
            FormatError::BlockCoverage { block: 1 }
        ));
        assert!(idx.encode().is_err());
        idx.blocks.pop();
        idx.encode().unwrap();
    }

    #[test]
    fn merkle_root_mismatch() {
        let (_, mut idx) = three();
        idx.merkle_root[0] ^= 1;
        assert!(matches!(parse_err(&idx), FormatError::MerkleRootMismatch));
        assert!(matches!(idx.encode(), Err(FormatError::MerkleRootMismatch)));
    }

    #[test]
    fn trailing_and_truncated() {
        let (_, idx) = three();
        let mut p = idx.encode().unwrap();
        p.push(0);
        assert!(matches!(
            Index::parse(&p, IDX_AT),
            Err(FormatError::TrailingBytes { what: "index" })
        ));
        let p = idx.encode().unwrap();
        for cut in [1usize, 3, 6] {
            let e = Index::parse(&p[..p.len() - cut], IDX_AT).unwrap_err();
            assert!(
                matches!(e, FormatError::Truncated { what: "index" }),
                "cut {cut}: {e:?}"
            );
        }
        // Cut right after the Merkle root.
        let used = idx.chunk_table.len() + 32;
        assert!(matches!(
            Index::parse(&p[..used], IDX_AT),
            Err(FormatError::Truncated { what: "index" })
        ));
        assert!(matches!(
            Index::parse(&p[..used - 1], IDX_AT),
            Err(FormatError::Truncated { what: "index" })
        ));
    }

    #[test]
    fn truncation_inside_the_envelope() {
        let (_, idx) = three();
        let p = idx.encode().unwrap();
        let start = idx.chunk_table.len() + 32;
        let mut env = Vec::new();
        idx.envelope.write(&mut env).unwrap();
        for cut in start..start + env.len() {
            let e = Index::parse(&p[..cut], IDX_AT).unwrap_err();
            assert!(
                matches!(e, FormatError::Truncated { what: "index" }),
                "cut {cut}: {e:?}"
            );
        }
    }

    #[test]
    fn envelope_understating_the_archive_is_a_mismatch() {
        let (_, idx) = three();
        // The three() blocks: plain 5, 16, 1; frames of 80 bytes (payload 43).
        let len = encode_unchecked(&idx).len() as u64;
        assert!((128..16_384).contains(&len));
        assert_eq!(idx.envelope.max_block_plain, 16);
        // The derived value: the index payload is the largest frame.
        assert_eq!(idx.envelope.max_frame_payload, len);
        for (tweak, field) in [
            (
                (|i: &mut Index| i.envelope.max_block_plain = 15) as fn(&mut Index),
                "max_block_plain",
            ),
            (|i| i.envelope.max_block_plain = 17, "max_block_plain"),
            (|i| i.envelope.max_frame_payload = 42, "max_frame_payload"),
            // Smaller than the index's own payload.
            (|i| i.envelope.max_frame_payload = 100, "max_frame_payload"),
        ] {
            let mut i = idx.clone();
            tweak(&mut i);
            let e = parse_err(&i);
            assert!(
                matches!(e, FormatError::EnvelopeMismatch { field: f } if f == field),
                "{field}: {e:?}"
            );
            assert!(matches!(
                i.encode(),
                Err(FormatError::EnvelopeMismatch { .. })
            ));
        }
        // Exactly the payload length is enough, one less is not (same varint size).
        let mut i = idx.clone();
        Index::parse(&encode_unchecked(&i), IDX_AT).unwrap();
        i.envelope.max_frame_payload = len - 1;
        assert!(matches!(
            parse_err(&i),
            FormatError::EnvelopeMismatch {
                field: "max_frame_payload"
            }
        ));
        // The writer's own derivation parses and fits the index payload.
        let mut i = idx.clone();
        i.envelope.max_window = 1;
        fit_envelope(&mut i);
        assert_eq!(i.envelope.max_block_plain, 16);
        i.encode().unwrap();
    }

    #[test]
    fn entry_table_and_records_count_towards_max_frame_payload() {
        let (_, mut idx) = three();
        // The entry table is the largest frame by far.
        idx.entry_table = FrameLocation {
            offset: 5000,
            len: 4000,
            sequence: 0,
        };
        let stale = idx.envelope;
        assert!(matches!(
            parse_err(&idx),
            FormatError::EnvelopeMismatch {
                field: "max_frame_payload"
            }
        ));
        fit_envelope(&mut idx);
        assert_eq!(idx.envelope.max_frame_payload, 4000 - 36 - 2);
        assert_ne!(idx.envelope, stale);
        idx.encode().unwrap();
        // One short of it is a mismatch.
        idx.envelope.max_frame_payload -= 1;
        assert!(matches!(
            parse_err(&idx),
            FormatError::EnvelopeMismatch {
                field: "max_frame_payload"
            }
        ));
        // Likewise the records frame.
        let (_, mut idx) = three();
        idx.records = Some(FrameLocation {
            offset: 5000,
            len: 4000,
            sequence: 0,
        });
        assert!(matches!(
            parse_err(&idx),
            FormatError::EnvelopeMismatch {
                field: "max_frame_payload"
            }
        ));
        fit_envelope(&mut idx);
        assert_eq!(idx.envelope.max_frame_payload, 4000 - 36 - 2);
        idx.encode().unwrap();
    }

    #[test]
    fn huge_block_count_is_bounded_by_input() {
        let mut p = table_of(&[]);
        p.extend_from_slice(&[0u8; 32]);
        // The envelope's six varints, then an empty prior list.
        p.extend_from_slice(&[0u8; 7]);
        varint::write(&mut p, u64::MAX).unwrap();
        p.extend_from_slice(&[0u8; 20]);
        assert!(matches!(
            Index::parse(&p, IDX_AT),
            Err(FormatError::Truncated { what: "index" })
        ));
        // A count the remaining bytes cannot hold at five bytes per block.
        let mut p = table_of(&[]);
        p.extend_from_slice(&[0u8; 32]);
        p.extend_from_slice(&[0u8; 7]);
        p.push(5);
        p.extend_from_slice(&[0u8; 24]);
        assert!(matches!(
            Index::parse(&p, IDX_AT),
            Err(FormatError::Truncated { what: "index" })
        ));
    }

    #[test]
    fn encode_rejects_a_table_with_trailing_bytes() {
        let (_, mut idx) = three();
        let mut t = idx.chunk_table.to_vec();
        t.push(0);
        idx.chunk_table = t.into();
        assert!(matches!(
            idx.encode(),
            Err(FormatError::TrailingBytes {
                what: "chunk table"
            })
        ));
    }

    #[test]
    fn chunk_index_matches_table_for_every_record() {
        let recs: Vec<ChunkRecord> = (0..1000u64)
            .map(|i| rec(if i % 7 == 0 { 0 } else { i * 3 }, (i % 251) as u8))
            .collect();
        let idx = make(&recs, &[300, 1, 400, 299]);
        let ci = idx.chunk_index().unwrap();
        let t = ChunkTable::parse(&idx.chunk_table).unwrap();
        assert_eq!(ci.len(), 1000);
        for i in 0..1000u64 {
            assert_eq!(ci.record(i), t.get(i).unwrap(), "chunk {i}");
        }
        assert_eq!(ci.record(1000), None);
        assert_eq!(ci.locate(1000), None);
    }

    #[test]
    fn locate_first_middle_last_of_each_block() {
        let recs: Vec<ChunkRecord> = (0..10u64).map(|i| rec(i % 4, i as u8)).collect();
        let idx = make(&recs, &[3, 4, 3]);
        let ci = idx.chunk_index().unwrap();
        for (bi, b) in idx.blocks.iter().enumerate() {
            let last = b.first_chunk + b.chunk_count - 1;
            let mid = b.first_chunk + b.chunk_count / 2;
            for c in [b.first_chunk, mid, last] {
                let before: u64 = recs[b.first_chunk as usize..c as usize]
                    .iter()
                    .map(|r| r.plain_len)
                    .sum();
                let p = ci.locate(c).unwrap();
                assert_eq!(p.block, bi);
                assert_eq!(p.offset_in_block, before);
                assert_eq!(p.plain_len, recs[c as usize].plain_len);
            }
        }
    }

    #[test]
    fn zero_length_chunks_share_offsets() {
        let recs = vec![rec(0, 1), rec(0, 2), rec(5, 3), rec(0, 4), rec(2, 5)];
        let idx = make(&recs, &[5]);
        let ci = idx.chunk_index().unwrap();
        let offs: Vec<u64> = (0..5)
            .map(|c| ci.locate(c).unwrap().offset_in_block)
            .collect();
        assert_eq!(offs, vec![0, 0, 0, 5, 5]);
    }

    #[test]
    fn chunk_index_of_empty_table() {
        let idx = make(&[], &[0]);
        let ci = idx.chunk_index().unwrap();
        assert!(ci.is_empty());
        assert_eq!(ci.locate(0), None);
    }

    #[test]
    fn scale_million_chunks_in_thousand_blocks() {
        let recs: Vec<ChunkRecord> = (0..1_000_000u64)
            .map(|i| rec(i % 1000 + 1, (i % 251) as u8))
            .collect();
        let mut idx = make(&recs, &[1000; 1000]);
        for (i, b) in idx.blocks.iter_mut().enumerate() {
            b.frame_offset = 100 + i as u64 * 100;
        }
        let p = idx.encode().unwrap();
        let big = 1u64 << 40;
        let parsed = Index::parse(&p, big).unwrap();
        assert_eq!(parsed.blocks.len(), 1000);
        let ci = parsed.chunk_index().unwrap();
        assert_eq!(ci.len(), 1_000_000);
        let last = ci.locate(999_999).unwrap();
        assert_eq!(last.block, 999);
        assert_eq!(last.plain_len, 1000);
        let before: u64 = (0..999u64).map(|k| k + 1).sum();
        assert_eq!(last.offset_in_block, before);
        assert_eq!(ci.record(999_999), Some(recs[999_999]));
    }

    fn with_priors(priors: Vec<[u8; 32]>) -> Index {
        let (_, mut idx) = three();
        idx.priors = priors;
        fit_envelope(&mut idx);
        idx
    }

    #[test]
    fn prior_list_round_trips() {
        for priors in [vec![], vec![[1u8; 32]], vec![[1u8; 32], [2; 32], [9; 32]]] {
            let idx = with_priors(priors.clone());
            let bytes = idx.encode().unwrap();
            let back = Index::parse(&bytes, IDX_AT).unwrap();
            assert_eq!(back.priors, priors);
            assert_eq!(back, idx);
        }
    }

    #[test]
    fn prior_list_must_ascend_and_be_nonzero() {
        let reason = |priors: Vec<[u8; 32]>| match parse_err(&with_priors(priors)) {
            FormatError::BadPriorList { reason } => reason,
            e => panic!("unexpected {e:?}"),
        };
        assert_eq!(reason(vec![[2; 32], [1; 32]]), "not ascending and unique");
        assert_eq!(reason(vec![[1; 32], [1; 32]]), "not ascending and unique");
        assert_eq!(reason(vec![[0; 32]]), "zero id");
        // `encode` refuses what a reader would refuse.
        assert!(matches!(
            with_priors(vec![[2; 32], [1; 32]]).encode(),
            Err(FormatError::BadPriorList { .. })
        ));
    }

    #[test]
    fn prior_count_is_bounded_by_the_bytes_left() {
        let mut p = table_of(&[]);
        p.extend_from_slice(&[0u8; 32]);
        p.extend_from_slice(&[0u8; 6]);
        varint::write(&mut p, 2).unwrap();
        p.extend_from_slice(&[1u8; 63]);
        assert!(matches!(
            Index::parse(&p, IDX_AT),
            Err(FormatError::Truncated { what: "index" })
        ));
    }

    #[test]
    fn layout_table_lists_every_field() {
        let t = index_layout_table();
        for f in [
            "prior_list",
            "chunk_table",
            "merkle_root",
            "block_count",
            "frame_offset",
            "entry_table_offset",
            "records_len",
        ] {
            assert!(t.contains(f), "{f}");
        }
    }
}
