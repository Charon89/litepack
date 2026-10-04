//! The chunk table (section 5), the index payload (section 6), the envelope (section 7), the prior
//! list (section 10) and the generation table (section 15).

use crate::error::{Error, Result};
use crate::merkle;
use crate::wire::{varint_len, Cursor, HEADER_LEN, MIN_FRAME_LEN};

/// One chunk record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRec {
    /// Length of the original data.
    pub plain_len: u64,
    /// BLAKE3 of the original data.
    pub hash: [u8; 32],
}

/// The decode envelope (section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Envelope {
    /// Largest window.
    pub max_window: u64,
    /// Largest BWT block.
    pub max_bwt_block: u64,
    /// Largest block plain length.
    pub max_block_plain: u64,
    /// Largest frame payload.
    pub max_frame_payload: u64,
    /// Decoder memory per thread.
    pub decode_memory: u64,
    /// Independent blocks at once.
    pub threads_hint: u64,
}

/// A block location (section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockLoc {
    /// Offset of the `ChunkData` frame.
    pub frame_offset: u64,
    /// Whole length of the frame.
    pub frame_len: u64,
    /// First chunk.
    pub first_chunk: u64,
    /// Number of chunks.
    pub chunk_count: u64,
    /// Sum of the chunks' plain lengths.
    pub plain_len: u64,
    /// Frame sequence.
    pub sequence: u64,
}

/// A frame location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loc {
    /// Absolute offset.
    pub offset: u64,
    /// Whole encoded length.
    pub len: u64,
    /// Frame sequence.
    pub sequence: u64,
}

impl Loc {
    /// The end offset (saturating).
    pub fn end(&self) -> u64 {
        self.offset.saturating_add(self.len)
    }
}

/// One generation of the generation table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenEntry {
    /// Generation number.
    pub generation: u64,
    /// Offset of its first frame.
    pub start_offset: u64,
    /// Sequence of its first frame.
    pub first_sequence: u64,
    /// Nonce salt.
    pub salt: [u8; 16],
}

/// A parsed and checked index.
#[derive(Debug, Clone)]
pub struct Index {
    /// The chunk table.
    pub chunks: Vec<ChunkRec>,
    /// Stored Merkle root.
    pub merkle_root: [u8; 32],
    /// The envelope.
    pub envelope: Envelope,
    /// Prior IDs.
    pub priors: Vec<[u8; 32]>,
    /// The block table.
    pub blocks: Vec<BlockLoc>,
    /// Entry table location.
    pub entry_table: Loc,
    /// BLAKE3 of the entry table payload as stored.
    pub entry_table_hash: [u8; 32],
    /// Records frame location.
    pub records: Option<Loc>,
    /// Recovery frame locations.
    pub recovery: Vec<Loc>,
    /// Generation table.
    pub generations: Vec<GenEntry>,
    /// Per chunk: its block.
    pub chunk_block: Vec<usize>,
    /// Per chunk: offset in its block's plain bytes.
    pub chunk_offset: Vec<u64>,
}

const WHAT: &str = "index";

fn loc(c: &mut Cursor<'_>) -> Result<Loc> {
    Ok(Loc {
        offset: c.varint()?,
        len: c.varint()?,
        sequence: c.varint()?,
    })
}

fn overlaps(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

fn location_ok(l: &Loc, index_offset: u64) -> bool {
    l.offset >= HEADER_LEN as u64
        && l.len >= MIN_FRAME_LEN
        && l.offset
            .checked_add(l.len)
            .is_some_and(|e| e <= index_offset)
}

/// Parses the chunk table from the cursor (section 5).
fn chunk_table(c: &mut Cursor<'_>) -> Result<Vec<ChunkRec>> {
    c.set_what("chunk table");
    let n = c.count(33)?;
    let mut v = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let plain_len = c.varint()?;
        let hash = c.array()?;
        v.push(ChunkRec { plain_len, hash });
    }
    c.set_what(WHAT);
    Ok(v)
}

/// Parses an index payload (plain bytes) of a frame at `index_offset` and checks the rules of
/// sections 6, 7 and 10.
pub fn parse_index(payload: &[u8], index_offset: u64) -> Result<Index> {
    let mut c = Cursor::new(payload, WHAT);
    let chunks = chunk_table(&mut c)?;
    let merkle_root = c.array()?;
    let envelope = Envelope {
        max_window: c.varint()?,
        max_bwt_block: c.varint()?,
        max_block_plain: c.varint()?,
        max_frame_payload: c.varint()?,
        decode_memory: c.varint()?,
        threads_hint: c.varint()?,
    };
    if envelope.threads_hint > u64::from(u32::MAX) {
        return Err(Error::new(
            "EnvelopeMismatch",
            "envelope field threads_hint above 4294967295",
        ));
    }
    let np = c.count(32)?;
    let mut priors: Vec<[u8; 32]> = Vec::with_capacity(np as usize);
    for _ in 0..np {
        let id: [u8; 32] = c.array()?;
        if id == [0; 32] || priors.last().is_some_and(|p| id <= *p) {
            return Err(Error::new(
                "BadPriorList",
                "prior list not ascending or zero id",
            ));
        }
        priors.push(id);
    }
    let nb = c.count(6)?;
    let mut blocks = Vec::with_capacity(nb as usize);
    for _ in 0..nb {
        blocks.push(BlockLoc {
            frame_offset: c.varint()?,
            frame_len: c.varint()?,
            first_chunk: c.varint()?,
            chunk_count: c.varint()?,
            plain_len: c.varint()?,
            sequence: c.varint()?,
        });
    }
    let entry_table = loc(&mut c)?;
    let entry_table_hash = c.array()?;
    let r = loc(&mut c)?;
    let records = if r.offset == 0 && r.len == 0 && r.sequence == 0 {
        None
    } else {
        Some(r)
    };
    let nr = c.count(3)?;
    let mut recovery = Vec::with_capacity(nr as usize);
    for _ in 0..nr {
        recovery.push(loc(&mut c)?);
    }
    let ng = c.count(19)?;
    let mut generations = Vec::with_capacity(ng as usize);
    for _ in 0..ng {
        generations.push(GenEntry {
            generation: c.varint()?,
            start_offset: c.varint()?,
            first_sequence: c.varint()?,
            salt: c.array()?,
        });
    }
    if c.remaining() != 0 {
        return Err(Error::trailing(WHAT));
    }

    // Block rules (section 6).
    let n = chunks.len() as u64;
    let mut chunk_block = vec![0usize; chunks.len()];
    let mut chunk_offset = vec![0u64; chunks.len()];
    let mut next = 0u64;
    let mut prev_end = 0u64;
    for (b, bl) in blocks.iter().enumerate() {
        if bl.first_chunk != next || (bl.chunk_count == 0 && (n != 0 || b > 0)) {
            return Err(Error::new(
                "BlockCoverage",
                format!("block {b} breaks coverage"),
            ));
        }
        let end = bl
            .first_chunk
            .checked_add(bl.chunk_count)
            .filter(|e| *e <= n)
            .ok_or_else(|| Error::new("BlockCoverage", format!("block {b} breaks coverage")))?;
        let mut sum = 0u64;
        for i in bl.first_chunk..end {
            let i = i as usize;
            chunk_block[i] = b;
            chunk_offset[i] = sum;
            sum = sum.checked_add(chunks[i].plain_len).ok_or_else(|| {
                Error::new("BlockLengthMismatch", format!("block {b} length overflows"))
            })?;
        }
        if sum != bl.plain_len {
            return Err(Error::new(
                "BlockLengthMismatch",
                format!(
                    "block {b} plain_len {} differs from its chunks {sum}",
                    bl.plain_len
                ),
            ));
        }
        let fl = Loc {
            offset: bl.frame_offset,
            len: bl.frame_len,
            sequence: bl.sequence,
        };
        if !location_ok(&fl, index_offset) {
            return Err(Error::new(
                "BlockOutOfRange",
                format!("block {b} out of range"),
            ));
        }
        if bl.frame_offset < prev_end {
            return Err(Error::new("BlockCoverage", format!("block {b} overlaps")));
        }
        prev_end = fl.end();
        next = end;
    }
    if next != n {
        return Err(Error::new(
            "BlockCoverage",
            format!("block {} breaks coverage", blocks.len()),
        ));
    }

    let block_ranges: Vec<(u64, u64)> = blocks
        .iter()
        .map(|b| (b.frame_offset, b.frame_offset + b.frame_len))
        .collect();
    let et = (entry_table.offset, entry_table.end());
    if !location_ok(&entry_table, index_offset) || block_ranges.iter().any(|r| overlaps(*r, et)) {
        return Err(Error::bad_location("entry table"));
    }
    if let Some(r) = &records {
        let rr = (r.offset, r.end());
        if !location_ok(r, index_offset)
            || block_ranges.iter().any(|b| overlaps(*b, rr))
            || overlaps(rr, et)
        {
            return Err(Error::bad_location("records"));
        }
    }
    let mut prev_rec_end = 0u64;
    for rl in &recovery {
        let rr = (rl.offset, rl.end());
        let bad = !location_ok(rl, index_offset)
            || rl.offset < prev_rec_end
            || block_ranges.iter().any(|b| overlaps(*b, rr))
            || overlaps(rr, et)
            || records.is_some_and(|r| overlaps(rr, (r.offset, r.end())));
        if bad {
            return Err(Error::bad_location("recovery"));
        }
        prev_rec_end = rl.end();
    }
    // The last recovery frame ends at the index, or (latest generation without recovery) at or
    // before the previous trailer.
    if let Some(last) = recovery.last() {
        let ok = last.end() == index_offset
            || (generations.len() > 1
                && generations
                    .last()
                    .is_some_and(|g| last.end() <= g.start_offset.saturating_sub(133)));
        if !ok {
            return Err(Error::bad_location("recovery"));
        }
    }

    // Generation table ordering (section 15).
    for (i, g) in generations.iter().enumerate() {
        let bad_order = i > 0
            && (g.start_offset <= generations[i - 1].start_offset
                || g.first_sequence <= generations[i - 1].first_sequence);
        if g.generation != i as u64 || bad_order || (i == 0 && g.start_offset != HEADER_LEN as u64)
        {
            return Err(Error::new(
                "BadGenerationTable",
                format!("generation table entry {i} is out of order"),
            ));
        }
    }

    let hashes: Vec<[u8; 32]> = chunks.iter().map(|c| c.hash).collect();
    if merkle::root(&hashes) != merkle_root {
        return Err(Error::new("MerkleRootMismatch", "merkle root mismatch"));
    }

    let max_plain = blocks.iter().map(|b| b.plain_len).max().unwrap_or(0);
    if envelope.max_block_plain != max_plain {
        return Err(Error::new(
            "EnvelopeMismatch",
            "envelope field max_block_plain differs from the block table",
        ));
    }
    let m = envelope.max_frame_payload;
    let bound = m.saturating_add(36).saturating_add(varint_len(m));
    let mut lens: Vec<u64> = blocks.iter().map(|b| b.frame_len).collect();
    lens.push(entry_table.len);
    if let Some(r) = &records {
        lens.push(r.len);
    }
    lens.extend(recovery.iter().map(|r| r.len));
    if payload.len() as u64 > m || lens.iter().any(|l| *l > bound) {
        return Err(Error::new(
            "EnvelopeMismatch",
            "envelope field max_frame_payload does not admit every frame",
        ));
    }

    Ok(Index {
        chunks,
        merkle_root,
        envelope,
        priors,
        blocks,
        entry_table,
        entry_table_hash,
        records,
        recovery,
        generations,
        chunk_block,
        chunk_offset,
    })
}
