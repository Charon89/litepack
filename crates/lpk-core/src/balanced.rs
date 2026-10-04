//! The Balanced tier: one raw LZMA1 stream per block (liblzma, a large dictionary), with zstd
//! `--ultra --long` as the alternative a trial on a sample of the block can prefer.
//!
//! Files are grouped by [`crate::cluster::cluster`] with a soft boundary: compressible clusters
//! are fed first and the stored-by-class clusters last, and a block closes at a cluster change
//! only once it holds `min_block_before_boundary` bytes (default half the dictionary); a block
//! takes the hint of its first cluster and stored-by-class data never joins an LZMA or zstd
//! block. The pipeline tells the encoder, through a [`BalancedHandle`], the class of what
//! follows. There is no entropy gate in this tier: a block is stored only by the class rule or
//! because the winner's output is not smaller than the plain bytes.
//!
//! The trial is like for like: the zstd window defaults to the LZMA dictionary's log2, and the
//! sample is four stripes (at 0, 1/4, 1/2 and 3/4 of the block, each a quarter of `sample_len`,
//! or the whole block when shorter) compressed separately by each candidate, the outputs summed;
//! the smaller sum encodes the block, `lzma` on a tie.
//!
//! The `lzma` step declares the dictionary the encoder used: the configured one, or the next
//! power of two above the block's length when that is smaller. The default configured
//! dictionary, 64 MiB, is the one the 7-Zip Ultra catalogue row uses, so a parity check compares
//! like with like. No filters (BCJ waits for a format revision).
//!
//! Memory, qualitatively: compressing needs about the block size plus several times the
//! dictionary; extracting needs the block size plus the dictionary.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use liblzma::stream::{
    Action, Filters, LzmaOptions, MatchFinder, Mode, MtStreamBuilder, Status, Stream,
    PRESET_EXTREME,
};
use lpk_format::{
    BlockEncoder, Encoded, FormatError, Graph, PrimitiveId, Step, StoreEncoder, WriterSummary,
};

use crate::classify::Class;
use crate::cluster::DictionaryKind;
use crate::error::CoreError;
use crate::fast::{stored_by_class, DictionaryPolicy, FastHandle, ZstdEncoder};
use crate::gate::Gate;
use crate::ingest::IngestOptions;
use crate::pipeline::Pipeline;

/// Smallest and largest dictionary liblzma accepts for LZMA1 (4 KiB; 1.5 GiB).
const DICT_RANGE: std::ops::RangeInclusive<u32> = (1 << 12)..=(3 << 29);

/// Settings of [`archive_balanced`].
#[derive(Debug, Clone)]
pub struct BalancedOptions {
    /// How the tree is walked.
    pub ingest: IngestOptions,
    /// The LZMA dictionary in bytes (4 KiB to 1.5 GiB), declared by every `lzma` step. Default
    /// 64 MiB, the dictionary of the 7-Zip Ultra catalogue row.
    pub dict_size: u32,
    /// zstd level of the alternative candidate (default 22, `--ultra`).
    pub zstd_level: i32,
    /// log2 of the zstd window, 10 to 28 (the reader's default `max_window` is 256 MiB).
    /// `None` (the default) is the log2 of `dict_size`, so both candidates reach the same distance.
    pub zstd_window_log: Option<u32>,
    /// Plain bytes the trial compresses per block, in four stripes (default 4 MiB).
    pub sample_len: usize,
    /// Plain bytes per block (default 256 MiB).
    pub block_size: u64,
    /// A block closes at a cluster change only once it holds this many bytes. `None` (the
    /// default) is half of `dict_size`.
    pub min_block_before_boundary: Option<u64>,
    /// Longest `Jpeg`-classed input the JPEG peel reads whole (see `FastOptions`).
    pub jpeg_max_file: u64,
}

impl Default for BalancedOptions {
    fn default() -> Self {
        BalancedOptions {
            ingest: IngestOptions::default(),
            dict_size: 64 << 20,
            zstd_level: 22,
            zstd_window_log: None,
            sample_len: 4 << 20,
            block_size: 256 << 20,
            min_block_before_boundary: None,
            jpeg_max_file: crate::peel::LEPTON_MAX_FILE,
        }
    }
}

/// What the Balanced encoder did with the blocks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BalancedSummary {
    /// Blocks coded with LZMA.
    pub lzma_blocks: u64,
    /// Blocks coded with zstd.
    pub zstd_blocks: u64,
    /// Always zero in this tier (it has no entropy gate); kept so the counts match the Fast tier's.
    pub stored_by_gate: u64,
    /// Blocks stored without a trial because their class is already compressed.
    pub stored_by_class: u64,
    /// Blocks stored because the winner's output was not smaller than the plain bytes.
    pub stored_no_gain: u64,
    /// Plain bytes the trials fed to each candidate.
    pub sample_bytes: u64,
    /// Approximate: liblzma's LZMA2 encoder memory query at one thread for these options (the
    /// safe API has no raw LZMA1 query); 0 when the query failed. Reported, not measured.
    pub lzma_encoder_memory: u64,
    /// What the peel stages did before the encoder saw the blocks (set by the pipeline).
    pub peel: crate::peel::PeelSummary,
}

fn io_err(e: impl std::fmt::Display) -> FormatError {
    FormatError::Io(std::io::Error::other(e.to_string()))
}

/// liblzma's raw LZMA1 encoder with the Balanced parameters.
#[derive(Debug, Clone)]
pub struct LzmaEncoder {
    dict_size: u32,
    lc: u8,
    lp: u8,
    pb: u8,
}

impl LzmaEncoder {
    /// An encoder declaring `dict_size`; `lc`, `lp`, `pb` are the LZMA properties (`lc <= 8`,
    /// `lp <= 4`, `pb <= 4`, `lc + lp <= 4`, the format's bounds). Errors with `InvalidOption`
    /// naming the parameter.
    pub fn new(dict_size: u32, lc: u8, lp: u8, pb: u8) -> Result<Self, CoreError> {
        let bad = |m: String| Err(CoreError::InvalidOption(m));
        if !DICT_RANGE.contains(&dict_size) {
            return bad(format!(
                "dict_size {dict_size} is outside {}..={}",
                DICT_RANGE.start(),
                DICT_RANGE.end()
            ));
        }
        if lc > 8 || lp > 4 || pb > 4 {
            return bad(format!(
                "lc {lc}, lp {lp}, pb {pb}: need lc <= 8, lp <= 4, pb <= 4"
            ));
        }
        if lc + lp > 4 {
            return bad(format!("lc + lp is {}, more than 4", lc + lp));
        }
        Ok(LzmaEncoder {
            dict_size,
            lc,
            lp,
            pb,
        })
    }

    /// The Balanced default parameters: `lc` 3, `lp` 0, `pb` 2.
    pub fn with_dict(dict_size: u32) -> Result<Self, CoreError> {
        Self::new(dict_size, 3, 0, 2)
    }

    /// The dictionary every step declares.
    pub fn dict_size(&self) -> u32 {
        self.dict_size
    }

    /// The dictionary used for `len` plain bytes: the configured one, or the next power of two
    /// above `len` (at least 4 KiB) when that is smaller.
    fn dict_for(&self, len: usize) -> u32 {
        let need = (len as u64).next_power_of_two().max(1 << 12);
        u32::try_from(need.min(u64::from(self.dict_size))).unwrap_or(self.dict_size)
    }

    fn graph(&self, dict: u32) -> Graph {
        let mut params = dict.to_le_bytes().to_vec();
        params.extend_from_slice(&[self.lc, self.lp, self.pb]);
        Graph {
            steps: vec![Step {
                primitive: PrimitiveId::Lzma,
                params,
            }],
        }
    }

    /// The raw stream (end marker included) for `plain` with dictionary `dict`.
    fn compress(&self, plain: &[u8], dict: u32) -> Result<Vec<u8>, FormatError> {
        let o = self.options(dict)?;
        let mut filters = Filters::new();
        filters.lzma1(&o);
        let mut s = Stream::new_raw_encoder(&filters).map_err(io_err)?;
        let mut out = Vec::with_capacity(plain.len() / 3 + 64);
        loop {
            let consumed = usize::try_from(s.total_in()).unwrap_or(plain.len());
            let st = s
                .process_vec(&plain[consumed..], &mut out, Action::Finish)
                .map_err(io_err)?;
            if st == Status::StreamEnd {
                break;
            }
            out.reserve(1 << 16);
        }
        Ok(out)
    }

    /// Preset 9 extreme (binary-tree match finder, deepest search) with the dictionary and the
    /// properties overridden and `nice_len` 273.
    fn options(&self, dict: u32) -> Result<LzmaOptions, FormatError> {
        let mut o = LzmaOptions::new_preset(9 | PRESET_EXTREME).map_err(io_err)?;
        o.dict_size(dict)
            .literal_context_bits(u32::from(self.lc))
            .literal_position_bits(u32::from(self.lp))
            .position_bits(u32::from(self.pb))
            .nice_len(273)
            .match_finder(MatchFinder::BinaryTree4)
            .mode(Mode::Normal);
        Ok(o)
    }

    /// Approximate: liblzma's LZMA2 encoder memory query at one thread for the configured
    /// dictionary (see [`BalancedSummary::lzma_encoder_memory`]); 0 when the options are refused.
    pub fn memory_usage(&self) -> u64 {
        let Ok(o) = self.options(self.dict_size) else {
            return 0;
        };
        let mut filters = Filters::new();
        filters.lzma2(&o);
        MtStreamBuilder::new()
            .filters(filters)
            .threads(1)
            .block_size(4096)
            .memusage()
    }
}

impl BlockEncoder for LzmaEncoder {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        let dict = self.dict_for(plain.len());
        let graph = self.graph(dict);
        Ok(Encoded {
            resources: graph.resources(),
            graph,
            bytes: self.compress(plain, dict)?,
        })
    }
}

#[derive(Debug)]
struct Shared {
    class: Class,
    summary: BalancedSummary,
}

/// The pipeline's side of the encoder: sets what the next blocks hold and reads the counts.
#[derive(Debug, Clone)]
pub struct BalancedHandle {
    shared: Arc<Mutex<Shared>>,
    zstd: FastHandle,
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl BalancedHandle {
    /// The class and dictionary kind of the blocks encoded from now on.
    pub fn set_hint(&self, class: Class, dictionary: DictionaryKind) {
        lock(&self.shared).class = class;
        self.zstd.set_hint(class, dictionary);
    }

    /// The counts so far.
    pub fn summary(&self) -> BalancedSummary {
        lock(&self.shared).summary
    }
}

/// The Balanced tier's block encoder: gate, class rule, the lzma-or-zstd trial.
pub struct BalancedEncoder {
    lzma: LzmaEncoder,
    zstd: ZstdEncoder,
    sample_len: usize,
    shared: Arc<Mutex<Shared>>,
}

impl std::fmt::Debug for BalancedEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BalancedEncoder")
            .field("lzma", &self.lzma)
            .field("zstd", &self.zstd)
            .field("sample_len", &self.sample_len)
            .finish_non_exhaustive()
    }
}

impl BalancedEncoder {
    /// A new encoder and the handle that steers it; every option is checked here
    /// (`InvalidOption`).
    pub fn new(o: &BalancedOptions) -> Result<(Self, BalancedHandle), CoreError> {
        if o.sample_len == 0 {
            return Err(CoreError::InvalidOption("sample_len is 0".into()));
        }
        let lzma = LzmaEncoder::with_dict(o.dict_size)?;
        if u64::from(o.dict_size) > lpk_format::DEFAULT_MAX_WINDOW {
            return Err(CoreError::InvalidOption(format!(
                "dict_size {} is above the reader's default max_window ({})",
                o.dict_size,
                lpk_format::DEFAULT_MAX_WINDOW
            )));
        }
        let window_log = o.zstd_window_log.unwrap_or(o.dict_size.ilog2());
        let (zstd, zhandle) =
            ZstdEncoder::new(o.zstd_level, window_log, &DictionaryPolicy::None, Gate::OFF)?;
        let shared = Arc::new(Mutex::new(Shared {
            class: Class::Other,
            summary: BalancedSummary {
                lzma_encoder_memory: lzma.memory_usage(),
                ..BalancedSummary::default()
            },
        }));
        let enc = BalancedEncoder {
            lzma,
            zstd,
            sample_len: o.sample_len,
            shared: Arc::clone(&shared),
        };
        Ok((
            enc,
            BalancedHandle {
                shared,
                zstd: zhandle,
            },
        ))
    }

    fn bump(&self, f: impl FnOnce(&mut BalancedSummary)) {
        f(&mut lock(&self.shared).summary);
    }
}

/// Whether zstd beats lzma on the sample: only a strictly smaller output wins; a tie is lzma's.
fn zstd_wins(lzma_len: usize, zstd_len: usize) -> bool {
    zstd_len < lzma_len
}

impl BlockEncoder for BalancedEncoder {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        let class = lock(&self.shared).class;
        if stored_by_class(class) {
            self.bump(|s| s.stored_by_class += 1);
            return StoreEncoder.encode(plain);
        }
        // Four stripes (the whole block when it fits the sample), each compressed separately
        // by each candidate; the zstd encoder's own no-gain store counts as the stripe's length.
        let whole = plain.len() <= self.sample_len;
        let stripes: Vec<&[u8]> = if whole {
            vec![plain]
        } else {
            let q = self.sample_len / 4;
            (0..4)
                .map(|i| {
                    let at = plain.len() / 4 * i;
                    &plain[at..plain.len().min(at + q)]
                })
                .collect()
        };
        let (mut l_len, mut z_len) = (0usize, 0usize);
        let mut first = None;
        for st in &stripes {
            let l = self.lzma.encode(st)?;
            let z = self.zstd.encode(st)?;
            l_len += l.bytes.len();
            z_len += z.bytes.len();
            first.get_or_insert((l, z));
            self.bump(|s| s.sample_bytes += st.len() as u64);
        }
        let zstd = zstd_wins(l_len, z_len);
        let winner = match (whole, zstd) {
            (true, true) => first.map(|(_, z)| z).ok_or_else(|| io_err("no stripe"))?,
            (true, false) => first.map(|(l, _)| l).ok_or_else(|| io_err("no stripe"))?,
            (false, true) => self.zstd.encode(plain)?,
            (false, false) => self.lzma.encode(plain)?,
        };
        if winner.bytes.len() >= plain.len() {
            self.bump(|s| s.stored_no_gain += 1);
            return StoreEncoder.encode(plain);
        }
        if zstd {
            self.bump(|s| s.zstd_blocks += 1);
        } else {
            self.bump(|s| s.lzma_blocks += 1);
        }
        Ok(winner)
    }
}

/// Walk `root` and write it with the Balanced tier to `out` (a front over [`Pipeline::run`]).
pub fn archive_balanced(
    root: &Path,
    out: impl Write,
    options: BalancedOptions,
) -> Result<(WriterSummary, BalancedSummary), CoreError> {
    let s = Pipeline::balanced(options).run(root, out)?;
    Ok((s.writer, s.balanced.unwrap_or_default()))
}

/// Like [`archive_balanced`], writing to a new file at `archive_path` with the guarantees of
/// `archive_store_file`: create-new, the output left out of its own archive, data synced before
/// the trailer, a failed write removes the file; options are validated before the file exists.
pub fn archive_balanced_file(
    root: &Path,
    archive_path: &Path,
    options: BalancedOptions,
) -> Result<(WriterSummary, BalancedSummary), CoreError> {
    let s = Pipeline::balanced(options).run_file(root, archive_path)?;
    Ok((s.writer, s.balanced.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::tests::{clear_readonly, make_tree};
    use lpk_format::{Archive, EntryKind, Resources, Writer, WriterOptions};
    use std::io::Cursor;

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

    /// Log-like lines: timestamps, levels and ids with a few free digits.
    fn logs(seed: u64, len: usize) -> Vec<u8> {
        let mut s = 0x9E37_79B9_7F4A_7C15u64 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
        let mut out = Vec::with_capacity(len + 128);
        let mut t = 1_700_000_000u64;
        while out.len() < len {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            t += s % 3;
            let level = ["INFO", "INFO", "WARN", "DEBUG"][(s >> 8) as usize % 4];
            let line = format!(
                "{t} {level} worker-{} handled request id={} in {} ms from 10.0.{}.{}
",
                s % 7,
                (s >> 16) % 100_000,
                (s >> 40) % 900,
                (s >> 20) % 4,
                (s >> 28) % 250
            );
            out.extend_from_slice(line.as_bytes());
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

    fn write_one(opts: &BalancedOptions, class: Class, data: &[u8]) -> (Vec<u8>, BalancedSummary) {
        let (enc, handle) = BalancedEncoder::new(opts).unwrap();
        handle.set_hint(class, DictionaryKind::None);
        let wopts = WriterOptions {
            encoder: Box::new(enc),
            ..WriterOptions::default()
        };
        let mut out = Vec::new();
        let mut w = Writer::new(&mut out, wopts).unwrap();
        w.add_file("p.txt", Default::default(), 0, &mut &data[..])
            .unwrap();
        w.finish().unwrap();
        (out, handle.summary())
    }

    fn read_back(bytes: &[u8], limits: &Resources) -> Vec<u8> {
        let mut a = Archive::open(Cursor::new(bytes), limits).unwrap();
        let table = a.entry_table().unwrap();
        let e = table.table().unwrap().iter().next().unwrap().unwrap();
        let mut out = Vec::new();
        a.extract(&e, &mut out).unwrap();
        out
    }

    #[test]
    fn an_lzma_block_round_trips_through_the_formats_decoder() {
        let data = logs(1, 3 << 20);
        let mut enc = LzmaEncoder::with_dict(1 << 24).unwrap();
        let e = enc.encode(&data).unwrap();
        assert_eq!(e.graph.steps[0].primitive, PrimitiveId::Lzma);
        assert_eq!(
            e.graph.steps[0].params,
            [0, 0, 0x40, 0, 3, 0, 2],
            "3 MiB of data: the next power of two, 4 MiB, LE; lc 3, lp 0, pb 2"
        );
        assert_eq!(e.resources.window, 1 << 22);
        assert!(e.bytes.len() < data.len() / 2);
        let (out, s) = write_one(
            &BalancedOptions {
                dict_size: 1 << 24,
                ..BalancedOptions::default()
            },
            Class::Text,
            &data,
        );
        assert_eq!((s.lzma_blocks, s.zstd_blocks), (1, 0));
        // The reader accepts it: the marker is present (a final step may carry one).
        assert_eq!(read_back(&out, &Resources::default()), data);
        assert!(s.lzma_encoder_memory > 0);
    }

    #[test]
    fn construction_checks_the_bounds() {
        for (d, lc, lp, pb, name) in [
            (1 << 20, 3, 2, 2, "lc + lp"),
            (1 << 20, 9, 0, 2, "lc"),
            (1 << 20, 3, 0, 5, "pb"),
            (4095, 3, 0, 2, "dict_size"),
            (u32::MAX, 3, 0, 2, "dict_size"),
        ] {
            match LzmaEncoder::new(d, lc, lp, pb) {
                Err(CoreError::InvalidOption(m)) => assert!(m.contains(name), "{m}"),
                other => panic!("{other:?}"),
            }
        }
        assert!(LzmaEncoder::new(4096, 0, 4, 0).is_ok());
        let bad = BalancedOptions {
            dict_size: 100,
            ..BalancedOptions::default()
        };
        assert!(matches!(
            BalancedEncoder::new(&bad),
            Err(CoreError::InvalidOption(_))
        ));
        let bad = BalancedOptions {
            zstd_window_log: Some(40),
            ..BalancedOptions::default()
        };
        assert!(BalancedEncoder::new(&bad).is_err());
    }

    #[test]
    fn the_trial_picks_lzma_on_text_and_zstd_where_its_sample_is_smaller() {
        let text = logs(2, 1 << 20);
        let opts = BalancedOptions::default();
        let (_, s) = write_one(&opts, Class::Text, &text);
        assert_eq!((s.lzma_blocks, s.zstd_blocks), (1, 0));
        assert_eq!(s.sample_bytes, text.len() as u64);
        // With a 4 KiB dictionary lzma cannot see the repeats of a 40 KB prose unit; zstd's
        // long window can.
        let unit = prose_seeded(3, 40_000);
        let repeated: Vec<u8> = unit.iter().cycle().take(unit.len() * 6).copied().collect();
        let opts = BalancedOptions {
            dict_size: 4096,
            zstd_window_log: Some(20),
            ..BalancedOptions::default()
        };
        let (out, s) = write_one(&opts, Class::Text, &repeated);
        assert_eq!((s.lzma_blocks, s.zstd_blocks), (0, 1));
        assert_eq!(read_back(&out, &Resources::default()), repeated);
        // The winner is the candidate with the smaller sample output.
        let (mut enc, h) = BalancedEncoder::new(&opts).unwrap();
        h.set_hint(Class::Text, DictionaryKind::None);
        let l = LzmaEncoder::with_dict(4096).unwrap().encode(&repeated);
        let won = enc.encode(&repeated).unwrap();
        assert!(won.bytes.len() < l.unwrap().bytes.len());
        assert_eq!(won.graph.steps[0].primitive, PrimitiveId::Zstd);
    }

    #[test]
    fn a_tie_goes_to_lzma() {
        assert!(!zstd_wins(100, 100));
        assert!(!zstd_wins(100, 101));
        assert!(zstd_wins(100, 99));
    }

    #[test]
    fn a_sample_shorter_than_the_block_decides_for_the_whole_block() {
        let data = logs(4, 1 << 20);
        let opts = BalancedOptions {
            sample_len: 64 << 10,
            ..BalancedOptions::default()
        };
        let (out, s) = write_one(&opts, Class::Text, &data);
        assert_eq!(s.sample_bytes, 64 << 10);
        assert_eq!(s.lzma_blocks + s.zstd_blocks, 1);
        assert_eq!(read_back(&out, &Resources::default()), data);
    }

    #[test]
    fn stored_blocks_skip_the_trial() {
        let opts = BalancedOptions::default();
        let (_, s) = write_one(&opts, Class::Video, &prose_seeded(5, 1 << 16));
        assert_eq!((s.stored_by_class, s.sample_bytes), (1, 0));
        // No entropy gate in this tier: random bytes are tried, and stored for no gain.
        let (_, s) = write_one(&opts, Class::Other, &random(1 << 20));
        assert_eq!((s.stored_by_gate, s.stored_no_gain), (0, 1));
        assert!(s.sample_bytes > 0);
        let (mut enc, h) = BalancedEncoder::new(&opts).unwrap();
        h.set_hint(Class::Text, DictionaryKind::Prose);
        let e = enc.encode(b"hi there").unwrap();
        assert_eq!(e.graph.steps[0].primitive, PrimitiveId::Store);
        assert_eq!(h.summary().stored_no_gain, 1);
    }

    fn tree(p: &Path) {
        std::fs::create_dir(p.join("d")).unwrap();
        std::fs::write(p.join("a.txt"), logs(6, 50_000)).unwrap();
        std::fs::write(p.join("b.c"), "int main(void) { return 0; }\n".repeat(200)).unwrap();
        std::fs::write(p.join("d").join("r.bin"), random(100_000)).unwrap();
    }

    #[test]
    fn a_tree_round_trips_and_the_envelope_names_the_largest_window() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());
        let opts = BalancedOptions {
            dict_size: 1 << 22,
            ..BalancedOptions::default()
        };
        let mut out = Vec::new();
        let (ws, s) = archive_balanced(dir.path(), &mut out, opts).unwrap();
        assert_eq!(ws.entries, 4);
        // The prose-like and source clusters share one block (soft boundary); the random file
        // is stored, in a block of its own.
        assert_eq!(s.lzma_blocks + s.zstd_blocks, 1, "{s:?}");
        assert_eq!(s.stored_by_class + s.stored_no_gain, 1, "{s:?}");
        let mut a = Archive::open(Cursor::new(&out[..]), &Resources::default()).unwrap();
        assert_eq!(a.index().blocks.len(), 2);
        let env = a.index().envelope;
        // The declared window is the dictionary used: the next power of two above the block.
        assert_eq!(env.max_window, 1 << 16);
        assert_eq!(env.max_block_plain, 100_000, "the largest block written");
        let table = a.entry_table().unwrap();
        let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
        for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
            let mut got = Vec::new();
            a.extract(e, &mut got).unwrap();
            assert_eq!(got, std::fs::read(dir.path().join(&e.path)).unwrap());
        }
        // A reader whose limit is below the declared window refuses.
        let small = Resources {
            max_window: 1 << 15,
            ..Resources::default()
        };
        match Archive::open(Cursor::new(&out[..]), &small) {
            Err(e) => assert!(e.to_string().contains("max_window"), "{e}"),
            Ok(_) => panic!("opened under a small max_window"),
        }
    }

    #[test]
    fn a_boundary_waits_for_the_minimum_and_stored_clusters_come_last() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());
        let blocks = |min: Option<u64>| {
            let opts = BalancedOptions {
                dict_size: 1 << 22,
                min_block_before_boundary: min,
                ..BalancedOptions::default()
            };
            let mut out = Vec::new();
            archive_balanced(dir.path(), &mut out, opts).unwrap();
            let a = Archive::open(Cursor::new(&out[..]), &Resources::default()).unwrap();
            a.index().blocks.len()
        };
        // Default minimum (half the dictionary): prose and source merge, the stored file follows.
        assert_eq!(blocks(None), 2);
        // A minimum of one byte: every cluster is its own block again.
        assert_eq!(blocks(Some(1)), 3);
    }

    #[test]
    fn a_dictionary_or_window_the_default_reader_refuses_is_refused_at_construction() {
        let o = BalancedOptions {
            dict_size: 1 << 29,
            ..BalancedOptions::default()
        };
        match BalancedEncoder::new(&o) {
            Err(CoreError::InvalidOption(m)) => {
                assert!(m.contains("dict_size") && m.contains("max_window"), "{m}")
            }
            other => panic!("{other:?}"),
        }
        let o = BalancedOptions {
            zstd_window_log: Some(29),
            ..BalancedOptions::default()
        };
        assert!(matches!(
            BalancedEncoder::new(&o),
            Err(CoreError::InvalidOption(_))
        ));
        // The zstd window follows the dictionary by default.
        assert!(BalancedEncoder::new(&BalancedOptions::default()).is_ok());
    }

    #[test]
    fn archive_balanced_file_refuses_to_overwrite_and_excludes_its_output() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let arch = dir.path().join("out.lpk");
        let opts = || BalancedOptions {
            dict_size: 1 << 20,
            ..BalancedOptions::default()
        };
        let (ws, _) = archive_balanced_file(dir.path(), &arch, opts()).unwrap();
        assert_eq!(ws.entries, 17);
        let again = archive_balanced_file(dir.path(), &arch, opts());
        assert!(matches!(again, Err(CoreError::Io { .. })));
        assert!(arch.exists());
        let bad = BalancedOptions {
            dict_size: 1,
            ..BalancedOptions::default()
        };
        let other = dir.path().join("o2.lpk");
        assert!(archive_balanced_file(dir.path(), &other, bad).is_err());
        assert!(!other.exists());
        clear_readonly(&dir.path().join("ro.txt"));
    }
}
