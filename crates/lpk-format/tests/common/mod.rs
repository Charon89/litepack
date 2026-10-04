//! Shared by the integration tests: seeded data, a zstd compressor and the
//! test-only [`ZstdTestEncoder`] (the crate itself has no zstd encoder; the
//! production one lives in `lpk-core`).
#![allow(dead_code, clippy::unwrap_used)]

use lpk_format::{
    BlockEncoder, FormatError, Graph, GraphResources, PrimitiveId, Step, Writer, WriterOptions,
};
use std::io::Write;

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
/// archive is written by the writer with the zstd test encoder, so the bytes
/// are a function of this table, the dictionary file and the zstd library.
pub const VECTORS: [&str; 4] = [
    "zstd-basic.lpk",
    "zstd-multiblock.lpk",
    "zstd-dict.lpk",
    "zstd-window.lpk",
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
        other => panic!("no vector {other}"),
    }
}

/// The writer options of a vector; `dict` is the dictionary of `zstd-dict.lpk`.
pub fn vector_options(name: &str, dict: Option<&[u8]>) -> WriterOptions {
    let (block_size, encoder) = match name {
        "zstd-basic.lpk" => (1 << 20, ZstdTestEncoder::new(3, 20, None)),
        "zstd-multiblock.lpk" => (32 * 1024, ZstdTestEncoder::new(3, 20, None)),
        "zstd-dict.lpk" => (
            1 << 20,
            ZstdTestEncoder::new(3, 20, Some(dict.unwrap().to_vec())),
        ),
        "zstd-window.lpk" => (1 << 20, ZstdTestEncoder::new(3, 24, None)),
        other => panic!("no vector {other}"),
    };
    WriterOptions {
        chunk_size: 4096,
        block_size,
        archive_id: [0x5A; 16],
        encoder: Box::new(encoder),
    }
}

/// Build a vector's archive in memory.
pub fn build_vector(name: &str, dict: Option<&[u8]>) -> Vec<u8> {
    write_archive(vector_options(name, dict), &vector_files(name))
}
