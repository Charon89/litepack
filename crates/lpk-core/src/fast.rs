//! The Fast tier: one zstd frame per block, a long match window, optional bundled dictionaries,
//! and blocks that never span two clusters.
//!
//! Files are grouped by [`cluster`] (class and dictionary kind) and fed to the writer cluster by
//! cluster with `Writer::close_block` between them; before each cluster the pipeline tells the
//! encoder, through a [`FastHandle`], the class and dictionary kind of what follows. The encoder
//! stores a block without trying zstd when its class is already compressed, or when the entropy
//! gate calls the block incompressible, and falls back to `store` when zstd does not shrink it.
//!
//! Extracting a Fast archive needs the dictionaries it names: through `lpk_format::Archive` pass
//! [`BundledPriors`] to `set_priors`; with the format tool, give one `--prior` per dictionary
//! file under `crates/lpk-core/priors/`, for example
//! `lpk-decode extract <archive> --prior crates/lpk-core/priors/prose.dict`.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use lpk_format::{
    BlockEncoder, Encoded, EntryKind, FormatError, Graph, PrimitiveId, Step, StoreEncoder, Writer,
    WriterOptions, WriterSummary, DEFAULT_BLOCK_SIZE,
};
use std::io::{BufWriter, Write};
use zstd::zstd_safe::CParameter;

use crate::classify::Class;
use crate::cluster::{cluster, DictionaryKind};
use crate::error::CoreError;
use crate::gate::Gate;
use crate::ingest::{validate_input, walk, IngestOptions, Input};
use crate::priors::BundledPriors;
use crate::source::Source;
use crate::store::{create_new_and_run, Counting, SyncFn, OUT_BUF};

/// Smallest and largest `window_log` a `zstd` step may declare.
const WINDOW_LOG_RANGE: std::ops::RangeInclusive<u32> = 10..=31;

/// Whether blocks may use the bundled dictionaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DictionaryPolicy {
    /// Use the dictionary of the cluster's kind (prose, structured, source).
    #[default]
    Bundled,
    /// Never use a dictionary.
    None,
}

/// Settings of [`archive_fast`].
#[derive(Debug, Clone, Copy)]
pub struct FastOptions {
    /// How the tree is walked.
    pub ingest: IngestOptions,
    /// zstd compression level. The default, 3, is the setting the Phase 0 G4 proxy measured.
    pub level: i32,
    /// log2 of the match window in bytes, 10 to 31. The default, 27, is the window of the
    /// catalogue's zstd row (`--long=27`); the decoder is told this number and reserves that
    /// much memory.
    pub window_log: u32,
    /// Plain bytes per block (the format's default).
    pub block_size: u64,
    /// Bundled dictionaries on or off.
    pub dictionaries: DictionaryPolicy,
    /// The entropy gate that sends incompressible blocks to `store`.
    pub gate: Gate,
}

impl Default for FastOptions {
    fn default() -> Self {
        FastOptions {
            ingest: IngestOptions::default(),
            level: 3,
            window_log: 27,
            block_size: DEFAULT_BLOCK_SIZE,
            dictionaries: DictionaryPolicy::Bundled,
            gate: Gate::DEFAULT,
        }
    }
}

/// What the Fast encoder did with the blocks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FastSummary {
    /// Blocks coded with zstd.
    pub zstd_blocks: u64,
    /// Blocks stored because the gate called them incompressible.
    pub stored_by_gate: u64,
    /// Blocks stored without the gate because their class is already compressed.
    pub stored_by_class: u64,
    /// Blocks stored because the zstd frame was not smaller than the plain bytes.
    pub stored_no_gain: u64,
}

#[derive(Debug)]
struct Shared {
    class: Class,
    dictionary: DictionaryKind,
    summary: FastSummary,
}

/// The pipeline's side of the encoder: sets what the next blocks hold and reads the counts.
#[derive(Debug, Clone)]
pub struct FastHandle(Arc<Mutex<Shared>>);

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl FastHandle {
    /// The class and dictionary kind of the blocks encoded from now on.
    pub fn set_hint(&self, class: Class, dictionary: DictionaryKind) {
        let mut s = lock(&self.0);
        s.class = class;
        s.dictionary = dictionary;
    }

    /// The counts so far.
    pub fn summary(&self) -> FastSummary {
        lock(&self.0).summary
    }
}

/// Classes stored without trying to compress them.
fn stored_by_class(class: Class) -> bool {
    matches!(
        class,
        Class::HighEntropy | Class::Video | Class::Compressed | Class::Jpeg | Class::Png
    )
}

/// The Fast tier's block encoder (zstd, gate, dictionaries).
pub struct ZstdEncoder {
    level: i32,
    window_log: u32,
    gate: Gate,
    policy: DictionaryPolicy,
    priors: BundledPriors,
    shared: Arc<Mutex<Shared>>,
    /// One prepared compressor per dictionary kind (index of [`slot`]).
    compressors: [Option<zstd::bulk::Compressor<'static>>; 4],
}

impl std::fmt::Debug for ZstdEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZstdEncoder")
            .field("level", &self.level)
            .field("window_log", &self.window_log)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

fn slot(kind: DictionaryKind) -> usize {
    match kind {
        DictionaryKind::None => 0,
        DictionaryKind::Prose => 1,
        DictionaryKind::Structured => 2,
        DictionaryKind::Source => 3,
    }
}

impl ZstdEncoder {
    /// A new encoder and the handle that steers it. Errors when `level` or `window_log` is out
    /// of range.
    pub fn new(
        level: i32,
        window_log: u32,
        policy: DictionaryPolicy,
        gate: Gate,
    ) -> Result<(Self, FastHandle), CoreError> {
        if !WINDOW_LOG_RANGE.contains(&window_log) {
            return Err(CoreError::InvalidOption(format!(
                "window_log {window_log} is outside 10..=31"
            )));
        }
        let range = zstd::compression_level_range();
        if !range.contains(&level) {
            return Err(CoreError::InvalidOption(format!(
                "level {level} is outside {}..={}",
                range.start(),
                range.end()
            )));
        }
        let shared = Arc::new(Mutex::new(Shared {
            class: Class::Other,
            dictionary: DictionaryKind::None,
            summary: FastSummary::default(),
        }));
        let enc = ZstdEncoder {
            level,
            window_log,
            gate,
            policy,
            priors: BundledPriors::new(),
            shared: Arc::clone(&shared),
            compressors: [None, None, None, None],
        };
        Ok((enc, FastHandle(shared)))
    }

    fn compressor(
        &mut self,
        kind: DictionaryKind,
        dictionary: Option<&[u8]>,
    ) -> Result<&mut zstd::bulk::Compressor<'static>, FormatError> {
        let i = slot(kind);
        if self.compressors[i].is_none() {
            let mut c = match dictionary {
                Some(d) => zstd::bulk::Compressor::with_dictionary(self.level, d)?,
                None => zstd::bulk::Compressor::new(self.level)?,
            };
            c.set_parameter(CParameter::WindowLog(self.window_log))?;
            c.set_parameter(CParameter::EnableLongDistanceMatching(true))?;
            // The format hashes every frame and chunk; the content size is written.
            c.set_parameter(CParameter::ChecksumFlag(false))?;
            c.set_parameter(CParameter::ContentSizeFlag(true))?;
            self.compressors[i] = Some(c);
        }
        match self.compressors[i].as_mut() {
            Some(c) => Ok(c),
            None => Err(FormatError::Io(std::io::Error::other("no compressor"))),
        }
    }

    fn bump(&self, f: impl FnOnce(&mut FastSummary)) {
        f(&mut lock(&self.shared).summary);
    }
}

impl BlockEncoder for ZstdEncoder {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        let (class, kind) = {
            let s = lock(&self.shared);
            (s.class, s.dictionary)
        };
        if stored_by_class(class) {
            self.bump(|s| s.stored_by_class += 1);
            return StoreEncoder.encode(plain);
        }
        if self.gate.is_incompressible(plain) {
            self.bump(|s| s.stored_by_gate += 1);
            return StoreEncoder.encode(plain);
        }
        let kind = match self.policy {
            DictionaryPolicy::Bundled => kind,
            DictionaryPolicy::None => DictionaryKind::None,
        };
        let dict = self.priors.dictionary(kind);
        let (id, kind) = match dict {
            Some((id, _)) => (id, kind),
            None => ([0u8; 32], DictionaryKind::None),
        };
        let bytes = self
            .compressor(kind, dict.map(|(_, d)| d))?
            .compress(plain)?;
        if bytes.len() >= plain.len() {
            self.bump(|s| s.stored_no_gain += 1);
            return StoreEncoder.encode(plain);
        }
        self.bump(|s| s.zstd_blocks += 1);
        let mut params = vec![self.window_log as u8];
        params.extend_from_slice(&id);
        let graph = Graph {
            steps: vec![Step {
                primitive: PrimitiveId::Zstd,
                params,
            }],
        };
        Ok(Encoded {
            resources: graph.resources(),
            graph,
            bytes,
        })
    }
}

/// Walk `root` and write it with the Fast tier to `out`.
pub fn archive_fast(
    root: &Path,
    out: impl Write,
    options: FastOptions,
) -> Result<(WriterSummary, FastSummary), CoreError> {
    let inputs = walk(root, &options.ingest)?;
    write_fast_inputs(&inputs, out, options, None)
}

/// Like [`archive_fast`], writing to a new file at `archive_path` with the guarantees of
/// `archive_store_file`: create-new, the output left out of its own archive, data synced before
/// the trailer, a failed write removes the file.
pub fn archive_fast_file(
    root: &Path,
    archive_path: &Path,
    options: FastOptions,
) -> Result<(WriterSummary, FastSummary), CoreError> {
    let ingest = options.ingest;
    create_new_and_run(root, archive_path, &ingest, |inputs, file, sync| {
        write_fast_inputs(&inputs, file, options, Some(sync))
    })
}

fn write_fast_inputs(
    inputs: &[Input],
    out: impl Write,
    options: FastOptions,
    sync: Option<SyncFn>,
) -> Result<(WriterSummary, FastSummary), CoreError> {
    let (encoder, handle) = ZstdEncoder::new(
        options.level,
        options.window_log,
        options.dictionaries,
        options.gate,
    )?;
    for input in inputs {
        validate_input(input)?;
    }
    let clusters = cluster(inputs)?;
    let source = Source::new();
    let wopts = WriterOptions {
        block_size: options.block_size,
        encoder: Box::new(encoder),
        ..WriterOptions::default()
    };
    let mut writer = Writer::new(BufWriter::with_capacity(OUT_BUF, out), wopts)?;
    if let Some(sync) = sync {
        writer = writer.with_sync(sync);
    }
    let mut meta: Vec<&Input> = inputs
        .iter()
        .filter(|i| i.kind != EntryKind::File)
        .collect();
    meta.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    for input in meta {
        if input.kind == EntryKind::Directory {
            writer.add_directory(&input.path, input.flags, input.mtime_ns)?;
        } else {
            let target = input.symlink_target.as_deref().unwrap_or_default();
            writer.add_symlink(&input.path, input.flags, input.mtime_ns, target)?;
        }
    }
    for c in &clusters {
        handle.set_hint(c.class, c.dictionary);
        for input in &c.inputs {
            let mut r = Counting {
                inner: source.open(input)?,
                n: 0,
            };
            writer.add_file(&input.path, input.flags, input.mtime_ns, &mut r)?;
            if r.n != input.len {
                return Err(CoreError::ChangedWhileReading {
                    path: input.source.clone(),
                });
            }
        }
        writer.close_block()?;
    }
    let summary = writer.finish()?;
    Ok((summary, handle.summary()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::tests::{clear_readonly, make_tree};
    use lpk_format::{prior_id, Archive, Resources};
    use std::io::Cursor;

    /// Compressible English-like text of exactly `len` bytes.
    fn prose(len: usize) -> Vec<u8> {
        const WORDS: [&str; 16] = [
            "the", "king", "of", "France", "and", "his", "army", "marched", "over", "a", "long",
            "road", "toward", "the", "sea", "while",
        ];
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let mut out = Vec::with_capacity(len + 16);
        while out.len() < len {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            out.extend_from_slice(WORDS[(s % 16) as usize].as_bytes());
            out.push(if s & 0x700 == 0 { b'\n' } else { b' ' });
        }
        out.truncate(len);
        out
    }

    fn random(len: usize) -> Vec<u8> {
        let mut s = 0x1234_5678_9ABC_DEF1u64;
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect()
    }

    fn encoder(policy: DictionaryPolicy) -> (ZstdEncoder, FastHandle) {
        ZstdEncoder::new(3, 27, policy, Gate::DEFAULT).unwrap()
    }

    /// Read every file of an archive back with the bundled priors.
    type Extracted = (Archive<Cursor<Vec<u8>>>, Vec<(String, Vec<u8>)>);

    fn extract_all(bytes: Vec<u8>) -> Extracted {
        let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        a.set_priors(Box::new(BundledPriors::new()));
        let table = a.entry_table().unwrap();
        let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
        let mut files = Vec::new();
        for e in &entries {
            if e.kind == EntryKind::File {
                let mut out = Vec::new();
                a.extract(e, &mut out).unwrap();
                files.push((e.path.clone(), out));
            }
        }
        (a, files)
    }

    #[test]
    fn a_prose_block_uses_the_prose_dictionary_and_decodes() {
        let data = prose(1 << 20);
        let (enc, handle) = encoder(DictionaryPolicy::Bundled);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let wopts = WriterOptions {
            encoder: Box::new(enc),
            ..WriterOptions::default()
        };
        let mut out = Vec::new();
        let mut w = Writer::new(&mut out, wopts).unwrap();
        w.add_file("p.txt", Default::default(), 0, &mut data.as_slice())
            .unwrap();
        w.finish().unwrap();
        let s = handle.summary();
        assert_eq!(
            (s.zstd_blocks, s.stored_by_gate, s.stored_by_class),
            (1, 0, 0)
        );
        assert!(out.len() < data.len() / 2);
        let (a, files) = extract_all(out);
        let want = BundledPriors::new()
            .dictionary(DictionaryKind::Prose)
            .unwrap()
            .0;
        assert_eq!(a.priors(), &[want]);
        assert_eq!(files[0].1, data);
    }

    #[test]
    fn without_dictionaries_the_step_id_is_zero_and_the_frame_names_none() {
        let data = prose(1 << 20);
        let (mut enc, handle) = encoder(DictionaryPolicy::None);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let e = enc.encode(&data).unwrap();
        assert_eq!(e.graph.steps[0].primitive, PrimitiveId::Zstd);
        assert_eq!(e.graph.steps[0].params[0], 27);
        assert_eq!(&e.graph.steps[0].params[1..], &[0u8; 32]);
        assert!(e.graph.prior_ids().is_empty());
        // Frame_Header_Descriptor bits 0..1 are Dictionary_ID_flag.
        assert_eq!(e.bytes[4] & 0b11, 0);
        assert_eq!(e.resources.window, 1 << 27);
        // With a dictionary the frame names it and the step carries its id.
        let (mut enc, handle) = encoder(DictionaryPolicy::Bundled);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let e = enc.encode(&data).unwrap();
        assert_ne!(e.bytes[4] & 0b11, 0);
        assert_eq!(e.graph.prior_ids().len(), 1);
        let id = prior_id(
            BundledPriors::new()
                .dictionary(DictionaryKind::Prose)
                .unwrap()
                .1,
        );
        assert_eq!(e.graph.prior_ids()[0], id);
    }

    #[test]
    fn random_blocks_are_stored_by_the_gate_and_video_by_class() {
        let data = random(1 << 20);
        let (mut enc, handle) = encoder(DictionaryPolicy::Bundled);
        handle.set_hint(Class::Other, DictionaryKind::None);
        let e = enc.encode(&data).unwrap();
        assert_eq!(e.graph.steps[0].primitive, PrimitiveId::Store);
        assert_eq!(e.bytes, data);
        assert_eq!(handle.summary().stored_by_gate, 1);
        // Compressible bytes under a Video hint skip the gate and are stored.
        handle.set_hint(Class::Video, DictionaryKind::None);
        let e = enc.encode(&prose(1 << 16)).unwrap();
        assert_eq!(e.graph.steps[0].primitive, PrimitiveId::Store);
        let s = handle.summary();
        assert_eq!(
            (s.stored_by_gate, s.stored_by_class, s.zstd_blocks),
            (1, 1, 0)
        );
    }

    #[test]
    fn window_and_level_reach_the_frame() {
        let data = prose(1 << 20);
        let (mut enc, handle) =
            ZstdEncoder::new(3, 12, DictionaryPolicy::None, Gate::DEFAULT).unwrap();
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let e = enc.encode(&data).unwrap();
        let fhd = e.bytes[4];
        assert_eq!(fhd & 0x20, 0, "not single-segment");
        // Window_Descriptor: exponent in the high five bits, base 2^10.
        assert_eq!(10 + u32::from(e.bytes[5] >> 3), 12);
        assert_eq!(e.resources.window, 1 << 12);
        assert_eq!(e.graph.steps[0].params[0], 12);
        for level in [1, 3, 19] {
            let (mut enc, handle) =
                ZstdEncoder::new(level, 20, DictionaryPolicy::None, Gate::DEFAULT).unwrap();
            handle.set_hint(Class::Text, DictionaryKind::Prose);
            assert!(enc.encode(&data).is_ok(), "level {level}");
        }
        assert!(ZstdEncoder::new(3, 9, DictionaryPolicy::None, Gate::DEFAULT).is_err());
        assert!(ZstdEncoder::new(3, 32, DictionaryPolicy::None, Gate::DEFAULT).is_err());
    }

    /// A tree with two tiny clusters (prose and source), a directory and a random file.
    fn small_tree(p: &Path) {
        std::fs::create_dir(p.join("d")).unwrap();
        std::fs::write(p.join("a.txt"), prose(5000)).unwrap();
        std::fs::write(p.join("b.c"), "int main(void) { return 0; }\n".repeat(100)).unwrap();
        std::fs::write(p.join("d").join("r.bin"), random(100_000)).unwrap();
    }

    #[test]
    fn each_cluster_gets_its_own_block_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        small_tree(dir.path());
        let mut out = Vec::new();
        let (ws, fs) = archive_fast(dir.path(), &mut out, FastOptions::default()).unwrap();
        assert_eq!(ws.entries, 4);
        // Prose, source, and the random file's own block: three blocks, never merged.
        let (a, files) = extract_all(out);
        assert_eq!(a.index().blocks.len(), 3);
        assert_eq!(fs.zstd_blocks, 2);
        assert_eq!(fs.stored_by_gate + fs.stored_by_class, 1);
        assert_eq!(files.len(), 3);
        for (path, bytes) in files {
            assert_eq!(
                bytes,
                std::fs::read(dir.path().join(&path)).unwrap(),
                "{path}"
            );
        }
        // Two dictionaries: prose and source.
        assert_eq!(a.priors().len(), 2);
    }

    #[test]
    fn archive_fast_file_refuses_to_overwrite_and_excludes_its_output() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let arch = dir.path().join("out.lpk");
        let (ws, _) = archive_fast_file(dir.path(), &arch, FastOptions::default()).unwrap();
        assert_eq!(ws.entries, 17);
        let (_, files) = extract_all(std::fs::read(&arch).unwrap());
        assert!(!files.iter().any(|(p, _)| p == "out.lpk"));
        assert_eq!(files.len(), 12);
        let again = archive_fast_file(dir.path(), &arch, FastOptions::default());
        assert!(matches!(again, Err(CoreError::Io { .. })));
        assert!(arch.exists(), "the first archive survives a refused rerun");
        clear_readonly(&dir.path().join("ro.txt"));
    }

    #[test]
    fn a_failed_fast_file_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let arch = dir.path().join("out.lpk");
        let r = archive_fast_file(
            &dir.path().join("missing-root"),
            &arch,
            FastOptions::default(),
        );
        assert!(r.is_err());
        assert!(!arch.exists());
    }
}
