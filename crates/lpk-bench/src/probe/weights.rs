//! `probe weights` (PLAN P0-4): byte-plane splitting of floating-point model weights.
//!
//! Class `model-weights`. Every file is tried as a safetensors file: an 8-byte little-endian
//! header length, a JSON header mapping tensor name to dtype, shape and `data_offsets`, then the
//! data. For each floating-point tensor (F16, BF16, F32, F64) the bytes are split into planes
//! (plane `i` holds byte `i` of every element, plane 0 being the lowest byte), each plane is
//! compressed with zstd level 19, and the total is compared with zstd level 19 on the tensor's
//! bytes unsplit. The whole file is also compressed with level 19. Merging the planes must
//! reproduce the tensor bytes exactly, or the probe stops with an error.
//!
//! Timing: zstd runs single-threaded; "split + compress" and "decompress + merge" are each timed
//! alone, per tensor, on data already in memory. The size-only compressions (plain tensors and
//! the whole file) run before, in parallel over `--threads`.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

use super::{mbps, md_header, md_table, par_map, pct, timed, Ctx, Envelope, Output};

pub const CLASS: &str = "model-weights";
pub const ZSTD_LEVEL: i32 = 19;
/// safetensors refuses headers larger than this.
const MAX_HEADER_BYTES: u64 = 100_000_000;

// ---------------------------------------------------------------------------------------------
// The result

/// The probe's own part of the result file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub class: String,
    pub zstd_level: i32,
    /// Threads zstd used inside every timed section.
    pub library_threads: u32,
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
    /// zstd level 19 on the whole file.
    pub whole_file_compressed_bytes: u64,
    /// Tensors of other dtypes (integers, booleans, 8-bit floats) and their bytes.
    pub other_tensors: u64,
    pub other_bytes: u64,
    /// One record per floating-point dtype present, sorted by dtype name.
    pub dtypes: Vec<DtypeRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DtypeRecord {
    pub dtype: String,
    pub element_bytes: u32,
    pub tensors: u64,
    pub original_bytes: u64,
    /// Sum over tensors of zstd level 19 on the tensor's bytes.
    pub plain_compressed_bytes: u64,
    /// Sum over tensors and planes of zstd level 19 on a plane.
    pub split_compressed_bytes: u64,
    /// `split_compressed_bytes` by plane (index 0 = lowest byte of each element).
    pub plane_compressed_bytes: Vec<u64>,
    /// Splitting plus compressing all planes, summed over tensors.
    pub split_compress_seconds: f64,
    /// Decompressing all planes plus merging them, summed over tensors.
    pub decompress_merge_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotParsed {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub bytes: u64,
    /// Why the file was not parsed; never contains file or tensor names.
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

/// Parse and check the header of a safetensors file held in memory. Never panics; the error
/// text says what is wrong without quoting anything from the file.
pub fn parse_header(file: &[u8]) -> std::result::Result<Header, String> {
    let total = file.len() as u64;
    let Some(len_bytes) = file.get(..8) else {
        return Err("shorter than the 8-byte header length".to_string());
    };
    let mut raw = [0u8; 8];
    raw.copy_from_slice(len_bytes);
    let n = u64::from_le_bytes(raw);
    if n > MAX_HEADER_BYTES {
        return Err(format!(
            "declared header length {n} exceeds the limit of {MAX_HEADER_BYTES}"
        ));
    }
    if n > total - 8 {
        return Err(format!(
            "declared header length {n} exceeds the {} bytes after the length field",
            total - 8
        ));
    }
    let json = &file[8..8 + n as usize];
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(json).map_err(|e| format!("header is not a JSON object: {e}"))?;
    let data_len = total - 8 - n;
    let mut tensors = Vec::new();
    for (name, value) in map {
        if name == "__metadata__" {
            continue;
        }
        let t: RawTensor = serde_json::from_value(value)
            .map_err(|e| format!("a tensor entry is malformed: {e}"))?;
        let [begin, end] = t.data_offsets;
        if begin > end {
            return Err("a tensor has begin offset after its end offset".to_string());
        }
        if end > data_len {
            return Err(format!(
                "a tensor ends at {end}, beyond the {data_len}-byte data area"
            ));
        }
        if let Some(w) = element_width(&t.dtype) {
            let elements = t
                .shape
                .iter()
                .try_fold(1u64, |acc, d| acc.checked_mul(*d))
                .ok_or_else(|| "a tensor's shape overflows".to_string())?;
            if elements.checked_mul(w) != Some(end - begin) {
                return Err(format!(
                    "a {} tensor's data size {} does not match its shape",
                    t.dtype,
                    end - begin
                ));
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
// Byte planes

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

fn zstd_len(data: &[u8]) -> Result<u64> {
    if data.is_empty() {
        return Ok(0);
    }
    Ok(zstd::bulk::compress(data, ZSTD_LEVEL)?.len() as u64)
}

// ---------------------------------------------------------------------------------------------
// Measuring

#[derive(Default)]
struct DtypeAcc {
    element_bytes: u32,
    tensors: u64,
    original: u64,
    plain: u64,
    split_planes: Vec<u64>,
    split_seconds: f64,
    merge_seconds: f64,
}

/// Measure one file held in memory. `Ok(Err(reason))`: not a usable safetensors file;
/// `Err`: the measurement itself failed (compression error, planes that do not merge back).
fn analyze(
    file: &[u8],
    index: u32,
    path: Option<String>,
    threads: usize,
) -> Result<std::result::Result<FileRecord, String>> {
    let header = match parse_header(file) {
        Ok(h) => h,
        Err(reason) => return Ok(Err(reason)),
    };
    let mut slices: Vec<(&Tensor, &[u8])> = Vec::new();
    let mut other_tensors = 0u64;
    let mut other_bytes = 0u64;
    for t in &header.tensors {
        let data = tensor_bytes(file, &header, t)
            .ok_or_else(|| anyhow!("a tensor lies outside the file after the header check"))?;
        if is_float(&t.dtype) {
            slices.push((t, data));
        } else {
            other_tensors += 1;
            other_bytes += data.len() as u64;
        }
    }

    // Size-only compressions, in parallel: the whole file first, then every float tensor.
    let mut jobs: Vec<&[u8]> = vec![file];
    jobs.extend(slices.iter().map(|(_, d)| *d));
    let sizes = par_map(&jobs, threads, |_, d| zstd_len(d));
    let mut sizes = sizes.into_iter();
    let whole = sizes.next().ok_or_else(|| anyhow!("no job result"))??;

    let mut acc: BTreeMap<String, DtypeAcc> = BTreeMap::new();
    for ((t, data), plain) in slices.iter().zip(sizes) {
        let plain = plain?;
        let width = element_width(&t.dtype).unwrap_or(0) as usize;
        let a = acc.entry(t.dtype.clone()).or_default();
        a.element_bytes = width as u32;
        a.tensors += 1;
        a.original += data.len() as u64;
        a.plain += plain;
        if a.split_planes.len() != width {
            a.split_planes = vec![0; width];
        }
        if data.is_empty() {
            continue;
        }
        // Timed alone: split and compress every plane.
        let (planes_c, t_split) = timed(|| -> Result<Vec<Vec<u8>>> {
            split_planes(data, width)?
                .iter()
                .map(|p| Ok(zstd::bulk::compress(p, ZSTD_LEVEL)?))
                .collect()
        });
        let planes_c = planes_c?;
        // Timed alone: decompress every plane and merge.
        let plane_len = data.len() / width;
        let (merged, t_merge) = timed(|| -> Result<Vec<u8>> {
            let planes = planes_c
                .iter()
                .map(|c| Ok(zstd::bulk::decompress(c, plane_len)?))
                .collect::<Result<Vec<_>>>()?;
            merge_planes(&planes)
        });
        if merged? != *data {
            bail!(
                "merging the decompressed planes of a {} tensor did not reproduce its bytes",
                t.dtype
            );
        }
        for (slot, c) in a.split_planes.iter_mut().zip(&planes_c) {
            *slot += c.len() as u64;
        }
        a.split_seconds += t_split;
        a.merge_seconds += t_merge;
    }
    let dtypes = acc
        .into_iter()
        .map(|(dtype, a)| DtypeRecord {
            dtype,
            element_bytes: a.element_bytes,
            tensors: a.tensors,
            original_bytes: a.original,
            plain_compressed_bytes: a.plain,
            split_compressed_bytes: a.split_planes.iter().sum(),
            plane_compressed_bytes: a.split_planes,
            split_compress_seconds: a.split_seconds,
            decompress_merge_seconds: a.merge_seconds,
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
    let mut notes = Vec::new();
    let mut data = Data {
        class: CLASS.to_string(),
        zstd_level: ZSTD_LEVEL,
        library_threads: 1,
        files: Vec::new(),
        not_parsed: Vec::new(),
    };
    match ctx.class_files(CLASS) {
        None => notes.push(format!(
            "the corpus has no class `{CLASS}`: nothing was measured"
        )),
        Some(files) => {
            for (i, f) in files.iter().enumerate() {
                let index = u32::try_from(i)?;
                let bytes = ctx.read_file(CLASS, f)?;
                match analyze(&bytes, index, ctx.label(f), ctx.threads as usize)? {
                    Ok(rec) => data.files.push(rec),
                    Err(reason) => data.not_parsed.push(NotParsed {
                        index,
                        path: ctx.label(f),
                        bytes: bytes.len() as u64,
                        reason,
                    }),
                }
            }
            if data.files.iter().all(|f| f.dtypes.is_empty()) {
                notes.push(
                    "no floating-point tensor was found: the table has no dtype rows".to_string(),
                );
            }
        }
    }
    let libraries = [("libzstd", zstd::zstd_safe::version_string())]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Ok(Output {
        data,
        libraries,
        notes,
    })
}

// ---------------------------------------------------------------------------------------------
// Validation rules and the table

/// Consistency rules of `data`, each as `<json pointer>: <message>`.
pub fn check(e: &Envelope<Data>) -> Vec<String> {
    let d = &e.data;
    let mut p = Vec::new();
    if d.class != CLASS {
        p.push(format!("/data/class: `{}` (expected `{CLASS}`)", d.class));
    }
    if d.zstd_level != ZSTD_LEVEL {
        p.push(format!(
            "/data/zstd_level: {} (expected {ZSTD_LEVEL})",
            d.zstd_level
        ));
    }
    if d.library_threads == 0 {
        p.push("/data/library_threads: must be at least 1".to_string());
    }
    let private = e.corpus.private;
    let mut indices: Vec<(u32, String)> = Vec::new();
    fn check_path(p: &mut Vec<String>, private: bool, at: &str, path: &Option<String>) {
        if private && path.is_some() {
            p.push(format!("{at}/path: a private corpus carries no file names"));
        }
        if !private && path.is_none() {
            p.push(format!("{at}/path: missing"));
        }
    }
    for (i, f) in d.files.iter().enumerate() {
        check_path(&mut p, private, &format!("/data/files/{i}"), &f.path);
        indices.push((f.index, format!("/data/files/{i}")));
    }
    for (i, f) in d.not_parsed.iter().enumerate() {
        check_path(&mut p, private, &format!("/data/not_parsed/{i}"), &f.path);
        indices.push((f.index, format!("/data/not_parsed/{i}")));
    }
    for (i, f) in d.files.iter().enumerate() {
        let at = format!("/data/files/{i}");
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
            if t.plane_compressed_bytes.len() != t.element_bytes as usize {
                p.push(format!(
                    "{at}/plane_compressed_bytes: {} entries for {}-byte elements",
                    t.plane_compressed_bytes.len(),
                    t.element_bytes
                ));
            }
            let planes: u64 = t.plane_compressed_bytes.iter().sum();
            if planes != t.split_compressed_bytes {
                p.push(format!(
                    "{at}/split_compressed_bytes: {} but the planes add up to {planes}",
                    t.split_compressed_bytes
                ));
            }
            if t.element_bytes > 0 && !t.original_bytes.is_multiple_of(u64::from(t.element_bytes)) {
                p.push(format!(
                    "{at}/original_bytes: not a whole number of elements"
                ));
            }
            for (name, v) in [
                ("split_compress_seconds", t.split_compress_seconds),
                ("decompress_merge_seconds", t.decompress_merge_seconds),
            ] {
                if !(v.is_finite() && v >= 0.0) {
                    p.push(format!("{at}/{name}: must be a non-negative number"));
                }
            }
        }
    }
    indices.sort();
    for pair in indices.windows(2) {
        if pair[0].0 == pair[1].0 {
            p.push(format!(
                "{}/index: {} is used twice ({} and {})",
                pair[1].1, pair[1].0, pair[0].1, pair[1].1
            ));
        }
    }
    p
}

#[derive(Default)]
struct Totals {
    tensors: u64,
    original: u64,
    plain: u64,
    split: u64,
    planes: Vec<(u64, u64)>,
    split_s: f64,
    merge_s: f64,
}

fn gain(plain: u64, split: u64) -> String {
    if plain == 0 {
        "n/a".to_string()
    } else {
        format!(
            "{:+.2}%",
            100.0 * (plain as f64 - split as f64) / plain as f64
        )
    }
}

/// The table, as a pure function of the parsed JSON.
pub fn render(e: &Envelope<Data>) -> String {
    let d = &e.data;
    let mut s = md_header(e);
    s.push_str(&format!(
        "Class `{}`: {} file(s) parsed as safetensors, {} not parsed. zstd level {}, \
         {} library thread(s) in timed sections. Planes: plane 0 is the lowest byte of each \
         element. Gain = (plain - split) / plain; negative means the split is larger. \
         Speeds are MB/s of the tensor bytes (10^6 bytes per second).\n\n",
        d.class,
        d.files.len(),
        d.not_parsed.len(),
        d.zstd_level,
        d.library_threads
    ));

    let mut by_dtype: BTreeMap<&str, Totals> = BTreeMap::new();
    for f in &d.files {
        for t in &f.dtypes {
            let a = by_dtype.entry(t.dtype.as_str()).or_default();
            a.tensors += t.tensors;
            a.original += t.original_bytes;
            a.plain += t.plain_compressed_bytes;
            a.split += t.split_compressed_bytes;
            if a.planes.len() < t.plane_compressed_bytes.len() {
                a.planes.resize(t.plane_compressed_bytes.len(), (0, 0));
            }
            let plane_len = t.original_bytes / u64::from(t.element_bytes.max(1));
            for (slot, c) in a.planes.iter_mut().zip(&t.plane_compressed_bytes) {
                slot.0 += plane_len;
                slot.1 += c;
            }
            a.split_s += t.split_compress_seconds;
            a.merge_s += t.decompress_merge_seconds;
        }
    }

    s.push_str("## By dtype, all files\n\n");
    let rows: Vec<Vec<String>> = by_dtype
        .iter()
        .map(|(dtype, a)| {
            vec![
                dtype.to_string(),
                a.tensors.to_string(),
                a.original.to_string(),
                format!("{} ({})", a.plain, pct(a.plain, a.original)),
                format!("{} ({})", a.split, pct(a.split, a.original)),
                gain(a.plain, a.split),
                mbps(a.original, a.split_s),
                mbps(a.original, a.merge_s),
            ]
        })
        .collect();
    if rows.is_empty() {
        s.push_str("No floating-point tensors.\n\n");
    } else {
        s.push_str(&md_table(
            &[
                "dtype",
                "tensors",
                "original bytes",
                "plain zstd bytes (of original)",
                "split zstd bytes (of original)",
                "gain",
                "split+compress MB/s",
                "decompress+merge MB/s",
            ],
            &rows,
        ));
        s.push('\n');
        s.push_str("## Planes, all files\n\n");
        let mut prow = Vec::new();
        for (dtype, a) in &by_dtype {
            for (i, (plane_bytes, c)) in a.planes.iter().enumerate() {
                prow.push(vec![
                    dtype.to_string(),
                    i.to_string(),
                    plane_bytes.to_string(),
                    c.to_string(),
                    pct(*c, *plane_bytes),
                ]);
            }
        }
        s.push_str(&md_table(
            &[
                "dtype",
                "plane",
                "plane bytes",
                "compressed bytes",
                "compressed / plane",
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
        let plain: u64 = f.dtypes.iter().map(|t| t.plain_compressed_bytes).sum();
        let split: u64 = f.dtypes.iter().map(|t| t.split_compressed_bytes).sum();
        frows.push(vec![
            name,
            f.bytes.to_string(),
            f.tensors.to_string(),
            format!(
                "{} ({})",
                f.whole_file_compressed_bytes,
                pct(f.whole_file_compressed_bytes, f.bytes)
            ),
            float.to_string(),
            pct(plain, float),
            pct(split, float),
            gain(plain, split),
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
                "plain zstd / float",
                "split zstd / float",
                "gain",
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
        // Truncated at every length.
        for cut in 0..ok.len() {
            let r = parse_header(&ok[..cut]);
            if cut < ok.len() {
                assert!(r.is_err(), "cut at {cut} was accepted");
            }
        }
        // Huge declared length.
        let mut huge = u64::MAX.to_le_bytes().to_vec();
        huge.extend_from_slice(b"{}");
        assert!(parse_header(&huge).expect_err("huge").contains("exceeds"));
        let mut big = (MAX_HEADER_BYTES + 1).to_le_bytes().to_vec();
        big.extend_from_slice(b"{}");
        assert!(parse_header(&big).is_err());
        // Declared length beyond the file, but under the limit.
        let mut beyond = 100u64.to_le_bytes().to_vec();
        beyond.extend_from_slice(b"{}");
        assert!(parse_header(&beyond)
            .expect_err("beyond")
            .contains("exceeds"));
        // Not JSON, not an object, bad entry.
        assert!(parse_header(&with_header("nope", b"")).is_err());
        assert!(parse_header(&with_header("[]", b"")).is_err());
        assert!(parse_header(&with_header(r#"{"a":{"dtype":"F32"}}"#, b"")).is_err());
        // Offsets out of range, reversed, and shape mismatch.
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
    fn analysis_measures_float_tensors_and_counts_the_rest() {
        let file = st(&[
            ("w", "F32", vec![100, 30], sample_floats(3000, 4)),
            ("h", "F16", vec![500], sample_floats(500, 2)),
            ("e", "BF16", vec![0], vec![]),
            ("ids", "I64", vec![4], vec![9; 32]),
        ]);
        let rec = analyze(&file, 3, Some("c/m.safetensors".into()), 2)
            .expect("measured")
            .expect("parsed");
        assert_eq!(rec.index, 3);
        assert_eq!(rec.tensors, 4);
        assert_eq!((rec.other_tensors, rec.other_bytes), (1, 32));
        let names: Vec<&str> = rec.dtypes.iter().map(|d| d.dtype.as_str()).collect();
        assert_eq!(names, ["BF16", "F16", "F32"]);
        let f32r = &rec.dtypes[2];
        assert_eq!((f32r.element_bytes, f32r.original_bytes), (4, 12_000));
        assert_eq!(f32r.plane_compressed_bytes.len(), 4);
        assert_eq!(
            f32r.plane_compressed_bytes.iter().sum::<u64>(),
            f32r.split_compressed_bytes
        );
        assert!(f32r.plain_compressed_bytes > 0 && f32r.split_compressed_bytes > 0);
        let empty = &rec.dtypes[0];
        assert_eq!(
            (
                empty.tensors,
                empty.original_bytes,
                empty.plain_compressed_bytes
            ),
            (1, 0, 0)
        );
        assert!(rec.whole_file_compressed_bytes > 0);
        // A non-safetensors file is reported as not parsed, not as a failure.
        let np = analyze(b"just some text", 0, None, 1).expect("no failure");
        assert!(np.is_err());
    }
}
