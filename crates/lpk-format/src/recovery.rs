//! Recovery frames: Reed-Solomon shards over groups of whole frames of the archive
//! body, which let a reader rebuild damaged bytes (spec section 13).
//!
//! Memory rules (independent of the archive size):
//! - writer: the shards of the open group (at most `group_shards * shard_len`
//!   bytes, the data actually written), then that group's encoder
//!   ([`encoder_work_bytes`]) and its recovery shards (`recovery_shards *
//!   shard_len`), which are held only until the group's frame is written right
//!   after the group;
//! - repair: one shard while scanning, and for a group with damage the decoder's
//!   buffer for that group ([`decoder_work_bytes`]).

use crate::archive::Archive;
use crate::envelope::{Refusal, Resources, DEFAULT_MEMORY};
use crate::error::FormatError;
use crate::frame::FrameKind;
use crate::header::Header;
use crate::index::FrameLocation;
use reed_solomon_simd::{ReedSolomonDecoder, ReedSolomonEncoder};
use std::io::{Read, Seek, SeekFrom, Write};

/// Shard lengths are multiples of this many bytes.
pub const SHARD_ALIGN: u32 = 64;
/// Default shard length: 64 KiB.
pub const DEFAULT_SHARD_LEN: u32 = 1 << 16;
/// Largest shard length: 16 MiB.
pub const MAX_SHARD_LEN: u32 = 16 << 20;
/// Default data shards per group.
pub const DEFAULT_GROUP_SHARDS: u32 = 2048;
/// Most data shards in one group.
pub const MAX_GROUP_SHARDS: u32 = 32768;
/// Most data shards plus recovery shards in one group.
pub const MAX_TOTAL_SHARDS: u32 = 65535;
/// Largest `group_shards * shard_len`: 1 GiB.
pub const MAX_GROUP_BYTES: u64 = 1 << 30;
/// Largest recovery percentage a writer accepts.
pub const MAX_PERCENT: u8 = 20;
/// Bytes of a recovery payload before the shard hashes.
pub const RECOVERY_HEAD_LEN: usize = 32;

const WHAT: &str = "recovery";

/// Recovery settings of a writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryOptions {
    /// Recovery shards as a share of a group's data shards, in percent: 0
    /// writes no recovery frame, otherwise 1 to 20.
    pub percent: u8,
    /// Shard length in bytes: a multiple of 64, at most 16 MiB.
    pub shard_len: u32,
    /// Data shards per group (and per recovery frame): 1 to 32768.
    pub group_shards: u32,
}

impl Default for RecoveryOptions {
    fn default() -> Self {
        RecoveryOptions {
            percent: 0,
            shard_len: DEFAULT_SHARD_LEN,
            group_shards: DEFAULT_GROUP_SHARDS,
        }
    }
}

fn bad_opt(reason: &'static str) -> FormatError {
    FormatError::BadOptions { reason }
}

/// `ceil(group_shards * percent / 100)`, at least 1.
pub fn group_recovery_shards(group_shards: u32, percent: u8) -> u32 {
    (u64::from(group_shards) * u64::from(percent))
        .div_ceil(100)
        .max(1) as u32
}

/// Bytes the encoder's work buffer takes for one group: `work_count *
/// shard_len` with `work_count` the group's shards rounded up to a multiple of
/// `next_pow2(recovery_shards)`.
pub fn encoder_work_bytes(group_shards: u32, recovery_shards: u32, shard_len: u32) -> u64 {
    let m = u64::from(recovery_shards.max(1).next_power_of_two());
    u64::from(group_shards).next_multiple_of(m) * u64::from(shard_len)
}

/// Bytes the decoder's buffer takes for one group: `(next_pow2(recovery_shards)
/// + group_shards).next_pow2() * shard_len`.
pub fn decoder_work_bytes(group_shards: u32, recovery_shards: u32, shard_len: u32) -> u64 {
    let m = u64::from(recovery_shards.max(1).next_power_of_two());
    (m + u64::from(group_shards)).next_power_of_two() * u64::from(shard_len)
}

impl RecoveryOptions {
    /// Check the options a writer is given (only when `percent` is not 0).
    pub fn check(&self) -> Result<(), FormatError> {
        if self.percent > MAX_PERCENT {
            return Err(bad_opt("recovery percent above 20"));
        }
        if self.percent == 0 {
            return Ok(());
        }
        if self.shard_len == 0 || !self.shard_len.is_multiple_of(SHARD_ALIGN) {
            return Err(bad_opt("recovery shard_len not a multiple of 64"));
        }
        if self.shard_len > MAX_SHARD_LEN {
            return Err(bad_opt("recovery shard_len above 16 MiB"));
        }
        if self.group_shards == 0 || self.group_shards > MAX_GROUP_SHARDS {
            return Err(bad_opt("recovery group_shards"));
        }
        if u64::from(self.group_shards) * u64::from(self.shard_len) > MAX_GROUP_BYTES {
            return Err(bad_opt("recovery group above 1 GiB"));
        }
        let r = group_recovery_shards(self.group_shards, self.percent);
        if u64::from(self.group_shards) + u64::from(r) > u64::from(MAX_TOTAL_SHARDS) {
            return Err(bad_opt("recovery shards"));
        }
        // A full group must be repairable within the default memory limit.
        if decoder_work_bytes(self.group_shards, r, self.shard_len) > DEFAULT_MEMORY {
            return Err(bad_opt("repair memory"));
        }
        Ok(())
    }
}

/// A decoded recovery frame payload: one group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryFrame {
    /// Absolute offset of the first covered byte.
    pub cover_offset: u64,
    /// Number of covered bytes.
    pub cover_len: u64,
    /// Length of every shard in bytes.
    pub shard_len: u32,
    /// Real data shards in this group: `ceil(cover_len / shard_len)`, at most `group_shards`.
    pub data_shards: u32,
    /// Data shards the group is coded with; those after `data_shards` are implicit zero shards.
    pub group_shards: u32,
    /// Number of recovery shards.
    pub recovery_shards: u32,
    /// BLAKE3 of every real data shard, the last one padded with zeros.
    pub shard_hashes: Vec<[u8; 32]>,
    /// The recovery shards, `recovery_shards * shard_len` bytes.
    pub recovery: Vec<u8>,
}

fn bad(reason: &'static str) -> FormatError {
    FormatError::BadRecovery { reason }
}

#[allow(clippy::too_many_arguments)]
fn check_fields(
    cover_offset: u64,
    cover_len: u64,
    shard_len: u32,
    data_shards: u32,
    group_shards: u32,
    recovery_shards: u32,
) -> Result<(), FormatError> {
    if shard_len == 0 || !shard_len.is_multiple_of(SHARD_ALIGN) || shard_len > MAX_SHARD_LEN {
        return Err(bad("shard_len"));
    }
    if cover_offset < Header::LEN as u64 {
        return Err(bad("cover range"));
    }
    if group_shards == 0 || group_shards > MAX_GROUP_SHARDS {
        return Err(bad("group_shards"));
    }
    if u64::from(group_shards) * u64::from(shard_len) > MAX_GROUP_BYTES {
        return Err(bad("group size"));
    }
    if data_shards == 0
        || data_shards > group_shards
        || cover_len.div_ceil(u64::from(shard_len)) != u64::from(data_shards)
    {
        return Err(bad("data_shards"));
    }
    if recovery_shards == 0 {
        return Err(bad("recovery_shards"));
    }
    if u64::from(group_shards) + u64::from(recovery_shards) > u64::from(MAX_TOTAL_SHARDS) {
        return Err(bad("shard count"));
    }
    // The coding library's own limits on the shard counts.
    if !ReedSolomonDecoder::supports(group_shards as usize, recovery_shards as usize) {
        return Err(bad("shard count"));
    }
    Ok(())
}

impl RecoveryFrame {
    fn check(&self) -> Result<(), FormatError> {
        check_fields(
            self.cover_offset,
            self.cover_len,
            self.shard_len,
            self.data_shards,
            self.group_shards,
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
        out.extend_from_slice(&self.group_shards.to_le_bytes());
        out.extend_from_slice(&self.recovery_shards.to_le_bytes());
        for h in &self.shard_hashes {
            out.extend_from_slice(h);
        }
        out
    }

    /// Check every rule and return the frame without its recovery bytes and
    /// the offset where they start in `payload`.
    fn parse_head(
        payload: &[u8],
        index_offset: u64,
    ) -> Result<(RecoveryFrame, usize), FormatError> {
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
        let group_shards = u32_at(24);
        let recovery_shards = u32_at(28);
        check_fields(
            cover_offset,
            cover_len,
            shard_len,
            data_shards,
            group_shards,
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
        Ok((
            RecoveryFrame {
                cover_offset,
                cover_len,
                shard_len,
                data_shards,
                group_shards,
                recovery_shards,
                shard_hashes,
                recovery: Vec::new(),
            },
            hashes_end,
        ))
    }

    /// Parse a payload found in an archive whose index starts at
    /// `index_offset`: every consistency rule of section 13 is checked, and
    /// nothing is allocated beyond the payload's own length.
    pub fn parse(payload: &[u8], index_offset: u64) -> Result<RecoveryFrame, FormatError> {
        let (mut f, at) = Self::parse_head(payload, index_offset)?;
        f.recovery = payload[at..].to_vec();
        Ok(f)
    }

    /// Like [`RecoveryFrame::parse`], reusing the payload's own allocation for
    /// the recovery bytes (no second copy).
    pub fn parse_vec(
        mut payload: Vec<u8>,
        index_offset: u64,
    ) -> Result<RecoveryFrame, FormatError> {
        let (mut f, at) = Self::parse_head(&payload, index_offset)?;
        payload.drain(..at);
        f.recovery = payload;
        Ok(f)
    }

    /// Byte range of data shard `i` inside the archive: `(offset, len)`; the
    /// last shard is shorter than `shard_len` when the cover does not fill it.
    pub fn shard_range(&self, i: u32) -> (u64, usize) {
        let start = u64::from(i) * u64::from(self.shard_len);
        let len = (self.cover_len - start).min(u64::from(self.shard_len));
        (self.cover_offset + start, len as usize)
    }
}

pub(crate) fn rs_error(e: reed_solomon_simd::Error) -> FormatError {
    FormatError::RecoveryError {
        reason: e.to_string(),
    }
}

/// One finished group: what its frame needs.
pub(crate) struct Group {
    pub cover_len: u64,
    /// Shards the group is coded with (the real ones: no padding is needed).
    pub group_shards: u32,
    pub recovery_shards: u32,
    pub hashes: Vec<[u8; 32]>,
    pub recovery: Vec<u8>,
}

/// Collects the bytes of the frames of the open group; the writer closes the
/// group between frames, and only then is its encoder built, sized by the data
/// actually written.
pub(crate) struct GroupEncoder {
    shard_len: usize,
    cap_bytes: usize,
    percent: u8,
    data: Vec<u8>,
    peak_recovery: u64,
}

impl GroupEncoder {
    pub(crate) fn new(o: &RecoveryOptions) -> GroupEncoder {
        GroupEncoder {
            shard_len: o.shard_len as usize,
            cap_bytes: o.group_shards as usize * o.shard_len as usize,
            percent: o.percent,
            data: Vec::new(),
            peak_recovery: 0,
        }
    }

    /// Bytes of the open group held in memory (at most one group).
    pub(crate) fn buffered(&self) -> usize {
        self.data.len()
    }

    /// Most bytes of recovery shards held at once so far (one group's).
    pub(crate) fn peak_recovery(&self) -> u64 {
        self.peak_recovery
    }

    /// Bytes of the open group so far.
    pub(crate) fn group_bytes(&self) -> u64 {
        self.data.len() as u64
    }

    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Result<(), FormatError> {
        if self.data.len() + bytes.len() > self.cap_bytes {
            return Err(FormatError::BadOptions {
                reason: "group smaller than a block",
            });
        }
        self.data.extend_from_slice(bytes);
        Ok(())
    }

    /// Close the open group: hash its shards (the last padded with zeros) and
    /// encode them. `None` when the group is empty.
    pub(crate) fn end_group(&mut self) -> Result<Option<Group>, FormatError> {
        if self.data.is_empty() {
            return Ok(None);
        }
        let cover_len = self.data.len() as u64;
        self.data
            .resize(self.data.len().next_multiple_of(self.shard_len), 0);
        let shards = self.data.len() / self.shard_len;
        let recovery_shards = group_recovery_shards(shards as u32, self.percent);
        let mut encoder = ReedSolomonEncoder::new(shards, recovery_shards as usize, self.shard_len)
            .map_err(rs_error)?;
        let mut hashes = Vec::with_capacity(shards);
        for s in self.data.chunks_exact(self.shard_len) {
            hashes.push(*blake3::hash(s).as_bytes());
            encoder.add_original_shard(s).map_err(rs_error)?;
        }
        let mut recovery = Vec::with_capacity(recovery_shards as usize * self.shard_len);
        {
            let result = encoder.encode().map_err(rs_error)?;
            for s in result.recovery_iter() {
                recovery.extend_from_slice(s);
            }
        }
        self.peak_recovery = self.peak_recovery.max(recovery.len() as u64);
        self.data.clear();
        Ok(Some(Group {
            cover_len,
            group_shards: shards as u32,
            recovery_shards,
            hashes,
            recovery,
        }))
    }
}

/// What a recovery scan found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepairReport {
    /// Recovery frames the index lists.
    pub frames: u64,
    /// Frames that failed their hash, their location or a field rule: skipped,
    /// their coverage is unprotected.
    pub frames_unusable: u64,
    /// Data shards whose hash differs from the frame's, over all usable frames.
    pub shards_damaged: u64,
    /// Shards rebuilt and written to the repaired copy.
    pub shards_repaired: u64,
}

/// Read data shard `i` of `frame` from `r`, zero-padded to `shard_len`.
fn read_shard<R: Read + Seek>(
    r: &mut R,
    frame: &RecoveryFrame,
    i: u32,
    buf: &mut Vec<u8>,
) -> Result<(), FormatError> {
    let (offset, len) = frame.shard_range(i);
    r.seek(SeekFrom::Start(offset))?;
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf)?;
    buf.resize(frame.shard_len as usize, 0);
    Ok(())
}

/// Each frame covers exactly the bytes written since the previous recovery
/// frame (or the header): they start where the previous frame ends and end
/// where this frame starts. This also keeps the ranges ascending, disjoint,
/// clear of every recovery frame and tiling the data frames.
fn check_cover(
    frame: &RecoveryFrame,
    i: usize,
    locations: &[FrameLocation],
) -> Result<(), FormatError> {
    let start = match i.checked_sub(1) {
        None => Header::LEN as u64,
        Some(p) => locations[p].offset.saturating_add(locations[p].len),
    };
    if frame.cover_offset != start
        || frame.cover_offset.saturating_add(frame.cover_len) != locations[i].offset
    {
        return Err(bad("coverage"));
    }
    Ok(())
}

type Visit<'a, R> =
    dyn FnMut(&mut Archive<R>, usize, &RecoveryFrame, &[u32]) -> Result<u64, FormatError> + 'a;

/// Walk the recovery frames: for each usable one, hash its data shards and
/// call `visit(archive, position, frame, damaged)` with the damaged shard
/// numbers (ascending, possibly none); `visit` returns the shards it repaired.
fn scan<R: Read + Seek>(
    a: &mut Archive<R>,
    visit: &mut Visit<'_, R>,
) -> Result<RepairReport, FormatError> {
    let locations = a.recovery_frames().to_vec();
    let index_at = a.trailer().index_offset;
    let mut report = RepairReport {
        frames: locations.len() as u64,
        ..RepairReport::default()
    };
    let mut buf = Vec::new();
    for (i, loc) in locations.iter().enumerate() {
        let payload = match a.read_frame_at(*loc, FrameKind::Recovery) {
            Ok(f) => f.payload,
            Err(FormatError::Io(e)) => return Err(FormatError::Io(e)),
            // A damaged frame (hash, kind or length): skipped.
            Err(_) => {
                report.frames_unusable += 1;
                continue;
            }
        };
        let frame = match RecoveryFrame::parse_vec(payload, index_at)
            .and_then(|f| check_cover(&f, i, &locations).map(|()| f))
        {
            Ok(f) => f,
            // The hash passed but a rule is broken: also unusable.
            Err(_) => {
                report.frames_unusable += 1;
                continue;
            }
        };
        let mut damaged = Vec::new();
        for s in 0..frame.data_shards {
            read_shard(a.raw_reader(), &frame, s, &mut buf)?;
            if blake3::hash(&buf).as_bytes() != &frame.shard_hashes[s as usize] {
                damaged.push(s);
            }
        }
        report.shards_damaged += damaged.len() as u64;
        report.shards_repaired += visit(a, i, &frame, &damaged)?;
    }
    Ok(report)
}

/// Rebuild the damaged shards of `frame`: the shard number and its bytes
/// (padded), each checked against the frame's hash.
fn rebuild<R: Read + Seek>(
    r: &mut R,
    frame: &RecoveryFrame,
    damaged: &[u32],
) -> Result<Vec<(u32, Vec<u8>)>, FormatError> {
    let shard_len = frame.shard_len as usize;
    let mut decoder = ReedSolomonDecoder::new(
        frame.group_shards as usize,
        frame.recovery_shards as usize,
        shard_len,
    )
    .map_err(rs_error)?;
    let mut buf = Vec::new();
    for s in 0..frame.data_shards {
        if damaged.binary_search(&s).is_err() {
            read_shard(r, frame, s, &mut buf)?;
            decoder
                .add_original_shard(s as usize, &buf)
                .map_err(rs_error)?;
        }
    }
    // The implicit zero shards of a short last group are always intact.
    let zero = vec![0u8; shard_len];
    for s in frame.data_shards..frame.group_shards {
        decoder
            .add_original_shard(s as usize, &zero)
            .map_err(rs_error)?;
    }
    for (j, shard) in frame
        .recovery
        .chunks_exact(shard_len)
        .take(damaged.len())
        .enumerate()
    {
        decoder.add_recovery_shard(j, shard).map_err(rs_error)?;
    }
    let result = decoder.decode().map_err(rs_error)?;
    let mut fixed = Vec::with_capacity(damaged.len());
    for (idx, bytes) in result.restored_original_iter() {
        let ok = frame
            .shard_hashes
            .get(idx)
            .is_some_and(|h| blake3::hash(bytes).as_bytes() == h);
        if !ok {
            return Err(FormatError::RecoveryError {
                reason: "a rebuilt shard does not match its hash".to_string(),
            });
        }
        fixed.push((idx as u32, bytes.to_vec()));
    }
    if fixed.len() != damaged.len() {
        return Err(FormatError::RecoveryError {
            reason: "the decoder rebuilt the wrong number of shards".to_string(),
        });
    }
    Ok(fixed)
}

impl<R: Read + Seek> Archive<R> {
    /// Detect damage without repairing: hash every data shard of every usable
    /// recovery frame and count the damaged ones. `shards_repaired` is 0.
    pub fn check_recovery(&mut self) -> Result<RepairReport, FormatError> {
        self.need_index()?;
        self.scan_recovery_frames()
    }

    /// [`Archive::check_recovery`] without the key check, over whatever
    /// recovery frame list the archive holds.
    pub(crate) fn scan_recovery_frames(&mut self) -> Result<RepairReport, FormatError> {
        scan(self, &mut |_, _, _, _| Ok(0))
    }
}

/// Copy the archive `archive` to `out`, repairing what the recovery frames
/// can rebuild (spec section 13). The archive must open (a damaged index is
/// returned as the error `Archive::open` gives, and nothing is written). If a
/// frame has more damaged shards than it can rebuild, the copy still carries
/// every repair the other frames made and the first such frame is returned as
/// [`FormatError::Unrepairable`]. Memory: one shard while scanning; a group
/// with damage is rebuilt in the decoder's buffer for one group
/// ([`decoder_work_bytes`]), refused with `Refused` (field `recovery group`)
/// when that exceeds `resources.memory`.
pub fn repair<R: Read + Seek, W: Write + Seek>(
    archive: R,
    out: W,
    resources: &Resources,
) -> Result<RepairReport, FormatError> {
    match repair_with_report(archive, out, resources)? {
        (report, None) => Ok(report),
        (_, Some(e)) => Err(e),
    }
}

/// Like [`repair`], but an `Unrepairable` outcome is returned beside the
/// report instead of replacing it.
pub fn repair_with_report<R: Read + Seek, W: Write + Seek>(
    archive: R,
    out: W,
    resources: &Resources,
) -> Result<(RepairReport, Option<FormatError>), FormatError> {
    repair_with_credentials(archive, out, resources, None)
}

/// Like [`repair_with_report`] for an encrypted archive: the index, which
/// lists the recovery frames, is sealed, so the credentials are needed to open
/// it (the repair itself works on the sealed bytes and never decrypts a
/// frame). A listable archive given no credentials is `PasswordRequired`.
pub fn repair_with_credentials<R: Read + Seek, W: Write + Seek>(
    archive: R,
    mut out: W,
    resources: &Resources,
    credentials: Option<&crate::crypto::Credentials>,
) -> Result<(RepairReport, Option<FormatError>), FormatError> {
    let mut a = Archive::open_with(archive, resources, credentials)?;
    a.need_index()?;
    {
        let r = a.raw_reader();
        r.seek(SeekFrom::Start(0))?;
        out.seek(SeekFrom::Start(0))?;
        std::io::copy(r, &mut out)?;
    }
    let mut unrepairable = None;
    let report = scan(&mut a, &mut |a, i, frame, damaged| {
        if damaged.is_empty() {
            return Ok(0);
        }
        let capacity = u64::from(frame.recovery_shards);
        if damaged.len() as u64 > capacity {
            unrepairable.get_or_insert(FormatError::Unrepairable {
                frame: i,
                damaged: damaged.len() as u64,
                capacity,
            });
            return Ok(0);
        }
        let needed = decoder_work_bytes(frame.group_shards, frame.recovery_shards, frame.shard_len);
        if needed > resources.memory {
            return Err(FormatError::Refused(Refusal {
                field: "recovery group",
                needed,
                allowed: resources.memory,
            }));
        }
        let fixed = rebuild(a.raw_reader(), frame, damaged)?;
        for (s, bytes) in &fixed {
            let (offset, len) = frame.shard_range(*s);
            out.seek(SeekFrom::Start(offset))?;
            out.write_all(&bytes[..len])?;
        }
        Ok(fixed.len() as u64)
    })?;
    out.flush()?;
    Ok((report, unrepairable))
}

/// The Markdown table of the recovery payload, pasted verbatim into the spec.
pub fn recovery_layout_table() -> String {
    "| Field | Size | Meaning |\n|---|---|---|\n\
     | cover_offset | 8 | absolute offset of the first covered byte (little-endian); at least the header length |\n\
     | cover_len | 8 | number of covered bytes; the range ends at or before the index and overlaps no recovery frame |\n\
     | shard_len | 4 | length of every shard in bytes; a multiple of 64, not 0, at most 16777216 |\n\
     | data_shards | 4 | real shards in this group: `ceil(cover_len / shard_len)`, at least 1, at most `group_shards` |\n\
     | group_shards | 4 | data shards the group is coded with, 1 to 32768; shards after `data_shards` are implicit zero shards; `group_shards * shard_len` is at most 1073741824 |\n\
     | recovery_shards | 4 | number of recovery shards; at least 1; `group_shards + recovery_shards` is at most 65535 |\n\
     | shard_hashes | data_shards * 32 | BLAKE3-256 of each real data shard, the last one padded with zeros to `shard_len` |\n\
     | recovery | recovery_shards * shard_len | the Reed-Solomon recovery shards, in order |\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(cover_len: u64, shard_len: u32, group: u32, rec: u32) -> RecoveryFrame {
        let data = cover_len.div_ceil(u64::from(shard_len)) as u32;
        RecoveryFrame {
            cover_offset: 32,
            cover_len,
            shard_len,
            data_shards: data,
            group_shards: group,
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
    fn recovery_shard_counts() {
        for group in [1u32, 2, 64, 1024, 32768] {
            for percent in [1u8, 5, 20] {
                let r = group_recovery_shards(group, percent);
                assert!(r >= 1);
                assert_eq!(
                    u64::from(r),
                    (u64::from(group) * u64::from(percent)).div_ceil(100).max(1)
                );
                assert!(group + r <= MAX_TOTAL_SHARDS);
            }
        }
        assert_eq!(group_recovery_shards(1024, 5), 52);
    }

    #[test]
    fn work_buffer_formulas() {
        // 1024 shards, 52 recovery: next_pow2(52) = 64, 1024 is a multiple.
        assert_eq!(encoder_work_bytes(1024, 52, 64), 1024 * 64);
        assert_eq!(encoder_work_bytes(10, 3, 64), 12 * 64);
        assert_eq!(decoder_work_bytes(1024, 52, 64), 2048 * 64);
        assert_eq!(decoder_work_bytes(10, 3, 64), 16 * 64);
    }

    #[test]
    fn options_checked() {
        let ok = RecoveryOptions {
            percent: 5,
            shard_len: 128,
            group_shards: 16,
        };
        assert!(ok.check().is_ok());
        let no = |o: RecoveryOptions| matches!(o.check(), Err(FormatError::BadOptions { .. }));
        assert!(no(RecoveryOptions { percent: 21, ..ok }));
        assert!(no(RecoveryOptions {
            shard_len: 100,
            ..ok
        }));
        assert!(no(RecoveryOptions { shard_len: 0, ..ok }));
        assert!(no(RecoveryOptions {
            shard_len: MAX_SHARD_LEN + 64,
            ..ok
        }));
        assert!(no(RecoveryOptions {
            group_shards: 0,
            ..ok
        }));
        assert!(no(RecoveryOptions {
            group_shards: 32769,
            ..ok
        }));
        // 32768 shards of 64 KiB is 2 GiB.
        assert!(no(RecoveryOptions {
            shard_len: 65536,
            group_shards: 32768,
            ..ok
        }));
        // 1 GiB exactly is fine.
        assert!(RecoveryOptions {
            shard_len: 1 << 16,
            group_shards: 16384,
            ..ok
        }
        .check()
        .is_ok());
        // With percent 0 nothing is looked at.
        assert!(RecoveryOptions {
            percent: 0,
            shard_len: 3,
            group_shards: 0
        }
        .check()
        .is_ok());
        assert!(RecoveryOptions::default().check().is_ok());
    }

    #[test]
    fn round_trip() {
        for (cover, shard, group, rec) in [
            (1u64, 64u32, 1u32, 1u32),
            (64, 64, 4, 1),
            (65, 64, 2, 2),
            (1000, 128, 8, 3),
        ] {
            let f = frame(cover, shard, group, rec);
            let p = f.encode().unwrap();
            assert_eq!(RecoveryFrame::parse(&p, INDEX_AT).unwrap(), f);
            assert_eq!(RecoveryFrame::parse_vec(p, INDEX_AT).unwrap(), f);
        }
    }

    #[test]
    fn shard_range_of_last_shard_is_short() {
        let f = frame(130, 64, 4, 1);
        assert_eq!(f.shard_range(0), (32, 64));
        assert_eq!(f.shard_range(2), (32 + 128, 2));
    }

    #[test]
    fn every_rule_has_a_negative_test() {
        let good = frame(200, 64, 8, 2).encode().unwrap();
        let patch = |at: usize, v: &[u8]| {
            let mut p = good.clone();
            p[at..at + v.len()].copy_from_slice(v);
            p
        };
        let r = |p: Vec<u8>| reason(RecoveryFrame::parse(&p, INDEX_AT));
        assert_eq!(r(patch(16, &65u32.to_le_bytes())), "shard_len");
        assert_eq!(r(patch(16, &0u32.to_le_bytes())), "shard_len");
        assert_eq!(
            r(patch(16, &(MAX_SHARD_LEN + 64).to_le_bytes())),
            "shard_len"
        );
        assert_eq!(r(patch(0, &31u64.to_le_bytes())), "cover range");
        assert_eq!(r(patch(20, &5u32.to_le_bytes())), "data_shards");
        assert_eq!(r(patch(8, &0u64.to_le_bytes())), "data_shards");
        // More real shards than the group has (cover 200 = 4 shards, group 3).
        assert_eq!(r(patch(24, &3u32.to_le_bytes())), "data_shards");
        assert_eq!(r(patch(24, &0u32.to_le_bytes())), "group_shards");
        assert_eq!(r(patch(24, &32769u32.to_le_bytes())), "group_shards");
        assert_eq!(r(patch(28, &0u32.to_le_bytes())), "recovery_shards");
        assert_eq!(r(patch(28, &70000u32.to_le_bytes())), "shard count");
        // Inside our bounds (sum 65000) but outside the coding library's.
        let mut lib = patch(24, &32000u32.to_le_bytes());
        lib[28..32].copy_from_slice(&33000u32.to_le_bytes());
        assert_eq!(r(lib), "shard count");
        assert_eq!(reason(RecoveryFrame::parse(&good, 231)), "cover range");
        assert!(RecoveryFrame::parse(&good, 232).is_ok());
        assert_eq!(r(patch(0, &u64::MAX.to_le_bytes())), "cover range");
        // group_shards * shard_len above 1 GiB.
        let mut big = frame(200, 64, 8, 2);
        big.shard_len = 1 << 20;
        big.group_shards = 2048;
        assert_eq!(
            reason(RecoveryFrame::parse(&big.head_bytes(), INDEX_AT)),
            "group size"
        );
    }

    #[test]
    fn payload_length_mismatch_and_truncation() {
        let good = frame(200, 64, 8, 2).encode().unwrap();
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
        p[20..24].copy_from_slice(&32768u32.to_le_bytes());
        p[8..16].copy_from_slice(&(32768u64 * 64).to_le_bytes());
        p[24..28].copy_from_slice(&32768u32.to_le_bytes());
        assert_eq!(reason(RecoveryFrame::parse(&p, u64::MAX)), "payload length");
    }

    #[test]
    fn cover_must_tile_the_frames_between_recovery_frames() {
        let f = frame(200, 64, 8, 2); // covers 32..232
        let at = |offset, len| FrameLocation {
            offset,
            len,
            sequence: 0,
        };
        assert!(check_cover(&f, 0, &[at(232, 100)]).is_ok());
        for bad_locs in [[at(233, 100)], [at(231, 100)], [at(100, 50)]] {
            assert!(matches!(
                check_cover(&f, 0, &bad_locs),
                Err(FormatError::BadRecovery { reason: "coverage" })
            ));
        }
        // Frame 1 starts where frame 0's location ends.
        let mut g = frame(200, 64, 8, 2);
        g.cover_offset = 332;
        assert!(check_cover(&g, 1, &[at(232, 100), at(532, 100)]).is_ok());
        assert!(check_cover(&g, 1, &[at(232, 99), at(532, 100)]).is_err());
        assert!(check_cover(&f, 1, &[at(0, 40), at(232, 100)]).is_err());
    }

    #[test]
    fn encode_refuses_inconsistent_frames() {
        let mut f = frame(200, 64, 8, 2);
        f.shard_hashes.pop();
        assert!(f.encode().is_err());
        let mut f = frame(200, 64, 8, 2);
        f.recovery.pop();
        assert!(f.encode().is_err());
    }
}
