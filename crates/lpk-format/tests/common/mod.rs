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

/// `zstd` params: `window_log` and the prior id (zeros = none).
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
        Ok(compress(
            self.level,
            Some(u32::from(self.window_log)),
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
