//! `probe weights` (PLAN P0-4): byte-plane splitting of floating-point model weights.
//!
//! Class `model-weights`. Every file is tried as a safetensors file: an 8-byte little-endian
//! header length, a JSON header mapping tensor name to dtype, shape and `data_offsets`, then the
//! data. For each floating-point tensor (F16, BF16, F32, F64) three variants are compressed with
//! zstd level 19 (window log 27, long-distance matching, one thread, one compressor and one
//! decompressor reused for everything):
//!
//! * `plain`: the tensor's bytes as they are;
//! * `byte_planes`: the bytes split into planes, plane `i` holding byte `i` of every element
//!   (plane 0 is the lowest byte), each plane compressed on its own;
//! * `rotated_planes`: every element first rotated left by one bit, then split as above. For
//!   BF16 and F32 this moves the whole exponent into the highest byte (the sign bit goes to
//!   the lowest bit), which is the "exponent plane / mantissa planes" split.
//!
//! Both split variants are reversed and compared with the tensor's bytes, and the plain one is
//! decompressed and compared too; a mismatch stops the probe with an error. The whole file is
//! also compressed with the same settings. Each variant's compression and decompression is
//! timed alone, per tensor, on data already in memory.
//!
//! Reasons for files that are not parsed come from a fixed list of categories: nothing from the
//! file's content (serde's messages quote values) ever reaches the result.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Result};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use super::codec::{ZstdContext, ZstdSettings};
use super::{mbps, md_header, md_table, pct, timed, Ctx, Envelope, Output};

pub const CLASS: &str = "model-weights";
/// safetensors refuses headers larger than this.
const MAX_HEADER_BYTES: u64 = 100_000_000;

// ---------------------------------------------------------------------------------------------
// The result

/// The probe's own part of the result file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub class: String,
    /// Number of files the manifest lists for the class: every one is in `files` or
    /// `not_parsed`, by index 0..manifest_files.
    pub manifest_files: u32,
    /// The same settings for the plain, the split and the whole-file compression.
    pub zstd: ZstdSettings,
    /// Files parsed as safetensors, in manifest order.
    pub files: Vec<FileRecord>,
    /// Files that are not safetensors (or whose header is malformed), in manifest order.
    pub not_parsed: Vec<NotParsed>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    /// Position of the file within the class's manifest list.
    pub index: u32,
    /// Manifest path; absent for a private corpus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub bytes: u64,
    /// The 8-byte length field plus the JSON header.
    pub header_bytes: u64,
    pub tensors: u64,
    /// The whole file with the probe's zstd settings.
    pub whole_file_compressed_bytes: u64,
    /// Tensors of other dtypes (integers, booleans, 8-bit floats) and their bytes.
    pub other_tensors: u64,
    pub other_bytes: u64,
    /// One record per floating-point dtype present, sorted by dtype name.
    pub dtypes: Vec<DtypeRecord>,
}

/// A variant compressed as one piece per tensor.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Whole {
    /// Sum over tensors.
    pub compressed_bytes: u64,
    pub compress_seconds: f64,
    pub decompress_seconds: f64,
}

/// A variant compressed as one piece per byte plane.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Planes {
    /// Sum over tensors and planes.
    pub compressed_bytes: u64,
    /// `compressed_bytes` by plane (index 0 = lowest byte of each element).
    pub plane_compressed_bytes: Vec<u64>,
    /// Rotation (rotated variant only), splitting and compressing every plane, summed over tensors.
    pub compress_seconds: f64,
    /// Decompressing every plane, merging (and un-rotating), summed over tensors.
    pub decompress_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DtypeRecord {
    pub dtype: String,
    pub element_bytes: u32,
    pub tensors: u64,
    pub original_bytes: u64,
    pub plain: Whole,
    pub byte_planes: Planes,
    pub rotated_planes: Planes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotParsed {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub bytes: u64,
    /// One of a fixed list of categories; never contains file or tensor names or values.
    pub reason: String,
}

// ---------------------------------------------------------------------------------------------
// safetensors header

/// One tensor of a header (name left out: it is not needed and not recorded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tensor {
    pub dtype: String,
    pub shape: Vec<u64>,
    /// Offsets relative to the start of the data area.
    pub begin: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Length field plus JSON: where the data area starts.
    pub header_bytes: u64,
    /// Sorted by `begin`.
    pub tensors: Vec<Tensor>,
}

#[derive(Deserialize)]
struct RawTensor {
    dtype: String,
    shape: Vec<u64>,
    data_offsets: [u64; 2],
}

/// The header object read directly into typed entries (`__metadata__` skipped).
struct Entries(Vec<RawTensor>);

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Entries;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a safetensors header object")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut m: A,
            ) -> std::result::Result<Entries, A::Error> {
                let mut v = Vec::new();
                while let Some(key) = m.next_key::<String>()? {
                    if key == "__metadata__" {
                        m.next_value::<IgnoredAny>()?;
                    } else {
                        v.push(m.next_value::<RawTensor>()?);
                    }
                }
                Ok(Entries(v))
            }
        }
        d.deserialize_map(V)
    }
}

/// Bytes per element of a safetensors dtype, `None` for a dtype this probe does not know.
pub fn element_width(dtype: &str) -> Option<u64> {
    match dtype {
        "BOOL" | "U8" | "I8" | "F8_E5M2" | "F8_E4M3" | "F8_E8M0" => Some(1),
        "F16" | "BF16" | "I16" | "U16" => Some(2),
        "F32" | "I32" | "U32" => Some(4),
        "F64" | "I64" | "U64" | "C64" => Some(8),
        _ => None,
    }
}

pub fn is_float(dtype: &str) -> bool {
    matches!(dtype, "F16" | "BF16" | "F32" | "F64")
}

/// Parse and check the header of a safetensors file held in memory. Never panics; the error is
/// one of a fixed list of texts and quotes nothing from the file.
pub fn parse_header(file: &[u8]) -> std::result::Result<Header, String> {
    if file.starts_with(b"PK\x03\x04") || file.starts_with(b"PK\x05\x06") {
        return Err("not safetensors: ZIP archive (PyTorch checkpoint)".to_string());
    }
    let total = file.len() as u64;
    let Some(len_bytes) = file.get(..8) else {
        return Err("shorter than the 8-byte header length".to_string());
    };
    let mut raw = [0u8; 8];
    raw.copy_from_slice(len_bytes);
    let n = u64::from_le_bytes(raw);
    if n > MAX_HEADER_BYTES {
        return Err("declared header length exceeds the limit".to_string());
    }
    if n > total - 8 {
        return Err("declared header length exceeds the file".to_string());
    }
    let json = &file[8..8 + n as usize];
    let entries: Entries = serde_json::from_slice(json).map_err(|e| {
        match e.classify() {
            serde_json::error::Category::Syntax => "header JSON is not valid",
            serde_json::error::Category::Eof => "header JSON ends early",
            _ => "header JSON has an unexpected structure",
        }
        .to_string()
    })?;
    let data_len = total - 8 - n;
    let mut tensors = Vec::new();
    for t in entries.0 {
        let [begin, end] = t.data_offsets;
        if begin > end {
            return Err("a tensor has its begin offset after its end offset".to_string());
        }
        if end > data_len {
            return Err("a tensor ends beyond the data area".to_string());
        }
        if let Some(w) = element_width(&t.dtype) {
            let elements = t
                .shape
                .iter()
                .try_fold(1u64, |acc, d| acc.checked_mul(*d))
                .ok_or_else(|| "a tensor's shape overflows".to_string())?;
            if elements.checked_mul(w) != Some(end - begin) {
                return Err("a tensor's data size does not match its shape".to_string());
            }
        }
        tensors.push(Tensor {
            dtype: t.dtype,
            shape: t.shape,
            begin,
            end,
        });
    }
    tensors.sort_by_key(|t| (t.begin, t.end));
    for pair in tensors.windows(2) {
        if pair[0].end > pair[1].begin {
            return Err("two tensors overlap".to_string());
        }
    }
    Ok(Header {
        header_bytes: 8 + n,
        tensors,
    })
}

/// The bytes of a tensor, given a header returned by [`parse_header`] for the same file.
pub fn tensor_bytes<'a>(file: &'a [u8], h: &Header, t: &Tensor) -> Option<&'a [u8]> {
    let start = usize::try_from(h.header_bytes.checked_add(t.begin)?).ok()?;
    let end = usize::try_from(h.header_bytes.checked_add(t.end)?).ok()?;
    file.get(start..end)
}

// ---------------------------------------------------------------------------------------------
// Byte planes and rotation

/// Split `data` into `width` planes: plane `i` holds byte `i` of every element.
pub fn split_planes(data: &[u8], width: usize) -> Result<Vec<Vec<u8>>> {
    if width == 0 || !data.len().is_multiple_of(width) {
        bail!(
            "{} bytes are not a whole number of {width}-byte elements",
            data.len()
        );
    }
    Ok((0..width)
        .map(|i| data.iter().skip(i).step_by(width).copied().collect())
        .collect())
}

/// Interleave planes back into element order; the inverse of [`split_planes`].
pub fn merge_planes(planes: &[Vec<u8>]) -> Result<Vec<u8>> {
    let Some(first) = planes.first() else {
        return Ok(Vec::new());
    };
    if planes.iter().any(|p| p.len() != first.len()) {
        bail!("planes differ in length");
    }
    let width = planes.len();
    let mut out = vec![0u8; first.len() * width];
    for (i, plane) in planes.iter().enumerate() {
        for (e, b) in plane.iter().enumerate() {
            out[e * width + i] = *b;
        }
    }
    Ok(out)
}

/// Rotate every `width`-byte little-endian element left by one bit.
pub fn rotate_left1(data: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 || !data.len().is_multiple_of(width) {
        bail!(
            "{} bytes are not a whole number of {width}-byte elements",
            data.len()
        );
    }
    let mut out = Vec::with_capacity(data.len());
    for el in data.chunks_exact(width) {
        for i in 0..width {
            let below = if i == 0 { el[width - 1] } else { el[i - 1] };
            out.push((el[i] << 1) | (below >> 7));
        }
    }
    Ok(out)
}

/// Rotate every `width`-byte little-endian element right by one bit; the inverse of
/// [`rotate_left1`].
pub fn rotate_right1(data: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 || !data.len().is_multiple_of(width) {
        bail!(
            "{} bytes are not a whole number of {width}-byte elements",
            data.len()
        );
    }
    let mut out = Vec::with_capacity(data.len());
    for el in data.chunks_exact(width) {
        for i in 0..width {
            let above = if i + 1 == width { el[0] } else { el[i + 1] };
            out.push((el[i] >> 1) | (above << 7));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Measuring

#[derive(Default)]
struct PlanesAcc {
    planes: Vec<u64>,
    compress_s: f64,
    decompress_s: f64,
}

#[derive(Default)]
struct DtypeAcc {
    element_bytes: u32,
    tensors: u64,
    original: u64,
    plain: Whole,
    byte: PlanesAcc,
    rotated: PlanesAcc,
}

/// Compress the planes of `data` (after `rotate`, when set), then decompress and merge them back,
/// each step timed; check that the result equals `data`.
fn measure_planes(
    zc: &mut ZstdContext,
    data: &[u8],
    width: usize,
    rotate: bool,
    acc: &mut PlanesAcc,
) -> Result<()> {
    let (planes_c, t_c) = timed(|| -> Result<Vec<Vec<u8>>> {
        let rotated;
        let src = if rotate {
            rotated = rotate_left1(data, width)?;
            &rotated[..]
        } else {
            data
        };
        split_planes(src, width)?
            .iter()
            .map(|p| zc.compress(p))
            .collect()
    });
    let planes_c = planes_c?;
    let plane_len = data.len() / width;
    let (merged, t_d) = timed(|| -> Result<Vec<u8>> {
        let planes = planes_c
            .iter()
            .map(|c| zc.decompress(c, plane_len))
            .collect::<Result<Vec<_>>>()?;
        let merged = merge_planes(&planes)?;
        if rotate {
            rotate_right1(&merged, width)
        } else {
            Ok(merged)
        }
    });
    if merged? != data {
        bail!(
            "reversing the {} variant did not reproduce a tensor's bytes",
            if rotate {
                "rotated-plane"
            } else {
                "byte-plane"
            }
        );
    }
    if acc.planes.len() != width {
        acc.planes = vec![0; width];
    }
    for (slot, c) in acc.planes.iter_mut().zip(&planes_c) {
        *slot += c.len() as u64;
    }
    acc.compress_s += t_c;
    acc.decompress_s += t_d;
    Ok(())
}

fn planes_record(a: PlanesAcc) -> Planes {
    Planes {
        compressed_bytes: a.planes.iter().sum(),
        plane_compressed_bytes: a.planes,
        compress_seconds: a.compress_s,
        decompress_seconds: a.decompress_s,
    }
}

/// Measure one file held in memory. `Ok(Err(reason))`: not a usable safetensors file;
/// `Err`: the measurement itself failed (compression error, a variant that does not reverse).
fn analyze(
    zc: &mut ZstdContext,
    file: &[u8],
    index: u32,
    path: Option<String>,
) -> Result<std::result::Result<FileRecord, String>> {
    let header = match parse_header(file) {
        Ok(h) => h,
        Err(reason) => return Ok(Err(reason)),
    };
    let whole = zc.compress(file)?.len() as u64;
    let mut other_tensors = 0u64;
    let mut other_bytes = 0u64;
    let mut acc: BTreeMap<String, DtypeAcc> = BTreeMap::new();
    for t in &header.tensors {
        let data = tensor_bytes(file, &header, t)
            .ok_or_else(|| anyhow!("a tensor lies outside the file after the header check"))?;
        if !is_float(&t.dtype) {
            other_tensors += 1;
            other_bytes += data.len() as u64;
            continue;
        }
        let width = element_width(&t.dtype).unwrap_or(0) as usize;
        let a = acc.entry(t.dtype.clone()).or_default();
        a.element_bytes = width as u32;
        a.tensors += 1;
        a.original += data.len() as u64;
        if a.byte.planes.len() != width {
            a.byte.planes = vec![0; width];
            a.rotated.planes = vec![0; width];
        }
        if data.is_empty() {
            continue;
        }
        // Each of these runs alone, on data in memory.
        let (c, t_c) = timed(|| zc.compress(data));
        let c = c?;
        let (d, t_d) = timed(|| zc.decompress(&c, data.len()));
        if d? != data {
            bail!("a tensor did not decompress to its own bytes");
        }
        a.plain.compressed_bytes += c.len() as u64;
        a.plain.compress_seconds += t_c;
        a.plain.decompress_seconds += t_d;
        measure_planes(zc, data, width, false, &mut a.byte)?;
        measure_planes(zc, data, width, true, &mut a.rotated)?;
    }
    let dtypes = acc
        .into_iter()
        .map(|(dtype, a)| DtypeRecord {
            dtype,
            element_bytes: a.element_bytes,
            tensors: a.tensors,
            original_bytes: a.original,
            plain: a.plain,
            byte_planes: planes_record(a.byte),
            rotated_planes: planes_record(a.rotated),
        })
        .collect();
    Ok(Ok(FileRecord {
        index,
        path,
        bytes: file.len() as u64,
        header_bytes: header.header_bytes,
        tensors: header.tensors.len() as u64,
        whole_file_compressed_bytes: whole,
        other_tensors,
        other_bytes,
        dtypes,
    }))
}

pub fn run(ctx: &Ctx<'_>) -> Result<Output<Data>> {
    let settings = ZstdSettings::level19();
    let mut data = Data {
        class: CLASS.to_string(),
        manifest_files: 0,
        zstd: settings.clone(),
        files: Vec::new(),
        not_parsed: Vec::new(),
    };
    let mut notes = Vec::new();
    match ctx.class_files(CLASS) {
        None => notes.push(format!(
            "the corpus has no class `{CLASS}`: nothing was measured"
        )),
        Some(files) => {
            data.manifest_files = u32::try_from(files.len())?;
            let mut zc = settings.context()?;
            for (i, f) in files.iter().enumerate() {
                let index = u32::try_from(i)?;
                let bytes = ctx.read_file(CLASS, f)?;
                match analyze(&mut zc, &bytes, index, ctx.label(f))? {
                    Ok(rec) => data.files.push(rec),
                    Err(reason) => data.not_parsed.push(NotParsed {
                        index,
                        path: ctx.label(f),
                        bytes: bytes.len() as u64,
                        reason,
                    }),
                }
            }
            if data.files.is_empty() {
                notes.push(
                    "no file of the class could be parsed as safetensors: this corpus profile \
                     cannot exercise this probe"
                        .to_string(),
                );
            } else if data.files.iter().all(|f| f.dtypes.is_empty()) {
                notes.push("no floating-point tensor was found".to_string());
            }
        }
    }
    let mut out = Output::new(data, settings.threads).with_zstd();
    out.notes = notes;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Validation rules and the table

fn check_planes(p: &mut Vec<String>, at: &str, name: &str, v: &Planes, width: u32) {
    let at = format!("{at}/{name}");
    if v.plane_compressed_bytes.len() != width as usize {
        p.push(format!(
            "{at}/plane_compressed_bytes: {} entries for {width}-byte elements",
            v.plane_compressed_bytes.len()
        ));
    }
    let sum: u64 = v.plane_compressed_bytes.iter().sum();
    if sum != v.compressed_bytes {
        p.push(format!(
            "{at}/compressed_bytes: {} but the planes add up to {sum}",
            v.compressed_bytes
        ));
    }
    for (field, s) in [
        ("compress_seconds", v.compress_seconds),
        ("decompress_seconds", v.decompress_seconds),
    ] {
        if !(s.is_finite() && s >= 0.0) {
            p.push(format!("{at}/{field}: must be a non-negative number"));
        }
    }
}

fn check_path(p: &mut Vec<String>, private: bool, at: &str, path: &Option<String>) {
    if private && path.is_some() {
        p.push(format!("{at}/path: a private corpus carries no file names"));
    }
    if !private && path.is_none() {
        p.push(format!("{at}/path: missing"));
    }
}

/// Consistency rules of `data`, each as `<json pointer>: <message>`.
pub fn check(e: &Envelope<Data>) -> Vec<String> {
    let d = &e.data;
    let mut p = Vec::new();
    if d.class != CLASS {
        p.push(format!("/data/class: `{}` (expected `{CLASS}`)", d.class));
    }
    if d.zstd != ZstdSettings::level19() {
        p.push("/data/zstd: the settings are not the probe's (level 19, window log 27, long-distance matching, 1 thread)".to_string());
    }
    if e.library_threads != 1 {
        p.push("/library_threads: this probe runs zstd on one thread".to_string());
    }
    let private = e.corpus.private;
    // Every manifest index appears exactly once, in `files` or `not_parsed`.
    let mut seen: Vec<u32> = d
        .files
        .iter()
        .map(|f| f.index)
        .chain(d.not_parsed.iter().map(|f| f.index))
        .collect();
    seen.sort_unstable();
    let expected: Vec<u32> = (0..d.manifest_files).collect();
    if seen != expected {
        p.push(format!(
            "/data/manifest_files: {} files are listed in the manifest but the records cover \
             indices {:?}",
            d.manifest_files,
            if seen.len() > 20 {
                &seen[..20]
            } else {
                &seen[..]
            }
        ));
    }
    for (i, f) in d.not_parsed.iter().enumerate() {
        check_path(&mut p, private, &format!("/data/not_parsed/{i}"), &f.path);
        if i > 0 && d.not_parsed[i - 1].index >= f.index {
            p.push(format!(
                "/data/not_parsed/{i}/index: must be in strictly increasing manifest order"
            ));
        }
    }
    for (i, f) in d.files.iter().enumerate() {
        let at = format!("/data/files/{i}");
        check_path(&mut p, private, &at, &f.path);
        if i > 0 && d.files[i - 1].index >= f.index {
            p.push(format!(
                "{at}/index: files must be in strictly increasing manifest order"
            ));
        }
        let dtype_tensors: u64 = f.dtypes.iter().map(|t| t.tensors).sum();
        if dtype_tensors + f.other_tensors != f.tensors {
            p.push(format!(
                "{at}/tensors: {} but the dtype records and other_tensors add up to {}",
                f.tensors,
                dtype_tensors + f.other_tensors
            ));
        }
        let covered: u64 =
            f.header_bytes + f.other_bytes + f.dtypes.iter().map(|t| t.original_bytes).sum::<u64>();
        if covered > f.bytes {
            p.push(format!(
                "{at}/bytes: {} but header, other tensors and float tensors add up to {covered}",
                f.bytes
            ));
        }
        if f.bytes > 0 && f.whole_file_compressed_bytes == 0 {
            p.push(format!(
                "{at}/whole_file_compressed_bytes: zero for a non-empty file"
            ));
        }
        for (j, t) in f.dtypes.iter().enumerate() {
            let at = format!("{at}/dtypes/{j}");
            if !is_float(&t.dtype) || element_width(&t.dtype) != Some(u64::from(t.element_bytes)) {
                p.push(format!(
                    "{at}/dtype: `{}` with element_bytes {} is not a floating-point dtype",
                    t.dtype, t.element_bytes
                ));
            }
            if j > 0 && f.dtypes[j - 1].dtype >= t.dtype {
                p.push(format!(
                    "{at}/dtype: records must be sorted by dtype, once each"
                ));
            }
            check_planes(&mut p, &at, "byte_planes", &t.byte_planes, t.element_bytes);
            check_planes(
                &mut p,
                &at,
                "rotated_planes",
                &t.rotated_planes,
                t.element_bytes,
            );
            if t.element_bytes > 0 && !t.original_bytes.is_multiple_of(u64::from(t.element_bytes)) {
                p.push(format!(
                    "{at}/original_bytes: not a whole number of elements"
                ));
            }
            for (field, s) in [
                ("plain/compress_seconds", t.plain.compress_seconds),
                ("plain/decompress_seconds", t.plain.decompress_seconds),
            ] {
                if !(s.is_finite() && s >= 0.0) {
                    p.push(format!("{at}/{field}: must be a non-negative number"));
                }
            }
        }
    }
    p
}

#[derive(Default)]
struct Totals {
    tensors: u64,
    original: u64,
    plain: Whole,
    byte: Planes,
    rotated: Planes,
}

fn add_planes(a: &mut Planes, b: &Planes) {
    a.compressed_bytes += b.compressed_bytes;
    a.compress_seconds += b.compress_seconds;
    a.decompress_seconds += b.decompress_seconds;
    if a.plane_compressed_bytes.len() < b.plane_compressed_bytes.len() {
        a.plane_compressed_bytes
            .resize(b.plane_compressed_bytes.len(), 0);
    }
    for (x, y) in a
        .plane_compressed_bytes
        .iter_mut()
        .zip(&b.plane_compressed_bytes)
    {
        *x += y;
    }
}

/// `(plain - variant) / plain` as a signed percentage.
fn gain(plain: u64, variant: u64) -> String {
    if plain == 0 {
        "n/a".to_string()
    } else {
        format!(
            "{:+.2}%",
            100.0 * (plain as f64 - variant as f64) / plain as f64
        )
    }
}

fn sized(bytes: u64, of: u64) -> String {
    format!("{bytes} ({})", pct(bytes, of))
}

/// The table, as a pure function of the parsed JSON.
pub fn render(e: &Envelope<Data>) -> String {
    let d = &e.data;
    let mut s = md_header(e);
    s.push_str(&format!(
        "Class `{}`: {} file(s) parsed as safetensors, {} not parsed. zstd level {}, window log {}, \
         long-distance matching {}, {} thread(s), one compressor and decompressor reused. \
         Variants: plain = tensor bytes as they are; byte planes = bytes split by position in the \
         element (plane 0 is the lowest byte); rotated planes = each element rotated left by one \
         bit first, then split (for BF16 and F32 the whole exponent is in the top plane). Gain = \
         (plain - variant) / plain; negative means the variant is larger. Speeds are MB/s of the \
         tensor bytes (10^6 bytes per second), each step timed alone.\n\n",
        d.class,
        d.files.len(),
        d.not_parsed.len(),
        d.zstd.level,
        d.zstd
            .window_log
            .map_or("default".to_string(), |w| w.to_string()),
        if d.zstd.long_distance_matching { "on" } else { "off" },
        d.zstd.threads
    ));

    if d.files.is_empty() {
        s.push_str(
            "This corpus profile cannot exercise this probe: no file of the class could be \
             parsed as safetensors.\n\n",
        );
    }

    let mut by_dtype: BTreeMap<&str, Totals> = BTreeMap::new();
    for f in &d.files {
        for t in &f.dtypes {
            let a = by_dtype.entry(t.dtype.as_str()).or_default();
            a.tensors += t.tensors;
            a.original += t.original_bytes;
            a.plain.compressed_bytes += t.plain.compressed_bytes;
            a.plain.compress_seconds += t.plain.compress_seconds;
            a.plain.decompress_seconds += t.plain.decompress_seconds;
            add_planes(&mut a.byte, &t.byte_planes);
            add_planes(&mut a.rotated, &t.rotated_planes);
        }
    }

    if !by_dtype.is_empty() {
        s.push_str("## Sizes by dtype, all files\n\n");
        let rows: Vec<Vec<String>> = by_dtype
            .iter()
            .map(|(dtype, a)| {
                vec![
                    dtype.to_string(),
                    a.tensors.to_string(),
                    a.original.to_string(),
                    sized(a.plain.compressed_bytes, a.original),
                    sized(a.byte.compressed_bytes, a.original),
                    gain(a.plain.compressed_bytes, a.byte.compressed_bytes),
                    sized(a.rotated.compressed_bytes, a.original),
                    gain(a.plain.compressed_bytes, a.rotated.compressed_bytes),
                ]
            })
            .collect();
        s.push_str(&md_table(
            &[
                "dtype",
                "tensors",
                "original bytes",
                "plain zstd bytes (of original)",
                "byte-plane bytes (of original)",
                "byte-plane gain",
                "rotated-plane bytes (of original)",
                "rotated-plane gain",
            ],
            &rows,
        ));
        s.push('\n');

        s.push_str("## Speed by dtype, all files (MB/s)\n\n");
        let rows: Vec<Vec<String>> = by_dtype
            .iter()
            .map(|(dtype, a)| {
                vec![
                    dtype.to_string(),
                    mbps(a.original, a.plain.compress_seconds),
                    mbps(a.original, a.plain.decompress_seconds),
                    mbps(a.original, a.byte.compress_seconds),
                    mbps(a.original, a.byte.decompress_seconds),
                    mbps(a.original, a.rotated.compress_seconds),
                    mbps(a.original, a.rotated.decompress_seconds),
                ]
            })
            .collect();
        s.push_str(&md_table(
            &[
                "dtype",
                "plain compress",
                "plain decompress",
                "byte-plane split+compress",
                "byte-plane decompress+merge",
                "rotated split+compress",
                "rotated decompress+merge",
            ],
            &rows,
        ));
        s.push('\n');

        s.push_str("## Planes, all files\n\n");
        let mut prow = Vec::new();
        for (dtype, a) in &by_dtype {
            let width = a.byte.plane_compressed_bytes.len().max(1) as u64;
            let plane_bytes = a.original / width;
            for i in 0..a.byte.plane_compressed_bytes.len() {
                let b = a.byte.plane_compressed_bytes[i];
                let r = a
                    .rotated
                    .plane_compressed_bytes
                    .get(i)
                    .copied()
                    .unwrap_or(0);
                prow.push(vec![
                    dtype.to_string(),
                    i.to_string(),
                    plane_bytes.to_string(),
                    sized(b, plane_bytes),
                    sized(r, plane_bytes),
                ]);
            }
        }
        s.push_str(&md_table(
            &[
                "dtype",
                "plane",
                "plane bytes",
                "byte-plane compressed (of plane)",
                "rotated-plane compressed (of plane)",
            ],
            &prow,
        ));
        s.push('\n');
    }

    s.push_str("## By file\n\n");
    let mut frows = Vec::new();
    for f in &d.files {
        let name = f.path.clone().unwrap_or_else(|| format!("#{}", f.index));
        let float: u64 = f.dtypes.iter().map(|t| t.original_bytes).sum();
        let plain: u64 = f.dtypes.iter().map(|t| t.plain.compressed_bytes).sum();
        let byte: u64 = f
            .dtypes
            .iter()
            .map(|t| t.byte_planes.compressed_bytes)
            .sum();
        let rot: u64 = f
            .dtypes
            .iter()
            .map(|t| t.rotated_planes.compressed_bytes)
            .sum();
        frows.push(vec![
            name,
            f.bytes.to_string(),
            f.tensors.to_string(),
            sized(f.whole_file_compressed_bytes, f.bytes),
            float.to_string(),
            pct(plain, float),
            pct(byte, float),
            gain(plain, byte),
            pct(rot, float),
            gain(plain, rot),
        ]);
    }
    if frows.is_empty() {
        s.push_str("No file was parsed.\n\n");
    } else {
        s.push_str(&md_table(
            &[
                "file",
                "bytes",
                "tensors",
                "whole-file zstd bytes (of file)",
                "float tensor bytes",
                "plain / float",
                "byte planes / float",
                "byte-plane gain",
                "rotated planes / float",
                "rotated-plane gain",
            ],
            &frows,
        ));
        s.push('\n');
    }

    s.push_str("## Not parsed\n\n");
    if d.not_parsed.is_empty() {
        s.push_str("None.\n");
    } else {
        let nrows: Vec<Vec<String>> = d
            .not_parsed
            .iter()
            .map(|n| {
                vec![
                    n.path.clone().unwrap_or_else(|| format!("#{}", n.index)),
                    n.bytes.to_string(),
                    n.reason.clone(),
                ]
            })
            .collect();
        s.push_str(&md_table(&["file", "bytes", "reason"], &nrows));
    }
    s
}

// ---------------------------------------------------------------------------------------------
// Tests

/// A safetensors file for tests: `(name, dtype, shape, data)` tensors in the given order.
#[cfg(test)]
pub(crate) fn build_safetensors(tensors: &[(&str, &str, Vec<u64>, Vec<u8>)]) -> Vec<u8> {
    let mut header = serde_json::Map::new();
    header.insert("__metadata__".into(), serde_json::json!({"format": "pt"}));
    let mut offset = 0u64;
    let mut body = Vec::new();
    for (name, dtype, shape, data) in tensors {
        header.insert(
            (*name).to_string(),
            serde_json::json!({
                "dtype": dtype,
                "shape": shape,
                "data_offsets": [offset, offset + data.len() as u64],
            }),
        );
        offset += data.len() as u64;
        body.extend_from_slice(data);
    }
    let json = serde_json::to_vec(&serde_json::Value::Object(header)).unwrap_or_default();
    let mut out = (json.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(&json);
    out.extend_from_slice(&body);
    out
}

/// Smooth-ish deterministic float data of `elements` elements of `width` bytes.
#[cfg(test)]
pub(crate) fn sample_floats(elements: usize, width: usize) -> Vec<u8> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut out = Vec::with_capacity(elements * width);
    for i in 0..elements {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let v = (((i % 97) as f32) * 0.01 + ((state >> 40) as f32) * 1e-9).sin();
        match width {
            2 => out.extend_from_slice(&((v.to_bits() >> 16) as u16).to_le_bytes()),
            4 => out.extend_from_slice(&v.to_le_bytes()),
            _ => out.extend_from_slice(&f64::from(v).to_le_bytes()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(tensors: &[(&str, &str, Vec<u64>, Vec<u8>)]) -> Vec<u8> {
        build_safetensors(tensors)
    }

    fn with_header(json: &str, data: &[u8]) -> Vec<u8> {
        let mut out = (json.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(json.as_bytes());
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn a_valid_header_is_parsed_and_sorted_by_offset() {
        let file = st(&[
            ("b", "F32", vec![2, 3], vec![1; 24]),
            ("a", "BF16", vec![5], vec![2; 10]),
            ("c", "I64", vec![1], vec![3; 8]),
        ]);
        let h = parse_header(&file).expect("valid");
        assert_eq!(h.tensors.len(), 3);
        assert_eq!(h.tensors[0].dtype, "F32");
        assert_eq!(h.tensors[1].begin, 24);
        assert_eq!(h.tensors[2].end, 42);
        let t = &h.tensors[1];
        assert_eq!(tensor_bytes(&file, &h, t), Some(&[2u8; 10][..]));
        assert_eq!(h.header_bytes + 42, file.len() as u64);
    }

    #[test]
    fn malformed_headers_are_rejected_without_panicking() {
        let ok = st(&[("a", "F32", vec![2], vec![0; 8])]);
        for cut in 0..ok.len() {
            assert!(
                parse_header(&ok[..cut]).is_err(),
                "cut at {cut} was accepted"
            );
        }
        let mut huge = u64::MAX.to_le_bytes().to_vec();
        huge.extend_from_slice(b"{}");
        assert!(parse_header(&huge).expect_err("huge").contains("exceeds"));
        let mut big = (MAX_HEADER_BYTES + 1).to_le_bytes().to_vec();
        big.extend_from_slice(b"{}");
        assert!(parse_header(&big).is_err());
        let mut beyond = 100u64.to_le_bytes().to_vec();
        beyond.extend_from_slice(b"{}");
        assert!(parse_header(&beyond)
            .expect_err("beyond")
            .contains("exceeds"));
        assert!(parse_header(&with_header("nope", b"")).is_err());
        assert!(parse_header(&with_header("[]", b"")).is_err());
        assert!(parse_header(&with_header(r#"{"a":{"dtype":"F32"}}"#, b"")).is_err());
        let out_of_range = r#"{"a":{"dtype":"F32","shape":[4],"data_offsets":[0,16]}}"#;
        assert!(parse_header(&with_header(out_of_range, &[0; 8]))
            .expect_err("range")
            .contains("beyond"));
        let reversed = r#"{"a":{"dtype":"U8","shape":[0],"data_offsets":[4,2]}}"#;
        assert!(parse_header(&with_header(reversed, &[0; 8])).is_err());
        let mismatch = r#"{"a":{"dtype":"F32","shape":[3],"data_offsets":[0,8]}}"#;
        assert!(parse_header(&with_header(mismatch, &[0; 8]))
            .expect_err("shape")
            .contains("shape"));
        let overflow =
            r#"{"a":{"dtype":"F32","shape":[4294967296,4294967296],"data_offsets":[0,8]}}"#;
        assert!(parse_header(&with_header(overflow, &[0; 8])).is_err());
    }

    #[test]
    fn reasons_never_quote_the_file() {
        let secret = "SECRETVALUE";
        let bad: [String; 6] = [
            format!(r#"{{"a":{{"dtype":5,"shape":"{secret}","data_offsets":[0,0]}}}}"#),
            format!(
                r#"{{"{secret}":{{"dtype":"F32","shape":[1],"data_offsets":["{secret}",1]}}}}"#
            ),
            format!(r#"{{"{secret}": nope}}"#),
            format!(r#"{{"{secret}":"#),
            format!(r#"["{secret}"]"#),
            format!(r#"{{"{secret}":{{"dtype":"{secret}","shape":[1],"data_offsets":[5,1]}}}}"#),
        ];
        for json in &bad {
            let reason = parse_header(&with_header(json, &[0; 8])).expect_err("rejected");
            assert!(!reason.contains(secret), "{reason}");
            assert!(
                !reason.contains("invalid type"),
                "serde text leaked: {reason}"
            );
        }
        // An unknown dtype is accepted, and its name is not recorded in any reason.
        let unknown =
            format!(r#"{{"{secret}":{{"dtype":"{secret}","shape":[1],"data_offsets":[0,3]}}}}"#);
        assert!(parse_header(&with_header(&unknown, &[0; 8])).is_ok());
    }

    #[test]
    fn a_zip_file_is_named_as_a_pytorch_checkpoint() {
        let reason = parse_header(b"PK\x03\x04 and then some bytes").expect_err("zip");
        assert_eq!(reason, "not safetensors: ZIP archive (PyTorch checkpoint)");
    }

    #[test]
    fn overlapping_tensors_are_rejected_and_touching_ones_are_not() {
        let overlap = r#"{"a":{"dtype":"U8","shape":[6],"data_offsets":[0,6]},"b":{"dtype":"U8","shape":[6],"data_offsets":[4,10]}}"#;
        assert!(parse_header(&with_header(overlap, &[0; 10]))
            .expect_err("overlap")
            .contains("overlap"));
        let touching = r#"{"a":{"dtype":"U8","shape":[4],"data_offsets":[0,4]},"b":{"dtype":"U8","shape":[6],"data_offsets":[4,10]},"z":{"dtype":"U8","shape":[0],"data_offsets":[4,4]}}"#;
        assert!(parse_header(&with_header(touching, &[0; 10])).is_ok());
    }

    #[test]
    fn planes_split_and_merge_for_every_element_width() {
        for width in 1..=8usize {
            for elements in [0usize, 1, 2, 7, 1000] {
                let data: Vec<u8> = (0..elements * width).map(|i| (i * 31 + 7) as u8).collect();
                let planes = split_planes(&data, width).expect("split");
                assert_eq!(planes.len(), width);
                for (i, plane) in planes.iter().enumerate() {
                    assert_eq!(plane.len(), elements);
                    if elements > 0 {
                        assert_eq!(plane[0], data[i], "plane {i} starts with byte {i}");
                    }
                }
                assert_eq!(merge_planes(&planes).expect("merge"), data, "width {width}");
            }
        }
        assert!(split_planes(&[1, 2, 3], 2).is_err());
        assert!(split_planes(&[1, 2], 0).is_err());
        assert!(merge_planes(&[vec![1], vec![1, 2]]).is_err());
    }

    #[test]
    fn rotation_round_trips_and_puts_the_exponent_in_the_top_byte() {
        for width in 1..=8usize {
            for elements in [0usize, 1, 5, 300] {
                let data: Vec<u8> = (0..elements * width).map(|i| (i * 37 + 11) as u8).collect();
                let r = rotate_left1(&data, width).expect("rotl");
                assert_eq!(
                    rotate_right1(&r, width).expect("rotr"),
                    data,
                    "width {width}"
                );
            }
        }
        // bf16 1.0 = 0x3F80, little endian [0x80, 0x3F]; rotated left by one bit it is 0x7F00:
        // the exponent (127) is the whole top byte.
        assert_eq!(rotate_left1(&[0x80, 0x3F], 2).expect("rot"), [0x00, 0x7F]);
        // bf16 -1.0 = 0xBF80 -> 0x7F01: the sign bit lands in the lowest bit.
        assert_eq!(rotate_left1(&[0x80, 0xBF], 2).expect("rot"), [0x01, 0x7F]);
        // f32 1.0 = 0x3F800000 -> 0x7F000000.
        assert_eq!(
            rotate_left1(&[0, 0, 0x80, 0x3F], 4).expect("rot"),
            [0, 0, 0, 0x7F]
        );
        assert!(rotate_left1(&[1, 2, 3], 2).is_err());
        assert!(rotate_right1(&[1, 2, 3], 2).is_err());
    }

    #[test]
    fn analysis_measures_three_variants_and_counts_the_rest() {
        let file = st(&[
            ("w", "F32", vec![100, 30], sample_floats(3000, 4)),
            ("h", "F16", vec![500], sample_floats(500, 2)),
            ("e", "BF16", vec![0], vec![]),
            ("ids", "I64", vec![4], vec![9; 32]),
        ]);
        let mut zc = ZstdSettings::level19().context().expect("ctx");
        let rec = analyze(&mut zc, &file, 3, Some("c/m.safetensors".into()))
            .expect("measured")
            .expect("parsed");
        assert_eq!(rec.index, 3);
        assert_eq!(rec.tensors, 4);
        assert_eq!((rec.other_tensors, rec.other_bytes), (1, 32));
        let names: Vec<&str> = rec.dtypes.iter().map(|d| d.dtype.as_str()).collect();
        assert_eq!(names, ["BF16", "F16", "F32"]);
        let f32r = &rec.dtypes[2];
        assert_eq!((f32r.element_bytes, f32r.original_bytes), (4, 12_000));
        for v in [&f32r.byte_planes, &f32r.rotated_planes] {
            assert_eq!(v.plane_compressed_bytes.len(), 4);
            assert_eq!(
                v.plane_compressed_bytes.iter().sum::<u64>(),
                v.compressed_bytes
            );
            assert!(v.compressed_bytes > 0);
        }
        assert!(f32r.plain.compressed_bytes > 0);
        let empty = &rec.dtypes[0];
        assert_eq!(
            (
                empty.tensors,
                empty.original_bytes,
                empty.plain.compressed_bytes
            ),
            (1, 0, 0)
        );
        assert_eq!(empty.byte_planes.plane_compressed_bytes, [0, 0]);
        assert!(rec.whole_file_compressed_bytes > 0);
        let np = analyze(&mut zc, b"just some text", 0, None).expect("no failure");
        assert!(np.is_err());
    }
}
