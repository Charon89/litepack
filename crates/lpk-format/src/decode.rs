//! Running a decode graph: the decoder trait, the registry of decoders and
//! `decode_block` (spec section 8).

use crate::envelope::Resources;
use crate::error::FormatError;
use crate::graph::{BlockHeader, MAX_STEPS};
use crate::lzma::LzmaDecoder;
use crate::primitive::PrimitiveId;
use crate::priors::{NoPriors, PriorStore};
use crate::zstd::ZstdDecoder;
use std::sync::Arc;

/// A decoder for one primitive.
pub trait PrimitiveDecoder: Send + Sync {
    /// Decode `input` with the step's `params` (already validated).
    /// `expected_len` is the most output the decoder may produce; for the last
    /// step of a block it is exactly the block's `plain_len`, for the others
    /// the archive's envelope `max_block_plain`. A decoder must not allocate
    /// output beyond it and reports output that would exceed it as
    /// `PayloadTooLarge`; on the last step `decode_block` turns that into
    /// `BlockLengthMismatch`.
    fn decode(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        limits: &Resources,
    ) -> Result<Vec<u8>, FormatError>;

    /// Decode one step of a graph; `last` is true for the graph's last step,
    /// whose output length is exactly `expected_len`. A step that is not the
    /// last only has `expected_len` as a bound. The default ignores `last`;
    /// the `lzma` decoder uses it (a non-final `lzma` step needs its
    /// end-of-payload marker, spec section 8).
    fn decode_step(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        last: bool,
        limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        let _ = last;
        self.decode(params, input, expected_len, limits)
    }
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

/// One decoder per primitive of the registry, and the store of priors the
/// decoders that need one look them up in.
pub struct Registry {
    decoders: Vec<Box<dyn PrimitiveDecoder>>,
    implemented: Vec<bool>,
    priors: Arc<dyn PriorStore>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("primitives", &self.decoders.len())
            .finish()
    }
}

impl Registry {
    /// Every v1 primitive present: `store`, `zstd` and `lzma` are real, the others
    /// report `UnimplementedPrimitive` until a decoder is registered. The
    /// store of priors has nothing.
    pub fn v1() -> Registry {
        let priors: Arc<dyn PriorStore> = Arc::new(NoPriors);
        let decoders = PrimitiveId::ALL
            .iter()
            .map(|&p| -> Box<dyn PrimitiveDecoder> {
                match p {
                    PrimitiveId::Store => Box::new(StoreDecoder),
                    PrimitiveId::Zstd => Box::new(ZstdDecoder::new(Arc::clone(&priors))),
                    PrimitiveId::Lzma => Box::new(LzmaDecoder),
                    other => Box::new(Unimplemented(other)),
                }
            })
            .collect();
        let implemented = PrimitiveId::ALL
            .iter()
            .map(|&p| {
                matches!(
                    p,
                    PrimitiveId::Store | PrimitiveId::Zstd | PrimitiveId::Lzma
                )
            })
            .collect();
        Registry {
            decoders,
            implemented,
            priors,
        }
    }

    /// Use `store` for priors: the zstd decoder is replaced by one that looks
    /// in it, so a custom zstd decoder registered earlier is lost (register it
    /// after this call). Other decoders are kept.
    pub fn with_priors(mut self, store: Box<dyn PriorStore>) -> Registry {
        let store: Arc<dyn PriorStore> = Arc::from(store);
        self.decoders[PrimitiveId::Zstd as usize] = Box::new(ZstdDecoder::new(Arc::clone(&store)));
        self.priors = store;
        self
    }

    /// The store of priors decoders look in.
    pub fn priors(&self) -> &dyn PriorStore {
        self.priors.as_ref()
    }

    /// The decoder of `id`.
    pub fn decoder(&self, id: PrimitiveId) -> &dyn PrimitiveDecoder {
        // `decoders` has one entry per `PrimitiveId::ALL`, in ID order.
        self.decoders[id as usize].as_ref()
    }

    /// True when `id` has a real decoder (`store`, and every one registered).
    pub fn is_implemented(&self, id: PrimitiveId) -> bool {
        self.implemented[id as usize]
    }

    /// Replace the decoder of `id` (how the zstd and LZMA decoders plug in).
    pub fn register(&mut self, id: PrimitiveId, decoder: Box<dyn PrimitiveDecoder>) {
        self.decoders[id as usize] = decoder;
        self.implemented[id as usize] = true;
    }
}

/// Run the header's graph over `encoded` and return the plain bytes. `block`
/// is the block's index in the block table, used in errors.
///
/// `limits.max_block_plain` bounds the block and every intermediate output;
/// a reader passes the archive's envelope `max_block_plain` there (which it
/// has already checked against its own resources), so whether a block
/// decodes does not depend on the reader's value. The other fields of
/// `limits` are the reader's resources.
///
/// Before anything runs: the step count must be 1..=16 (`BadGraph`), every
/// step's params must validate (`BadParams`), `plain_len` must be at most
/// `limits.max_block_plain` (`PayloadTooLarge`), `encoded.len()` must equal
/// `encoded_len` (`BlockLengthMismatch`), and every step's primitive must be
/// implemented (`UnimplementedPrimitive`). Then the steps run in order: an
/// intermediate output larger than `limits.max_block_plain` is
/// `PayloadTooLarge`; the last step must produce exactly `plain_len`
/// (`BlockLengthMismatch`, also when the decoder reports too much output).
pub fn decode_block(
    registry: &Registry,
    header: &BlockHeader,
    block: usize,
    encoded: &[u8],
    limits: &Resources,
) -> Result<Vec<u8>, FormatError> {
    let steps = &header.graph.steps;
    if steps.is_empty() || steps.len() > MAX_STEPS {
        return Err(FormatError::BadGraph {
            reason: "step count",
        });
    }
    for step in steps {
        step.primitive.validate_params(&step.params)?;
    }
    if header.plain_len > limits.max_block_plain {
        return Err(FormatError::PayloadTooLarge {
            len: header.plain_len,
            max: limits.max_block_plain,
        });
    }
    if encoded.len() as u64 != header.encoded_len {
        return Err(FormatError::BlockLengthMismatch { block });
    }
    if let Some(step) = steps.iter().find(|s| !registry.is_implemented(s.primitive)) {
        return Err(FormatError::UnimplementedPrimitive {
            id: step.primitive as u16,
        });
    }
    let last = steps.len() - 1;
    let mut data = std::borrow::Cow::Borrowed(encoded);
    for (i, step) in steps.iter().enumerate() {
        let expected = if i == last {
            header.plain_len
        } else {
            limits.max_block_plain
        };
        let out = match registry.decoder(step.primitive).decode_step(
            &step.params,
            &data,
            expected,
            i == last,
            limits,
        ) {
            Err(FormatError::PayloadTooLarge { .. }) if i == last => {
                return Err(FormatError::BlockLengthMismatch { block })
            }
            other => other?,
        };
        let len = out.len() as u64;
        if i == last {
            if len != header.plain_len {
                return Err(FormatError::BlockLengthMismatch { block });
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn step(p: PrimitiveId) -> Step {
        Step {
            primitive: p,
            params: vec![],
        }
    }

    fn store() -> Step {
        step(PrimitiveId::Store)
    }

    fn header(steps: Vec<Step>, plain: u64, enc: u64) -> BlockHeader {
        BlockHeader {
            graph: Graph { steps },
            plain_len: plain,
            encoded_len: enc,
        }
    }

    fn run(h: &BlockHeader, data: &[u8], limits: &Resources) -> Result<Vec<u8>, FormatError> {
        decode_block(&Registry::v1(), h, 7, data, limits)
    }

    #[test]
    fn store_is_identity() {
        let h = header(vec![store()], 5, 5);
        assert_eq!(run(&h, b"hello", &Resources::default()).unwrap(), b"hello");
    }

    #[test]
    fn two_store_steps() {
        let h = header(vec![store(), store()], 5, 5);
        assert_eq!(run(&h, b"hello", &Resources::default()).unwrap(), b"hello");
    }

    #[test]
    fn plain_len_mismatch_names_the_block() {
        let limits = Resources::default();
        // Too short, too long (the decoder says PayloadTooLarge), wrong encoded_len.
        for (plain, enc) in [(6, 5), (4, 5), (5, 4)] {
            let h = header(vec![store()], plain, enc);
            assert!(matches!(
                run(&h, b"hello", &limits).unwrap_err(),
                FormatError::BlockLengthMismatch { block: 7 }
            ));
        }
    }

    #[test]
    fn unimplemented_primitives() {
        let h = header(
            vec![Step {
                primitive: PrimitiveId::Bwt,
                params: vec![0, 0, 1, 0],
            }],
            5,
            5,
        );
        assert!(matches!(
            run(&h, b"hello", &Resources::default()).unwrap_err(),
            FormatError::UnimplementedPrimitive { id: 3 }
        ));
        let r = Registry::v1();
        for p in PrimitiveId::ALL {
            let real = matches!(
                p,
                PrimitiveId::Store | PrimitiveId::Zstd | PrimitiveId::Lzma
            );
            assert_eq!(r.is_implemented(p), real);
            let e = r
                .decoder(p)
                .decode(&[], b"", 0, &Resources::default())
                .map(|_| ());
            match p {
                PrimitiveId::Store => e.unwrap(),
                PrimitiveId::Zstd => assert!(matches!(
                    e.unwrap_err(),
                    FormatError::BadParams { id: 1, .. }
                )),
                PrimitiveId::Lzma => assert!(matches!(
                    e.unwrap_err(),
                    FormatError::BadParams { id: 2, .. }
                )),
                _ => assert!(matches!(
                    e.unwrap_err(),
                    FormatError::UnimplementedPrimitive { id } if id == p as u16
                )),
            }
        }
    }

    #[test]
    fn plain_len_above_max_block_plain_is_refused_first() {
        let small = Resources {
            max_block_plain: 4,
            ..Resources::default()
        };
        let h = header(vec![store(), store()], 8, 8);
        assert!(matches!(
            run(&h, &[0; 8], &small).unwrap_err(),
            FormatError::PayloadTooLarge { len: 8, max: 4 }
        ));
    }

    /// Ignores `expected_len` and returns `n` bytes.
    struct Over(usize);
    impl PrimitiveDecoder for Over {
        fn decode(
            &self,
            _: &[u8],
            _: &[u8],
            _: u64,
            _: &Resources,
        ) -> Result<Vec<u8>, FormatError> {
            Ok(vec![0; self.0])
        }
    }

    #[test]
    fn intermediate_output_above_max_block_plain() {
        let small = Resources {
            max_block_plain: 4,
            ..Resources::default()
        };
        let mut r = Registry::v1();
        r.register(PrimitiveId::BcjX86, Box::new(Over(9)));
        let h = header(vec![step(PrimitiveId::BcjX86), store()], 4, 2);
        assert!(matches!(
            decode_block(&r, &h, 0, b"ab", &small).unwrap_err(),
            FormatError::PayloadTooLarge { len: 9, max: 4 }
        ));
        // On the last step the same over-production is a length mismatch.
        let h = header(vec![store(), step(PrimitiveId::BcjX86)], 4, 2);
        assert!(matches!(
            decode_block(&r, &h, 3, b"ab", &small).unwrap_err(),
            FormatError::BlockLengthMismatch { block: 3 }
        ));
    }

    #[test]
    fn header_is_validated_before_running() {
        let limits = Resources::default();
        let h = header(vec![], 0, 0);
        assert!(matches!(
            run(&h, b"", &limits).unwrap_err(),
            FormatError::BadGraph {
                reason: "step count"
            }
        ));
        let h = header(vec![store(); 17], 0, 0);
        assert!(matches!(
            run(&h, b"", &limits).unwrap_err(),
            FormatError::BadGraph { .. }
        ));
        let h = header(
            vec![Step {
                primitive: PrimitiveId::Zstd,
                params: vec![],
            }],
            0,
            0,
        );
        assert!(matches!(
            run(&h, b"", &limits).unwrap_err(),
            FormatError::BadParams { id: 1, .. }
        ));
    }

    #[test]
    fn unimplemented_later_step_is_found_before_running() {
        struct Flag(Arc<AtomicBool>);
        impl PrimitiveDecoder for Flag {
            fn decode(
                &self,
                _: &[u8],
                input: &[u8],
                _: u64,
                _: &Resources,
            ) -> Result<Vec<u8>, FormatError> {
                self.0.store(true, Ordering::SeqCst);
                Ok(input.to_vec())
            }
        }
        let ran = Arc::new(AtomicBool::new(false));
        let mut r = Registry::v1();
        r.register(PrimitiveId::BcjX86, Box::new(Flag(ran.clone())));
        assert!(r.is_implemented(PrimitiveId::BcjX86));
        let h = header(
            vec![
                step(PrimitiveId::BcjX86),
                Step {
                    primitive: PrimitiveId::JpegReconstruct,
                    params: vec![0],
                },
            ],
            2,
            2,
        );
        assert!(matches!(
            decode_block(&r, &h, 0, b"ab", &Resources::default()).unwrap_err(),
            FormatError::UnimplementedPrimitive { id: 7 }
        ));
        assert!(!ran.load(Ordering::SeqCst));
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
        let h = header(vec![step(PrimitiveId::BcjX86), store()], 2, 2);
        assert_eq!(
            decode_block(&r, &h, 0, b"ab", &Resources::default()).unwrap(),
            b"AB"
        );
    }
}
