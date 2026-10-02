//! Derived source kinds: files produced from files of other classes already built in `<out>`
//! (declared with `inputs`), or from nothing but fixed parameters.
//!
//! A derivation pins nothing in the lock (its inputs are pinned) and is a pure function of its
//! inputs: the same input bytes give the same output bytes on every run, and for everything done
//! in-process on Windows and Linux alike. Nothing depends on thread count, CPU features, the
//! clock, the locale, directory iteration order or temporary file names. The one exception by
//! nature is [`video`], which runs the external `ffmpeg` program.
//!
//! Kinds (registry `kind`):
//! * `encrypted-random` ([`encrypted`]): a fixed-seed ChaCha20 keystream and the same bytes
//!   encrypted with AES-256-CTR under a fixed key.
//! * `small-files` ([`smallfiles`]): many small files cut at line boundaries from the logs and CSV
//!   inputs, part of them JSON lines made from CSV rows.
//! * `flac-to-wav` ([`wav`]): one WAV per FLAC input, decoded locally.
//! * `png-to-jpeg` ([`jpegs`]): a baseline and a progressive JPEG per PNG input, written by the
//!   in-tree encoder ([`jpegenc`]).
//! * `jpeg-crop` ([`jpegs`]): every N-th JPEG input decoded, cropped and re-saved.
//! * `photo-convert` ([`jpegs`]): PNG and BMP versions of JPEG inputs within a byte budget.
//! * `built-zips` ([`zips`]): ZIPs written in-process from fixed subsets of other classes, each
//!   with several Deflate encoders.
//! * `ffmpeg-encode` ([`video`]): short HEVC / AV1 encodes through the external `ffmpeg`.
//!
//! Inputs: [`input_files`] lists the files of the source's `inputs` classes in sorted path order.
//! Without a `from` list it uses every non-derived source of those classes (never the source's own
//! output); with `from` it uses exactly the named sources. Only sources built in this run count
//! when the class is part of the run; for a class a partial (`--only`) run does not select, the
//! files already in `<out>` are used.
//!
//! A derived source that finds nothing to work on returns a [`Skip`]: fatal, unless the source is
//! `optional`, in which case it is listed under `skipped` with the reason.

pub mod encrypted;
#[cfg(test)]
mod golden_tests;
pub mod jpegenc;
pub mod jpegs;
pub mod smallfiles;
pub mod video;
pub mod wav;
pub mod zips;

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::build::Ctx;
use super::extract::sanitize_path;
use super::manifest::ManifestFile;
use super::registry::{Source, SourceSpec};

/// A derivation that cannot run here (missing input class, missing external program). Tolerated
/// by `optional` sources, recorded under `skipped`.
#[derive(Debug)]
pub struct Skip(pub String);

impl fmt::Display for Skip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Skip {}

/// What the derived kinds need to know about the current run.
#[derive(Debug, Default)]
pub struct State {
    /// `(class, source id)` of every source built so far in this run.
    pub built: Vec<(String, String)>,
    /// Classes that have a selected source in this run.
    pub run_classes: BTreeSet<String>,
    /// Ids of all registry sources that are derived (have `inputs`).
    pub derived_ids: BTreeSet<String>,
    /// Name or path of the `ffmpeg` program (`None`: `ffmpeg`).
    pub ffmpeg: Option<String>,
    /// Notes recorded by derivations that built only part of their output: `(source, reason)`.
    /// They go under `skipped` in `build-info.json`.
    pub skipped: Vec<(String, String)>,
    /// Registry source ids per class in the current profile (the only directories a class that
    /// this run does not select may be read from).
    pub class_sources: std::collections::BTreeMap<String, BTreeSet<String>>,
}

/// One input file of a derivation.
#[derive(Debug, Clone)]
pub struct InputFile {
    pub class: String,
    pub source: String,
    /// Path below `<out>/<class>/<source>/`, `/`-separated.
    pub rel: String,
    pub path: PathBuf,
}

impl InputFile {
    /// `<class>/<source>/<rel>`: the sort key and the manifest path of the input.
    pub fn full(&self) -> String {
        format!("{}/{}/{}", self.class, self.source, self.rel)
    }
}

/// Every regular file below `dir`, as `(relative path with '/', absolute path)`, sorted by path
/// bytes. Symbolic links are not followed.
pub fn walk_files(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    fn rec(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
            names.push(e?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        for name in names {
            let path = dir.join(&name);
            let child = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                rec(&path, &child, out)?;
            } else if meta.is_file() {
                out.push((child, path));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    rec(dir, "", &mut out)?;
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(out)
}

/// The input files of `source` (see the module docs), sorted by `<class>/<source>/<rel>`.
pub fn input_files(ctx: &Ctx<'_>, source: &Source, from: &[String]) -> Result<Vec<InputFile>> {
    let st = ctx.derive_state();
    let mut files = Vec::new();
    for class in &source.inputs {
        let class_dir = ctx.out().join(class);
        let ids: Vec<String> = if st.run_classes.contains(class) {
            st.built
                .iter()
                .filter(|(c, _)| c == class)
                .map(|(_, id)| id.clone())
                .collect()
        } else {
            let mut ids = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&class_dir) {
                for e in rd.flatten() {
                    if e.path().is_dir() {
                        ids.push(e.file_name().to_string_lossy().into_owned());
                    }
                }
            }
            // Only directories of sources the registry has for this class and profile (not
            // leftovers of removed or renamed sources).
            ids.retain(|id| st.class_sources.get(class).is_some_and(|s| s.contains(id)));
            ids
        };
        for id in ids {
            if id == source.id {
                continue;
            }
            if from.is_empty() {
                if st.derived_ids.contains(&id) {
                    continue;
                }
            } else if !from.contains(&id) {
                continue;
            }
            for (rel, path) in walk_files(&class_dir.join(&id))? {
                files.push(InputFile {
                    class: class.clone(),
                    source: id.clone(),
                    rel,
                    path,
                });
            }
        }
    }
    files.sort_by(|a, b| a.full().as_bytes().cmp(b.full().as_bytes()));
    Ok(files)
}

/// Like [`input_files`], keeping only files whose lower-cased name ends with one of `exts`
/// (given with the dot, e.g. `.flac`). An empty result is a [`Skip`].
pub fn input_files_with(
    ctx: &Ctx<'_>,
    source: &Source,
    from: &[String],
    exts: &[&str],
) -> Result<Vec<InputFile>> {
    let all = input_files(ctx, source, from)?;
    let files: Vec<InputFile> = all
        .into_iter()
        .filter(|f| {
            let lower = f.rel.to_ascii_lowercase();
            exts.iter().any(|e| lower.ends_with(e))
        })
        .collect();
    if files.is_empty() {
        return Err(Skip(format!(
            "no {} files among the inputs ({}{})",
            exts.join("/"),
            source.inputs.join(", "),
            if from.is_empty() {
                String::new()
            } else {
                format!("; from: {}", from.join(", "))
            }
        ))
        .into());
    }
    Ok(files)
}

/// Collects the files a derivation writes below its directory and their manifest entries.
pub struct Output<'a> {
    source: &'a Source,
    dir: &'a Path,
    files: Vec<ManifestFile>,
}

impl<'a> Output<'a> {
    pub fn new(source: &'a Source, dir: &'a Path) -> Self {
        Output {
            source,
            dir,
            files: Vec::new(),
        }
    }

    /// The destination of `rel` (validated, parent directories created, must not exist).
    pub fn path_for(&self, rel: &str) -> Result<PathBuf> {
        let comps = sanitize_path(rel)?;
        if comps.is_empty() {
            bail!("empty output path");
        }
        let mut dest = self.dir.to_path_buf();
        for c in &comps {
            dest.push(c);
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if dest.exists() {
            bail!("source `{}`: `{rel}` is written twice", self.source.id);
        }
        Ok(dest)
    }

    /// Write `data` as `rel`.
    pub fn write(&mut self, rel: &str, data: &[u8]) -> Result<()> {
        let dest = self.path_for(rel)?;
        std::fs::write(&dest, data).with_context(|| format!("writing {}", dest.display()))?;
        self.record(
            rel,
            data.len() as u64,
            blake3::hash(data).to_hex().to_string(),
        );
        Ok(())
    }

    /// Record a file already written at `path_for(rel)`.
    pub fn record(&mut self, rel: &str, bytes: u64, blake3: String) {
        let rel = rel.replace('\\', "/");
        self.files.push(ManifestFile {
            blake3,
            bytes,
            licence: self.source.licence.clone(),
            path: format!("{}/{}/{rel}", self.source.class, self.source.id),
            source: self.source.id.clone(),
        });
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn finish(self) -> Vec<ManifestFile> {
        self.files
    }
}

/// SplitMix64: a tiny, fixed, platform-independent pseudo-random generator for choices that must
/// repeat exactly (not for the cryptographic data of `encrypted-random`).
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        SplitMix64(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`; the modulo bias is irrelevant here).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// Build one derived source into `dir`.
pub fn build(ctx: &mut Ctx<'_>, source: &Source, dir: &Path) -> Result<Vec<ManifestFile>> {
    let mut out = Output::new(source, dir);
    match &source.spec {
        SourceSpec::EncryptedRandom(s) => encrypted::build(s, &mut out)?,
        SourceSpec::SmallFiles(s) => smallfiles::build(ctx, source, s, &mut out)?,
        SourceSpec::FlacToWav(s) => wav::build(ctx, source, s, &mut out)?,
        SourceSpec::PngToJpeg(s) => jpegs::build_png_to_jpeg(ctx, source, s, &mut out)?,
        SourceSpec::JpegCrop(s) => jpegs::build_crop(ctx, source, s, &mut out)?,
        SourceSpec::PhotoConvert(s) => jpegs::build_convert(ctx, source, s, &mut out)?,
        SourceSpec::BuiltZips(s) => zips::build(ctx, source, s, &mut out)?,
        SourceSpec::FfmpegEncode(s) => video::build(ctx, source, s, &mut out)?,
        _ => bail!("source `{}` is not a derived kind", source.id),
    }
    Ok(out.finish())
}

#[cfg(test)]
pub mod testutil {
    //! Shared helpers for the derivation tests: a registry source and a context over a temp dir.

    use std::path::Path;

    use super::*;
    use crate::corpus::fetch::fake::FakeFetcher;
    use crate::corpus::registry::{Profile, Source, SourceSpec};

    pub fn source(id: &str, class: &str, inputs: &[&str], spec: SourceSpec) -> Source {
        Source {
            id: id.into(),
            class: class.into(),
            licence: "CC0-1.0".into(),
            origin: "test".into(),
            profiles: vec![Profile::Small],
            optional: false,
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            spec,
        }
    }

    /// Write `data` at `<root>/out/<class>/<id>/<rel>`.
    pub fn put(root: &Path, class: &str, id: &str, rel: &str, data: &[u8]) {
        let p = root.join("out").join(class).join(id).join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, data).expect("write");
    }

    /// Run `src` over a context whose run has built the `(class, id)` pairs in `built`.
    /// Returns the manifest files; output goes to `<root>/out/<class>/<id>/`.
    pub fn run(
        root: &Path,
        src: &Source,
        built: &[(&str, &str)],
    ) -> Result<Vec<crate::corpus::manifest::ManifestFile>> {
        let fetcher = FakeFetcher::default();
        let mut ctx = Ctx::for_tests(&fetcher, root, false);
        ctx.derive = State {
            built: built
                .iter()
                .map(|(c, i)| (c.to_string(), i.to_string()))
                .collect(),
            run_classes: built.iter().map(|(c, _)| c.to_string()).collect(),
            derived_ids: BTreeSet::new(),
            ffmpeg: None,
            skipped: Vec::new(),
            class_sources: Default::default(),
        };
        let dir = root.join("out").join(&src.class).join(&src.id);
        std::fs::create_dir_all(&dir)?;
        build(&mut ctx, src, &dir)
    }

    /// `(path, bytes)` of every file below `<root>/out/<class>/<id>`.
    pub fn read_all(root: &Path, class: &str, id: &str) -> Vec<(String, Vec<u8>)> {
        walk_files(&root.join("out").join(class).join(id))
            .expect("walk")
            .into_iter()
            .map(|(rel, p)| (rel, std::fs::read(p).expect("read")))
            .collect()
    }
}
