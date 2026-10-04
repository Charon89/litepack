//! Running a decode graph: the decoder trait, the registry of decoders and
//! `decode_block` (spec section 8).

use crate::envelope::Resources;
use crate::error::FormatError;
use crate::graph::BlockHeader;
use crate::primitive::PrimitiveId;

/// A decoder for one primitive.
pub trait PrimitiveDecoder: Send + Sync {
    /// Decode `input` with the step's `params` (already validated).
    /// `expected_len` is the most output the decoder may produce; for the last
    /// step of a block it is exactly the block's `plain_len`, for the others
    /// `limits.max_block_plain`. A decoder must not allocate output beyond it.
    fn decode(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        limits: &Resources,
    ) -> Result<Vec<u8>, FormatError>;
}

/// The identity decoder of `store`.
struct StoreDecoder;

impl PrimitiveDecoder for StoreDecoder {
    fn decode(
        &self,
        _params: &[u8],
        input: &[u8],
        expected_len: u64,
        _limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        let len = input.len() as u64;
        if len > expected_len {
            return Err(FormatError::PayloadTooLarge {
                len,
                max: expected_len,
            });
        }
        Ok(input.to_vec())
    }
}

/// The decoder of a primitive this reader does not run.
struct Unimplemented(PrimitiveId);

impl PrimitiveDecoder for Unimplemented {
    fn decode(
        &self,
        _params: &[u8],
        _input: &[u8],
        _expected_len: u64,
        _limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        Err(FormatError::UnimplementedPrimitive { id: self.0 as u16 })
    }
}

/// One decoder per primitive of the registry.
pub struct Registry {
    decoders: Vec<Box<dyn PrimitiveDecoder>>,
}

impl Registry {
    /// Every v1 primitive present: `store` is real, the others report
    /// `UnimplementedPrimitive` until a decoder is registered.
    pub fn v1() -> Registry {
        let decoders = PrimitiveId::ALL
            .iter()
            .map(|&p| -> Box<dyn PrimitiveDecoder> {
                match p {
                    PrimitiveId::Store => Box::new(StoreDecoder),
                    other => Box::new(Unimplemented(other)),
                }
            })
            .collect();
        Registry { decoders }
    }

    /// The decoder of `id`.
    pub fn decoder(&self, id: PrimitiveId) -> &dyn PrimitiveDecoder {
        // `decoders` has one entry per `PrimitiveId::ALL`, in ID order.
        self.decoders[id as usize].as_ref()
    }

    /// Replace the decoder of `id` (how the zstd and LZMA decoders plug in).
    pub fn register(&mut self, id: PrimitiveId, decoder: Box<dyn PrimitiveDecoder>) {
        self.decoders[id as usize] = decoder;
    }
}

/// Run the header's graph over `encoded` and return the plain bytes.
///
/// Checks: `plain_len` at most `limits.max_block_plain` (`PayloadTooLarge`);
/// `encoded.len()` equal to `encoded_len` and the last step's output length
/// equal to `plain_len` (`BlockLengthMismatch`, `block` 0); an intermediate
/// output larger than `limits.max_block_plain` is `PayloadTooLarge`.
pub fn decode_block(
    registry: &Registry,
    header: &BlockHeader,
    encoded: &[u8],
    limits: &Resources,
) -> Result<Vec<u8>, FormatError> {
    if header.plain_len > limits.max_block_plain {
        return Err(FormatError::PayloadTooLarge {
            len: header.plain_len,
            max: limits.max_block_plain,
        });
    }
    if encoded.len() as u64 != header.encoded_len {
        return Err(FormatError::BlockLengthMismatch { block: 0 });
    }
    let last = header.graph.steps.len().saturating_sub(1);
    let mut data = std::borrow::Cow::Borrowed(encoded);
    for (i, step) in header.graph.steps.iter().enumerate() {
        let expected = if i == last {
            header.plain_len
        } else {
            limits.max_block_plain
        };
        let out = registry
            .decoder(step.primitive)
            .decode(&step.params, &data, expected, limits)?;
        let len = out.len() as u64;
        if i == last {
            if len != header.plain_len {
                return Err(FormatError::BlockLengthMismatch { block: 0 });
            }
        } else if len > limits.max_block_plain {
            return Err(FormatError::PayloadTooLarge {
                len,
                max: limits.max_block_plain,
            });
        }
        data = std::borrow::Cow::Owned(out);
    }
    Ok(data.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Step};

    fn store() -> Step {
        Step {
            primitive: PrimitiveId::Store,
            params: vec![],
        }
    }

    fn header(steps: Vec<Step>, plain: u64, enc: u64) -> BlockHeader {
        BlockHeader {
            graph: Graph { steps },
            plain_len: plain,
            encoded_len: enc,
        }
    }

    #[test]
    fn store_is_identity() {
        let r = Registry::v1();
        let h = header(vec![store()], 5, 5);
        let out = decode_block(&r, &h, b"hello", &Resources::default()).unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn two_store_steps() {
        let r = Registry::v1();
        let h = header(vec![store(), store()], 5, 5);
        let out = decode_block(&r, &h, b"hello", &Resources::default()).unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn plain_len_mismatch() {
        let r = Registry::v1();
        let limits = Resources::default();
        let h = header(vec![store()], 6, 5);
        assert!(matches!(
            decode_block(&r, &h, b"hello", &limits).unwrap_err(),
            FormatError::BlockLengthMismatch { .. }
        ));
        let h = header(vec![store()], 4, 5);
        assert!(matches!(
            decode_block(&r, &h, b"hello", &limits).unwrap_err(),
            FormatError::PayloadTooLarge { len: 5, max: 4 }
        ));
        let h = header(vec![store()], 5, 4);
        assert!(matches!(
            decode_block(&r, &h, b"hello", &limits).unwrap_err(),
            FormatError::BlockLengthMismatch { .. }
        ));
    }

    #[test]
    fn unimplemented_primitives() {
        let r = Registry::v1();
        let mut p = vec![20u8];
        p.extend_from_slice(&[0; 32]);
        let h = header(
            vec![Step {
                primitive: PrimitiveId::Zstd,
                params: p,
            }],
            5,
            5,
        );
        assert!(matches!(
            decode_block(&r, &h, b"hello", &Resources::default()).unwrap_err(),
            FormatError::UnimplementedPrimitive { id: 1 }
        ));
        for p in PrimitiveId::ALL {
            let e = r
                .decoder(p)
                .decode(&[], b"", 0, &Resources::default())
                .map(|_| ());
            match p {
                PrimitiveId::Store => e.unwrap(),
                _ => assert!(matches!(
                    e.unwrap_err(),
                    FormatError::UnimplementedPrimitive { id } if id == p as u16
                )),
            }
        }
    }

    #[test]
    fn output_above_max_block_plain() {
        let r = Registry::v1();
        let small = Resources {
            max_block_plain: 4,
            ..Resources::default()
        };
        // The declared plain length is already above the limit.
        let h = header(vec![store(), store()], 8, 8);
        assert!(matches!(
            decode_block(&r, &h, &[0; 8], &small).unwrap_err(),
            FormatError::PayloadTooLarge { len: 8, max: 4 }
        ));
        // An intermediate output above the limit.
        let h = header(vec![store(), store()], 4, 8);
        assert!(matches!(
            decode_block(&r, &h, &[0; 8], &small).unwrap_err(),
            FormatError::PayloadTooLarge { max: 4, .. }
        ));
    }

    #[test]
    fn a_registered_decoder_is_used() {
        struct Upper;
        impl PrimitiveDecoder for Upper {
            fn decode(
                &self,
                _: &[u8],
                input: &[u8],
                _: u64,
                _: &Resources,
            ) -> Result<Vec<u8>, FormatError> {
                Ok(input.to_ascii_uppercase())
            }
        }
        let mut r = Registry::v1();
        r.register(PrimitiveId::BcjX86, Box::new(Upper));
        let h = header(
            vec![
                Step {
                    primitive: PrimitiveId::BcjX86,
                    params: vec![],
                },
                store(),
            ],
            2,
            2,
        );
        assert_eq!(
            decode_block(&r, &h, b"ab", &Resources::default()).unwrap(),
            b"AB"
        );
    }
}
