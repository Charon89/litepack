//! In-process zstd and xz, with one definition of each setting for every probe: "zstd level 19"
//! and "xz preset 9" mean the same thing wherever they appear.
//!
//! [`zstd_measure`] and [`xz_measure`] compress `data` (timed), decompress it (timed), compare
//! the result with the input and return the sizes and times. They are timed sections: call them
//! alone, on data already in memory. [`ZstdContext`] keeps one compressor and one decompressor
//! alive across many calls, for probes that time thousands of small buffers.

#![allow(dead_code)] // shared helpers for probes that land in later tasks

use std::io::{Read, Write};

use anyhow::{anyhow, Result};
use liblzma::stream::{Check, MtStreamBuilder, Stream};
use serde::{Deserialize, Serialize};
use zstd::zstd_safe::{CParameter, DParameter};

use super::timed;

/// zstd settings as recorded in a result file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZstdSettings {
    pub level: i32,
    /// Explicit window log, or `None` for the library's choice for the level.
    pub window_log: Option<u32>,
    pub long_distance_matching: bool,
    /// Worker threads of the library; 1 means the calling thread only.
    pub threads: u32,
}

impl ZstdSettings {
    /// The level with the library's defaults, single-threaded.
    pub fn level(level: i32) -> Self {
        ZstdSettings {
            level,
            window_log: None,
            long_distance_matching: false,
            threads: 1,
        }
    }

    /// The probes' "zstd level 19": window log 27 with long-distance matching, single-threaded.
    pub fn level19() -> Self {
        ZstdSettings {
            level: 19,
            window_log: Some(27),
            long_distance_matching: true,
            threads: 1,
        }
    }

    pub fn context(&self) -> Result<ZstdContext> {
        let mut c = zstd::bulk::Compressor::new(self.level)?;
        let mut d = zstd::bulk::Decompressor::new()?;
        if let Some(w) = self.window_log {
            c.set_parameter(CParameter::WindowLog(w))?;
            d.set_parameter(DParameter::WindowLogMax(w))?;
        }
        c.set_parameter(CParameter::EnableLongDistanceMatching(
            self.long_distance_matching,
        ))?;
        if self.threads > 1 {
            c.set_parameter(CParameter::NbWorkers(self.threads))?;
        }
        Ok(ZstdContext { c, d })
    }
}

/// A reusable zstd compressor and decompressor with fixed settings.
pub struct ZstdContext {
    c: zstd::bulk::Compressor<'static>,
    d: zstd::bulk::Decompressor<'static>,
}

impl std::fmt::Debug for ZstdContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ZstdContext")
    }
}

impl ZstdContext {
    pub fn compress(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        Ok(self.c.compress(data)?)
    }

    /// `plain_len` is the exact size of the original data.
    pub fn decompress(&mut self, compressed: &[u8], plain_len: usize) -> Result<Vec<u8>> {
        Ok(self.d.decompress(compressed, plain_len)?)
    }
}

/// xz settings as recorded in a result file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XzSettings {
    pub preset: u32,
    /// Encoder threads; 1 means the single-threaded encoder.
    pub threads: u32,
}

impl XzSettings {
    /// The probes' "xz preset 9", single-threaded.
    pub fn preset9() -> Self {
        XzSettings {
            preset: 9,
            threads: 1,
        }
    }

    fn encoder(&self) -> Result<Stream> {
        if self.threads > 1 {
            let mut b = MtStreamBuilder::new();
            b.preset(self.preset)
                .threads(self.threads)
                .check(Check::Crc64);
            Ok(b.encoder()?)
        } else {
            Ok(Stream::new_easy_encoder(self.preset, Check::Crc64)?)
        }
    }
}

/// One compress and decompress round of a codec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measured {
    pub compressed_bytes: u64,
    pub compress_seconds: f64,
    pub decompress_seconds: f64,
    /// Decompression reproduced the input exactly.
    pub round_trip_ok: bool,
}

/// Compress and decompress `data` with zstd, timing each step.
pub fn zstd_measure(data: &[u8], s: &ZstdSettings) -> Result<Measured> {
    let mut ctx = s.context()?;
    let (c, compress_seconds) = timed(|| ctx.compress(data));
    let c = c?;
    let (d, decompress_seconds) = timed(|| ctx.decompress(&c, data.len()));
    Ok(Measured {
        compressed_bytes: c.len() as u64,
        compress_seconds,
        decompress_seconds,
        round_trip_ok: d? == data,
    })
}

/// Compress `data` with zstd without timing (sizes only).
pub fn zstd_size(data: &[u8], s: &ZstdSettings) -> Result<u64> {
    Ok(s.context()?.compress(data)?.len() as u64)
}

/// Compress and decompress `data` with xz, timing each step.
pub fn xz_measure(data: &[u8], s: &XzSettings) -> Result<Measured> {
    let (c, compress_seconds) = timed(|| -> Result<Vec<u8>> {
        let mut enc = liblzma::write::XzEncoder::new_stream(Vec::new(), s.encoder()?);
        enc.write_all(data)?;
        Ok(enc.finish()?)
    });
    let c = c?;
    let (d, decompress_seconds) = timed(|| -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(data.len());
        liblzma::read::XzDecoder::new(&c[..])
            .read_to_end(&mut out)
            .map_err(|e| anyhow!("xz decompression failed: {:?}", e.kind()))?;
        Ok(out)
    });
    Ok(Measured {
        compressed_bytes: c.len() as u64,
        compress_seconds,
        decompress_seconds,
        round_trip_ok: d? == data,
    })
}

/// Compress `data` with xz without timing (sizes only).
pub fn xz_size(data: &[u8], s: &XzSettings) -> Result<u64> {
    let mut enc = liblzma::write::XzEncoder::new_stream(Vec::new(), s.encoder()?);
    enc.write_all(data)?;
    Ok(enc.finish()?.len() as u64)
}

/// Version of the linked libzstd.
pub fn zstd_version() -> String {
    zstd::zstd_safe::version_string().to_string()
}

/// Version of the linked xz: the `liblzma-sys` crate, which bundles a fixed xz release.
pub fn xz_version() -> String {
    format!("liblzma-sys {}", env!("LPK_LIBLZMA_SYS_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        (0..50_000u32).map(|i| ((i * i) >> 7) as u8).collect()
    }

    #[test]
    fn zstd_round_trips_with_every_setting_and_the_context_is_reusable() {
        let data = sample();
        for s in [
            ZstdSettings::level(3),
            ZstdSettings::level19(),
            ZstdSettings {
                threads: 2,
                ..ZstdSettings::level(5)
            },
        ] {
            let m = zstd_measure(&data, &s).expect("measure");
            assert!(m.round_trip_ok && m.compressed_bytes > 0);
            assert!(m.compress_seconds >= 0.0 && m.decompress_seconds >= 0.0);
            assert_eq!(zstd_size(&data, &s).expect("size"), m.compressed_bytes);
        }
        let mut ctx = ZstdSettings::level19().context().expect("ctx");
        for chunk in data.chunks(7_000) {
            let c = ctx.compress(chunk).expect("c");
            assert_eq!(ctx.decompress(&c, chunk.len()).expect("d"), chunk);
        }
    }

    #[test]
    fn xz_round_trips_single_and_multi_threaded() {
        let data = sample();
        for s in [
            XzSettings::preset9(),
            XzSettings {
                preset: 6,
                threads: 2,
            },
        ] {
            let m = xz_measure(&data, &s).expect("measure");
            assert!(m.round_trip_ok && m.compressed_bytes > 0);
            assert!(xz_size(&data, &s).expect("size") > 0);
        }
    }

    #[test]
    fn library_versions_are_reported() {
        assert!(zstd_version().starts_with("1."));
        assert!(xz_version().starts_with("liblzma-sys 0."));
    }
}
