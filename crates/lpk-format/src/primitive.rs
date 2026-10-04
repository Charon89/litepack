//! The primitive registry v1: every codec and transform an archive may reference
//! (spec section 8). Nothing executable is ever stored in an archive; a block's
//! decoding is a graph of these fixed primitives.

use crate::error::FormatError;

/// A primitive of the registry, by its 16-bit ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum PrimitiveId {
    /// Identity.
    Store = 0,
    /// Zstandard.
    Zstd = 1,
    /// LZMA1.
    Lzma = 2,
    /// Burrows-Wheeler transform.
    Bwt = 3,
    /// x86 branch converter.
    BcjX86 = 4,
    /// ARM64 branch converter.
    BcjArm64 = 5,
    /// Delta against another chunk.
    Delta = 6,
    /// JPEG bit-exact reconstruction.
    JpegReconstruct = 7,
    /// Deflate stream reconstruction.
    DeflateReconstruct = 8,
    /// PNG filter reconstruction.
    PngFilter = 9,
    /// Base64 text decoding.
    Base64 = 10,
    /// UTF-16 text transcoding.
    Utf16 = 11,
    /// Container (zip, office, apk) reconstruction.
    ContainerReconstruct = 12,
}

/// The resources a graph needs from the decoder, for the envelope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphResources {
    /// Largest match-finder window in bytes.
    pub window: u64,
    /// Largest BWT block in bytes.
    pub bwt_block: u64,
}

fn bad(id: PrimitiveId, reason: &'static str) -> FormatError {
    FormatError::BadParams {
        id: id as u16,
        reason,
    }
}

impl PrimitiveId {
    /// Every primitive, in ID order.
    pub const ALL: [PrimitiveId; 13] = [
        PrimitiveId::Store,
        PrimitiveId::Zstd,
        PrimitiveId::Lzma,
        PrimitiveId::Bwt,
        PrimitiveId::BcjX86,
        PrimitiveId::BcjArm64,
        PrimitiveId::Delta,
        PrimitiveId::JpegReconstruct,
        PrimitiveId::DeflateReconstruct,
        PrimitiveId::PngFilter,
        PrimitiveId::Base64,
        PrimitiveId::Utf16,
        PrimitiveId::ContainerReconstruct,
    ];

    /// The primitive with this ID, if the registry knows it.
    pub fn from_u16(id: u16) -> Option<PrimitiveId> {
        PrimitiveId::ALL.get(usize::from(id)).copied()
    }

    /// The registry name.
    pub fn name(self) -> &'static str {
        match self {
            PrimitiveId::Store => "store",
            PrimitiveId::Zstd => "zstd",
            PrimitiveId::Lzma => "lzma",
            PrimitiveId::Bwt => "bwt",
            PrimitiveId::BcjX86 => "bcj-x86",
            PrimitiveId::BcjArm64 => "bcj-arm64",
            PrimitiveId::Delta => "delta",
            PrimitiveId::JpegReconstruct => "jpeg-reconstruct",
            PrimitiveId::DeflateReconstruct => "deflate-reconstruct",
            PrimitiveId::PngFilter => "png-filter",
            PrimitiveId::Base64 => "base64",
            PrimitiveId::Utf16 => "utf16",
            PrimitiveId::ContainerReconstruct => "container-reconstruct",
        }
    }

    /// The exact parameter length of the primitive.
    fn params_len(self) -> usize {
        match self {
            PrimitiveId::Zstd => 33,
            PrimitiveId::Lzma => 7,
            PrimitiveId::Bwt => 4,
            PrimitiveId::Delta => 9,
            PrimitiveId::Base64 => 3,
            PrimitiveId::Utf16 => 2,
            _ => 0,
        }
    }

    /// Check `params` against the primitive's layout.
    pub fn validate_params(self, params: &[u8]) -> Result<(), FormatError> {
        if params.len() != self.params_len() {
            return Err(bad(self, "length"));
        }
        match self {
            PrimitiveId::Zstd if !(10..=31).contains(&params[0]) => Err(bad(self, "window_log")),
            PrimitiveId::Lzma if params[4] > 8 => Err(bad(self, "lc")),
            PrimitiveId::Lzma if params[5] > 4 => Err(bad(self, "lp")),
            PrimitiveId::Lzma if params[6] > 4 => Err(bad(self, "pb")),
            PrimitiveId::Bwt if params.iter().all(|&b| b == 0) => Err(bad(self, "block_size")),
            PrimitiveId::Delta if params[8] > 1 => Err(bad(self, "patch_format")),
            PrimitiveId::Base64 if params[0] > 1 => Err(bad(self, "variant")),
            PrimitiveId::Utf16 if params[0] > 1 => Err(bad(self, "endian")),
            PrimitiveId::Utf16 if params[1] > 1 => Err(bad(self, "bom")),
            _ => Ok(()),
        }
    }

    /// The decoder resources this primitive needs with these parameters. The
    /// parameters are assumed valid; malformed ones yield no resources.
    pub fn resources(self, params: &[u8]) -> GraphResources {
        let le32 = |at: usize| {
            params
                .get(at..at + 4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map_or(0, |b| u64::from(u32::from_le_bytes(b)))
        };
        match self {
            PrimitiveId::Zstd => GraphResources {
                window: params
                    .first()
                    .and_then(|&w| 1u64.checked_shl(u32::from(w)))
                    .unwrap_or(0),
                bwt_block: 0,
            },
            PrimitiveId::Lzma => GraphResources {
                window: le32(0),
                bwt_block: 0,
            },
            PrimitiveId::Bwt => GraphResources {
                window: 0,
                bwt_block: le32(0),
            },
            _ => GraphResources::default(),
        }
    }
}

/// The Markdown table of the registry, pasted verbatim into the spec.
pub fn primitive_table() -> String {
    "| ID | Name | Parameters | Resources |\n|---|---|---|---|\n\
     | 0x0000 | `store` | none (length 0) | none |\n\
     | 0x0001 | `zstd` | `window_log: u8` (window = 2^window_log bytes; 10..=31), `dictionary: [u8; 32]` (BLAKE3 id of a prior, all zeros = none) | window = 2^window_log |\n\
     | 0x0002 | `lzma` | `dict_size: u32` LE, `lc: u8`, `lp: u8`, `pb: u8` (the LZMA1 properties; lc <= 8, lp <= 4, pb <= 4) | window = dict_size |\n\
     | 0x0003 | `bwt` | `block_size: u32` LE (bytes; not 0) | bwt block = block_size |\n\
     | 0x0004 | `bcj-x86` | none | none |\n\
     | 0x0005 | `bcj-arm64` | none | none |\n\
     | 0x0006 | `delta` | `base_chunk: u64` LE (the chunk the patch applies to), `patch_format: u8` (0 = zstd patch, 1 = suffix-array patch) | none |\n\
     | 0x0007 | `jpeg-reconstruct` | none (the record is in the `Records` frame) | memory per image, declared by `decode_memory` |\n\
     | 0x0008 | `deflate-reconstruct` | none | none |\n\
     | 0x0009 | `png-filter` | none | none |\n\
     | 0x000A | `base64` | `variant: u8` (0 standard, 1 url-safe), `line_len: u16` LE (0 = no line breaks) | none |\n\
     | 0x000B | `utf16` | `endian: u8` (0 LE, 1 BE), `bom: u8` (0 none, 1 present) | none |\n\
     | 0x000C | `container-reconstruct` | none | none |\n\
     | 0x000D..=0x7FFF | reserved for later versions of this spec | - | - |\n\
     | 0x8000..=0xFFFF | experimental; a conforming writer never emits them | - | - |\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zstd(wl: u8) -> Vec<u8> {
        let mut p = vec![wl];
        p.extend_from_slice(&[0; 32]);
        p
    }

    #[test]
    fn all_ids_and_names() {
        assert_eq!(PrimitiveId::ALL.len(), 13);
        for (i, p) in PrimitiveId::ALL.iter().enumerate() {
            assert_eq!(*p as u16, i as u16);
            assert_eq!(PrimitiveId::from_u16(i as u16), Some(*p));
        }
        assert_eq!(PrimitiveId::from_u16(13), None);
        assert_eq!(PrimitiveId::from_u16(0x8000), None);
        assert_eq!(PrimitiveId::from_u16(u16::MAX), None);
        let names: Vec<_> = PrimitiveId::ALL.iter().map(|p| p.name()).collect();
        assert_eq!(
            names,
            [
                "store",
                "zstd",
                "lzma",
                "bwt",
                "bcj-x86",
                "bcj-arm64",
                "delta",
                "jpeg-reconstruct",
                "deflate-reconstruct",
                "png-filter",
                "base64",
                "utf16",
                "container-reconstruct"
            ]
        );
    }

    #[test]
    fn validate_accepts_each_layout() {
        let ok = |p: PrimitiveId, b: &[u8]| p.validate_params(b).unwrap();
        ok(PrimitiveId::Store, &[]);
        ok(PrimitiveId::Zstd, &zstd(10));
        ok(PrimitiveId::Zstd, &zstd(31));
        ok(PrimitiveId::Lzma, &[0, 0, 0, 1, 8, 4, 4]);
        ok(PrimitiveId::Bwt, &[0, 0, 0x10, 0]);
        ok(PrimitiveId::BcjX86, &[]);
        ok(PrimitiveId::BcjArm64, &[]);
        ok(PrimitiveId::Delta, &[0, 0, 0, 0, 0, 0, 0, 0, 1]);
        ok(PrimitiveId::JpegReconstruct, &[]);
        ok(PrimitiveId::DeflateReconstruct, &[]);
        ok(PrimitiveId::PngFilter, &[]);
        ok(PrimitiveId::Base64, &[1, 76, 0]);
        ok(PrimitiveId::Utf16, &[1, 1]);
        ok(PrimitiveId::ContainerReconstruct, &[]);
    }

    fn reason(p: PrimitiveId, b: &[u8]) -> &'static str {
        match p.validate_params(b).unwrap_err() {
            FormatError::BadParams { id, reason } => {
                assert_eq!(id, p as u16);
                reason
            }
            e => panic!("unexpected {e:?}"),
        }
    }

    #[test]
    fn validate_rejects() {
        assert_eq!(reason(PrimitiveId::Zstd, &zstd(9)), "window_log");
        assert_eq!(reason(PrimitiveId::Zstd, &zstd(32)), "window_log");
        assert_eq!(reason(PrimitiveId::Zstd, &zstd(20)[..32]), "length");
        assert_eq!(reason(PrimitiveId::Lzma, &[0, 0, 0, 1, 9, 0, 0]), "lc");
        assert_eq!(reason(PrimitiveId::Lzma, &[0, 0, 0, 1, 0, 5, 0]), "lp");
        assert_eq!(reason(PrimitiveId::Lzma, &[0, 0, 0, 1, 0, 0, 5]), "pb");
        assert_eq!(reason(PrimitiveId::Lzma, &[0; 6]), "length");
        assert_eq!(reason(PrimitiveId::Bwt, &[0; 4]), "block_size");
        assert_eq!(reason(PrimitiveId::Bwt, &[1; 3]), "length");
        assert_eq!(
            reason(PrimitiveId::Delta, &[0, 0, 0, 0, 0, 0, 0, 0, 2]),
            "patch_format"
        );
        assert_eq!(reason(PrimitiveId::Delta, &[0; 8]), "length");
        assert_eq!(reason(PrimitiveId::Base64, &[2, 0, 0]), "variant");
        assert_eq!(reason(PrimitiveId::Base64, &[0, 0]), "length");
        assert_eq!(reason(PrimitiveId::Utf16, &[2, 0]), "endian");
        assert_eq!(reason(PrimitiveId::Utf16, &[0, 2]), "bom");
        for p in [
            PrimitiveId::Store,
            PrimitiveId::BcjX86,
            PrimitiveId::BcjArm64,
            PrimitiveId::JpegReconstruct,
            PrimitiveId::DeflateReconstruct,
            PrimitiveId::PngFilter,
            PrimitiveId::ContainerReconstruct,
        ] {
            assert_eq!(reason(p, &[0]), "length");
        }
    }

    #[test]
    fn resources_of_params() {
        let r = PrimitiveId::Zstd.resources(&zstd(27));
        assert_eq!(
            r,
            GraphResources {
                window: 1 << 27,
                bwt_block: 0
            }
        );
        let r = PrimitiveId::Lzma.resources(&[0, 0, 0, 4, 3, 0, 2]);
        assert_eq!(
            r,
            GraphResources {
                window: 0x0400_0000,
                bwt_block: 0
            }
        );
        let r = PrimitiveId::Bwt.resources(&[0, 0, 0x10, 0]);
        assert_eq!(
            r,
            GraphResources {
                window: 0,
                bwt_block: 0x10_0000
            }
        );
        assert_eq!(PrimitiveId::Store.resources(&[]), GraphResources::default());
        assert_eq!(PrimitiveId::Zstd.resources(&[]), GraphResources::default());
    }
}
