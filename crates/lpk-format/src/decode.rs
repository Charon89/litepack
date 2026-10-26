//! Running a decode graph: the decoder trait, the registry of decoders and
//! `decode_block` (spec section 8).

use crate::envelope::Resources;
use crate::error::FormatError;
use crate::graph::{BlockHeader, MAX_STEPS};
use crate::lzma::LzmaDecoder;
use crate::primitive::PrimitiveId;
use crate::priors::{NoPriors, PriorStore};
use crate::record::Record;
use crate::zstd::ZstdDecoder;
use std::sync::Arc;

/// What a reconstruction decoder may ask the reader for while one block is
/// decoded: the archive's records and the plain bytes of the chunks a record
/// names (spec sections 8 and 12).
pub trait DecodeContext {
    /// The index of the block being decoded.
    fn block(&self) -> usize;
    /// The record `id` of the archive's `Records` frame, hash-checked and
    /// parsed (`RecordOutOfRange` when there is no such record).
    fn record(&mut self, id: u64) -> Result<Record, FormatError>;
    /// The plain bytes of chunk `index`, compared with the chunk table, for
    /// record `record`. The chunk must lie in a block with a lower index than
    /// the block being decoded (`BadRecord`, `chunk order`), so a record never
    /// needs the block rebuilt from it.
    fn chunk(&mut self, record: u64, index: u64) -> Result<Vec<u8>, FormatError>;
    /// The reader's own memory resource (the extraction's budget), which a
    /// decoder whose library may need more than the archive declares checks
    /// that need against; `None` when there is no archive behind the decode,
    /// and the decoder then uses the limits it was given.
    fn memory_budget(&self) -> Option<u64> {
        None
    }
    /// The extraction's gate for a decode that needs more than the archive
    /// declares ([`DecodeGate`]); `None` outside a pooled extraction.
    fn gate(&self) -> Option<Arc<dyn DecodeGate>> {
        None
    }
}

/// A pooled extraction's gate for decodes whose working memory exceeds what
/// the archive's `decode_memory` declares: such a decode runs alone (no other
/// block decodes meanwhile), within the memory the pool leaves a lone decode.
pub trait DecodeGate: Send + Sync + std::fmt::Debug {
    /// Bytes a decode running alone may use beyond its block's plain bytes.
    fn alone_memory(&self) -> u64;
    /// Waits until no other block decodes and keeps others from starting;
    /// `false` when the extraction is cancelled meanwhile.
    fn enter_alone(&self) -> bool;
    /// Ends [`DecodeGate::enter_alone`]'s exclusivity.
    fn leave_alone(&self);
}

/// The context of a decode that has no archive behind it: every request fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoContext;

impl DecodeContext for NoContext {
    fn block(&self) -> usize {
        0
    }

    fn record(&mut self, id: u64) -> Result<Record, FormatError> {
        Err(FormatError::RecordOutOfRange {
            record: id,
            count: 0,
        })
    }

    fn chunk(&mut self, _record: u64, index: u64) -> Result<Vec<u8>, FormatError> {
        Err(FormatError::ChunkIndexOutOfRange {
            chunk: index,
            len: 0,
        })
    }
}

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

    /// Decode one step with access to the archive (`ctx`): the records and the
    /// chunks a reconstruction primitive needs. The default ignores `ctx` and
    /// calls [`PrimitiveDecoder::decode_step`], so the 1.0 decoders are
    /// unchanged; a reconstruction decoder (revision 1.1's `jpeg-reconstruct`)
    /// overrides it.
    fn decode_in(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        last: bool,
        limits: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Vec<u8>, FormatError> {
        let _ = ctx;
        self.decode_step(params, input, expected_len, last, limits)
    }

    /// The most input this step accepts, asked by the reader before it runs
    /// the step before this one: `Some(n)` makes `n` the output bound of the
    /// previous step instead of the envelope's `max_block_plain`; `None` (the
    /// default) keeps `max_block_plain`. Never asked for a graph's first step.
    /// An error here is the block's error and the previous step does not run,
    /// so a reconstruction decoder can check its record here first.
    ///
    /// The reader trusts the bound: it may exceed `max_block_plain` (that is
    /// its purpose) and the reader does not clamp it. An implementation that
    /// returns `Some(n)` must already have checked `n` against the archive's
    /// envelope and the reader's limits (`limits`, including the decode
    /// memory), and refused with an error what they do not allow.
    ///
    /// `last` and `expected_len` describe this step as [`decode_in`] will see
    /// it, as far as they are known before the previous step runs: for the
    /// graph's last step `last` is true and `expected_len` is the block's
    /// `plain_len`; otherwise `expected_len` is the next step's input bound
    /// when the next step gives one (one level of look-ahead, asked first;
    /// revision 1.3's Bounds), else the envelope's `max_block_plain`
    /// (revision 1.2's check 3 of `deflate-reconstruct`).
    ///
    /// [`decode_in`]: PrimitiveDecoder::decode_in
    fn input_bound(
        &self,
        params: &[u8],
        expected_len: u64,
        last: bool,
        limits: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Option<u64>, FormatError> {
        let _ = (params, expected_len, last, limits, ctx);
        Ok(None)
    }

    /// Whether this decoder bounds its input: true for a decoder whose
    /// [`input_bound`] can return `Some` (`deflate-reconstruct`, `bwt`,
    /// revision 1.4's `png-filter`). The
    /// reader's look-ahead asks the step after a step for its input bound
    /// only when that step bounds its input (spec section 8, revision 1.3's
    /// Bounds); after a step that bounds nothing, no later step is asked and
    /// its output bound is `max_block_plain` (`plain_len` when it is last).
    /// The default is false; a decoder that overrides `input_bound` to return
    /// `Some` must override this too.
    ///
    /// [`input_bound`]: PrimitiveDecoder::input_bound
    fn bounds_input(&self) -> bool {
        false
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
/// decoders that need one look them up in. Cloning shares the decoders (a
/// parallel reader gives each worker its own registry over the same ones).
#[derive(Clone)]
pub struct Registry {
    decoders: Vec<Arc<dyn PrimitiveDecoder>>,
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
            .map(|&p| -> Arc<dyn PrimitiveDecoder> {
                match p {
                    PrimitiveId::Store => Arc::new(StoreDecoder),
                    PrimitiveId::Zstd => Arc::new(ZstdDecoder::new(Arc::clone(&priors))),
                    PrimitiveId::Lzma => Arc::new(LzmaDecoder),
                    other => Arc::new(Unimplemented(other)),
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
        self.decoders[PrimitiveId::Zstd as usize] = Arc::new(ZstdDecoder::new(Arc::clone(&store)));
        self.priors = store;
        self
    }

    /// The 1.0 registry over the same store of priors: `store`, `zstd` and
    /// `lzma`, every other primitive unimplemented. The reference reader
    /// decodes the blocks holding a record's chunks with it, so a
    /// reconstruction decoder never runs inside another (one level).
    pub fn nested(&self) -> Registry {
        let mut r = Registry::v1();
        r.decoders[PrimitiveId::Zstd as usize] =
            Arc::new(ZstdDecoder::new(Arc::clone(&self.priors)));
        r.priors = Arc::clone(&self.priors);
        r
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
        self.decoders[id as usize] = Arc::from(decoder);
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
/// implemented (`UnimplementedPrimitive`). Then the steps run in order: before
/// a step that is not the last runs, the next step's decoder is asked for its
/// [`PrimitiveDecoder::input_bound`] (an error there is the block's error); an
/// intermediate output larger than that bound (`limits.max_block_plain` when
/// the decoder gives none) is `PayloadTooLarge`; the last step must produce exactly `plain_len`
/// (`BlockLengthMismatch`, also when the decoder reports too much output).
///
/// The next step is asked with its own output bound, found by one level of
/// look-ahead when the next step bounds its input
/// ([`PrimitiveDecoder::bounds_input`]): when it is not the last step, the
/// step after it is asked for its input bound first (with `plain_len` when
/// that one is the last step, `max_block_plain` otherwise; the look-ahead
/// goes no further), and its error, when it has one, is the block's error
/// before anything else runs. After a next step that bounds nothing no later
/// step is asked, and that step's output bound is `max_block_plain`
/// (`plain_len` when it is last). So in `[M, bwt, deflate-reconstruct]` the
/// `bwt` step learns the inflated length before `M` runs (revision 1.3's
/// Bounds); in a graph whose later steps give no bound every step is asked
/// exactly as before.
pub fn decode_block(
    registry: &Registry,
    header: &BlockHeader,
    block: usize,
    encoded: &[u8],
    limits: &Resources,
) -> Result<Vec<u8>, FormatError> {
    decode_block_in(registry, header, block, encoded, limits, &mut NoContext)
}

/// [`decode_block`] with the archive behind it: every step runs through
/// [`PrimitiveDecoder::decode_in`] with `ctx`, so a reconstruction decoder
/// can read its record and the chunks the record names.
pub fn decode_block_in(
    registry: &Registry,
    header: &BlockHeader,
    block: usize,
    encoded: &[u8],
    limits: &Resources,
    ctx: &mut dyn DecodeContext,
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
            // The next step may bound its input, and so this step's output,
            // by something other than `max_block_plain`. Its own output
            // bound comes from one level of look-ahead: the step after it.
            // The look-ahead is asked only when the next step bounds its
            // input; after a step that bounds nothing no later step is
            // asked (spec section 8, "The look-ahead").
            let next = &steps[i + 1];
            let next_last = i + 1 == last;
            let next_decoder = registry.decoder(next.primitive);
            let next_expected = if next_last {
                header.plain_len
            } else if next_decoder.bounds_input() {
                output_bound(registry, header, steps, i + 1, limits, ctx)?
            } else {
                limits.max_block_plain
            };
            next_decoder
                .input_bound(&next.params, next_expected, next_last, limits, ctx)?
                .unwrap_or(limits.max_block_plain)
        };
        let out = match registry.decoder(step.primitive).decode_in(
            &step.params,
            &data,
            expected,
            i == last,
            limits,
            ctx,
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
        } else if len > expected {
            return Err(FormatError::PayloadTooLarge { len, max: expected });
        }
        data = std::borrow::Cow::Owned(out);
    }
    Ok(data.into_owned())
}

/// The output bound of step `i` of `steps` (not the last step, and a step
/// that bounds its input) as the
/// look-ahead sees it: the input bound of step `i + 1` when that decoder
/// gives one, otherwise `limits.max_block_plain`. Step `i + 1` is asked with
/// what is known of it without looking further: the block's `plain_len` when
/// it is the last step, otherwise `max_block_plain` (one level of look-ahead
/// only, spec section 8, revision 1.3's Bounds). An error here is the
/// block's error.
fn output_bound(
    registry: &Registry,
    header: &BlockHeader,
    steps: &[crate::graph::Step],
    i: usize,
    limits: &Resources,
    ctx: &mut dyn DecodeContext,
) -> Result<u64, FormatError> {
    let after = &steps[i + 1];
    let after_last = i + 2 == steps.len();
    let after_expected = if after_last {
        header.plain_len
    } else {
        limits.max_block_plain
    };
    Ok(registry
        .decoder(after.primitive)
        .input_bound(&after.params, after_expected, after_last, limits, ctx)?
        .unwrap_or(limits.max_block_plain))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Step};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    fn step(p: PrimitiveId) -> Step {
        Step {
            primitive: p,
            params: vec![],
        }
    }

    fn store() -> Step {
        step(PrimitiveId::Store)
    }

    fn bwt() -> Step {
        Step {
            primitive: PrimitiveId::Bwt,
            params: vec![0, 0, 1, 0],
        }
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

    /// Gives `bound` as its input bound (or the hook error when `bound` is
    /// `Err`), and returns the first `expected_len` bytes of its input.
    struct Bounded(Result<Option<u64>, ()>);
    impl PrimitiveDecoder for Bounded {
        fn decode(
            &self,
            _: &[u8],
            input: &[u8],
            expected_len: u64,
            _: &Resources,
        ) -> Result<Vec<u8>, FormatError> {
            let n = usize::try_from(expected_len).unwrap().min(input.len());
            Ok(input[..n].to_vec())
        }

        fn input_bound(
            &self,
            _: &[u8],
            _: u64,
            _: bool,
            _: &Resources,
            _: &mut dyn DecodeContext,
        ) -> Result<Option<u64>, FormatError> {
            self.0.map_err(|()| FormatError::BadRecord {
                record: 5,
                reason: "hook",
            })
        }

        fn bounds_input(&self) -> bool {
            true
        }
    }

    /// Returns `n` bytes and records that it ran.
    struct OverFlag(usize, Arc<AtomicBool>);
    impl PrimitiveDecoder for OverFlag {
        fn decode(
            &self,
            _: &[u8],
            _: &[u8],
            _: u64,
            _: &Resources,
        ) -> Result<Vec<u8>, FormatError> {
            self.1.store(true, Ordering::SeqCst);
            Ok(vec![7; self.0])
        }
    }

    fn bounded_run(bound: Result<Option<u64>, ()>) -> (Result<Vec<u8>, FormatError>, bool) {
        let small = Resources {
            max_block_plain: 4,
            ..Resources::default()
        };
        let ran = Arc::new(AtomicBool::new(false));
        let mut r = Registry::v1();
        r.register(PrimitiveId::BcjX86, Box::new(OverFlag(9, ran.clone())));
        r.register(PrimitiveId::BcjArm64, Box::new(Bounded(bound)));
        let h = header(
            vec![step(PrimitiveId::BcjX86), step(PrimitiveId::BcjArm64)],
            2,
            2,
        );
        let out = decode_block(&r, &h, 0, b"ab", &small);
        (out, ran.load(Ordering::SeqCst))
    }

    #[test]
    fn input_bound_replaces_max_block_plain() {
        // The first step's output (9 bytes) exceeds `max_block_plain` (4):
        // a bound that allows it decodes ...
        let (out, ran) = bounded_run(Ok(Some(9)));
        assert_eq!(out.unwrap(), vec![7, 7]);
        assert!(ran);
        let (out, _) = bounded_run(Ok(Some(100)));
        assert_eq!(out.unwrap(), vec![7, 7]);
        // ... one that does not, and no bound at all, are PayloadTooLarge.
        let (out, _) = bounded_run(Ok(Some(8)));
        assert!(matches!(
            out.unwrap_err(),
            FormatError::PayloadTooLarge { len: 9, max: 8 }
        ));
        let (out, _) = bounded_run(Ok(None));
        assert!(matches!(
            out.unwrap_err(),
            FormatError::PayloadTooLarge { len: 9, max: 4 }
        ));
    }

    #[test]
    fn input_bound_error_is_the_block_error() {
        let (out, ran) = bounded_run(Err(()));
        assert!(matches!(
            out.unwrap_err(),
            FormatError::BadRecord {
                record: 5,
                reason: "hook"
            }
        ));
        // The step before the hook never ran.
        assert!(!ran);
    }

    #[test]
    fn input_bound_of_a_first_step_is_never_asked() {
        // A bounded decoder as the only step: its hook error is not raised.
        let mut r = Registry::v1();
        r.register(PrimitiveId::BcjArm64, Box::new(Bounded(Err(()))));
        let h = header(vec![step(PrimitiveId::BcjArm64)], 2, 2);
        assert_eq!(
            decode_block(&r, &h, 0, b"ab", &Resources::default()).unwrap(),
            b"ab"
        );
    }

    /// A fake of a bounded step: answers `bound` to `input_bound` and records
    /// each call's (`expected_len`, `last`); decodes to the first
    /// `expected_len` bytes of its input, noting that it ran.
    struct Probe {
        bound: Result<Option<u64>, &'static str>,
        asked: Arc<Mutex<Vec<(u64, bool)>>>,
        ran: Arc<AtomicBool>,
    }

    impl Probe {
        fn new(bound: Result<Option<u64>, &'static str>) -> Probe {
            Probe {
                bound,
                asked: Arc::default(),
                ran: Arc::default(),
            }
        }
    }

    impl PrimitiveDecoder for Probe {
        fn decode(
            &self,
            _: &[u8],
            input: &[u8],
            expected_len: u64,
            _: &Resources,
        ) -> Result<Vec<u8>, FormatError> {
            self.ran.store(true, Ordering::SeqCst);
            let n = usize::try_from(expected_len).unwrap().min(input.len());
            Ok(input[..n].to_vec())
        }

        fn input_bound(
            &self,
            _: &[u8],
            expected_len: u64,
            last: bool,
            _: &Resources,
            _: &mut dyn DecodeContext,
        ) -> Result<Option<u64>, FormatError> {
            self.asked.lock().unwrap().push((expected_len, last));
            self.bound
                .map_err(|reason| FormatError::BadRecord { record: 9, reason })
        }

        fn bounds_input(&self) -> bool {
            true
        }
    }

    /// `[BcjX86 = first, BcjArm64 = middle, Bwt = last]` over `max_block_plain`
    /// 4 and a 2-byte block; `first` returns 9 bytes.
    fn chain(
        middle: Result<Option<u64>, &'static str>,
        last: Result<Option<u64>, &'static str>,
    ) -> (Result<Vec<u8>, FormatError>, Probe, Probe, Probe) {
        let small = Resources {
            max_block_plain: 4,
            ..Resources::default()
        };
        let first = Probe::new(Ok(None));
        let middle = Probe::new(middle);
        let last = Probe::new(last);
        let mut r = Registry::v1();
        r.register(
            PrimitiveId::BcjX86,
            Box::new(OverFlag(9, first.ran.clone())),
        );
        r.register(
            PrimitiveId::BcjArm64,
            Box::new(Probe {
                bound: middle.bound,
                asked: middle.asked.clone(),
                ran: middle.ran.clone(),
            }),
        );
        r.register(
            PrimitiveId::Bwt,
            Box::new(Probe {
                bound: last.bound,
                asked: last.asked.clone(),
                ran: last.ran.clone(),
            }),
        );
        let h = header(
            vec![
                step(PrimitiveId::BcjX86),
                step(PrimitiveId::BcjArm64),
                bwt(),
            ],
            2,
            2,
        );
        let out = decode_block(&r, &h, 0, b"ab", &small);
        (out, first, middle, last)
    }

    fn asked(p: &Probe) -> Vec<(u64, bool)> {
        p.asked.lock().unwrap().clone()
    }

    #[test]
    fn look_ahead_passes_the_last_steps_bound_to_the_middle_step() {
        // The last step bounds its input by 20: the middle step is asked with
        // 20 as its output bound (not `max_block_plain`), and may let the
        // first step's 9 bytes through.
        let (out, first, middle, last) = chain(Ok(Some(9)), Ok(Some(20)));
        assert_eq!(out.unwrap(), vec![7, 7]);
        assert!(first.ran.load(Ordering::SeqCst));
        assert!(middle.ran.load(Ordering::SeqCst));
        assert!(last.ran.load(Ordering::SeqCst));
        // Before the first step: the last step (look-ahead, as the last step
        // with `plain_len` 2), then the middle step with the bound 20. Before
        // the middle step: the last step again, the same way.
        assert_eq!(asked(&last), vec![(2, true), (2, true)]);
        assert_eq!(asked(&middle), vec![(20, false)]);
    }

    #[test]
    fn look_ahead_without_a_bound_asks_as_before() {
        // The last step gives no bound: the middle step is asked with
        // `max_block_plain`, exactly as revision 1.2 asked it.
        let (out, _, middle, last) = chain(Ok(Some(9)), Ok(None));
        assert_eq!(out.unwrap(), vec![7, 7]);
        assert_eq!(asked(&middle), vec![(4, false)]);
        assert_eq!(asked(&last), vec![(2, true), (2, true)]);
        // And a middle step without a bound keeps `max_block_plain` for the
        // first step's output, so 9 bytes are too many.
        let (out, _, middle, _) = chain(Ok(None), Ok(Some(20)));
        assert!(matches!(
            out.unwrap_err(),
            FormatError::PayloadTooLarge { len: 9, max: 4 }
        ));
        assert_eq!(asked(&middle), vec![(20, false)]);
    }

    #[test]
    fn the_later_steps_checks_come_first_and_its_error_is_the_blocks() {
        // The last step's hook fails: nothing ran, the middle step was never
        // asked, and the error is reported unchanged.
        let (out, first, middle, last) = chain(Ok(Some(9)), Err("last hook"));
        assert!(matches!(
            out.unwrap_err(),
            FormatError::BadRecord {
                record: 9,
                reason: "last hook"
            }
        ));
        assert!(!first.ran.load(Ordering::SeqCst));
        assert!(!middle.ran.load(Ordering::SeqCst));
        assert!(!last.ran.load(Ordering::SeqCst));
        assert_eq!(asked(&last), vec![(2, true)]);
        assert!(asked(&middle).is_empty());
        // The middle step's hook fails after the last step's passed: the
        // middle error is the block's, and the first step never ran.
        let (out, first, _, last) = chain(Err("middle hook"), Ok(Some(20)));
        assert!(matches!(
            out.unwrap_err(),
            FormatError::BadRecord {
                record: 9,
                reason: "middle hook"
            }
        ));
        assert!(!first.ran.load(Ordering::SeqCst));
        assert_eq!(asked(&last), vec![(2, true)]);
    }

    #[test]
    fn look_ahead_is_not_asked_after_a_step_that_bounds_nothing() {
        // R36: `[A (9 bytes), store, probe whose hook errors]` over
        // `max_block_plain` 4. `store` bounds nothing, so the probe after it
        // is never asked before `A` runs: `A` runs, and its 9 bytes over the
        // `store` step's bound (`max_block_plain`) are the block's outcome,
        // not the probe's hook error.
        let small = Resources {
            max_block_plain: 4,
            ..Resources::default()
        };
        let ran = Arc::new(AtomicBool::new(false));
        let third = Probe::new(Err("third hook"));
        let mut r = Registry::v1();
        r.register(PrimitiveId::BcjX86, Box::new(OverFlag(9, ran.clone())));
        r.register(
            PrimitiveId::Bwt,
            Box::new(Probe {
                bound: third.bound,
                asked: third.asked.clone(),
                ran: third.ran.clone(),
            }),
        );
        let h = header(vec![step(PrimitiveId::BcjX86), store(), bwt()], 2, 2);
        assert!(matches!(
            decode_block(&r, &h, 0, b"ab", &small).unwrap_err(),
            FormatError::PayloadTooLarge { len: 9, max: 4 }
        ));
        assert!(ran.load(Ordering::SeqCst));
        assert!(asked(&third).is_empty());
        assert!(!third.ran.load(Ordering::SeqCst));

        // The same graph with the first step's output within the bound: the
        // probe is asked only before `store` runs (as the last step), and its
        // error is then the block's.
        let ran = Arc::new(AtomicBool::new(false));
        let third = Probe::new(Err("third hook"));
        let mut r = Registry::v1();
        r.register(PrimitiveId::BcjX86, Box::new(OverFlag(3, ran.clone())));
        r.register(
            PrimitiveId::Bwt,
            Box::new(Probe {
                bound: third.bound,
                asked: third.asked.clone(),
                ran: third.ran.clone(),
            }),
        );
        assert!(matches!(
            decode_block(&r, &h, 0, b"ab", &small).unwrap_err(),
            FormatError::BadRecord {
                record: 9,
                reason: "third hook"
            }
        ));
        assert!(ran.load(Ordering::SeqCst));
        assert_eq!(asked(&third), vec![(2, true)]);
    }

    /// Task A of revision 1.4 (`docs/notes/e2-16-design.md`, section 3): in
    /// `[M, png-filter, deflate-reconstruct]` the reader asks the
    /// `deflate-reconstruct` step for its input bound (as the last step, with
    /// the block's `plain_len`) before `M` runs, and passes that bound to the
    /// `png-filter` step's `input_bound` as its output bound; the
    /// `deflate-reconstruct` step's checks come first and its error, or the
    /// `png-filter` step's, is the block's, with `M` not run.
    #[test]
    fn png_filter_is_asked_with_the_deflate_records_bound_before_m_runs() {
        let small = Resources {
            max_block_plain: 4,
            ..Resources::default()
        };
        let record = |id: u8| vec![id];
        let run = |png: Result<Option<u64>, &'static str>,
                   deflate: Result<Option<u64>, &'static str>| {
            let m = Arc::new(AtomicBool::new(false));
            let png = Probe::new(png);
            let deflate = Probe::new(deflate);
            let mut r = Registry::v1();
            r.register(PrimitiveId::BcjX86, Box::new(OverFlag(9, m.clone())));
            r.register(
                PrimitiveId::PngFilter,
                Box::new(Probe {
                    bound: png.bound,
                    asked: png.asked.clone(),
                    ran: png.ran.clone(),
                }),
            );
            r.register(
                PrimitiveId::DeflateReconstruct,
                Box::new(Probe {
                    bound: deflate.bound,
                    asked: deflate.asked.clone(),
                    ran: deflate.ran.clone(),
                }),
            );
            let h = header(
                vec![
                    step(PrimitiveId::BcjX86),
                    Step {
                        primitive: PrimitiveId::PngFilter,
                        params: record(0),
                    },
                    Step {
                        primitive: PrimitiveId::DeflateReconstruct,
                        params: record(1),
                    },
                ],
                2,
                2,
            );
            let out = decode_block(&r, &h, 0, b"ab", &small);
            (out, m.load(Ordering::SeqCst), png, deflate)
        };
        // The deflate record's `plain_len` (20) is the png-filter step's
        // output bound; its input bound (9) lets M's 9 bytes through,
        // although they pass `max_block_plain` (4).
        let (out, m, png, deflate) = run(Ok(Some(9)), Ok(Some(20)));
        assert_eq!(out.unwrap(), vec![7, 7]);
        assert!(m);
        assert_eq!(asked(&png), vec![(20, false)]);
        assert_eq!(asked(&deflate), vec![(2, true), (2, true)]);
        // A png-filter bound below M's output: PayloadTooLarge before the
        // png-filter step runs.
        let (out, _, png, _) = run(Ok(Some(8)), Ok(Some(20)));
        assert!(matches!(
            out.unwrap_err(),
            FormatError::PayloadTooLarge { len: 9, max: 8 }
        ));
        assert!(!png.ran.load(Ordering::SeqCst));
        // The deflate step's checks fail: its error, nothing run, png-filter
        // never asked.
        let (out, m, png, _) = run(Ok(Some(9)), Err("deflate"));
        assert!(matches!(
            out.unwrap_err(),
            FormatError::BadRecord {
                reason: "deflate",
                ..
            }
        ));
        assert!(!m);
        assert!(asked(&png).is_empty());
        // The png-filter step's checks fail (with the deflate bound): its
        // error, M not run.
        let (out, m, png, _) = run(Err("png"), Ok(Some(20)));
        assert!(matches!(
            out.unwrap_err(),
            FormatError::BadRecord { reason: "png", .. }
        ));
        assert!(!m);
        assert_eq!(asked(&png), vec![(20, false)]);
    }

    #[test]
    fn look_ahead_is_one_level_only() {
        // Four steps: before the first runs, the third is asked with
        // `max_block_plain` (not the fourth's bound), then the second with
        // the third's answer.
        let small = Resources {
            max_block_plain: 4,
            ..Resources::default()
        };
        let second = Probe::new(Ok(Some(9)));
        let third = Probe::new(Ok(Some(30)));
        let fourth = Probe::new(Ok(Some(40)));
        let mut r = Registry::v1();
        r.register(PrimitiveId::BcjX86, Box::new(Over(9)));
        for (id, p) in [
            (PrimitiveId::BcjArm64, &second),
            (PrimitiveId::Bwt, &third),
            (PrimitiveId::Base64, &fourth),
        ] {
            r.register(
                id,
                Box::new(Probe {
                    bound: p.bound,
                    asked: p.asked.clone(),
                    ran: p.ran.clone(),
                }),
            );
        }
        let h = header(
            vec![
                step(PrimitiveId::BcjX86),
                step(PrimitiveId::BcjArm64),
                bwt(),
                Step {
                    primitive: PrimitiveId::Base64,
                    params: vec![0],
                },
            ],
            2,
            2,
        );
        assert_eq!(decode_block(&r, &h, 0, b"ab", &small).unwrap(), vec![0, 0]);
        // Before step 1: third (look-ahead, 4), second (30). Before step 2:
        // fourth (look-ahead, 2 as last), third (40). Before step 3: fourth (2).
        assert_eq!(asked(&second), vec![(30, false)]);
        assert_eq!(asked(&third), vec![(4, false), (40, false)]);
        assert_eq!(asked(&fourth), vec![(2, true), (2, true)]);
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
