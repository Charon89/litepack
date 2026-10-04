//! The declared decode envelope: the resources a decoder needs, stated in the
//! index, and the refusal rule a reader applies before decoding (spec section 7).

use crate::error::FormatError;
use crate::index::{BlockLocation, FrameLocation};
use crate::primitive::GraphResources;
use crate::varint;
use std::fmt;
use std::io::{Read, Write};

const WHAT: &str = "index";

/// Bytes of a frame besides its payload and the `payload_len` varint: kind,
/// flags and the hash.
const FRAME_FIXED_LEN: u64 = 4 + 32;

/// A writer's default cap on `max_window`: 256 MiB.
pub const DEFAULT_MAX_WINDOW: u64 = 268_435_456;
/// A writer's default cap on `max_bwt_block`: 64 MiB.
pub const DEFAULT_MAX_BWT_BLOCK: u64 = 67_108_864;
/// Default local limit on `max_block_plain`: 1 GiB.
pub const DEFAULT_MAX_BLOCK_PLAIN: u64 = 1 << 30;
/// Default local limit on `max_frame_payload`: 1 GiB.
pub const DEFAULT_MAX_FRAME_PAYLOAD: u64 = 1 << 30;
/// Default local limit on `decode_memory`: 2 GiB.
pub const DEFAULT_MEMORY: u64 = 2 << 30;

/// The resources a decoder needs for an archive, as the writer declares them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Envelope {
    /// Largest match-finder window (dictionary) any block needs, in bytes.
    pub max_window: u64,
    /// Largest BWT block any block needs, in bytes; 0 when no BWT is used.
    pub max_bwt_block: u64,
    /// Largest `plain_len` of any `ChunkData` block.
    pub max_block_plain: u64,
    /// Largest frame payload in the archive.
    pub max_frame_payload: u64,
    /// Writer's estimate of peak decoder memory for one decoding thread, in bytes.
    pub decode_memory: u64,
    /// Independent blocks that may be decoded at once (0 = no hint).
    pub threads_hint: u32,
}

/// The lengths of the archive's non-block frames, as `Envelope::for_archive` takes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArchiveSizes {
    /// Payload length of the index frame.
    pub index_payload_len: u64,
    /// Whole encoded length of the entry table frame.
    pub entry_table_len: u64,
    /// Whole encoded length of the records frame; 0 when there is none.
    pub records_len: u64,
    /// Largest whole encoded length among the recovery frames; 0 when there are none.
    pub recovery_len: u64,
}

/// What the local machine allows a decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resources {
    /// Largest accepted `max_window`.
    pub max_window: u64,
    /// Largest accepted `max_bwt_block`.
    pub max_bwt_block: u64,
    /// Largest accepted `max_block_plain`.
    pub max_block_plain: u64,
    /// Largest accepted `max_frame_payload`; also the frame limit while the
    /// index is being read.
    pub max_frame_payload: u64,
    /// Largest accepted `decode_memory`.
    pub memory: u64,
}

impl Default for Resources {
    fn default() -> Self {
        Resources {
            max_window: DEFAULT_MAX_WINDOW,
            max_bwt_block: DEFAULT_MAX_BWT_BLOCK,
            max_block_plain: DEFAULT_MAX_BLOCK_PLAIN,
            max_frame_payload: DEFAULT_MAX_FRAME_PAYLOAD,
            memory: DEFAULT_MEMORY,
        }
    }
}

/// An envelope field that exceeds what the reader allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The envelope field that is too large.
    pub field: &'static str,
    /// The value the archive declares.
    pub needed: u64,
    /// The value the reader allows.
    pub allowed: u64,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the archive needs {} of {} bytes; this reader allows {}",
            self.field, self.needed, self.allowed
        )
    }
}

impl std::error::Error for Refusal {}

fn rv(r: &mut impl Read) -> Result<u64, FormatError> {
    match varint::read(r) {
        Err(FormatError::Truncated { .. }) => Err(FormatError::Truncated { what: WHAT }),
        other => other,
    }
}

/// True when a frame of whole encoded length `frame_len` can hold a payload
/// of at most `max_payload` bytes: `frame_len <= max_payload + 36 +
/// len(varint(max_payload))`. The right side is increasing in `max_payload`.
pub(crate) fn frame_len_admissible(frame_len: u64, max_payload: u64) -> bool {
    frame_len <= max_payload.saturating_add(FRAME_FIXED_LEN + varint::len(max_payload) as u64)
}

/// The smallest `max_payload` for which `frame_len` is admissible.
fn min_payload_for(frame_len: u64) -> u64 {
    // The answer is within ten of `frame_len - 37`.
    let mut m = frame_len.saturating_sub(FRAME_FIXED_LEN + 10);
    while !frame_len_admissible(frame_len, m) {
        m += 1;
    }
    m
}

impl Envelope {
    /// Write the six varints.
    pub fn write(&self, w: &mut impl Write) -> Result<(), FormatError> {
        for v in [
            self.max_window,
            self.max_bwt_block,
            self.max_block_plain,
            self.max_frame_payload,
            self.decode_memory,
            u64::from(self.threads_hint),
        ] {
            varint::write(w, v)?;
        }
        Ok(())
    }

    /// Read the six varints; an input that ends early is `Truncated` with
    /// `what` `index`, a `threads_hint` above `u32::MAX` is `EnvelopeMismatch`.
    pub fn read(r: &mut impl Read) -> Result<Envelope, FormatError> {
        let max_window = rv(r)?;
        let max_bwt_block = rv(r)?;
        let max_block_plain = rv(r)?;
        let max_frame_payload = rv(r)?;
        let decode_memory = rv(r)?;
        let threads_hint = u32::try_from(rv(r)?).map_err(|_| FormatError::EnvelopeMismatch {
            field: "threads_hint",
        })?;
        Ok(Envelope {
            max_window,
            max_bwt_block,
            max_block_plain,
            max_frame_payload,
            decode_memory,
            threads_hint,
        })
    }

    /// Compare with the local resources; the first field, in the order of the
    /// layout table, that exceeds its limit is the refusal.
    pub fn check(&self, local: &Resources) -> Result<(), Refusal> {
        for (field, needed, allowed) in [
            ("max_window", self.max_window, local.max_window),
            ("max_bwt_block", self.max_bwt_block, local.max_bwt_block),
            (
                "max_block_plain",
                self.max_block_plain,
                local.max_block_plain,
            ),
            (
                "max_frame_payload",
                self.max_frame_payload,
                local.max_frame_payload,
            ),
            ("decode_memory", self.decode_memory, local.memory),
        ] {
            if needed > allowed {
                return Err(Refusal {
                    field,
                    needed,
                    allowed,
                });
            }
        }
        Ok(())
    }

    /// An envelope whose `max_block_plain` and `max_frame_payload` are computed
    /// from the archive's frames: the blocks, the entry table, the records frame
    /// and the index's own payload length (see [`ArchiveSizes`]); `max_window`
    /// and `max_bwt_block` are the graphs' maxima (`graph`), the other fields
    /// the writer's. `max_frame_payload` is the smallest value that admits
    /// every recorded frame length and the index payload.
    pub fn for_archive(
        blocks: &[BlockLocation],
        sizes: ArchiveSizes,
        graph: GraphResources,
        decode_memory: u64,
        threads_hint: u32,
    ) -> Envelope {
        let ArchiveSizes {
            index_payload_len,
            entry_table_len,
            records_len,
            recovery_len,
        } = sizes;
        let frames = blocks
            .iter()
            .map(|b| b.frame_len)
            .chain([entry_table_len, records_len, recovery_len])
            .map(min_payload_for)
            .max()
            .unwrap_or(0);
        Envelope {
            max_window: graph.window,
            max_bwt_block: graph.bwt_block,
            max_block_plain: blocks.iter().map(|b| b.plain_len).max().unwrap_or(0),
            max_frame_payload: frames.max(index_payload_len),
            decode_memory,
            threads_hint,
        }
    }

    /// Every recovery frame is admissible under `max_frame_payload`.
    pub(crate) fn validate_recovery(&self, recovery: &[FrameLocation]) -> Result<(), FormatError> {
        if recovery
            .iter()
            .all(|r| frame_len_admissible(r.len, self.max_frame_payload))
        {
            Ok(())
        } else {
            Err(FormatError::EnvelopeMismatch {
                field: "max_frame_payload",
            })
        }
    }

    /// The consistency rules between the envelope and the index: the index's
    /// payload length fits `max_frame_payload`, every recorded frame (blocks,
    /// entry table, records) is admissible under it, and `max_block_plain` is
    /// the maximum block `plain_len`.
    pub(crate) fn validate(
        &self,
        blocks: &[BlockLocation],
        entry_table: FrameLocation,
        records: Option<FrameLocation>,
        index_payload_len: u64,
    ) -> Result<(), FormatError> {
        let plain = blocks.iter().map(|b| b.plain_len).max().unwrap_or(0);
        if self.max_block_plain != plain {
            return Err(FormatError::EnvelopeMismatch {
                field: "max_block_plain",
            });
        }
        let m = self.max_frame_payload;
        let ok = index_payload_len <= m
            && blocks.iter().all(|b| frame_len_admissible(b.frame_len, m))
            && frame_len_admissible(entry_table.len, m)
            && records.is_none_or(|r| frame_len_admissible(r.len, m));
        if !ok {
            return Err(FormatError::EnvelopeMismatch {
                field: "max_frame_payload",
            });
        }
        Ok(())
    }
}

/// The Markdown table of the envelope fields, pasted verbatim into the spec.
pub fn envelope_layout_table() -> String {
    "| Field | Size | Meaning |\n|---|---|---|\n\
     | max_window | varint | largest match-finder window (dictionary) any block needs, in bytes |\n\
     | max_bwt_block | varint | largest BWT block any block needs, in bytes; 0 when no BWT is used |\n\
     | max_block_plain | varint | largest `plain_len` of any block; must equal the maximum over the block table |\n\
     | max_frame_payload | varint | largest frame payload in the archive; must admit the index's own payload and every recorded frame (blocks, entry table, records, recovery) |\n\
     | decode_memory | varint | the writer's estimate of peak decoder memory for one decoding thread, in bytes |\n\
     | threads_hint | varint | independent blocks a reader may decode at once within `decode_memory` times this; 0 = no hint; at most 4294967295 |\n"
        .to_string()
}

/// The Markdown table of the default local resources, pasted verbatim into the spec.
pub fn resources_default_table() -> String {
    format!(
        "| Resource | Default (bytes) | Bounds the envelope field |\n|---|---|---|\n\
         | max_window | {DEFAULT_MAX_WINDOW} (256 MiB) | max_window |\n\
         | max_bwt_block | {DEFAULT_MAX_BWT_BLOCK} (64 MiB) | max_bwt_block |\n\
         | max_block_plain | {DEFAULT_MAX_BLOCK_PLAIN} (1 GiB) | max_block_plain |\n\
         | max_frame_payload | {DEFAULT_MAX_FRAME_PAYLOAD} (1 GiB) | max_frame_payload |\n\
         | memory | {DEFAULT_MEMORY} (2 GiB) | decode_memory |\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Frame, FrameFlags, FrameKind};

    fn env() -> Envelope {
        Envelope {
            max_window: 1 << 20,
            max_bwt_block: 1 << 16,
            max_block_plain: 5000,
            max_frame_payload: 6000,
            decode_memory: 123_456_789,
            threads_hint: 4,
        }
    }

    fn enc(e: &Envelope) -> Vec<u8> {
        let mut v = Vec::new();
        e.write(&mut v).unwrap();
        v
    }

    #[test]
    fn round_trip() {
        let extremes = Envelope {
            max_window: u64::MAX,
            max_bwt_block: 0,
            max_block_plain: u64::MAX,
            max_frame_payload: 1,
            decode_memory: u64::MAX,
            threads_hint: u32::MAX,
        };
        for e in [env(), extremes] {
            let b = enc(&e);
            let mut s = &b[..];
            assert_eq!(Envelope::read(&mut s).unwrap(), e);
            assert!(s.is_empty());
        }
    }

    #[test]
    fn truncation_is_index() {
        let b = enc(&env());
        for cut in 0..b.len() {
            let e = Envelope::read(&mut &b[..cut]).unwrap_err();
            assert!(
                matches!(e, FormatError::Truncated { what: "index" }),
                "cut {cut}: {e:?}"
            );
        }
    }

    #[test]
    fn threads_hint_above_u32_is_rejected() {
        let mut b = Vec::new();
        for v in [1u64, 2, 3, 4, 5, u64::from(u32::MAX) + 1] {
            varint::write(&mut b, v).unwrap();
        }
        assert!(matches!(
            Envelope::read(&mut &b[..]),
            Err(FormatError::EnvelopeMismatch {
                field: "threads_hint"
            })
        ));
    }

    #[test]
    fn defaults_are_the_stated_caps() {
        let d = Resources::default();
        assert_eq!(d.max_window, 268_435_456);
        assert_eq!(d.max_bwt_block, 67_108_864);
        assert_eq!(d.max_block_plain, 1 << 30);
        assert_eq!(d.max_frame_payload, 1 << 30);
        assert_eq!(d.memory, 2 << 30);
    }

    fn big() -> Resources {
        Resources {
            max_window: 100,
            max_bwt_block: 200,
            max_block_plain: 300,
            max_frame_payload: 400,
            memory: 500,
        }
    }

    fn fits() -> Envelope {
        Envelope {
            max_window: 100,
            max_bwt_block: 200,
            max_block_plain: 300,
            max_frame_payload: 400,
            decode_memory: 500,
            threads_hint: 0,
        }
    }

    #[test]
    fn check_table() {
        // At the limit passes.
        assert_eq!(fits().check(&big()), Ok(()));
        type Tweak = fn(&mut Envelope);
        let cases: [(Tweak, &str, u64, u64); 5] = [
            (|e| e.max_window = 101, "max_window", 101, 100),
            (|e| e.max_bwt_block = 201, "max_bwt_block", 201, 200),
            (|e| e.max_block_plain = 301, "max_block_plain", 301, 300),
            (|e| e.max_frame_payload = 401, "max_frame_payload", 401, 400),
            (|e| e.decode_memory = 501, "decode_memory", 501, 500),
        ];
        for (tweak, field, needed, allowed) in cases {
            let mut e = fits();
            tweak(&mut e);
            assert_eq!(
                e.check(&big()),
                Err(Refusal {
                    field,
                    needed,
                    allowed
                })
            );
            // One below the limit passes: lower the tweaked field.
            let mut at = fits();
            tweak(&mut at);
            match field {
                "max_window" => at.max_window -= 1,
                "max_bwt_block" => at.max_bwt_block -= 1,
                "max_block_plain" => at.max_block_plain -= 1,
                "max_frame_payload" => at.max_frame_payload -= 1,
                _ => at.decode_memory -= 1,
            }
            assert_eq!(at.check(&big()), Ok(()));
        }
        // Several exceed: the first in field order is reported.
        let mut e = fits();
        e.max_bwt_block = 999;
        e.decode_memory = 999;
        e.max_frame_payload = 999;
        assert_eq!(e.check(&big()).unwrap_err().field, "max_bwt_block");
        e.max_bwt_block = 1;
        assert_eq!(e.check(&big()).unwrap_err().field, "max_frame_payload");
        // Local resources above the envelope.
        assert_eq!(env().check(&Resources::default()), Ok(()));
        // threads_hint never refuses.
        let mut t = fits();
        t.threads_hint = u32::MAX;
        assert_eq!(t.check(&big()), Ok(()));
    }

    #[test]
    fn refusal_text_names_the_limit() {
        let r = Refusal {
            field: "max_window",
            needed: 536_870_912,
            allowed: 268_435_456,
        };
        assert_eq!(
            r.to_string(),
            "the archive needs max_window of 536870912 bytes; this reader allows 268435456"
        );
        assert_eq!(
            FormatError::Refused(r).to_string(),
            "the archive needs max_window of 536870912 bytes; this reader allows 268435456"
        );
    }

    #[test]
    fn admissible_frame_lengths() {
        // Frames of payload p have length 36 + len(varint(p)) + p, and are admissible for M >= p.
        for p in [0u64, 1, 126, 127, 128, 16_383, 16_384, 100_000, 1 << 28] {
            let len = 36 + varint::len(p) as u64 + p;
            assert!(frame_len_admissible(len, p), "payload {p}");
            if p > 0 {
                assert!(!frame_len_admissible(len, p - 1), "payload {p} - 1");
            }
        }
        // Real frames.
        for n in [0usize, 5, 127, 128, 300] {
            let f = Frame {
                kind: FrameKind::ChunkData,
                flags: FrameFlags::EMPTY,
                payload: vec![0; n],
            };
            assert!(frame_len_admissible(f.encoded_len(), n as u64));
            assert!(n == 0 || !frame_len_admissible(f.encoded_len(), n as u64 - 1));
        }
        // 165 is no frame's length: payload 127 gives 164, 128 gives 166.
        assert!(frame_len_admissible(164, 127));
        assert!(!frame_len_admissible(165, 127));
        assert!(frame_len_admissible(165, 128));
        // Same at the three-byte varint boundary: 16421 and 16423.
        assert!(frame_len_admissible(16_421, 16_383));
        assert!(!frame_len_admissible(16_422, 16_383));
        assert!(frame_len_admissible(16_422, 16_384));
        assert!(frame_len_admissible(16_423, 16_384));
        assert!(!frame_len_admissible(16_424, 16_384));
        // The smallest M: monotone, and never overflowing.
        assert_eq!(min_payload_for(165), 128);
        assert_eq!(min_payload_for(16_422), 16_384);
        assert_eq!(min_payload_for(0), 0);
        assert_eq!(min_payload_for(37), 0);
        assert_eq!(min_payload_for(38), 1);
        let m = min_payload_for(u64::MAX);
        assert!(frame_len_admissible(u64::MAX, m));
        assert!(!frame_len_admissible(u64::MAX, m - 1));
    }

    fn block(frame_len: u64, plain_len: u64) -> BlockLocation {
        BlockLocation {
            frame_offset: 100,
            frame_len,
            first_chunk: 0,
            chunk_count: 1,
            plain_len,
            sequence: 0,
        }
    }

    fn sz(index_payload_len: u64, entry_table_len: u64, records_len: u64) -> ArchiveSizes {
        ArchiveSizes {
            index_payload_len,
            entry_table_len,
            records_len,
            recovery_len: 0,
        }
    }

    fn gr(window: u64, bwt_block: u64) -> GraphResources {
        GraphResources { window, bwt_block }
    }

    fn floc(len: u64) -> FrameLocation {
        FrameLocation {
            offset: 40,
            len,
            sequence: 0,
        }
    }

    #[test]
    fn for_archive_takes_the_maxima() {
        let blocks = [block(137, 50), block(1037, 20), block(537, 90)];
        let e = Envelope::for_archive(&blocks, sz(10, 40, 0), gr(7, 8), 9, 2);
        assert_eq!(e.max_block_plain, 90);
        assert_eq!(e.max_frame_payload, 1037 - 36 - 2);
        assert_eq!(
            (
                e.max_window,
                e.max_bwt_block,
                e.decode_memory,
                e.threads_hint
            ),
            (7, 8, 9, 2)
        );
        // The index payload, the entry table or the records frame can be the largest.
        let e = Envelope::for_archive(&blocks, sz(5000, 40, 0), gr(0, 0), 0, 0);
        assert_eq!(e.max_frame_payload, 5000);
        let e = Envelope::for_archive(&blocks, sz(10, 3000, 40), gr(0, 0), 0, 0);
        assert_eq!(e.max_frame_payload, 3000 - 36 - 2);
        let e = Envelope::for_archive(&blocks, sz(10, 40, 4000), gr(0, 0), 0, 0);
        assert_eq!(e.max_frame_payload, 4000 - 36 - 2);
        let e = Envelope::for_archive(&[], sz(77, 40, 0), gr(0, 0), 0, 0);
        assert_eq!((e.max_block_plain, e.max_frame_payload), (0, 77));
        e.validate(&[], floc(40), None, 77).unwrap();
    }

    fn mismatch(r: Result<(), FormatError>, field: &str) {
        assert!(
            matches!(r, Err(FormatError::EnvelopeMismatch { field: f }) if f == field),
            "{field}: {r:?}"
        );
    }

    #[test]
    fn validate_rules() {
        let blocks = [block(137, 50), block(1037, 90)];
        let good = Envelope::for_archive(&blocks, sz(100, 200, 0), gr(1, 1), 1, 1);
        good.validate(&blocks, floc(200), None, 100).unwrap();
        // Overstating the frame payload is allowed; the plain maximum is exact.
        let mut e = good;
        e.max_frame_payload += 1;
        e.validate(&blocks, floc(200), None, 100).unwrap();
        let mut e = good;
        e.max_block_plain = 89;
        mismatch(e.validate(&blocks, floc(200), None, 100), "max_block_plain");
        e.max_block_plain = 91;
        mismatch(e.validate(&blocks, floc(200), None, 100), "max_block_plain");
        let mut e = good;
        e.max_frame_payload = 1037 - 36 - 2 - 1;
        mismatch(
            e.validate(&blocks, floc(200), None, 100),
            "max_frame_payload",
        );
        let mut e = good;
        e.max_frame_payload = 100;
        e.max_block_plain = 50;
        assert!(e.validate(&blocks[..1], floc(136), None, 100).is_ok());
        mismatch(
            e.validate(&blocks[..1], floc(136), None, 101),
            "max_frame_payload",
        );
        // The entry table and the records frame count too (payload 100 admits length 137).
        assert!(e.validate(&blocks[..1], floc(137), None, 100).is_ok());
        mismatch(
            e.validate(&blocks[..1], floc(138), None, 100),
            "max_frame_payload",
        );
        assert!(e
            .validate(&blocks[..1], floc(40), Some(floc(137)), 100)
            .is_ok());
        mismatch(
            e.validate(&blocks[..1], floc(40), Some(floc(138)), 100),
            "max_frame_payload",
        );
    }

    #[test]
    fn layout_tables_list_every_field() {
        let t = envelope_layout_table();
        for f in [
            "max_window",
            "max_bwt_block",
            "max_block_plain",
            "max_frame_payload",
            "decode_memory",
            "threads_hint",
        ] {
            assert!(t.contains(f), "{f}");
        }
        let r = resources_default_table();
        assert!(r.contains("268435456") && r.contains("67108864") && r.contains("2147483648"));
    }
}
