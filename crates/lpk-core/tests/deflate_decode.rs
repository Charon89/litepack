//! Revision 1.2's `deflate-reconstruct` end to end: a real Deflate stream (the walker's gzip
//! fixture) written as one block `[zstd or lzma, deflate-reconstruct]` whose inflated text is
//! larger than the archive's `max_block_plain`, decoded by the full reader through the input
//! bound (design section 2.2(b)); without the bound the same archive is `PayloadTooLarge`; an
//! archive that declares too little decode memory is `Refused`.
#![allow(clippy::unwrap_used)]

use std::io::Cursor;

use lpk_core::peel::deflate::{examine, memory_term, Limits};
use lpk_core::{register_full_reader, DeflateDecoder};
use lpk_format::{
    Archive, DecodeContext, DeflateRecord, Encoded, EntryFlags, EntryKind, FormatError, Graph,
    PrimitiveDecoder, PrimitiveId, Record, RecordBody, Resources, Step, Writer, WriterOptions,
};

/// The gzip fixture's raw Deflate stream and its record body.
fn stream() -> (Vec<u8>, Vec<u8>, DeflateRecord) {
    let bytes = include_bytes!("fixtures/deflate/gzip.gz");
    let mut done = Vec::new();
    examine(bytes, "gz", &Limits::standard(), |r| done.push(r));
    let r = done.remove(0);
    let deflate: Vec<u8> = r
        .spans
        .iter()
        .flat_map(|&(a, b)| bytes[a..b].iter().copied())
        .collect();
    let plain = r.plain.text().to_vec();
    let d = DeflateRecord {
        original_len: deflate.len() as u64,
        plain_len: plain.len() as u64,
        corrections: r.corrections,
        library: 0,
        original_hash: *blake3::hash(&deflate).as_bytes(),
    };
    (deflate, plain, d)
}

#[derive(Clone, Copy, Debug)]
enum Model {
    Zstd,
    Lzma,
}

/// The model step and its bytes for `plain`.
fn model_step(model: Model, plain: &[u8]) -> (Step, Vec<u8>) {
    match model {
        Model::Zstd => {
            let mut params = vec![17u8];
            params.extend_from_slice(&[0; 32]);
            let mut c = zstd::bulk::Compressor::new(3).unwrap();
            c.set_parameter(zstd::zstd_safe::CParameter::WindowLog(17))
                .unwrap();
            (
                Step {
                    primitive: PrimitiveId::Zstd,
                    params,
                },
                c.compress(plain).unwrap(),
            )
        }
        Model::Lzma => {
            use liblzma::stream::{Action, Filters, LzmaOptions, Status, Stream};
            let dict: u32 = 1 << 16;
            let mut o = LzmaOptions::new_preset(6).unwrap();
            o.dict_size(dict)
                .literal_context_bits(3)
                .literal_position_bits(0)
                .position_bits(2);
            let mut filters = Filters::new();
            filters.lzma1(&o);
            let mut s = Stream::new_raw_encoder(&filters).unwrap();
            let mut out = Vec::with_capacity(plain.len() + 1024);
            loop {
                let at = usize::try_from(s.total_in()).unwrap();
                out.reserve(1 << 16);
                if s.process_vec(&plain[at..], &mut out, Action::Finish)
                    .unwrap()
                    == Status::StreamEnd
                {
                    break;
                }
            }
            let mut params = dict.to_le_bytes().to_vec();
            params.extend_from_slice(&[3, 0, 2]);
            (
                Step {
                    primitive: PrimitiveId::Lzma,
                    params,
                },
                out,
            )
        }
    }
}

/// An archive (minor 2) of `stream.deflate` (the stream, one block through `model` and
/// `deflate-reconstruct`, declaring `memory`) and a small plain `readme.txt`.
fn archive(model: Model, memory: Option<u64>) -> (Vec<u8>, Vec<u8>, DeflateRecord) {
    let (deflate, plain, d) = stream();
    let (step, bytes) = model_step(model, &plain);
    let mut out = Vec::new();
    let options = WriterOptions {
        chunk_size: 4096,
        block_size: 16 * 1024,
        ..WriterOptions::default()
    };
    let mut w = Writer::new_revision(&mut out, options, 2).unwrap();
    w.begin_entry("stream.deflate", EntryFlags::EMPTY, 0)
        .unwrap();
    let id = w
        .add_record(Record::new(RecordBody::Deflate(d.clone())))
        .unwrap();
    let mut params = Vec::new();
    lpk_format::varint::write(&mut params, id).unwrap();
    let graph = Graph {
        steps: vec![
            step,
            Step {
                primitive: PrimitiveId::DeflateReconstruct,
                params,
            },
        ],
    };
    let encoded = Encoded {
        resources: graph.resources(),
        graph,
        bytes,
    };
    let memory = memory.unwrap_or(memory_term(d.plain_len));
    w.add_part_encoded(0, &deflate, encoded, memory).unwrap();
    w.end_entry().unwrap();
    w.add_file(
        "readme.txt",
        EntryFlags::EMPTY,
        0,
        &mut &b"not a container\n"[..],
    )
    .unwrap();
    w.finish().unwrap();
    (out, deflate, d)
}

/// Every file of `a`, then a verification.
fn extract_all<R: std::io::Read + std::io::Seek>(
    a: &mut Archive<R>,
) -> Result<Vec<(String, Vec<u8>)>, FormatError> {
    let t = a.entry_table()?;
    let entries: Vec<_> = t.table()?.iter().collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut v = Vec::new();
        a.extract(e, &mut v)?;
        out.push((e.path.clone(), v));
    }
    a.verify()?;
    Ok(out)
}

#[test]
fn both_model_steps_round_trip_through_the_input_bound() {
    for model in [Model::Zstd, Model::Lzma] {
        let (bytes, deflate, d) = archive(model, None);
        let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        assert_eq!(a.header().version.minor, 2);
        let env = a.index().envelope;
        // The inflated text is larger than every block: only the bound lets it through.
        assert!(d.plain_len > env.max_block_plain, "{model:?}");
        assert_eq!(
            env.decode_memory,
            env.max_block_plain + memory_term(d.plain_len)
        );
        register_full_reader(&mut a);
        let files = extract_all(&mut a).unwrap();
        assert_eq!(
            files,
            vec![
                ("readme.txt".to_string(), b"not a container\n".to_vec()),
                ("stream.deflate".to_string(), deflate.clone()),
            ],
            "{model:?}"
        );
    }
}

/// The decoder with the default input bound (none): `max_block_plain` bounds the model step.
struct WithoutBound;
impl PrimitiveDecoder for WithoutBound {
    fn decode(&self, p: &[u8], i: &[u8], e: u64, l: &Resources) -> Result<Vec<u8>, FormatError> {
        DeflateDecoder.decode(p, i, e, l)
    }
    fn decode_in(
        &self,
        p: &[u8],
        i: &[u8],
        e: u64,
        last: bool,
        l: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Vec<u8>, FormatError> {
        DeflateDecoder.decode_in(p, i, e, last, l, ctx)
    }
}

#[test]
fn without_the_bound_the_model_output_is_payload_too_large() {
    for model in [Model::Zstd, Model::Lzma] {
        let (bytes, _, d) = archive(model, None);
        let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        let max = a.index().envelope.max_block_plain;
        register_full_reader(&mut a);
        a.registry_mut()
            .register(PrimitiveId::DeflateReconstruct, Box::new(WithoutBound));
        let e = a.verify().unwrap_err();
        match model {
            // zstd knows the frame's content size and refuses it before decoding.
            Model::Zstd => assert!(
                matches!(e, FormatError::PayloadTooLarge { len, max: m } if m == max && len >= d.plain_len.min(max + 1)),
                "{e:?}"
            ),
            // The lzma decoder sizes its window by the bound, so this text's first match
            // beyond `max_block_plain` bytes fails before the output reaches the bound.
            Model::Lzma => assert!(
                matches!(&e, FormatError::PayloadTooLarge { max: m, .. } if *m == max)
                    || matches!(&e, FormatError::LzmaError { reason } if reason.contains("beyond dictionary size")),
                "{e:?}"
            ),
        }
    }
}

#[test]
fn too_little_declared_memory_is_refused() {
    let (bytes, _, d) = archive(Model::Zstd, Some(0));
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    let env = a.index().envelope;
    assert_eq!(env.decode_memory, env.max_block_plain);
    register_full_reader(&mut a);
    match a.verify().unwrap_err() {
        FormatError::Refused(r) => {
            assert_eq!(r.field, "decode_memory");
            assert_eq!(r.needed, env.max_block_plain + memory_term(d.plain_len));
            assert_eq!(r.allowed, env.decode_memory);
        }
        e => panic!("{e:?}"),
    }
}

#[test]
fn the_minimal_reader_reports_unimplemented_primitive_8_and_extracts_the_plain_file() {
    let (bytes, _, _) = archive(Model::Lzma, None);
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().collect::<Result<_, _>>().unwrap();
    for e in &entries {
        let mut v = Vec::new();
        let r = a.extract(e, &mut v);
        if e.path == "readme.txt" {
            r.unwrap();
            assert_eq!(v, b"not a container\n");
        } else {
            assert!(matches!(
                r,
                Err(FormatError::UnimplementedPrimitive { id: 8 })
            ));
        }
    }
    assert!(matches!(
        a.verify().unwrap_err(),
        FormatError::UnimplementedPrimitive { id: 8 }
    ));
}
