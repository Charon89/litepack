//! The decode graph and the `ChunkData` block header that carries it (spec
//! section 8).

use crate::error::FormatError;
use crate::primitive::{GraphResources, PrimitiveId};
use crate::varint;

/// Most steps a graph may have.
pub const MAX_STEPS: usize = 16;
/// Longest `params` of one step, in bytes.
pub const MAX_PARAMS: usize = 256;

const GRAPH: &str = "graph";
const HEADER: &str = "block header";

/// One primitive application with its parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The primitive.
    pub primitive: PrimitiveId,
    /// The parameter bytes, laid out as the primitive defines.
    pub params: Vec<u8>,
}

/// A decode graph: steps applied in order to turn encoded bytes into plain bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graph {
    /// The steps, in decoding order.
    pub steps: Vec<Step>,
}

/// Read a varint at `*pos`, advancing it; a short input is `Truncated { what }`.
fn rv(buf: &[u8], pos: &mut usize, what: &'static str) -> Result<u64, FormatError> {
    let mut r = buf.get(*pos..).unwrap_or(&[]);
    let before = r.len();
    let v = match varint::read(&mut r) {
        Err(FormatError::Truncated { .. }) => return Err(FormatError::Truncated { what }),
        other => other?,
    };
    *pos += before - r.len();
    Ok(v)
}

fn take<'a>(
    buf: &'a [u8],
    pos: &mut usize,
    n: usize,
    what: &'static str,
) -> Result<&'a [u8], FormatError> {
    let end = pos
        .checked_add(n)
        .filter(|&e| e <= buf.len())
        .ok_or(FormatError::Truncated { what })?;
    let s = &buf[*pos..end];
    *pos = end;
    Ok(s)
}

impl Graph {
    /// Parse a graph from the start of `buf`; returns it and the bytes used.
    /// An unknown primitive ID is reported as soon as it is read, before any
    /// later byte is looked at.
    pub fn parse(buf: &[u8]) -> Result<(Graph, usize), FormatError> {
        let mut pos = 0;
        let count = rv(buf, &mut pos, GRAPH)?;
        if count == 0 || count > MAX_STEPS as u64 {
            return Err(FormatError::BadGraph {
                reason: "step count",
            });
        }
        let mut steps = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let id = take(buf, &mut pos, 2, GRAPH)?;
            let id = u16::from_le_bytes([id[0], id[1]]);
            let primitive =
                PrimitiveId::from_u16(id).ok_or(FormatError::UnknownPrimitive { id })?;
            if take(buf, &mut pos, 1, GRAPH)?[0] != 0 {
                return Err(FormatError::BadGraph {
                    reason: "step flags",
                });
            }
            let len = rv(buf, &mut pos, GRAPH)?;
            if len > MAX_PARAMS as u64 {
                return Err(FormatError::BadGraph {
                    reason: "params length",
                });
            }
            let params = take(buf, &mut pos, len as usize, GRAPH)?;
            primitive.validate_params(params)?;
            steps.push(Step {
                primitive,
                params: params.to_vec(),
            });
        }
        Ok((Graph { steps }, pos))
    }

    /// The graph's bytes. The caller keeps the graph valid.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        // Writing to a Vec cannot fail.
        let _ = varint::write(&mut out, self.steps.len() as u64);
        for s in &self.steps {
            out.extend_from_slice(&(s.primitive as u16).to_le_bytes());
            out.push(0);
            let _ = varint::write(&mut out, s.params.len() as u64);
            out.extend_from_slice(&s.params);
        }
        out
    }

    /// True when a step names a record (a reconstruction primitive).
    pub fn uses_records(&self) -> bool {
        self.steps.iter().any(|s| s.primitive.is_reconstruction())
    }

    /// Check that every record id the steps name is below `record_count`.
    pub fn check_records(&self, record_count: u64) -> Result<(), FormatError> {
        for s in &self.steps {
            if let Some(record) = s.primitive.record_id(&s.params) {
                if record >= record_count {
                    return Err(FormatError::RecordOutOfRange {
                        record,
                        count: record_count,
                    });
                }
            }
        }
        Ok(())
    }

    /// The IDs of the priors the steps name: ascending and unique.
    pub fn prior_ids(&self) -> Vec<[u8; 32]> {
        let ids: std::collections::BTreeSet<[u8; 32]> = self
            .steps
            .iter()
            .filter_map(|s| s.primitive.prior(&s.params))
            .collect();
        ids.into_iter().collect()
    }

    /// The maxima of the steps' resources.
    pub fn resources(&self) -> GraphResources {
        self.steps
            .iter()
            .map(|s| s.primitive.resources(&s.params))
            .fold(GraphResources::default(), |a, b| GraphResources {
                window: a.window.max(b.window),
                bwt_block: a.bwt_block.max(b.bwt_block),
            })
    }
}

/// The header at the start of a `ChunkData` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockHeader {
    /// How to decode the block.
    pub graph: Graph,
    /// Length of the plain bytes; must equal the block table's `plain_len`.
    pub plain_len: u64,
    /// Number of encoded bytes that follow the header.
    pub encoded_len: u64,
}

impl BlockHeader {
    /// Parse the header at the start of a `ChunkData` payload; returns it and
    /// its length, so the encoded bytes are `payload[used..]`. `encoded_len`
    /// must equal the payload length minus the header; a difference is
    /// `BlockLengthMismatch`; `block` is the block's index in the block table.
    /// `record_count` is the number of records in the archive's `Records`
    /// frame (0 without one); a reconstruction step naming a record id at or
    /// above it is `RecordOutOfRange`, raised right after the graph is parsed.
    pub fn parse(
        payload: &[u8],
        block: usize,
        record_count: u64,
    ) -> Result<(BlockHeader, usize), FormatError> {
        let (graph, pos) = Graph::parse(payload)?;
        graph.check_records(record_count)?;
        Self::from_graph(graph, pos, payload, block)
    }

    /// The rest of the header after its graph (`pos` bytes of `payload`).
    pub(crate) fn from_graph(
        graph: Graph,
        mut pos: usize,
        payload: &[u8],
        block: usize,
    ) -> Result<(BlockHeader, usize), FormatError> {
        let plain_len = rv(payload, &mut pos, HEADER)?;
        let encoded_len = rv(payload, &mut pos, HEADER)?;
        if encoded_len != (payload.len() - pos) as u64 {
            return Err(FormatError::BlockLengthMismatch { block });
        }
        Ok((
            BlockHeader {
                graph,
                plain_len,
                encoded_len,
            },
            pos,
        ))
    }

    /// The header's bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.graph.encode();
        let _ = varint::write(&mut out, self.plain_len);
        let _ = varint::write(&mut out, self.encoded_len);
        out
    }
}

/// The Markdown table of the decode graph bytes, pasted verbatim into the spec.
pub fn graph_layout_table() -> String {
    "| Field | Size | Meaning |\n|---|---|---|\n\
     | step_count | varint | number of steps, 1..=16 |\n\
     | primitive | u16 LE | per step: the primitive ID (section 8 registry) |\n\
     | flags | u8 | per step: bit 0 `MUST_UNDERSTAND` is reserved and must be 0 in v1; all other bits must be 0 |\n\
     | params_len | varint | per step: length of `params`, at most 256 |\n\
     | params | params_len bytes | per step: the parameters, laid out as the primitive defines |\n"
        .to_string()
}

/// The Markdown table of the `ChunkData` block header, pasted verbatim into the spec.
pub fn block_header_table() -> String {
    "| Field | Size | Meaning |\n|---|---|---|\n\
     | graph | variable | the decode graph (step_count and the steps) |\n\
     | plain_len | varint | length of the block's plain bytes; must equal the block table's `plain_len` |\n\
     | encoded_len | varint | number of bytes that follow; must equal the payload length minus the header |\n\
     | encoded | encoded_len bytes | the encoded bytes |\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn st(p: PrimitiveId, params: &[u8]) -> Step {
        Step {
            primitive: p,
            params: params.to_vec(),
        }
    }

    fn store() -> Step {
        st(PrimitiveId::Store, &[])
    }

    fn rt(g: &Graph) {
        let bytes = g.encode();
        let (back, used) = Graph::parse(&bytes).unwrap();
        assert_eq!(&back, g);
        assert_eq!(used, bytes.len());
    }

    #[test]
    fn round_trip_sizes() {
        for n in [1, 2, 16] {
            rt(&Graph {
                steps: vec![store(); n],
            });
        }
        rt(&Graph {
            steps: vec![
                st(PrimitiveId::Lzma, &[0, 0, 0, 1, 3, 0, 2]),
                st(PrimitiveId::BcjX86, &[]),
            ],
        });
    }

    #[test]
    fn wire_bytes() {
        let g = Graph {
            steps: vec![store()],
        };
        assert_eq!(g.encode(), [1, 0, 0, 0, 0]);
        let (_, used) = Graph::parse(&[1, 0, 0, 0, 0, 99, 99]).unwrap();
        assert_eq!(used, 5);
    }

    fn reason(e: FormatError) -> &'static str {
        match e {
            FormatError::BadGraph { reason } => reason,
            e => panic!("unexpected {e:?}"),
        }
    }

    #[test]
    fn step_counts() {
        assert_eq!(reason(Graph::parse(&[0]).unwrap_err()), "step count");
        let g = Graph {
            steps: vec![store(); 17],
        };
        assert_eq!(reason(Graph::parse(&g.encode()).unwrap_err()), "step count");
        assert!(matches!(
            Graph::parse(&[]).unwrap_err(),
            FormatError::Truncated { what: "graph" }
        ));
    }

    #[test]
    fn flags_and_params_length() {
        assert_eq!(
            reason(Graph::parse(&[1, 0, 0, 1, 0]).unwrap_err()),
            "step flags"
        );
        assert_eq!(
            reason(Graph::parse(&[1, 0, 0, 0x80, 0x00]).unwrap_err()),
            "step flags"
        );
        // params_len 257 = varint 0x81 0x02
        assert_eq!(
            reason(Graph::parse(&[1, 0, 0, 0, 0x81, 0x02]).unwrap_err()),
            "params length"
        );
        assert!(matches!(
            Graph::parse(&[1, 0, 0, 0, 1]).unwrap_err(),
            FormatError::Truncated { what: "graph" }
        ));
    }

    #[test]
    fn bad_params_are_reported() {
        let e = Graph::parse(&[1, 0, 0, 0, 1, 7]).unwrap_err();
        assert!(matches!(e, FormatError::BadParams { id: 0, .. }));
    }

    #[test]
    fn unknown_id_before_later_bytes() {
        // 0x000D and 0x8000, with nothing after the ID.
        for id in [0x000Du16, 0x8000, 0xFFFF] {
            let mut b = vec![1];
            b.extend_from_slice(&id.to_le_bytes());
            assert!(matches!(
                Graph::parse(&b).unwrap_err(),
                FormatError::UnknownPrimitive { id: got } if got == id
            ));
        }
        // An unknown second step after a valid first one, then truncation.
        let b = [2, 0, 0, 0, 0, 0x0D, 0x00];
        assert!(matches!(
            Graph::parse(&b).unwrap_err(),
            FormatError::UnknownPrimitive { id: 0x0D }
        ));
    }

    #[test]
    fn graph_resources_are_maxima() {
        let mut z = vec![20u8];
        z.extend_from_slice(&[0; 32]);
        let g = Graph {
            steps: vec![
                st(PrimitiveId::Zstd, &z),
                st(PrimitiveId::Lzma, &[0, 0, 0, 1, 3, 0, 2]),
                st(PrimitiveId::Bwt, &[0, 0, 1, 0]),
                st(PrimitiveId::Bwt, &[0, 0, 4, 0]),
            ],
        };
        assert_eq!(
            g.resources(),
            GraphResources {
                window: 0x0100_0000,
                bwt_block: 0x4_0000
            }
        );
    }

    #[test]
    fn block_header_round_trip_and_length_check() {
        let h = BlockHeader {
            graph: Graph {
                steps: vec![store()],
            },
            plain_len: 3,
            encoded_len: 3,
        };
        let mut payload = h.encode();
        let hl = payload.len();
        payload.extend_from_slice(b"abc");
        let (back, used) = BlockHeader::parse(&payload, 5, 0).unwrap();
        assert_eq!(back, h);
        assert_eq!(used, hl);
        assert_eq!(&payload[used..], b"abc");
        payload.push(0);
        assert!(matches!(
            BlockHeader::parse(&payload, 5, 0).unwrap_err(),
            FormatError::BlockLengthMismatch { block: 5 }
        ));
        payload.truncate(payload.len() - 2);
        assert!(matches!(
            BlockHeader::parse(&payload, 5, 0).unwrap_err(),
            FormatError::BlockLengthMismatch { block: 5 }
        ));
        assert!(matches!(
            BlockHeader::parse(&h.graph.encode(), 5, 0).unwrap_err(),
            FormatError::Truncated {
                what: "block header"
            }
        ));
    }

    #[test]
    fn record_ids_are_checked_against_the_count() {
        for prim in [
            PrimitiveId::JpegReconstruct,
            PrimitiveId::DeflateReconstruct,
            PrimitiveId::PngFilter,
            PrimitiveId::Base64,
            PrimitiveId::Utf16,
            PrimitiveId::ContainerReconstruct,
        ] {
            let h = BlockHeader {
                graph: Graph {
                    steps: vec![st(prim, &[2])],
                },
                plain_len: 0,
                encoded_len: 0,
            };
            assert!(h.graph.uses_records());
            let payload = h.encode();
            // With no records frame the count is 0: every id is out of range.
            for count in [0, 1, 2] {
                assert!(matches!(
                    BlockHeader::parse(&payload, 0, count).unwrap_err(),
                    FormatError::RecordOutOfRange { record: 2, count: c } if c == count
                ));
            }
            assert_eq!(BlockHeader::parse(&payload, 0, 3).unwrap().0, h);
        }
        assert!(!Graph {
            steps: vec![store()]
        }
        .uses_records());
        // A store block never looks at the count.
        let h = BlockHeader {
            graph: Graph {
                steps: vec![store()],
            },
            plain_len: 0,
            encoded_len: 0,
        };
        BlockHeader::parse(&h.encode(), 0, 0).unwrap();
    }

    fn arb_step() -> impl Strategy<Value = Step> {
        prop_oneof![
            Just(store()),
            (10u8..=31, any::<[u8; 32]>()).prop_map(|(w, d)| {
                let mut p = vec![w];
                p.extend_from_slice(&d);
                st(PrimitiveId::Zstd, &p)
            }),
            (any::<u32>(), 0u8..=8, 0u8..=4, 0u8..=4).prop_map(|(d, lc, lp, pb)| {
                let lp = lp.min(4 - lc.min(4));
                let lc = lc.min(4);
                let mut p = d.to_le_bytes().to_vec();
                p.extend_from_slice(&[lc, lp, pb]);
                st(PrimitiveId::Lzma, &p)
            }),
            (1u32..).prop_map(|b| st(PrimitiveId::Bwt, &b.to_le_bytes())),
            Just(st(PrimitiveId::BcjX86, &[])),
            Just(st(PrimitiveId::BcjArm64, &[])),
            (any::<u64>(), 0u8..=1).prop_map(|(c, f)| {
                let mut p = c.to_le_bytes().to_vec();
                p.push(f);
                st(PrimitiveId::Delta, &p)
            }),
            (7u16..=12, any::<u64>()).prop_map(|(p, id)| {
                let mut v = Vec::new();
                varint::write(&mut v, id).unwrap();
                st(PrimitiveId::from_u16(p).unwrap(), &v)
            }),
        ]
    }

    proptest! {
        #[test]
        fn graph_round_trip(steps in prop::collection::vec(arb_step(), 1..=16)) {
            let g = Graph { steps };
            let bytes = g.encode();
            let (back, used) = Graph::parse(&bytes).unwrap();
            prop_assert_eq!(back, g);
            prop_assert_eq!(used, bytes.len());
        }

        #[test]
        fn parse_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
            let _ = Graph::parse(&bytes);
            let _ = BlockHeader::parse(&bytes, 0, 3);
        }
    }
}
