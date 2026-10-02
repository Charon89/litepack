//! `flac-to-wav`: one WAV file per FLAC input, decoded in-process with `claxon`.
//!
//! The WAV holds the exact decoded samples as little-endian integer PCM (8-bit unsigned,
//! otherwise signed in `ceil(bits/8)` bytes, values of odd widths shifted up to the container
//! width). Mono and stereo up to 16 bits use the plain PCM header, everything else the
//! `WAVE_FORMAT_EXTENSIBLE` one. Output name: the input's path with `.flac` replaced by `.wav`.

use anyhow::{bail, Context, Result};

use super::{input_files_with, Output};
use crate::corpus::build::Ctx;
use crate::corpus::registry::{FlacToWavSpec, Source};

/// A canonical WAV file for interleaved `samples` of `bits` per sample.
pub fn wav_bytes(samples: &[i32], channels: u32, rate: u32, bits: u32) -> Result<Vec<u8>> {
    if channels == 0 || channels > 8 || !(1..=32).contains(&bits) {
        bail!("unsupported FLAC format: {channels} channels, {bits} bits");
    }
    let width = bits.div_ceil(8);
    let data_len = samples.len() as u64 * u64::from(width);
    let extensible = channels > 2 || width > 2;
    let fmt_len: u32 = if extensible { 40 } else { 16 };
    let riff_len = 4 + (8 + u64::from(fmt_len)) + (8 + data_len + (data_len & 1));
    let riff_len = u32::try_from(riff_len).context("decoded audio exceeds the 4 GB WAV limit")?;
    let mut out = Vec::with_capacity(riff_len as usize + 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&fmt_len.to_le_bytes());
    let tag: u16 = if extensible { 0xFFFE } else { 1 };
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&(channels as u16).to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    let align = channels * width;
    out.extend_from_slice(&(rate * align).to_le_bytes());
    out.extend_from_slice(&(align as u16).to_le_bytes());
    out.extend_from_slice(&((width * 8) as u16).to_le_bytes());
    if extensible {
        out.extend_from_slice(&22u16.to_le_bytes());
        out.extend_from_slice(&(bits as u16).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // channel mask: unspecified
                                                    // KSDATAFORMAT_SUBTYPE_PCM
        out.extend_from_slice(&[
            1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71,
        ]);
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    let shift = width * 8 - bits;
    for &s in samples {
        if width == 1 {
            out.push((s.wrapping_shl(shift) + 128) as u8);
        } else {
            let v = s.wrapping_shl(shift).to_le_bytes();
            out.extend_from_slice(&v[..width as usize]);
        }
    }
    if data_len & 1 == 1 {
        out.push(0);
    }
    Ok(out)
}

pub fn build(
    ctx: &Ctx<'_>,
    source: &Source,
    spec: &FlacToWavSpec,
    out: &mut Output<'_>,
) -> Result<()> {
    for f in input_files_with(ctx, source, &spec.from, &[".flac"])? {
        let reader = std::fs::File::open(&f.path)
            .with_context(|| format!("opening {}", f.path.display()))
            .map(std::io::BufReader::new)?;
        let mut flac = claxon::FlacReader::new(reader)
            .with_context(|| format!("`{}` is not a readable FLAC file", f.full()))?;
        let info = flac.streaminfo();
        let mut samples =
            Vec::with_capacity(info.samples.unwrap_or(0) as usize * info.channels as usize);
        for s in flac.samples() {
            samples.push(s.with_context(|| format!("decoding `{}`", f.full()))?);
        }
        let wav = wav_bytes(
            &samples,
            info.channels,
            info.sample_rate,
            info.bits_per_sample,
        )
        .with_context(|| format!("`{}`", f.full()))?;
        let stem = &f.rel[..f.rel.len() - ".flac".len()];
        out.write(&format!("{stem}.wav"), &wav)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{put, read_all, run, source};
    use super::super::Skip;
    use super::*;
    use crate::corpus::registry::SourceSpec;

    fn crc8(data: &[u8]) -> u8 {
        let mut crc = 0u8;
        for &b in data {
            crc ^= b;
            for _ in 0..8 {
                crc = if crc & 0x80 != 0 {
                    (crc << 1) ^ 0x07
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    fn crc16(data: &[u8]) -> u16 {
        let mut crc = 0u16;
        for &b in data {
            crc ^= u16::from(b) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x8005
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    /// A FLAC stream with one frame of verbatim 16-bit subframes (`channels` independent).
    pub fn flac_verbatim(samples: &[i32], channels: usize, rate: u32) -> Vec<u8> {
        let per = samples.len() / channels;
        let mut f = b"fLaC".to_vec();
        f.push(0x80); // last metadata block, STREAMINFO
        f.extend_from_slice(&[0, 0, 34]);
        f.extend_from_slice(&(per as u16).to_be_bytes());
        f.extend_from_slice(&(per as u16).to_be_bytes());
        f.extend_from_slice(&[0; 6]);
        let packed: u64 =
            (u64::from(rate) << 44) | ((channels as u64 - 1) << 41) | (15u64 << 36) | per as u64;
        f.extend_from_slice(&packed.to_be_bytes());
        f.extend_from_slice(&[0; 16]);
        let mut frame = vec![0xFF, 0xF8, 0x70, ((channels as u8 - 1) << 4) | 0x08, 0x00];
        frame.extend_from_slice(&((per - 1) as u16).to_be_bytes());
        frame.push(crc8(&frame));
        for c in 0..channels {
            frame.push(0x02); // verbatim subframe, no wasted bits
            for i in 0..per {
                frame.extend_from_slice(&(samples[i * channels + c] as i16).to_be_bytes());
            }
        }
        let crc = crc16(&frame);
        frame.extend_from_slice(&crc.to_be_bytes());
        f.extend_from_slice(&frame);
        f
    }

    fn src() -> Source {
        source(
            "wav",
            "audio",
            &["audio"],
            SourceSpec::FlacToWav(FlacToWavSpec { from: vec![] }),
        )
    }

    #[test]
    fn wav_holds_the_flac_samples_and_repeats() {
        let samples: Vec<i32> = (0..2000).map(|i| (i * 37) % 60000 - 30000).collect();
        let flac = flac_verbatim(&samples, 2, 44100);
        let mut dirs = Vec::new();
        for _ in 0..2 {
            let d = tempfile::tempdir().expect("tmp");
            put(d.path(), "audio", "commons-flac", "x/a.FLAC", &flac);
            put(d.path(), "audio", "commons-flac", "x/not-audio.mp3", b"mp3");
            run(d.path(), &src(), &[("audio", "commons-flac")]).expect("run");
            dirs.push(d);
        }
        let files = read_all(dirs[0].path(), "audio", "wav");
        assert_eq!(files, read_all(dirs[1].path(), "audio", "wav"));
        assert_eq!(files.len(), 1, "only the FLAC is converted");
        assert_eq!(files[0].0, "x/a.wav");
        let w = &files[0].1;
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([w[20], w[21]]), 1);
        assert_eq!(u16::from_le_bytes([w[22], w[23]]), 2);
        assert_eq!(u32::from_le_bytes([w[24], w[25], w[26], w[27]]), 44100);
        assert_eq!(u16::from_le_bytes([w[34], w[35]]), 16);
        assert_eq!(&w[36..40], b"data");
        let len = u32::from_le_bytes([w[40], w[41], w[42], w[43]]) as usize;
        assert_eq!(len, samples.len() * 2);
        assert_eq!(w.len(), 44 + len);
        let decoded: Vec<i32> = w[44..]
            .chunks(2)
            .map(|c| i32::from(i16::from_le_bytes([c[0], c[1]])))
            .collect();
        assert_eq!(decoded, samples);
        assert_eq!(
            u32::from_le_bytes([w[4], w[5], w[6], w[7]]) as usize,
            w.len() - 8
        );
    }

    #[test]
    fn samples_narrower_than_a_byte_are_shifted_up_to_8_bit_unsigned() {
        let w = wav_bytes(&[-8, 7, 0], 1, 8000, 4).expect("wav");
        assert_eq!(u16::from_le_bytes([w[34], w[35]]), 8);
        assert_eq!(&w[44..47], &[0, 240, 128]);
    }

    #[test]
    fn wide_samples_use_the_extensible_header() {
        let w = wav_bytes(&[1, -2, 3, -4], 2, 48000, 24).expect("wav");
        assert_eq!(u16::from_le_bytes([w[20], w[21]]), 0xFFFE);
        assert_eq!(&w[w.len() - 20..w.len() - 16], b"data");
        assert_eq!(&w[w.len() - 12..w.len() - 9], &[1, 0, 0]);
        assert_eq!(&w[w.len() - 3..], &[0xFC, 0xFF, 0xFF]);
    }

    #[test]
    fn no_flac_files_is_a_skip() {
        let d = tempfile::tempdir().expect("tmp");
        put(d.path(), "audio", "librivox", "a.mp3", b"mp3");
        let err = run(d.path(), &src(), &[("audio", "librivox")]).expect_err("no flac");
        assert!(err.downcast_ref::<Skip>().is_some(), "{err:#}");
    }
}
