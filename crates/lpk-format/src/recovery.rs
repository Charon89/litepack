//! Recovery frames: Reed-Solomon shards over the body of the archive, which let a
//! reader rebuild damaged bytes (spec section 13).

use crate::error::FormatError;
use crate::header::Header;

/// Shard lengths are multiples of this many bytes.
pub const SHARD_ALIGN: u32 = 64;
/// Default shard length: 64 KiB.
pub const DEFAULT_SHARD_LEN: u32 = 1 << 16;
/// Most data shards a writer makes for one recovery frame.
pub const MAX_DATA_SHARDS: u32 = 32768;
/// Most data shards plus recovery shards in one frame.
pub const MAX_TOTAL_SHARDS: u32 = 65535;
/// Largest recovery percentage a writer accepts.
pub const MAX_PERCENT: u8 = 20;
/// Bytes of a recovery payload before the shard hashes.
pub const RECOVERY_HEAD_LEN: usize = 28;

const WHAT: &str = "recovery";

/// Recovery settings of a writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryOptions {
    /// Recovery shards as a share of the data shards, in percent: 0 writes no
    /// recovery frame, otherwise 1 to 20.
    pub percent: u8,
    /// Shard length in bytes: a multiple of 64, at least 64.
    pub shard_len: u32,
}

impl Default for RecoveryOptions {
    fn default() -> Self {
        RecoveryOptions {
            percent: 0,
            shard_len: DEFAULT_SHARD_LEN,
        }
    }
}

impl RecoveryOptions {
    /// Check the options a writer is given (only when `percent` is not 0).
    pub fn check(&self) -> Result<(), FormatError> {
        if self.percent > MAX_PERCENT {
            return Err(FormatError::BadOptions {
                reason: "recovery percent above 20",
            });
        }
        if self.percent > 0 && (self.shard_len == 0 || !self.shard_len.is_multiple_of(SHARD_ALIGN))
        {
            return Err(FormatError::BadOptions {
                reason: "recovery shard_len not a multiple of 64",
            });
        }
        Ok(())
    }
}

/// The shard counts for a cover of `cover_len` bytes: `(data_shards,
/// recovery_shards)`. `BadOptions { reason: "recovery shards" }` when the
/// cover needs more than 32768 data shards (or `cover_len` is 0).
pub fn shard_geometry(
    cover_len: u64,
    shard_len: u32,
    percent: u8,
) -> Result<(u32, u32), FormatError> {
    let bad = FormatError::BadOptions {
        reason: "recovery shards",
    };
    if cover_len == 0 || shard_len == 0 || percent == 0 {
        return Err(bad);
    }
    let data = cover_len.div_ceil(u64::from(shard_len));
    if data > u64::from(MAX_DATA_SHARDS) {
        return Err(bad);
    }
    let recovery = (data * u64::from(percent)).div_ceil(100).max(1);
    if data + recovery > u64::from(MAX_TOTAL_SHARDS) {
        return Err(bad);
    }
    Ok((data as u32, recovery as u32))
}

/// A decoded recovery frame payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryFrame {
    /// Absolute offset of the first covered byte.
    pub cover_offset: u64,
    /// Number of covered bytes.
    pub cover_len: u64,
    /// Length of every shard in bytes.
    pub shard_len: u32,
    /// Number of data shards: `ceil(cover_len / shard_len)`.
    pub data_shards: u32,
    /// Number of recovery shards.
    pub recovery_shards: u32,
    /// BLAKE3 of every data shard, the last one padded with zeros.
    pub shard_hashes: Vec<[u8; 32]>,
    /// The recovery shards, `recovery_shards * shard_len` bytes.
    pub recovery: Vec<u8>,
}

fn bad(reason: &'static str) -> FormatError {
    FormatError::BadRecovery { reason }
}

impl RecoveryFrame {
    /// The rules that do not depend on the archive: shard length, counts,
    /// sums and the length of the hash and shard lists.
    fn check(&self) -> Result<(), FormatError> {
        check_fields(
            self.cover_offset,
            self.cover_len,
            self.shard_len,
            self.data_shards,
            self.recovery_shards,
        )?;
        if self.shard_hashes.len() != self.data_shards as usize {
            return Err(bad("shard hashes"));
        }
        if self.recovery.len() as u64 != u64::from(self.recovery_shards) * u64::from(self.shard_len)
        {
            return Err(bad("recovery bytes"));
        }
        Ok(())
    }

    /// Encode the payload; the rules a reader applies are checked first.
    pub fn encode(&self) -> Result<Vec<u8>, FormatError> {
        self.check()?;
        let mut out = self.head_bytes();
        out.reserve(self.recovery.len());
        out.extend_from_slice(&self.recovery);
        Ok(out)
    }

    /// The payload up to (excluding) the recovery shards: fixed head and hashes.
    pub(crate) fn head_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(RECOVERY_HEAD_LEN + self.shard_hashes.len() * 32);
        out.extend_from_slice(&self.cover_offset.to_le_bytes());
        out.extend_from_slice(&self.cover_len.to_le_bytes());
        out.extend_from_slice(&self.shard_len.to_le_bytes());
        out.extend_from_slice(&self.data_shards.to_le_bytes());
        out.extend_from_slice(&self.recovery_shards.to_le_bytes());
        for h in &self.shard_hashes {
            out.extend_from_slice(h);
        }
        out
    }

    /// Parse a payload found in an archive whose index starts at
    /// `index_offset`: every consistency rule of section 13 is checked, and
    /// nothing is allocated beyond the payload's own length.
    pub fn parse(payload: &[u8], index_offset: u64) -> Result<RecoveryFrame, FormatError> {
        if payload.len() < RECOVERY_HEAD_LEN {
            return Err(FormatError::Truncated { what: WHAT });
        }
        let u64_at =
            |at: usize| u64::from_le_bytes(payload[at..at + 8].try_into().unwrap_or_default());
        let u32_at =
            |at: usize| u32::from_le_bytes(payload[at..at + 4].try_into().unwrap_or_default());
        let cover_offset = u64_at(0);
        let cover_len = u64_at(8);
        let shard_len = u32_at(16);
        let data_shards = u32_at(20);
        let recovery_shards = u32_at(24);
        check_fields(
            cover_offset,
            cover_len,
            shard_len,
            data_shards,
            recovery_shards,
        )?;
        let end = cover_offset
            .checked_add(cover_len)
            .ok_or_else(|| bad("cover range"))?;
        if end > index_offset {
            return Err(bad("cover range"));
        }
        let expected = RECOVERY_HEAD_LEN as u64
            + u64::from(data_shards) * 32
            + u64::from(recovery_shards) * u64::from(shard_len);
        if payload.len() as u64 != expected {
            return Err(bad("payload length"));
        }
        let hashes_end = RECOVERY_HEAD_LEN + data_shards as usize * 32;
        let shard_hashes = payload[RECOVERY_HEAD_LEN..hashes_end]
            .as_chunks::<32>()
            .0
            .to_vec();
        Ok(RecoveryFrame {
            cover_offset,
            cover_len,
            shard_len,
            data_shards,
            recovery_shards,
            shard_hashes,
            recovery: payload[hashes_end..].to_vec(),
        })
    }

    /// Byte range of data shard `i` inside the archive: `(offset, len)`; the
    /// last shard is shorter than `shard_len` when the cover does not fill it.
    pub fn shard_range(&self, i: u32) -> (u64, usize) {
        let start = u64::from(i) * u64::from(self.shard_len);
        let len = (self.cover_len - start).min(u64::from(self.shard_len));
        (self.cover_offset + start, len as usize)
    }
}

fn check_fields(
    cover_offset: u64,
    cover_len: u64,
    shard_len: u32,
    data_shards: u32,
    recovery_shards: u32,
) -> Result<(), FormatError> {
    if shard_len == 0 || !shard_len.is_multiple_of(SHARD_ALIGN) {
        return Err(bad("shard_len"));
    }
    if cover_offset < Header::LEN as u64 {
        return Err(bad("cover range"));
    }
    if cover_len == 0 || cover_len.div_ceil(u64::from(shard_len)) != u64::from(data_shards) {
        return Err(bad("data_shards"));
    }
    if recovery_shards == 0 {
        return Err(bad("recovery_shards"));
    }
    if u64::from(data_shards) + u64::from(recovery_shards) > u64::from(MAX_TOTAL_SHARDS) {
        return Err(bad("shard count"));
    }
    Ok(())
}

/// The writer found more than 32768 data shards (carried inside an
/// `io::Error` so that it can cross `Write::write`).
#[derive(Debug)]
pub(crate) struct ShardCap;

impl std::fmt::Display for ShardCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("more than 32768 recovery data shards")
    }
}

impl std::error::Error for ShardCap {}

/// True when `e` is the cap error of [`ShardSpool::feed`].
pub(crate) fn is_shard_cap(e: &std::io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<ShardCap>())
}

pub(crate) fn rs_error(e: reed_solomon_simd::Error) -> FormatError {
    FormatError::RecoveryError {
        reason: e.to_string(),
    }
}

/// Collects the covered bytes of a stream into shards, one at a time.
///
/// Memory rule: this holds one shard buffer and 32 bytes per completed shard;
/// the shards themselves go to an anonymous temporary file, because the
/// Reed-Solomon encoder must know the number of data shards when it is created
/// and a streaming writer learns it only when the covered range ends. At the
/// end the file is read back once, shard by shard, into the encoder.
pub(crate) struct ShardSpool {
    shard_len: usize,
    buf: Vec<u8>,
    hashes: Vec<[u8; 32]>,
    file: std::fs::File,
    cover_len: u64,
}

/// The result of [`ShardSpool::finish`]: the frame without its recovery bytes
/// and the encoder that holds them.
pub(crate) struct Encoded {
    pub frame: RecoveryFrame,
    pub encoder: reed_solomon_simd::ReedSolomonEncoder,
}

impl ShardSpool {
    pub(crate) fn new(shard_len: u32) -> std::io::Result<ShardSpool> {
        Ok(ShardSpool {
            shard_len: shard_len as usize,
            buf: Vec::with_capacity(shard_len as usize),
            hashes: Vec::new(),
            file: tempfile::tempfile()?,
            cover_len: 0,
        })
    }

    /// Covered bytes held in memory right now (at most one shard).
    pub(crate) fn buffered(&self) -> usize {
        self.buf.len()
    }

    fn complete(&mut self) -> std::io::Result<()> {
        use std::io::Write;
        self.buf.resize(self.shard_len, 0);
        self.hashes.push(*blake3::hash(&self.buf).as_bytes());
        self.file.write_all(&self.buf)?;
        self.buf.clear();
        Ok(())
    }

    pub(crate) fn feed(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            if self.buf.is_empty() && self.hashes.len() >= MAX_DATA_SHARDS as usize {
                return Err(std::io::Error::other(ShardCap));
            }
            let take = (self.shard_len - self.buf.len()).min(bytes.len());
            self.buf.extend_from_slice(&bytes[..take]);
            self.cover_len += take as u64;
            bytes = &bytes[take..];
            if self.buf.len() == self.shard_len {
                self.complete()?;
            }
        }
        Ok(())
    }

    /// Close the covered range and encode: the geometry comes from
    /// `percent`; the frame's `cover_offset` is the end of the header.
    pub(crate) fn finish(mut self, percent: u8) -> Result<Encoded, FormatError> {
        use std::io::{Read, Seek, SeekFrom};
        if !self.buf.is_empty() {
            self.complete()?;
        }
        let shard_len = self.shard_len as u32;
        let (data, recovery) = shard_geometry(self.cover_len, shard_len, percent)?;
        let mut encoder = reed_solomon_simd::ReedSolomonEncoder::new(
            data as usize,
            recovery as usize,
            self.shard_len,
        )
        .map_err(rs_error)?;
        self.file.seek(SeekFrom::Start(0))?;
        let mut shard = vec![0u8; self.shard_len];
        for _ in 0..data {
            self.file.read_exact(&mut shard)?;
            encoder.add_original_shard(&shard).map_err(rs_error)?;
        }
        Ok(Encoded {
            frame: RecoveryFrame {
                cover_offset: Header::LEN as u64,
                cover_len: self.cover_len,
                shard_len,
                data_shards: data,
                recovery_shards: recovery,
                shard_hashes: self.hashes,
                recovery: Vec::new(),
            },
            encoder,
        })
    }
}

/// The Markdown table of the recovery payload, pasted verbatim into the spec.
pub fn recovery_layout_table() -> String {
    "| Field | Size | Meaning |\n|---|---|---|\n\
     | cover_offset | 8 | absolute offset of the first covered byte (little-endian); at least the header length |\n\
     | cover_len | 8 | number of covered bytes; the range ends at or before the index |\n\
     | shard_len | 4 | length of every shard in bytes; a multiple of 64, not 0 |\n\
     | data_shards | 4 | `ceil(cover_len / shard_len)` |\n\
     | recovery_shards | 4 | number of recovery shards; at least 1; `data_shards + recovery_shards` is at most 65535 |\n\
     | shard_hashes | data_shards * 32 | BLAKE3-256 of each data shard, the last one padded with zeros to `shard_len` |\n\
     | recovery | recovery_shards * shard_len | the Reed-Solomon recovery shards, in order |\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(cover_len: u64, shard_len: u32, rec: u32) -> RecoveryFrame {
        let data = cover_len.div_ceil(u64::from(shard_len)) as u32;
        RecoveryFrame {
            cover_offset: 32,
            cover_len,
            shard_len,
            data_shards: data,
            recovery_shards: rec,
            shard_hashes: (0..data).map(|i| [i as u8; 32]).collect(),
            recovery: (0..rec as usize * shard_len as usize)
                .map(|i| i as u8)
                .collect(),
        }
    }

    const INDEX_AT: u64 = 100_000;

    fn reason(r: Result<RecoveryFrame, FormatError>) -> &'static str {
        match r {
            Err(FormatError::BadRecovery { reason }) => reason,
            other => panic!("expected BadRecovery, got {other:?}"),
        }
    }

    #[test]
    fn geometry_for_listed_covers() {
        for cover in [1u64, 64, 65, 1 << 20, 100 << 20] {
            for percent in [1u8, 5, 20] {
                let (d, r) = shard_geometry(cover, 65536, percent).unwrap();
                assert_eq!(u64::from(d), cover.div_ceil(65536));
                assert!(r >= 1);
                assert_eq!(
                    u64::from(r),
                    (u64::from(d) * u64::from(percent)).div_ceil(100)
                );
                assert!(d + r <= MAX_TOTAL_SHARDS);
                let (d2, r2) = shard_geometry(cover, 64, percent).unwrap_or((0, 0));
                if d2 > 0 {
                    assert!(d2 <= MAX_DATA_SHARDS && r2 >= 1);
                }
            }
        }
    }

    #[test]
    fn geometry_cap_engages() {
        assert!(shard_geometry(32768 * 64, 64, 5).is_ok());
        assert!(matches!(
            shard_geometry(32768 * 64 + 1, 64, 5),
            Err(FormatError::BadOptions {
                reason: "recovery shards"
            })
        ));
        assert!(shard_geometry(100 << 20, 64, 5).is_err());
        assert!(shard_geometry(0, 64, 5).is_err());
    }

    #[test]
    fn options_checked() {
        let ok = RecoveryOptions {
            percent: 5,
            shard_len: 128,
        };
        assert!(ok.check().is_ok());
        assert!(RecoveryOptions { percent: 21, ..ok }.check().is_err());
        assert!(RecoveryOptions {
            shard_len: 100,
            ..ok
        }
        .check()
        .is_err());
        assert!(RecoveryOptions { shard_len: 0, ..ok }.check().is_err());
        assert!(RecoveryOptions::default().check().is_ok());
    }

    #[test]
    fn round_trip() {
        for (cover, shard, rec) in [
            (1u64, 64u32, 1u32),
            (64, 64, 1),
            (65, 64, 2),
            (1000, 128, 3),
        ] {
            let f = frame(cover, shard, rec);
            let p = f.encode().unwrap();
            assert_eq!(RecoveryFrame::parse(&p, INDEX_AT).unwrap(), f);
        }
    }

    #[test]
    fn shard_range_of_last_shard_is_short() {
        let f = frame(130, 64, 1);
        assert_eq!(f.shard_range(0), (32, 64));
        assert_eq!(f.shard_range(2), (32 + 128, 2));
    }

    #[test]
    fn every_rule_has_a_negative_test() {
        let base = frame(200, 64, 2);
        let good = base.encode().unwrap();
        let patch = |at: usize, v: &[u8]| {
            let mut p = good.clone();
            p[at..at + v.len()].copy_from_slice(v);
            p
        };
        assert_eq!(
            reason(RecoveryFrame::parse(
                &patch(16, &65u32.to_le_bytes()),
                INDEX_AT
            )),
            "shard_len"
        );
        assert_eq!(
            reason(RecoveryFrame::parse(
                &patch(16, &0u32.to_le_bytes()),
                INDEX_AT
            )),
            "shard_len"
        );
        assert_eq!(
            reason(RecoveryFrame::parse(
                &patch(0, &31u64.to_le_bytes()),
                INDEX_AT
            )),
            "cover range"
        );
        assert_eq!(
            reason(RecoveryFrame::parse(
                &patch(20, &5u32.to_le_bytes()),
                INDEX_AT
            )),
            "data_shards"
        );
        assert_eq!(
            reason(RecoveryFrame::parse(
                &patch(8, &0u64.to_le_bytes()),
                INDEX_AT
            )),
            "data_shards"
        );
        assert_eq!(
            reason(RecoveryFrame::parse(
                &patch(24, &0u32.to_le_bytes()),
                INDEX_AT
            )),
            "recovery_shards"
        );
        assert_eq!(
            reason(RecoveryFrame::parse(
                &patch(24, &70000u32.to_le_bytes()),
                INDEX_AT
            )),
            "shard count"
        );
        // The range must end before the index.
        assert_eq!(reason(RecoveryFrame::parse(&good, 231)), "cover range");
        assert!(RecoveryFrame::parse(&good, 232).is_ok());
        assert_eq!(
            reason(RecoveryFrame::parse(
                &patch(0, &u64::MAX.to_le_bytes()),
                INDEX_AT
            )),
            "cover range"
        );
    }

    #[test]
    fn payload_length_mismatch_and_truncation() {
        let good = frame(200, 64, 2).encode().unwrap();
        let mut longer = good.clone();
        longer.push(0);
        assert_eq!(
            reason(RecoveryFrame::parse(&longer, INDEX_AT)),
            "payload length"
        );
        assert_eq!(
            reason(RecoveryFrame::parse(&good[..good.len() - 1], INDEX_AT)),
            "payload length"
        );
        for cut in 0..RECOVERY_HEAD_LEN {
            assert!(matches!(
                RecoveryFrame::parse(&good[..cut], INDEX_AT),
                Err(FormatError::Truncated { what: "recovery" })
            ));
        }
        // A huge declared count never allocates: the length check comes first.
        let mut p = good.clone();
        p[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        p[8..16].copy_from_slice(&(u64::from(u32::MAX) * 64).to_le_bytes());
        assert!(RecoveryFrame::parse(&p, u64::MAX).is_err());
    }

    #[test]
    fn encode_refuses_inconsistent_frames() {
        let mut f = frame(200, 64, 2);
        f.shard_hashes.pop();
        assert!(f.encode().is_err());
        let mut f = frame(200, 64, 2);
        f.recovery.pop();
        assert!(f.encode().is_err());
    }
}
