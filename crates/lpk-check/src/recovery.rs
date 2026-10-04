//! Recovery frames (section 13): detection of damaged shards and repair.

use crate::error::{Error, Result};
use crate::index::{GenEntry, Loc};
use crate::wire::{kind, parse_frame, Cursor, HEADER_LEN};

/// The fixed part of a recovery payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryHead {
    /// First covered byte.
    pub cover_offset: u64,
    /// Covered length.
    pub cover_len: u64,
    /// Shard length.
    pub shard_len: u32,
    /// Real data shards.
    pub data_shards: u32,
    /// Data shards the group is coded with.
    pub group_shards: u32,
    /// Recovery shards.
    pub recovery_shards: u32,
}

/// A damaged shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamagedShard {
    /// Position of the frame in the index's list.
    pub frame: usize,
    /// Shard number in its group.
    pub shard: u32,
    /// Absolute start of the shard's bytes.
    pub start: u64,
    /// Absolute end (exclusive), without padding.
    pub end: u64,
}

/// The result of a scan.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Recovery frames listed.
    pub frames: usize,
    /// Frames that could not be used.
    pub unusable: usize,
    /// Damaged data shards found.
    pub damaged: usize,
    /// Shards rebuilt.
    pub repaired: usize,
    /// Every damaged shard.
    pub damaged_shards: Vec<DamagedShard>,
    /// The first `Unrepairable` or `RecoveryError`, when repairing.
    pub error: Option<Error>,
}

impl Report {
    /// The line the reference `check` prints.
    pub fn line(&self) -> String {
        format!(
            "recovery frames: {}, unusable: {}, damaged shards: {}, repaired shards: {}",
            self.frames, self.unusable, self.damaged, self.repaired
        )
    }
}

fn bad(reason: &str) -> Error {
    Error::new("BadRecovery", format!("bad recovery frame: {reason}"))
}

/// Parses and checks a recovery payload (section 13 consistency rules).
pub fn parse_payload(p: &[u8], index_offset: u64) -> Result<(RecoveryHead, &[u8], &[u8])> {
    if p.len() < 32 {
        return Err(Error::truncated("recovery"));
    }
    let mut c = Cursor::new(p, "recovery");
    let h = RecoveryHead {
        cover_offset: c.u64()?,
        cover_len: c.u64()?,
        shard_len: c.u32()?,
        data_shards: c.u32()?,
        group_shards: c.u32()?,
        recovery_shards: c.u32()?,
    };
    let sl = u64::from(h.shard_len);
    if sl == 0 || sl % 64 != 0 || sl > 16_777_216 {
        return Err(bad("shard_len"));
    }
    let range_ok = h.cover_offset >= HEADER_LEN as u64
        && h.cover_offset
            .checked_add(h.cover_len)
            .is_some_and(|e| e <= index_offset);
    if !range_ok {
        return Err(bad("cover range"));
    }
    if !(1..=32768).contains(&h.group_shards) {
        return Err(bad("group_shards"));
    }
    if u64::from(h.group_shards) * sl > 1_073_741_824 {
        return Err(bad("group size"));
    }
    let want_data = h.cover_len.div_ceil(sl);
    if u64::from(h.data_shards) != want_data || h.data_shards == 0 || h.data_shards > h.group_shards
    {
        return Err(bad("data_shards"));
    }
    if h.recovery_shards == 0 {
        return Err(bad("recovery_shards"));
    }
    if u64::from(h.group_shards) + u64::from(h.recovery_shards) > 65535
        || !reed_solomon_simd::ReedSolomonDecoder::supports(
            h.group_shards as usize,
            h.recovery_shards as usize,
        )
    {
        return Err(bad("shard count"));
    }
    let want_len = 32 + u64::from(h.data_shards) * 32 + u64::from(h.recovery_shards) * sl;
    if p.len() as u64 != want_len {
        return Err(bad("payload length"));
    }
    let hashes = c.bytes(h.data_shards as usize * 32)?;
    let rec = c.bytes(c.remaining())?;
    Ok((h, hashes, rec))
}

fn shard_bytes(data: &[u8], h: &RecoveryHead, s: u32) -> Vec<u8> {
    let sl = h.shard_len as usize;
    let start = h.cover_offset as usize + s as usize * sl;
    let end = (start + sl).min(h.cover_offset as usize + h.cover_len as usize);
    let mut v = vec![0u8; sl];
    if let Some(src) = data.get(start..end) {
        v[..src.len()].copy_from_slice(src);
    }
    v
}

/// Scans every recovery frame of `locs`; with `repair`, rebuilds damaged shards into that copy.
/// `gens` gives the generation starts for the coverage rule; `memory` bounds the decoder buffer.
pub fn scan(
    data: &[u8],
    locs: &[Loc],
    gens: &[GenEntry],
    index_offset: u64,
    mut repair: Option<&mut Vec<u8>>,
    memory: u64,
) -> Result<Report> {
    let mut rep = Report {
        frames: locs.len(),
        ..Report::default()
    };
    let gen_of = |off: u64| gens.iter().rposition(|g| g.start_offset <= off);
    for (i, l) in locs.iter().enumerate() {
        // Step 1: the frame itself, its rules and the coverage rule.
        let usable = (|| -> Result<(RecoveryHead, Vec<u8>, Vec<u8>)> {
            let end = l
                .offset
                .checked_add(l.len)
                .filter(|e| *e <= data.len() as u64)
                .ok_or_else(|| Error::bad_location("recovery"))? as usize;
            let f = parse_frame(&data[..end], l.offset as usize, u64::MAX)?;
            if f.kind != kind::RECOVERY || f.end != end || f.sealed() {
                return Err(Error::bad_location("recovery"));
            }
            f.check_hash()?;
            let (h, hashes, rec) = parse_payload(f.payload, index_offset)?;
            let first_of_gen = i == 0 || gen_of(locs[i - 1].offset) != gen_of(l.offset);
            let expect_start = if first_of_gen {
                gen_of(l.offset)
                    .map(|g| gens[g].start_offset)
                    .unwrap_or(HEADER_LEN as u64)
            } else {
                locs[i - 1].end()
            };
            if h.cover_offset != expect_start || h.cover_offset + h.cover_len != l.offset {
                return Err(bad("coverage"));
            }
            Ok((h, hashes.to_vec(), rec.to_vec()))
        })();
        let Ok((h, hashes, rec)) = usable else {
            rep.unusable += 1;
            continue;
        };
        // Step 2: hash every real shard.
        let mut damaged = Vec::new();
        for s in 0..h.data_shards {
            let b = shard_bytes(data, &h, s);
            let want = &hashes[s as usize * 32..s as usize * 32 + 32];
            if blake3::hash(&b).as_bytes() != want {
                damaged.push(s);
                let sl = u64::from(h.shard_len);
                let start = h.cover_offset + u64::from(s) * sl;
                rep.damaged_shards.push(DamagedShard {
                    frame: i,
                    shard: s,
                    start,
                    end: (start + sl).min(h.cover_offset + h.cover_len),
                });
            }
        }
        rep.damaged += damaged.len();
        let Some(copy) = repair.as_deref_mut() else {
            continue;
        };
        if damaged.is_empty() {
            continue;
        }
        if damaged.len() > h.recovery_shards as usize {
            if rep.error.is_none() {
                rep.error = Some(Error::new(
                    "Unrepairable",
                    format!(
                        "recovery frame {i}: {} shards damaged, capacity {}",
                        damaged.len(),
                        h.recovery_shards
                    ),
                ));
            }
            continue;
        }
        // Steps 3 and 4: rebuild and write back.
        let sl = h.shard_len as u64;
        let need = (u64::from(h.recovery_shards).next_power_of_two() + u64::from(h.group_shards))
            .next_power_of_two()
            * sl;
        if need > memory {
            return Err(Error::new(
                "Refused",
                format!(
                    "the archive needs recovery group of {need} bytes; this reader allows {memory}"
                ),
            ));
        }
        let zero = vec![0u8; h.shard_len as usize];
        let originals: Vec<(usize, Vec<u8>)> = (0..h.group_shards)
            .filter(|s| !damaged.contains(s))
            .map(|s| {
                let b = if s < h.data_shards {
                    shard_bytes(data, &h, s)
                } else {
                    zero.clone()
                };
                (s as usize, b)
            })
            .collect();
        let recovery: Vec<(usize, &[u8])> = (0..damaged.len())
            .map(|j| {
                (
                    j,
                    &rec[j * h.shard_len as usize..(j + 1) * h.shard_len as usize],
                )
            })
            .collect();
        let restored = match reed_solomon_simd::decode(
            h.group_shards as usize,
            h.recovery_shards as usize,
            originals,
            recovery,
        ) {
            Ok(m) => m,
            Err(e) => {
                rep.error
                    .get_or_insert(Error::new("RecoveryError", format!("decoder: {e}")));
                continue;
            }
        };
        for &s in &damaged {
            let want = &hashes[s as usize * 32..s as usize * 32 + 32];
            let Some(b) = restored.get(&(s as usize)) else {
                rep.error
                    .get_or_insert(Error::new("RecoveryError", "shard not restored"));
                continue;
            };
            if blake3::hash(b).as_bytes() != want {
                rep.error.get_or_insert(Error::new(
                    "RecoveryError",
                    format!("rebuilt shard {s} of frame {i} does not match its hash"),
                ));
                continue;
            }
            let start = (h.cover_offset + u64::from(s) * sl) as usize;
            let end = (start + sl as usize).min((h.cover_offset + h.cover_len) as usize);
            copy[start..end].copy_from_slice(&b[..end - start]);
            rep.repaired += 1;
        }
    }
    Ok(rep)
}
