//! The Fast tier: one zstd frame per block, a long match window, optional caller-supplied
//! dictionaries, and blocks that never span two clusters.
//!
//! Files are grouped by [`cluster`] (class and dictionary kind) and fed to the writer cluster by
//! cluster with `Writer::close_block` between them; before each cluster the pipeline tells the
//! encoder, through a [`FastHandle`], the class and dictionary kind of what follows. The encoder
//! stores a block without trying zstd when its class is already compressed, or when the entropy
//! gate calls the block incompressible, and falls back to `store` when zstd does not shrink it.
//!
//! No dictionary is bundled; by default (`DictionaryPolicy::None`) none is used. Extracting an
//! archive written with `DictionaryPolicy::Provided` needs the same dictionaries: through
//! `lpk_format::Archive` pass the [`ProvidedDictionaries`] to `set_priors`; with the format tool,
//! give one `--prior <dictionary file>` per dictionary.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use lpk_format::{
    BlockEncoder, Encoded, FormatError, Graph, PrimitiveId, Step, StoreEncoder, WriterSummary,
    DEFAULT_BLOCK_SIZE,
};
use std::io::Write;
use zstd::zstd_safe::CParameter;

use crate::classify::Class;
use crate::cluster::DictionaryKind;
use crate::error::CoreError;
use crate::gate::Gate;
use crate::ingest::IngestOptions;
use crate::pipeline::Pipeline;
use crate::priors::ProvidedDictionaries;

/// Smallest and largest `window_log` a `zstd` step may declare (the format's range; libzstd's
/// own bounds on this target are checked when the encoder is built).
const WINDOW_LOG_RANGE: std::ops::RangeInclusive<u32> = 10..=31;

/// Which dictionaries blocks may use.
#[derive(Debug, Clone, Default)]
pub enum DictionaryPolicy {
    /// Never use a dictionary.
    #[default]
    None,
    /// Use the dictionary the caller names for the cluster's kind (a kind without one gets
    /// none). The archive then needs the same dictionaries to be read.
    Provided(Arc<ProvidedDictionaries>),
}

/// Settings of [`archive_fast`].
#[derive(Debug, Clone)]
pub struct FastOptions {
    /// How the tree is walked.
    pub ingest: IngestOptions,
    /// zstd compression level. The default, 3, is the setting the Phase 0 G4 proxy measured.
    pub level: i32,
    /// log2 of the match window in bytes, 10 to 31: how far back the encoder may match. The
    /// default, 27, is the window of the catalogue's zstd row (`--long=27`). It is the maximum
    /// of the window each block declares (a block shorter than the window declares a smaller
    /// one), not a memory figure the decoder reserves up front.
    pub window_log: u32,
    /// Plain bytes per block (the format's default).
    pub block_size: u64,
    /// Caller-supplied dictionaries (default: none).
    pub dictionaries: DictionaryPolicy,
    /// The entropy gate that sends incompressible blocks to `store`.
    pub gate: Gate,
    /// Longest `Jpeg`-classed input the JPEG peel reads whole; a longer one takes the streaming
    /// path and is counted as a `SizeCap` fallback before any byte is read. Default
    /// [`crate::peel::LEPTON_MAX_FILE`], `lepton_jpeg`'s own file-size cap.
    pub jpeg_max_file: u64,
}

impl Default for FastOptions {
    fn default() -> Self {
        FastOptions {
            ingest: IngestOptions::default(),
            level: 3,
            window_log: 27,
            block_size: DEFAULT_BLOCK_SIZE,
            dictionaries: DictionaryPolicy::None,
            gate: Gate::DEFAULT,
            jpeg_max_file: crate::peel::LEPTON_MAX_FILE,
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
    /// What the peel stages did before the encoder saw the blocks (set by the pipeline).
    pub peel: crate::peel::PeelSummary,
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
    shared: Arc<Mutex<Shared>>,
    /// The compressor without a dictionary.
    plain: zstd::bulk::Compressor<'static>,
    /// One compressor per provided dictionary: kind, prior id, compressor.
    with_dict: Vec<(DictionaryKind, [u8; 32], zstd::bulk::Compressor<'static>)>,
}

impl std::fmt::Debug for ZstdEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZstdEncoder")
            .field("level", &self.level)
            .field("window_log", &self.window_log)
            .field("dictionaries", &self.with_dict.len())
            .finish_non_exhaustive()
    }
}

/// `ceil(log2(n))`, 0 for `n <= 1`.
fn ceil_log2(n: usize) -> u32 {
    if n <= 1 {
        0
    } else {
        usize::BITS - (n - 1).leading_zeros()
    }
}

/// A compressor with the Fast tier's fixed parameters; every parameter is checked against
/// libzstd's bounds here, so a bad option fails at construction and names the parameter.
fn build_compressor(
    level: i32,
    window_log: u32,
    dictionary: Option<&[u8]>,
) -> Result<zstd::bulk::Compressor<'static>, CoreError> {
    let bad = |name: &str, e: std::io::Error| CoreError::InvalidOption(format!("{name}: {e}"));
    if let Some(d) = dictionary {
        // Raw bytes without zstd's dictionary magic would load as raw content and the frame
        // would then not name a dictionary the reader accepts.
        if d.get(..4) != Some(&[0x37, 0xA4, 0x30, 0xEC]) {
            return Err(CoreError::InvalidOption(
                "dictionary: missing the zstd dictionary magic".into(),
            ));
        }
    }
    let mut c = match dictionary {
        Some(d) => zstd::bulk::Compressor::with_dictionary(level, d),
        None => zstd::bulk::Compressor::new(level),
    }
    .map_err(|e| bad("dictionary", e))?;
    c.set_parameter(CParameter::WindowLog(window_log))
        .map_err(|e| bad("window_log", e))?;
    c.set_parameter(CParameter::EnableLongDistanceMatching(true))
        .map_err(|e| bad("long_distance_matching", e))?;
    // The format hashes every frame and chunk; the content size is written.
    c.set_parameter(CParameter::ChecksumFlag(false))
        .map_err(|e| bad("checksum", e))?;
    c.set_parameter(CParameter::ContentSizeFlag(true))
        .map_err(|e| bad("content_size", e))?;
    if dictionary.is_some() {
        // A malformed dictionary with the magic fails here, before any output exists.
        c.compress(b"").map_err(|e| bad("dictionary", e))?;
    }
    Ok(c)
}

impl ZstdEncoder {
    /// A new encoder and the handle that steers it. Errors (`InvalidOption`, naming the
    /// parameter) when `level`, `window_log` or a dictionary is not accepted.
    pub fn new(
        level: i32,
        window_log: u32,
        policy: &DictionaryPolicy,
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
        let plain = build_compressor(level, window_log, None)?;
        let mut with_dict = Vec::new();
        if let DictionaryPolicy::Provided(set) = policy {
            for (kind, id, bytes) in set.iter() {
                with_dict.push((kind, id, build_compressor(level, window_log, Some(bytes))?));
            }
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
            shared: Arc::clone(&shared),
            plain,
            with_dict,
        };
        Ok((enc, FastHandle(shared)))
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
        // RFC 8878: a frame whose content size is known and fits its window is written
        // single-segment, and its Window_Size is then its content size. So a block shorter than
        // 2^w declares w (at least 10, the smallest the step allows) and the declaration holds;
        // a longer block declares the configured window, which libzstd honours as a maximum.
        let declared = self.window_log.min(ceil_log2(plain.len()).max(10));
        let (id, compressor) = match self.with_dict.iter_mut().find(|(k, _, _)| *k == kind) {
            Some((_, id, c)) => (*id, c),
            None => ([0u8; 32], &mut self.plain),
        };
        compressor.set_parameter(CParameter::WindowLog(declared))?;
        let bytes = compressor.compress(plain)?;
        if bytes.len() >= plain.len() {
            self.bump(|s| s.stored_no_gain += 1);
            return StoreEncoder.encode(plain);
        }
        self.bump(|s| s.zstd_blocks += 1);
        let mut params = vec![declared as u8];
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

/// Walk `root` and write it with the Fast tier to `out` (a front over
/// [`Pipeline::run`]).
pub fn archive_fast(
    root: &Path,
    out: impl Write,
    options: FastOptions,
) -> Result<(WriterSummary, FastSummary), CoreError> {
    let s = Pipeline::fast(options).run(root, out)?;
    Ok((s.writer, s.fast.unwrap_or_default()))
}

/// Like [`archive_fast`], writing to a new file at `archive_path` with the guarantees of
/// `archive_store_file`: create-new, the output left out of its own archive, data synced before
/// the trailer, a failed write removes the file. Options (including provided dictionaries) are
/// validated before the output file exists.
pub fn archive_fast_file(
    root: &Path,
    archive_path: &Path,
    options: FastOptions,
) -> Result<(WriterSummary, FastSummary), CoreError> {
    let s = Pipeline::fast(options).run_file(root, archive_path)?;
    Ok((s.writer, s.fast.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::tests::{clear_readonly, make_tree};
    use lpk_format::{Archive, EntryKind, Resources, Writer, WriterOptions};
    use std::io::Cursor;

    /// Compressible English-like text of exactly `len` bytes, varied by `seed`.
    fn prose_seeded(seed: u64, len: usize) -> Vec<u8> {
        const WORDS: [&str; 16] = [
            "the", "king", "of", "France", "and", "his", "army", "marched", "over", "a", "long",
            "road", "toward", "the", "sea", "while",
        ];
        let mut s = 0x9E37_79B9_7F4A_7C15u64 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
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

    fn prose(len: usize) -> Vec<u8> {
        prose_seeded(0, len)
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

    /// A prose dictionary trained at test time on synthetic samples.
    fn trained() -> Vec<u8> {
        let samples: Vec<Vec<u8>> = (1..400).map(|i| prose_seeded(i, 900)).collect();
        zstd::dict::from_samples(&samples, 8 * 1024).unwrap()
    }

    fn provided() -> (DictionaryPolicy, Arc<ProvidedDictionaries>) {
        let set = Arc::new(ProvidedDictionaries::new().with(DictionaryKind::Prose, trained()));
        (DictionaryPolicy::Provided(Arc::clone(&set)), set)
    }

    fn encoder(policy: &DictionaryPolicy) -> (ZstdEncoder, FastHandle) {
        ZstdEncoder::new(3, 27, policy, Gate::DEFAULT).unwrap()
    }

    type Extracted = (Archive<Cursor<Vec<u8>>>, Vec<(String, Vec<u8>)>);

    /// Read every file of an archive back; `priors` are the dictionaries it needs, if any.
    fn extract_with(
        bytes: Vec<u8>,
        resources: &Resources,
        priors: Option<Arc<ProvidedDictionaries>>,
    ) -> Extracted {
        let mut a = Archive::open(Cursor::new(bytes), resources).unwrap();
        if let Some(p) = priors {
            struct Shared(Arc<ProvidedDictionaries>);
            impl lpk_format::PriorStore for Shared {
                fn get(&self, id: &[u8; 32]) -> Option<&[u8]> {
                    self.0.get(id)
                }
            }
            a.set_priors(Box::new(Shared(p)));
        }
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

    fn extract_all(bytes: Vec<u8>) -> Extracted {
        extract_with(bytes, &Resources::default(), None)
    }

    /// Write one file with a hand-steered encoder; returns the archive.
    fn one_file(enc: ZstdEncoder, data: &[u8]) -> Vec<u8> {
        let wopts = WriterOptions {
            encoder: Box::new(enc),
            ..WriterOptions::default()
        };
        let mut out = Vec::new();
        let mut w = Writer::new(&mut out, wopts).unwrap();
        w.add_file("p.txt", Default::default(), 0, &mut &data[..])
            .unwrap();
        w.finish().unwrap();
        out
    }

    #[test]
    fn a_block_with_a_provided_dictionary_lists_its_id_once_and_decodes() {
        let data = prose(1 << 20);
        let (policy, set) = provided();
        let (enc, handle) = encoder(&policy);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let out = one_file(enc, &data);
        let s = handle.summary();
        assert_eq!(
            (s.zstd_blocks, s.stored_by_gate, s.stored_by_class),
            (1, 0, 0)
        );
        assert!(out.len() < data.len() / 2);
        let (a, files) = extract_with(out, &Resources::default(), Some(Arc::clone(&set)));
        let want = set.dictionary(DictionaryKind::Prose).unwrap().0;
        assert_eq!(a.priors(), &[want]);
        assert_eq!(files[0].1, data);
        // A kind without a provided dictionary gets none.
        let (mut enc, handle) = encoder(&policy);
        handle.set_hint(Class::Text, DictionaryKind::Source);
        let e = enc.encode(&data).unwrap();
        assert!(e.graph.prior_ids().is_empty());
    }

    #[test]
    fn without_dictionaries_the_step_id_is_zero_and_the_frame_names_none() {
        let data = prose(1 << 20);
        let (mut enc, handle) = encoder(&DictionaryPolicy::None);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let e = enc.encode(&data).unwrap();
        assert_eq!(e.graph.steps[0].primitive, PrimitiveId::Zstd);
        assert_eq!(e.graph.steps[0].params[0], 20);
        assert_eq!(&e.graph.steps[0].params[1..], &[0u8; 32]);
        assert!(e.graph.prior_ids().is_empty());
        // Frame_Header_Descriptor bits 0..1 are Dictionary_ID_flag.
        assert_eq!(e.bytes[4] & 0b11, 0);
        assert_eq!(e.resources.window, 1 << 20);
        // With a dictionary the frame names it and the step carries its id.
        let (policy, set) = provided();
        let (mut enc, handle) = encoder(&policy);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let e = enc.encode(&data).unwrap();
        assert_ne!(e.bytes[4] & 0b11, 0);
        assert_eq!(e.graph.prior_ids().len(), 1);
        assert_eq!(
            e.graph.prior_ids()[0],
            lpk_format::prior_id(set.dictionary(DictionaryKind::Prose).unwrap().1)
        );
    }

    #[test]
    fn random_blocks_are_stored_by_the_gate_and_video_by_class() {
        let data = random(1 << 20);
        let (mut enc, handle) = encoder(&DictionaryPolicy::None);
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
    fn a_tiny_block_that_zstd_cannot_shrink_is_stored_and_counted() {
        let (mut enc, handle) = encoder(&DictionaryPolicy::None);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let e = enc.encode(b"hi there").unwrap();
        assert_eq!(e.graph.steps[0].primitive, PrimitiveId::Store);
        assert_eq!(e.bytes, b"hi there");
        let s = handle.summary();
        assert_eq!(
            (s.stored_no_gain, s.zstd_blocks, s.stored_by_gate),
            (1, 0, 0)
        );
    }

    #[test]
    fn window_and_level_reach_the_frame() {
        let data = prose(1 << 20);
        let (mut enc, handle) =
            ZstdEncoder::new(3, 12, &DictionaryPolicy::None, Gate::DEFAULT).unwrap();
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
                ZstdEncoder::new(level, 20, &DictionaryPolicy::None, Gate::DEFAULT).unwrap();
            handle.set_hint(Class::Text, DictionaryKind::Prose);
            assert!(enc.encode(&data).is_ok(), "level {level}");
        }
        let none = DictionaryPolicy::None;
        for (level, wl, name) in [
            (3, 9, "window_log"),
            (3, 32, "window_log"),
            (99, 20, "level"),
        ] {
            match ZstdEncoder::new(level, wl, &none, Gate::DEFAULT) {
                Err(CoreError::InvalidOption(m)) => assert!(m.contains(name), "{m}"),
                other => panic!("{other:?}"),
            }
        }
        // A dictionary that is not a dictionary at all still loads as raw content in libzstd,
        // so only the parameters are rejected here.
    }

    #[test]
    fn a_provided_dictionary_is_validated_before_any_output_exists() {
        let raw = DictionaryPolicy::Provided(Arc::new(
            ProvidedDictionaries::new().with(DictionaryKind::Prose, b"just some bytes".to_vec()),
        ));
        match ZstdEncoder::new(3, 20, &raw, Gate::DEFAULT) {
            Err(CoreError::InvalidOption(m)) => assert!(m.contains("dictionary"), "{m}"),
            other => panic!("{other:?}"),
        }
        // Magic present but nothing after it.
        let hollow = DictionaryPolicy::Provided(Arc::new(ProvidedDictionaries::new().with(
            DictionaryKind::Prose,
            vec![0x37, 0xA4, 0x30, 0xEC, 1, 2, 3, 4],
        )));
        assert!(ZstdEncoder::new(3, 20, &hollow, Gate::DEFAULT).is_err());
        let (good, _) = provided();
        assert!(ZstdEncoder::new(3, 20, &good, Gate::DEFAULT).is_ok());
        // archive_fast_file refuses before it creates the file.
        let dir = tempfile::tempdir().unwrap();
        let arch = dir.path().join("out.lpk");
        let options = FastOptions {
            dictionaries: raw,
            ..FastOptions::default()
        };
        assert!(archive_fast_file(dir.path(), &arch, options).is_err());
        assert!(!arch.exists());
    }

    #[test]
    fn a_small_block_declares_a_small_window_and_decodes_under_a_small_limit() {
        let data = prose(3000);
        let (enc, handle) = encoder(&DictionaryPolicy::None);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let out = one_file(enc, &data);
        let mut probe = encoder(&DictionaryPolicy::None);
        probe.1.set_hint(Class::Text, DictionaryKind::Prose);
        let e = probe.0.encode(&data).unwrap();
        // ceil(log2(3000)) = 12; the content fits the window, so the frame is single-segment.
        assert_eq!(e.graph.steps[0].params[0], 12);
        assert_eq!(e.resources.window, 1 << 12);
        assert_ne!(e.bytes[4] & 0x20, 0, "single-segment");
        let small = Resources {
            max_window: 1 << 13,
            ..Resources::default()
        };
        let (_, files) = extract_with(out, &small, None);
        assert_eq!(files[0].1, data);
        // Tiny blocks never declare less than the step's minimum of 10.
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1025), 11);
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
        // The default policy uses no dictionary.
        assert!(a.priors().is_empty());
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
