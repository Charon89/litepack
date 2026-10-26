//! Creating, appending, rolling back, calibrating: the write side of the API.

use std::path::Path;
use std::time::Instant;

use lpk_core::{
    BalancedOptions, DictionaryPolicy, FastOptions, Monitor, Pipeline, RunSummary, Sources,
    StoreOptions,
};
use lpk_format::{Argon2Params, Credentials, FormatError, Suite};

use crate::error::Error;
use crate::session::GenerationInfo;

/// A tier the engine runs today (the Strong tier is reserved for Phase 2 and cannot be named).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tier {
    /// zstd with a long window (the default).
    #[default]
    Fast,
    /// LZMA with a large dictionary, or zstd per block by a trial.
    Balanced,
    /// Every block stored.
    Store,
}

impl Tier {
    /// The value `--tier` takes for this tier.
    pub fn name(self) -> &'static str {
        match self {
            Tier::Fast => "fast",
            Tier::Balanced => "balanced",
            Tier::Store => "store",
        }
    }

    /// The file order this tier uses when none is given (E2-26, D-104): the Balanced tier puts
    /// a file's versions next to each other ([`lpk_core::FileOrder::Versions`]; an input
    /// without sibling version roots is in path order, as with `None`), the Fast tier keeps
    /// path order, and the Store tier has no fold stage to order by. `lpk-core`'s own default
    /// stays [`lpk_core::FileOrder::None`].
    pub fn default_ordering(self) -> lpk_core::FileOrder {
        match self {
            Tier::Balanced => lpk_core::FileOrder::Versions,
            Tier::Fast | Tier::Store => lpk_core::FileOrder::None,
        }
    }
}

/// How a new archive is sealed.
#[derive(Clone)]
pub struct Seal {
    /// The cipher suite.
    pub suite: Suite,
    /// Leave the entry table in clear.
    pub listable: bool,
    /// The password and keyfile (wiped on drop).
    pub credentials: Credentials,
}

impl std::fmt::Debug for Seal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seal")
            .field("suite", &self.suite)
            .field("listable", &self.listable)
            .finish_non_exhaustive()
    }
}

/// The settings of `create`, `append` and `calibrate`: the CLI's tier flags as fields (the
/// pipeline they build is the one `lpk a` always built), the seal and the recovery percent.
/// `None` is the tier's default.
#[derive(Debug, Clone, Default)]
pub struct CreateSettings {
    /// The tier.
    pub tier: Tier,
    /// Blocks encoded at once at most.
    pub threads: Option<usize>,
    /// The run's memory budget.
    pub memory: Option<u64>,
    /// zstd level.
    pub level: Option<i32>,
    /// log2 of the zstd window.
    pub window_log: Option<u32>,
    /// The Balanced tier's LZMA dictionary.
    pub dict_size: Option<u32>,
    /// No dictionaries (the Fast tier).
    pub no_dictionaries: bool,
    /// No deduplication (also the fixed 1 MiB cut).
    pub no_dedup: bool,
    /// How files are ordered inside a cluster (`None`: [`Tier::default_ordering`]).
    pub ordering: Option<lpk_core::FileOrder>,
    /// The Deflate peel on or off.
    pub deflate_peel: Option<bool>,
    /// The JPEG peel on or off.
    pub jpeg_peel: Option<bool>,
    /// The Balanced tier's BWT stage on or off.
    pub bwt: Option<bool>,
    /// The Balanced tier's PNG row-filter re-encoding on or off.
    pub png_filter: Option<bool>,
    /// Seal the archive (`create` only; an append uses the archive's own key).
    pub seal: Option<Seal>,
    /// Recovery records as a percentage of the data, 0 to 20.
    pub recovery: u8,
}

impl CreateSettings {
    /// The file order these settings use: the one given, else the tier's default.
    pub fn ordering(&self) -> lpk_core::FileOrder {
        self.ordering
            .unwrap_or_else(|| self.tier.default_ordering())
    }

    /// The pipeline of these settings (what `lpk a`'s `build_pipeline` built), with the seal,
    /// the recovery percent and `monitor`.
    pub fn pipeline(&self, monitor: &Monitor) -> Pipeline {
        let tier = self.tier;
        let mut pipeline = if tier == Tier::Store {
            Pipeline::store(StoreOptions {
                threads: self.threads,
                ..StoreOptions::default()
            })
        } else if tier == Tier::Balanced {
            let mut o = BalancedOptions {
                threads: self.threads,
                ..BalancedOptions::default()
            };
            if let Some(l) = self.level {
                o.zstd_level = l;
            }
            if let Some(w) = self.window_log {
                o.zstd_window_log = Some(w);
            }
            if let Some(d) = self.dict_size {
                o.dict_size = d;
            }
            if let Some(b) = self.bwt {
                o.bwt = b;
            }
            Pipeline::balanced(o)
        } else {
            let mut o = FastOptions {
                threads: self.threads,
                ..FastOptions::default()
            };
            if let Some(l) = self.level {
                o.level = l;
            }
            if self.no_dictionaries {
                o.dictionaries = DictionaryPolicy::None;
            }
            if let Some(w) = self.window_log {
                o.window_log = w;
            }
            Pipeline::fast(o)
        };
        pipeline.seal.memory = self.memory;
        match self.deflate_peel {
            Some(true) => pipeline.deflate = Some(lpk_core::peel::deflate::DeflatePeel::default()),
            Some(false) => pipeline.deflate = None,
            None => {}
        }
        // The PNG row-filter re-encoding: on by default with the Balanced tier's Deflate peel.
        if let Some(d) = pipeline.deflate.as_mut() {
            d.png_filter = tier == Tier::Balanced && self.png_filter != Some(false);
        }
        // Both tiers' pipelines carry the JPEG peel; off removes every stage that peels the
        // JPEG class (a typed check, so a renamed stage cannot make it a no-op).
        if self.jpeg_peel == Some(false) {
            pipeline
                .peel
                .retain(|p| !p.applies_to(lpk_core::Class::Jpeg));
        }
        if self.no_dedup {
            pipeline.fold = None;
        } else if pipeline.fold.is_some() {
            pipeline.fold = Some(Box::new(lpk_core::Dedup::new(lpk_core::FoldOptions {
                ordering: self.ordering(),
                ..lpk_core::FoldOptions::default()
            })));
        }
        if let Some(s) = &self.seal {
            pipeline.seal.writer.seal = Some(lpk_format::SealOptions {
                suite: s.suite,
                argon2: Argon2Params::default(),
                listable: s.listable,
                credentials: s.credentials.clone(),
            });
        }
        pipeline.seal.writer.recovery.percent = self.recovery;
        pipeline.monitor = monitor.clone();
        pipeline
    }
}

/// Create the archive `out` (which must not exist) from `inputs` with `s`; progress and
/// cancellation through `m` (a cancelled or failed run removes `out`).
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let dir = tempfile::tempdir()?;
/// let src = dir.path().join("src");
/// std::fs::create_dir(&src)?;
/// std::fs::write(src.join("a.txt"), b"hello")?;
/// let out = dir.path().join("a.lpk");
/// let s = lpk_api::create(
///     &lpk_api::Sources::contents_of(&src),
///     &out,
///     &lpk_api::CreateSettings::default(),
///     &lpk_api::Monitor::default(),
/// )?;
/// assert_eq!(s.writer.entries, 1);
/// # Ok(()) }
/// ```
pub fn create(
    inputs: &Sources,
    out: &Path,
    s: &CreateSettings,
    m: &Monitor,
) -> Result<RunSummary, Error> {
    s.pipeline(m)
        .run_file_sources(inputs, out)
        .map_err(|e| Error::core(e, Some(out)))
}

/// Append the directory `dir` to the archive at `archive` as a new generation (the archive's
/// own key seals it; `s.seal` is ignored). What [`crate::Session::append`] runs once the
/// session is closed; usable without one (an archive that does not open, the CLI's `append`).
pub fn append(
    archive: &Path,
    dir: &Path,
    s: &CreateSettings,
    credentials: Option<&Credentials>,
    m: &Monitor,
) -> Result<RunSummary, Error> {
    let mut pipeline = s.pipeline(m);
    pipeline.seal.writer.seal = None;
    pipeline
        .append_file(dir, archive, credentials)
        .map_err(|e| Error::core(e, Some(archive)))
}

/// Truncate the archive at `path` to `generation` (D-59); no key is needed. What
/// [`crate::Session::rollback`] runs once the session is closed; usable without one (an
/// archive whose last generation was cut short does not open).
pub fn rollback(path: &Path, generation: u64) -> Result<GenerationInfo, Error> {
    let fail = |e: FormatError| Error::format(e, Some(path));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| fail(FormatError::Io(e)))?;
    lpk_format::rollback(&mut file, generation)
        .map(GenerationInfo::from)
        .map_err(fail)
}

/// The latest committed generation of the archive at `path`, from its trailer chain (the
/// chain [`rollback`] reads; a torn append's tail is skipped), opened read-only and without
/// credentials; nothing is written. Any error (the file held or unreadable, a damaged chain,
/// an unsupported version) is returned as it is.
pub fn committed_generation(path: &Path) -> Result<u64, Error> {
    let fail = |e: FormatError| Error::format(e, Some(path));
    let mut file = std::fs::File::open(path).map_err(|e| fail(FormatError::Io(e)))?;
    lpk_format::committed_chain(&mut file)
        .map(|(_, latest)| latest)
        .map_err(fail)
}

/// A measured compression rate on this machine (E5-5's only source of estimates).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rate {
    /// Input bytes of the sample.
    pub bytes_in: u64,
    /// Archive bytes written for it.
    pub bytes_out: u64,
    /// Wall time of the run.
    pub seconds: f64,
}

impl Rate {
    /// Input bytes per second (0 when nothing was timed).
    pub fn bytes_per_second(&self) -> f64 {
        if self.seconds > 0.0 {
            self.bytes_in as f64 / self.seconds
        } else {
            0.0
        }
    }
}

/// Run the pipeline of `s` in memory over a bounded sample of `inputs`: the first files in
/// walk order (distinct names) until `sample` bytes, timed (no archive written; the seal and
/// the recovery records are left out).
pub fn calibrate(
    inputs: &Sources,
    s: &CreateSettings,
    sample: u64,
    m: &Monitor,
) -> Result<Rate, Error> {
    let walked = lpk_core::walk_sources(inputs, &lpk_core::IngestOptions::default())
        .map_err(|e| Error::core(e, None))?;
    let mut roots = Vec::new();
    let mut names = std::collections::HashSet::new();
    let mut bytes = 0u64;
    for i in walked {
        if bytes >= sample {
            break;
        }
        if i.kind != lpk_format::EntryKind::File {
            continue;
        }
        let name = i.source.file_name().map(|n| n.to_os_string());
        if !names.insert(name) {
            continue;
        }
        bytes += i.len;
        roots.push(lpk_core::Root::named(&i.source).map_err(|e| Error::core(e, None))?);
    }
    if roots.is_empty() {
        return Ok(Rate::default());
    }
    let sources =
        Sources::new(roots, lpk_core::Selection::all()).map_err(|e| Error::core(e, None))?;
    let mut settings = s.clone();
    settings.seal = None;
    settings.recovery = 0;
    let mut out = Vec::new();
    let t = Instant::now();
    settings
        .pipeline(m)
        .run_sources(&sources, &mut out)
        .map_err(|e| Error::core(e, None))?;
    Ok(Rate {
        bytes_in: bytes,
        bytes_out: out.len() as u64,
        seconds: t.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::{CreateSettings, Tier};
    use lpk_core::FileOrder;

    /// E2-26 (D-104): the Balanced tier orders versions unless told otherwise; the other tiers
    /// and `lpk-core`'s own default keep path order.
    #[test]
    fn the_balanced_tier_defaults_to_the_versions_order() {
        assert_eq!(Tier::Balanced.default_ordering(), FileOrder::Versions);
        assert_eq!(Tier::Fast.default_ordering(), FileOrder::None);
        assert_eq!(Tier::Store.default_ordering(), FileOrder::None);
        assert_eq!(lpk_core::FoldOptions::default().ordering, FileOrder::None);
        let s = |tier, ordering| CreateSettings {
            tier,
            ordering,
            ..CreateSettings::default()
        };
        assert_eq!(s(Tier::Balanced, None).ordering(), FileOrder::Versions);
        assert_eq!(
            s(Tier::Balanced, Some(FileOrder::None)).ordering(),
            FileOrder::None
        );
        assert_eq!(s(Tier::Fast, None).ordering(), FileOrder::None);
        assert_eq!(
            s(Tier::Fast, Some(FileOrder::Versions)).ordering(),
            FileOrder::Versions
        );
        assert_eq!(CreateSettings::default().ordering(), FileOrder::None);
    }
}
