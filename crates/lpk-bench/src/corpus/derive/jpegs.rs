//! Image derivations: `png-to-jpeg`, `jpeg-crop` and `photo-convert`.
//!
//! * `png-to-jpeg`: for every `.png` input, `<stem>-baseline.jpg` and `<stem>-progressive.jpg`
//!   from the in-tree encoder ([`super::jpegenc`]) at the registry's two qualities.
//! * `jpeg-crop`: every N-th JPEG input in sorted order (the N-th, the 2N-th, ...) is decoded,
//!   `crop_permille` per mille of the width and height are cut from each edge (integer division,
//!   so `w * 25 / 1000` pixels at 2.5%), and the rest is re-saved as a baseline JPEG that carries the source's APP1..APP15 segments (EXIF, XMP, ICC) verbatim after the JFIF header.
//! * `photo-convert`: JPEG inputs in ascending order of file size (ties by path), up to `max_files`, as `<stem>.png` and
//!   `<stem>.bmp` (24-bit) while the bytes written stay within `max_bytes`; a photo whose two
//!   files do not fit the remaining budget is skipped.
//!
//! Decoding uses `jpeg-decoder` (integer IDCT, `platform_independent`) and `png`; a JPEG that
//! cannot be decoded (CMYK, corrupt) is skipped with a note on stderr, the same way on every run.

use anyhow::{bail, Context, Result};

use super::jpegenc::{app_segments, decode_rgb, encode, encode_with, jpeg_dims};
use super::{input_files_with, Output, Skip};
use crate::corpus::build::Ctx;
use crate::corpus::registry::{JpegCropSpec, PhotoConvertSpec, PngToJpegSpec, Source};

const JPEG_EXTS: [&str; 2] = [".jpg", ".jpeg"];

/// Path without its extension (the part after the last `.` of the last component).
fn stem(rel: &str) -> &str {
    let name_start = rel.rfind('/').map_or(0, |i| i + 1);
    match rel[name_start..].rfind('.') {
        Some(i) if i > 0 => &rel[..name_start + i],
        _ => rel,
    }
}

/// Decode a PNG to 8-bit RGB (palette expanded, 16-bit reduced, alpha dropped, grey expanded).
pub fn decode_png(data: &[u8]) -> Result<(Vec<u8>, usize, usize)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| anyhow::anyhow!("PNG header: {e}"))?;
    let size = reader
        .output_buffer_size()
        .context("PNG too large to decode")?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| anyhow::anyhow!("PNG decode: {e}"))?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width as usize, info.height as usize);
    let rgb: Vec<u8> = match info.color_type {
        png::ColorType::Rgb => buf,
        png::ColorType::Rgba => buf
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|g| [*g, *g, *g]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0]])
            .collect(),
        png::ColorType::Indexed => bail!("palette PNG was not expanded"),
    };
    if rgb.len() != w * h * 3 {
        bail!("decoded PNG has an unexpected size");
    }
    Ok((rgb, w, h))
}

/// Encode 8-bit RGB as a PNG (fixed settings: balanced compression, adaptive filtering).
pub fn encode_png(rgb: &[u8], w: usize, h: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Balanced);
        let mut writer = enc
            .write_header()
            .map_err(|e| anyhow::anyhow!("PNG header: {e}"))?;
        writer
            .write_image_data(rgb)
            .map_err(|e| anyhow::anyhow!("PNG encode: {e}"))?;
        writer
            .finish()
            .map_err(|e| anyhow::anyhow!("PNG finish: {e}"))?;
    }
    Ok(out)
}

/// Size of the 24-bit BMP of a `w` x `h` image.
pub fn bmp_size(w: usize, h: usize) -> u64 {
    54 + (((w * 3 + 3) & !3) as u64) * h as u64
}

/// A 24-bit uncompressed BMP (bottom-up rows padded to 4 bytes, BGR).
pub fn encode_bmp(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    let row = (w * 3 + 3) & !3;
    let size = bmp_size(w, h) as u32;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as u32).to_le_bytes());
    out.extend_from_slice(&(h as u32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]); // BI_RGB, image size 0, resolution 0, no palette
    for y in (0..h).rev() {
        for px in rgb[y * w * 3..(y + 1) * w * 3].as_chunks::<3>().0 {
            out.extend_from_slice(&[px[2], px[1], px[0]]);
        }
        out.resize(out.len() + (row - w * 3), 0);
    }
    out
}

pub fn build_png_to_jpeg(
    ctx: &Ctx<'_>,
    source: &Source,
    spec: &PngToJpegSpec,
    out: &mut Output<'_>,
) -> Result<()> {
    for f in input_files_with(ctx, source, &spec.from, &[".png"])? {
        let data = std::fs::read(&f.path).with_context(|| format!("reading {}", f.full()))?;
        let (rgb, w, h) = decode_png(&data).with_context(|| format!("`{}`", f.full()))?;
        let s = stem(&f.rel);
        let base = encode(&rgb, w, h, spec.baseline_quality, false)?;
        out.write(&format!("{s}-baseline.jpg"), &base)?;
        let prog = encode(&rgb, w, h, spec.progressive_quality, true)?;
        out.write(&format!("{s}-progressive.jpg"), &prog)?;
    }
    Ok(())
}

/// Crop margins for one axis: pixels removed from each edge.
pub fn crop_margin(len: usize, permille: u32) -> usize {
    len * permille as usize / 1000
}

pub fn build_crop(
    ctx: &Ctx<'_>,
    source: &Source,
    spec: &JpegCropSpec,
    out: &mut Output<'_>,
) -> Result<()> {
    let all = input_files_with(ctx, source, &spec.from, &JPEG_EXTS)?;
    for f in all.iter().skip(spec.every - 1).step_by(spec.every) {
        let data = std::fs::read(&f.path).with_context(|| format!("reading {}", f.full()))?;
        let (rgb, w, h) = match decode_rgb(&data) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  skipping {}: {e:#}", f.full());
                continue;
            }
        };
        let (mx, my) = (
            crop_margin(w, spec.crop_permille),
            crop_margin(h, spec.crop_permille),
        );
        let (cw, ch) = (w - 2 * mx, h - 2 * my);
        let mut cropped = Vec::with_capacity(cw * ch * 3);
        for y in my..my + ch {
            cropped.extend_from_slice(&rgb[(y * w + mx) * 3..(y * w + mx + cw) * 3]);
        }
        let jpg = encode_with(&cropped, cw, ch, spec.quality, false, &app_segments(&data))?;
        out.write(&f.rel, &jpg)?;
    }
    if out.len() == 0 {
        return Err(Skip("no input JPEG could be decoded".into()).into());
    }
    Ok(())
}

pub fn build_convert(
    ctx: &Ctx<'_>,
    source: &Source,
    spec: &PhotoConvertSpec,
    out: &mut Output<'_>,
) -> Result<()> {
    let mut converted = 0usize;
    let mut spent = 0u64;
    // Smallest JPEG first (ties by path), so a byte budget buys the most photos.
    let mut inputs = input_files_with(ctx, source, &spec.from, &JPEG_EXTS)?;
    let mut keyed = Vec::with_capacity(inputs.len());
    for f in inputs.drain(..) {
        keyed.push((std::fs::metadata(&f.path)?.len(), f.full(), f));
    }
    keyed.sort_by(|a, b| (a.0, a.1.as_bytes()).cmp(&(b.0, b.1.as_bytes())));
    for (_, _, f) in keyed {
        if converted >= spec.max_files {
            break;
        }
        let data = std::fs::read(&f.path).with_context(|| format!("reading {}", f.full()))?;
        let Ok((w, h)) = jpeg_dims(&data) else {
            eprintln!("  skipping {}: unreadable JPEG header", f.full());
            continue;
        };
        if spent + bmp_size(w, h) > spec.max_bytes {
            continue;
        }
        let (rgb, w, h) = match decode_rgb(&data) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  skipping {}: {e:#}", f.full());
                continue;
            }
        };
        let png = encode_png(&rgb, w, h)?;
        if spent + bmp_size(w, h) + png.len() as u64 > spec.max_bytes {
            continue;
        }
        let s = stem(&f.rel);
        out.write(&format!("{s}.png"), &png)?;
        out.write(&format!("{s}.bmp"), &encode_bmp(&rgb, w, h))?;
        spent += bmp_size(w, h) + png.len() as u64;
        converted += 1;
    }
    if converted == 0 {
        return Err(Skip(format!(
            "no input photo fits the {} byte budget (or none could be decoded)",
            spec.max_bytes
        ))
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::jpegenc::tests_support::picture;
    use super::super::testutil::{put, read_all, run, source};
    use super::*;
    use crate::corpus::registry::SourceSpec;

    fn png_of(w: usize, h: usize) -> Vec<u8> {
        encode_png(&picture(w, h), w, h).expect("png")
    }

    fn jpeg_of(w: usize, h: usize) -> Vec<u8> {
        encode(&picture(w, h), w, h, 95, false).expect("jpeg")
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mse = a
            .iter()
            .zip(b)
            .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        10.0 * (255.0 * 255.0 / mse.max(1e-9)).log10()
    }

    #[test]
    fn stems() {
        assert_eq!(stem("a/b.c/kodim01.png"), "a/b.c/kodim01");
        assert_eq!(stem("noext"), "noext");
        assert_eq!(stem("a.b/noext"), "a.b/noext");
    }

    #[test]
    fn kodak_style_pngs_become_a_baseline_and_a_progressive_jpeg() {
        let src = source(
            "kodak-jpeg",
            "photo-jpeg",
            &["photo-raw-png"],
            SourceSpec::PngToJpeg(PngToJpegSpec {
                from: vec!["kodak".into()],
                baseline_quality: 90,
                progressive_quality: 85,
            }),
        );
        let mut runs = Vec::new();
        for _ in 0..2 {
            let d = tempfile::tempdir().expect("tmp");
            put(
                d.path(),
                "photo-raw-png",
                "kodak",
                "kodim01.png",
                &png_of(96, 64),
            );
            put(
                d.path(),
                "photo-raw-png",
                "kodak",
                "kodim02.png",
                &png_of(33, 50),
            );
            put(
                d.path(),
                "photo-raw-png",
                "other",
                "ignored.png",
                &png_of(8, 8),
            );
            run(
                d.path(),
                &src,
                &[("photo-raw-png", "kodak"), ("photo-raw-png", "other")],
            )
            .expect("run");
            runs.push(read_all(d.path(), "photo-jpeg", "kodak-jpeg"));
        }
        assert_eq!(runs[0], runs[1]);
        let names: Vec<&str> = runs[0].iter().map(|f| f.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "kodim01-baseline.jpg",
                "kodim01-progressive.jpg",
                "kodim02-baseline.jpg",
                "kodim02-progressive.jpg"
            ]
        );
        for (name, data) in &runs[0] {
            let (w, h) = if name.starts_with("kodim01") {
                (96, 64)
            } else {
                (33, 50)
            };
            let marker: [u8; 2] = if name.contains("progressive") {
                [0xFF, 0xC2]
            } else {
                [0xFF, 0xC0]
            };
            assert!(data.windows(2).any(|p| p == marker), "{name}");
            let (rgb, dw, dh) = decode_rgb(data).expect("decode");
            assert_eq!((dw, dh), (w, h));
            assert!(psnr(&rgb, &picture(w, h)) > 25.0, "{name}");
        }
    }

    #[test]
    fn crop_takes_every_nth_photo_and_cuts_each_edge() {
        let src = source(
            "edited",
            "photo-jpeg-edited",
            &["photo-jpeg"],
            SourceSpec::JpegCrop(JpegCropSpec {
                from: vec!["commons".into()],
                every: 5,
                crop_permille: 25,
                quality: 85,
            }),
        );
        let d = tempfile::tempdir().expect("tmp");
        for i in 0..12 {
            put(
                d.path(),
                "photo-jpeg",
                "commons",
                &format!("p{i:02}.jpg"),
                &jpeg_of(400, 300),
            );
        }
        put(
            d.path(),
            "photo-jpeg",
            "libultrahdr",
            "x.jpg",
            &jpeg_of(40, 30),
        );
        run(
            d.path(),
            &src,
            &[("photo-jpeg", "commons"), ("photo-jpeg", "libultrahdr")],
        )
        .expect("run");
        let files = read_all(d.path(), "photo-jpeg-edited", "edited");
        assert_eq!(
            files.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(),
            ["p04.jpg", "p09.jpg"],
            "the 5th and 10th photo in sorted order"
        );
        let (orig, _, _) = decode_rgb(&jpeg_of(400, 300)).expect("orig");
        for (_, data) in &files {
            assert_eq!(jpeg_dims(data).expect("dims"), (380, 286));
            let (rgb, w, h) = decode_rgb(data).expect("decode");
            let mut expect = Vec::new();
            for y in 7..7 + 286 {
                expect.extend_from_slice(&orig[(y * 400 + 10) * 3..(y * 400 + 390) * 3]);
            }
            assert_eq!((w, h), (380, 286));
            assert!(psnr(&rgb, &expect) > 28.0);
        }
        // Repeats exactly.
        let d2 = tempfile::tempdir().expect("tmp");
        for i in 0..12 {
            put(
                d2.path(),
                "photo-jpeg",
                "commons",
                &format!("p{i:02}.jpg"),
                &jpeg_of(400, 300),
            );
        }
        run(d2.path(), &src, &[("photo-jpeg", "commons")]).expect("run");
        assert_eq!(files, read_all(d2.path(), "photo-jpeg-edited", "edited"));
        assert_eq!(crop_margin(400, 25), 10);
        assert_eq!(crop_margin(300, 25), 7);
    }

    fn parse_bmp(b: &[u8]) -> (Vec<u8>, usize, usize) {
        assert_eq!(&b[..2], b"BM");
        assert_eq!(
            u32::from_le_bytes([b[2], b[3], b[4], b[5]]) as usize,
            b.len()
        );
        let w = u32::from_le_bytes([b[18], b[19], b[20], b[21]]) as usize;
        let h = u32::from_le_bytes([b[22], b[23], b[24], b[25]]) as usize;
        let row = (w * 3 + 3) & !3;
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            let line = &b[54 + (h - 1 - y) * row..];
            for x in 0..w {
                rgb[(y * w + x) * 3..][..3].copy_from_slice(&[
                    line[x * 3 + 2],
                    line[x * 3 + 1],
                    line[x * 3],
                ]);
            }
        }
        (rgb, w, h)
    }

    #[test]
    fn conversions_are_lossless_copies_of_the_decoded_photo_within_the_budget() {
        let mk = |max_files, max_bytes| {
            source(
                "conv",
                "photo-raw-png",
                &["photo-jpeg"],
                SourceSpec::PhotoConvert(PhotoConvertSpec {
                    from: vec!["commons".into()],
                    max_files,
                    max_bytes,
                }),
            )
        };
        let d = tempfile::tempdir().expect("tmp");
        for (i, (w, h)) in [(60, 40), (61, 41), (500, 400), (50, 30), (64, 48)]
            .iter()
            .enumerate()
        {
            put(
                d.path(),
                "photo-jpeg",
                "commons",
                &format!("p{i}.jpg"),
                &jpeg_of(*w, *h),
            );
        }
        // Smallest files first; the 500x400 photo is skipped by the budget, the count cap stops at 3.
        let budget = 100_000;
        run(d.path(), &mk(3, budget), &[("photo-jpeg", "commons")]).expect("run");
        let files = read_all(d.path(), "photo-raw-png", "conv");
        let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(
            names,
            ["p0.bmp", "p0.png", "p3.bmp", "p3.png", "p4.bmp", "p4.png"]
        );
        assert!(files.iter().map(|f| f.1.len() as u64).sum::<u64>() <= budget);
        for pair in files.chunks(2) {
            let (bmp, w, h) = parse_bmp(&pair[0].1);
            let (png, pw, ph) = decode_png(&pair[1].1).expect("png");
            assert_eq!((w, h), (pw, ph));
            assert_eq!(bmp, png, "BMP and PNG hold the same pixels");
            let jpg = std::fs::read(
                d.path()
                    .join("out/photo-jpeg/commons")
                    .join(pair[0].0.replace(".bmp", ".jpg")),
            )
            .expect("jpg");
            assert_eq!(decode_rgb(&jpg).expect("jpg").0, bmp);
        }
        // A budget that fits nothing is a skip.
        let d2 = tempfile::tempdir().expect("tmp");
        put(
            d2.path(),
            "photo-jpeg",
            "commons",
            "p.jpg",
            &jpeg_of(60, 40),
        );
        let err = run(d2.path(), &mk(3, 100), &[("photo-jpeg", "commons")]).expect_err("budget");
        assert!(err.downcast_ref::<Skip>().is_some());
    }

    #[test]
    fn an_edited_photo_keeps_the_exif_and_other_app_segments_of_its_source() {
        let plain = jpeg_of(160, 120);
        // SOI + JFIF APP0 (18 bytes), then an EXIF APP1 and an XMP-like APP1, then the rest.
        let exif = {
            let payload = b"Exif\0\0MM\0*fake exif payload";
            let mut s = vec![0xFF, 0xE1];
            s.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
            s.extend_from_slice(payload);
            s
        };
        let icc = {
            let payload = b"ICC_PROFILE\0\x01\x01profile bytes";
            let mut s = vec![0xFF, 0xE2];
            s.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
            s.extend_from_slice(payload);
            s
        };
        let mut src_jpeg = plain[..20].to_vec();
        src_jpeg.extend_from_slice(&exif);
        src_jpeg.extend_from_slice(&icc);
        src_jpeg.extend_from_slice(&plain[20..]);
        assert_eq!(
            app_segments(&src_jpeg),
            [exif.clone(), icc.clone()].concat()
        );
        assert!(decode_rgb(&src_jpeg).is_ok());

        let src = source(
            "edited",
            "photo-jpeg-edited",
            &["photo-jpeg"],
            SourceSpec::JpegCrop(JpegCropSpec {
                from: vec![],
                every: 1,
                crop_permille: 25,
                quality: 85,
            }),
        );
        let d = tempfile::tempdir().expect("tmp");
        put(d.path(), "photo-jpeg", "commons", "a.jpg", &src_jpeg);
        run(d.path(), &src, &[("photo-jpeg", "commons")]).expect("run");
        let files = read_all(d.path(), "photo-jpeg-edited", "edited");
        let out = &files[0].1;
        assert_eq!(
            &out[20..20 + exif.len() + icc.len()],
            [exif, icc].concat().as_slice()
        );
        assert_eq!(out[2..4], [0xFF, 0xE0], "JFIF stays first");
        assert!(decode_rgb(out).is_ok());
    }

    #[test]
    fn an_edited_photo_drops_adobe_and_mpf_segments() {
        fn seg(marker: u8, payload: &[u8]) -> Vec<u8> {
            let mut s = vec![0xFF, marker];
            s.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
            s.extend_from_slice(payload);
            s
        }
        let plain = jpeg_of(160, 120);
        let exif = seg(0xE1, b"Exif\0\0MM\0*fake exif payload");
        let mpf = seg(0xE2, b"MPF\0fake mp index");
        let icc = seg(0xE2, b"ICC_PROFILE\0\x01\x01profile bytes");
        let adobe = seg(0xEE, b"Adobe\0d\x80\0\0\0\0");
        let mut src_jpeg = plain[..20].to_vec();
        for s in [&exif, &mpf, &adobe, &icc] {
            src_jpeg.extend_from_slice(s);
        }
        src_jpeg.extend_from_slice(&plain[20..]);
        assert_eq!(
            app_segments(&src_jpeg),
            [exif.clone(), icc.clone()].concat()
        );

        let src = source(
            "edited",
            "photo-jpeg-edited",
            &["photo-jpeg"],
            SourceSpec::JpegCrop(JpegCropSpec {
                from: vec![],
                every: 1,
                crop_permille: 25,
                quality: 85,
            }),
        );
        let d = tempfile::tempdir().expect("tmp");
        put(d.path(), "photo-jpeg", "commons", "a.jpg", &src_jpeg);
        run(d.path(), &src, &[("photo-jpeg", "commons")]).expect("run");
        let files = read_all(d.path(), "photo-jpeg-edited", "edited");
        let out = &files[0].1;
        assert_eq!(
            &out[20..20 + exif.len() + icc.len()],
            [exif, icc].concat().as_slice()
        );
        let has = |needle: &[u8]| out.windows(needle.len()).any(|w| w == needle);
        assert!(!has(b"MPF\0") && !has(b"Adobe\0"));
        assert!(decode_rgb(out).is_ok());
    }

    #[test]
    fn conversions_take_the_smallest_photos_first() {
        let src = source(
            "conv",
            "photo-raw-png",
            &["photo-jpeg"],
            SourceSpec::PhotoConvert(PhotoConvertSpec {
                from: vec![],
                max_files: 2,
                max_bytes: 10_000_000,
            }),
        );
        let d = tempfile::tempdir().expect("tmp");
        put(d.path(), "photo-jpeg", "c", "a-big.jpg", &jpeg_of(160, 120));
        put(d.path(), "photo-jpeg", "c", "b-small.jpg", &jpeg_of(40, 30));
        put(d.path(), "photo-jpeg", "c", "c-mid.jpg", &jpeg_of(80, 60));
        run(d.path(), &src, &[("photo-jpeg", "c")]).expect("run");
        let names: Vec<String> = read_all(d.path(), "photo-raw-png", "conv")
            .into_iter()
            .map(|f| f.0)
            .collect();
        assert_eq!(
            names,
            ["b-small.bmp", "b-small.png", "c-mid.bmp", "c-mid.png"]
        );
    }

    #[test]
    fn missing_inputs_are_a_skip() {
        let d = tempfile::tempdir().expect("tmp");
        let src = source(
            "edited",
            "photo-jpeg-edited",
            &["photo-jpeg"],
            SourceSpec::JpegCrop(JpegCropSpec {
                from: vec![],
                every: 5,
                crop_permille: 25,
                quality: 85,
            }),
        );
        let err = run(d.path(), &src, &[]).expect_err("no inputs");
        assert!(err.downcast_ref::<Skip>().is_some(), "{err:#}");
    }
}
