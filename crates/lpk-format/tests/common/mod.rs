//! Shared by the integration tests: seeded data, a zstd compressor and the
//! test-only [`ZstdTestEncoder`] (the crate itself has no zstd encoder; the
//! production one lives in `lpk-core`).
#![allow(dead_code, clippy::unwrap_used)]

use lpk_format::{
    BlockEncoder, FormatError, Graph, GraphResources, PrimitiveId, Step, Writer, WriterOptions,
};
use std::io::Write;

pub mod journal;
pub mod recovery;
pub mod sealed;

/// Deterministic, moderately compressible bytes: words drawn from a small
/// vocabulary by a xorshift generator.
pub fn pattern(seed: u64, len: usize) -> Vec<u8> {
    const WORDS: [&str; 12] = [
        "lite", "pack", "chunk", "block", "merkle", "frame", "index", "entry", "zstd", "prior",
        "window", "graph",
    ];
    let mut s = seed | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        out.extend_from_slice(WORDS[(s % 12) as usize].as_bytes());
        out.push(if s & 0x100 == 0 { b' ' } else { b'\n' });
        if s & 0x7000 == 0 {
            out.extend_from_slice(&s.to_le_bytes());
        }
    }
    out.truncate(len);
    out
}

/// Compress `data` into one zstd frame with a content checksum.
pub fn compress(level: i32, window_log: Option<u32>, dict: Option<&[u8]>, data: &[u8]) -> Vec<u8> {
    let mut c = match dict {
        Some(d) => zstd::bulk::Compressor::with_dictionary(level, d).unwrap(),
        None => zstd::bulk::Compressor::new(level).unwrap(),
    };
    c.include_checksum(true).unwrap();
    if let Some(w) = window_log {
        c.set_parameter(zstd::zstd_safe::CParameter::WindowLog(w))
            .unwrap();
    }
    c.compress(data).unwrap()
}

/// Compress `data` with the streaming encoder and no pledged size, so the
/// frame header declares the window (`window_log`) instead of a content size.
pub fn compress_stream(level: i32, window_log: u32, dict: Option<&[u8]>, data: &[u8]) -> Vec<u8> {
    let mut e = match dict {
        Some(d) => zstd::stream::write::Encoder::with_dictionary(Vec::new(), level, d).unwrap(),
        None => zstd::stream::write::Encoder::new(Vec::new(), level).unwrap(),
    };
    e.include_checksum(true).unwrap();
    e.set_parameter(zstd::zstd_safe::CParameter::WindowLog(window_log))
        .unwrap();
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// `zstd` params:`window_log` and the prior id (zeros = none).
pub fn zstd_params(window_log: u8, prior: Option<[u8; 32]>) -> Vec<u8> {
    let mut p = vec![window_log];
    p.extend_from_slice(&prior.unwrap_or([0; 32]));
    p
}

/// Encodes every block as one zstd frame.
pub struct ZstdTestEncoder {
    pub level: i32,
    pub window_log: u8,
    pub dictionary: Option<Vec<u8>>,
}

impl ZstdTestEncoder {
    pub fn new(level: i32, window_log: u8, dictionary: Option<Vec<u8>>) -> Self {
        ZstdTestEncoder {
            level,
            window_log,
            dictionary,
        }
    }

    fn prior(&self) -> Option<[u8; 32]> {
        self.dictionary.as_deref().map(lpk_format::prior_id)
    }
}

impl BlockEncoder for ZstdTestEncoder {
    fn graph(&self) -> Graph {
        Graph {
            steps: vec![Step {
                primitive: PrimitiveId::Zstd,
                params: zstd_params(self.window_log, self.prior()),
            }],
        }
    }

    fn encode(&mut self, plain: &[u8]) -> Result<Vec<u8>, FormatError> {
        Ok(compress_stream(
            self.level,
            u32::from(self.window_log),
            self.dictionary.as_deref(),
            plain,
        ))
    }

    fn resources(&self) -> GraphResources {
        self.graph().resources()
    }
}

/// Raw LZMA1 options: `preset`, then explicit dictionary and properties.
pub fn lzma_options(
    preset: u32,
    dict_size: u32,
    lc: u8,
    lp: u8,
    pb: u8,
) -> liblzma::stream::LzmaOptions {
    let mut o = liblzma::stream::LzmaOptions::new_preset(preset).unwrap();
    o.dict_size(dict_size)
        .literal_context_bits(u32::from(lc))
        .literal_position_bits(u32::from(lp))
        .position_bits(u32::from(pb));
    o
}

/// What liblzma's raw LZMA1 encoder emits for `data`.
pub fn lzma_raw(opts: &liblzma::stream::LzmaOptions, data: &[u8]) -> Vec<u8> {
    use liblzma::stream::{Action, Filters, Status, Stream};
    let mut filters = Filters::new();
    filters.lzma1(opts);
    let mut s = Stream::new_raw_encoder(&filters).unwrap();
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    loop {
        let consumed = usize::try_from(s.total_in()).unwrap();
        let st = s
            .process_vec(&data[consumed..], &mut out, Action::Finish)
            .unwrap();
        if st == Status::StreamEnd {
            return out;
        }
        out.reserve(4096);
    }
}

/// `lzma` params: `dict_size` LE, then `lc`, `lp`, `pb`.
pub fn lzma_params(dict_size: u32, lc: u8, lp: u8, pb: u8) -> Vec<u8> {
    let mut p = dict_size.to_le_bytes().to_vec();
    p.extend_from_slice(&[lc, lp, pb]);
    p
}

/// Encodes every block as one raw LZMA1 stream.
pub struct LzmaTestEncoder {
    pub preset: u32,
    pub dict_size: u32,
    pub lc: u8,
    pub lp: u8,
    pub pb: u8,
}

impl LzmaTestEncoder {
    pub fn new(preset: u32, dict_size: u32, lc: u8, lp: u8, pb: u8) -> Self {
        LzmaTestEncoder {
            preset,
            dict_size,
            lc,
            lp,
            pb,
        }
    }
}

impl BlockEncoder for LzmaTestEncoder {
    fn graph(&self) -> Graph {
        Graph {
            steps: vec![Step {
                primitive: PrimitiveId::Lzma,
                params: lzma_params(self.dict_size, self.lc, self.lp, self.pb),
            }],
        }
    }

    fn encode(&mut self, plain: &[u8]) -> Result<Vec<u8>, FormatError> {
        let o = lzma_options(self.preset, self.dict_size, self.lc, self.lp, self.pb);
        Ok(lzma_raw(&o, plain))
    }

    fn resources(&self) -> GraphResources {
        self.graph().resources()
    }
}

/// Write `files` (sorted by path) with `options`.
pub fn write_archive(options: WriterOptions, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out as &mut dyn Write, options).unwrap();
    for (path, data) in files {
        w.add_file(
            path,
            lpk_format::EntryFlags::EMPTY,
            1_000,
            &mut data.as_slice(),
        )
        .unwrap();
    }
    w.finish().unwrap();
    out
}

/// A dictionary trained on 400 seeded samples of the `pattern` family.
pub fn trained_dictionary(seed: u64) -> Vec<u8> {
    let samples: Vec<Vec<u8>> = (0..400).map(|i| pattern(seed * 1000 + i, 900)).collect();
    zstd::dict::from_samples(&samples, 8 * 1024).unwrap()
}

/// The committed test vectors: archive file names and what they hold. Each
/// archive is written by the writer with the zstd or LZMA test encoder, so the
/// bytes are a function of this table, the dictionary file and the zstd and
/// liblzma library versions.
pub const VECTORS: [&str; 7] = [
    "zstd-basic.lpk",
    "zstd-multiblock.lpk",
    "zstd-dict.lpk",
    "zstd-window.lpk",
    "lzma-basic.lpk",
    "lzma-multiblock.lpk",
    "lzma-props.lpk",
];

/// The dictionary's file name, committed beside `zstd-dict.lpk`.
pub const DICT_FILE: &str = "zstd-dict.prior";

pub fn vectors_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
}

/// The files of a vector (sorted by path) with their generated contents.
pub fn vector_files(name: &str) -> Vec<(&'static str, Vec<u8>)> {
    match name {
        "zstd-basic.lpk" => vec![
            ("a.txt", pattern(11, 300)),
            ("b.txt", pattern(12, 5_000)),
            ("c.txt", pattern(13, 12_000)),
        ],
        "zstd-multiblock.lpk" => vec![
            ("big/one", pattern(21, 20_000)),
            ("big/three", pattern(23, 70_000)),
            ("big/two", pattern(22, 50_000)),
        ],
        "zstd-dict.lpk" => vec![
            ("d/a", pattern(31, 900)),
            ("d/b", pattern(32, 1_800)),
            ("d/c", pattern(33, 2_700)),
        ],
        "zstd-window.lpk" => vec![("w/data", pattern(41, 300_000))],
        "lzma-basic.lpk" => vec![
            ("a.txt", pattern(51, 300)),
            ("b.txt", pattern(52, 5_000)),
            ("c.txt", pattern(53, 12_000)),
        ],
        "lzma-multiblock.lpk" => vec![
            ("big/one", pattern(61, 20_000)),
            ("big/three", pattern(63, 70_000)),
            ("big/two", pattern(62, 50_000)),
        ],
        "lzma-props.lpk" => vec![("p/a", pattern(71, 2_000)), ("p/b", pattern(72, 30_000))],
        other => panic!("no vector {other}"),
    }
}

/// The writer options of a vector; `dict` is the dictionary of `zstd-dict.lpk`.
pub fn vector_options(name: &str, dict: Option<&[u8]>) -> WriterOptions {
    let lzma = |p, d, lc, lp, pb| -> Box<dyn BlockEncoder> {
        Box::new(LzmaTestEncoder::new(p, d, lc, lp, pb))
    };
    let zstd = |l, w, d| -> Box<dyn BlockEncoder> { Box::new(ZstdTestEncoder::new(l, w, d)) };
    let (block_size, encoder) = match name {
        "zstd-basic.lpk" => (1 << 20, zstd(3, 20, None)),
        "zstd-multiblock.lpk" => (32 * 1024, zstd(3, 20, None)),
        "zstd-dict.lpk" => (1 << 20, zstd(3, 20, Some(dict.unwrap().to_vec()))),
        "zstd-window.lpk" => (1 << 20, zstd(3, 24, None)),
        // Preset 6 and its 8 MiB dictionary, lc 3, lp 0, pb 2.
        "lzma-basic.lpk" => (1 << 20, lzma(6, 1 << 23, 3, 0, 2)),
        "lzma-multiblock.lpk" => (32 * 1024, lzma(6, 1 << 23, 3, 0, 2)),
        "lzma-props.lpk" => (1 << 20, lzma(6, 1 << 20, 0, 2, 0)),
        other => panic!("no vector {other}"),
    };
    WriterOptions {
        chunk_size: 4096,
        block_size,
        archive_id: [0x5A; 16],
        encoder,
        records: Vec::new(),
        recovery: Default::default(),
        seal: None,
    }
}

/// Build a vector's archive in memory.
pub fn build_vector(name: &str, dict: Option<&[u8]>) -> Vec<u8> {
    write_archive(vector_options(name, dict), &vector_files(name))
}
