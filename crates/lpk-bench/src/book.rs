//! The docs book's benchmark pages (PLAN E9-6): `lpk-bench book`.
//!
//! ```text
//! lpk-bench book [--root DIR] [--out-dir DIR] [--check]
//! ```
//!
//! Two pages made from committed files and nothing else: `results.md` (the measured results of the
//! published run, from the report model) and `corpus.md` (the corpus classes and sources, from
//! `docs/CORPUS.md`, `bench/corpus.lock` and `bench/corpus-sources.toml`). `bench/site.toml` names
//! the published inputs. The pages carry no gate verdict and no machine detail (host name,
//! antivirus), and every number is followed by the index of the result file it was read from.
//! `--check` regenerates the pages in memory and fails when the committed ones differ.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::Args;
use regex::Regex;
use serde::Deserialize;

use crate::report::{self, render, traced::brackets, ReportArgs};

/// Where the published inputs are named, relative to the root.
pub const SITE_FILE: &str = "bench/site.toml";
/// Where the pages are written, relative to the root.
pub const OUT_DIR: &str = "book/src/bench";
const FOOTER: &str = "\n{{#include ../footer.md}}\n";

#[derive(Debug, Args)]
pub struct BookArgs {
    /// The repository root (`bench/site.toml` and every path in it are relative to it)
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub root: PathBuf,
    /// Where the pages are written or checked (default: book/src/bench under the root)
    #[arg(long, value_name = "DIR")]
    pub out_dir: Option<PathBuf>,
    /// Write nothing; fail when the pages in the output directory differ from the generated ones
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Site {
    /// Results directories, in the order the report takes them.
    results: Vec<String>,
    #[serde(default)]
    probes: Vec<String>,
    mixes: String,
    corpus: CorpusFiles,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusFiles {
    spec: String,
    lock: String,
    sources: String,
    /// A class that `docs/CORPUS.md` names only as a derived variant under another spelling.
    #[serde(default)]
    derived: BTreeMap<String, String>,
}

/// The two generated pages.
pub struct Pages {
    pub results: String,
    pub corpus: String,
}

pub const PAGE_NAMES: [&str; 2] = ["results.md", "corpus.md"];

impl Pages {
    pub fn get(&self, name: &str) -> &str {
        if name == "results.md" {
            &self.results
        } else {
            &self.corpus
        }
    }
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

/// `2026-10-30-megatron` becomes `2026-10-30 run 1`, `2026-10-30-megatron-3` `2026-10-30 run 3`.
fn run_name(dir: &str, host: &str) -> Result<String> {
    let (date, rest) = match (dir.get(..10), dir.get(10..)) {
        (Some(d), Some(r)) if r.starts_with('-') => (d, &r[1..]),
        _ => bail!("results directory `{dir}` is not named <date>-<host>[-<n>]"),
    };
    if rest == host {
        return Ok(format!("{date} run 1"));
    }
    let n = rest
        .strip_prefix(host)
        .and_then(|r| r.strip_prefix('-'))
        .and_then(|r| r.parse::<u32>().ok().filter(|n| *n >= 2));
    match n {
        Some(n) => Ok(format!("{date} run {n}")),
        None => bail!("results directory `{dir}` is not named <date>-<host>[-<n>] (host `{host}`)"),
    }
}

/// A source or directory label in the site's form: a results directory by its date and run
/// number, followed by the file name; any other label unchanged.
fn site_label(label: &str, host: &str) -> Result<String> {
    let Some(rest) = label.strip_prefix("bench/results/") else {
        return Ok(label.to_string());
    };
    match rest.split_once('/') {
        Some((dir, file)) => Ok(format!("{} {file}", run_name(dir, host)?)),
        None => run_name(rest, host),
    }
}

/// The source indices cited in `text`: every `[1,3-5]` bracket of digits, commas and dashes.
fn cited(text: &str) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len()
                && (bytes[j].is_ascii_digit() || bytes[j] == b',' || bytes[j] == b'-')
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b']' && j > start && bytes[start].is_ascii_digit() {
                for part in text[start..j].split(',') {
                    let ends: Vec<usize> = part.split('-').filter_map(|n| n.parse().ok()).collect();
                    match ends.as_slice() {
                        [a] => {
                            out.insert(*a);
                        }
                        [a, b] => out.extend(*a..=*b),
                        _ => {}
                    }
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// `cited` over the lines outside code fences: the renderer prints its own citations in prose and
/// tables, while a fence holds a file printed verbatim (the mixes TOML), whose brackets are not
/// citations. The renderer's output is made of strings, so this scan reads the bracket lists the
/// renderer itself emits (`Traced::tag`), not the model's values.
fn cited_outside_fences(text: &str) -> BTreeSet<usize> {
    let mut fenced = false;
    let mut out = BTreeSet::new();
    for line in text.lines() {
        if line.starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            out.extend(cited(line));
        }
    }
    out
}

fn sources_section(
    sources: &[String],
    ids: &BTreeSet<usize>,
    lab: &dyn Fn(&str) -> String,
) -> Result<String> {
    let mut s = String::from("## Sources\n\n");
    for id in ids {
        let Some(label) = id.checked_sub(1).and_then(|i| sources.get(i)) else {
            bail!("the page cites source [{id}], which the report has no such source for");
        };
        s.push_str(&format!("{id}. `{}`\n", lab(label)));
    }
    s.push('\n');
    Ok(s)
}

fn load_inputs(root: &Path, site: &Site) -> Result<report::model::Inputs> {
    let args = ReportArgs {
        results: site.results.iter().map(|d| root.join(d)).collect(),
        probes: site.probes.iter().map(|d| root.join(d)).collect(),
        mixes: root.join(&site.mixes),
        out: None,
        allow_unclean: false,
    };
    report::load(&args)
}

fn results_page(inputs: &report::model::Inputs, lab: &dyn Fn(&str) -> String) -> Result<String> {
    let (header, classes, blended, caveats) = render::site_sections(inputs, lab);
    let mut body = String::new();
    body.push_str(&header);
    body.push_str(&classes);
    body.push_str(&blended);
    let tail: &str = &caveats;
    let cites = cited_outside_fences(&body);
    let mut s = String::from("# Results\n\n");
    s.push_str(
        "This page shows the measured results of LitePack and of the other archivers on the \
         benchmark corpus: for each class of data, and blended over the disk mixes, the archive \
         size, the speed of compression and extraction, and the memory. Every number is followed \
         by the index of the result file it was read from, in brackets; the files are listed \
         under Sources, and they are in the repository under `bench/results/`. The page is made \
         by `lpk-bench book` from `bench/site.toml` and the committed result files, never by \
         hand.\n\n",
    );
    s.push_str(&body);
    s.push_str(&sources_section(&inputs.sources, &cites, lab)?);
    s.push_str(tail);
    Ok(s)
}

// ---------------------------------------------------------------------------------------------
// The corpus page

#[derive(Debug, Deserialize)]
struct Lock {
    profiles: BTreeMap<String, Vec<LockEntry>>,
}

#[derive(Debug, Deserialize)]
struct LockEntry {
    source: String,
    /// A pin of a git commit has no size.
    bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SourcesFile {
    source: Vec<Source>,
}

#[derive(Debug, Deserialize)]
struct Source {
    id: String,
    class: String,
    licence: Option<String>,
    profiles: Vec<String>,
}

/// What `docs/CORPUS.md`'s class table says: the description of a class, and of its derived variants.
struct Descriptions {
    rows: BTreeMap<String, String>,
    /// derived variant name: (parent class, its note)
    derived: BTreeMap<String, (String, Option<String>)>,
}

/// The text of a trailing parenthetical `(...)` of `text`, and `text` without it.
fn split_parenthetical(text: &str) -> (String, Option<String>) {
    let t = text.trim();
    if t.ends_with(')') {
        if let Some(open) = t.rfind(" (") {
            return (
                t[..open].trim().to_string(),
                Some(t[open + 2..t.len() - 1].trim().to_string()),
            );
        }
    }
    (t.to_string(), None)
}

fn parse_spec(text: &str) -> Result<Descriptions> {
    let name_re = Regex::new(r"`([^`]+)`:").expect("regex");
    let mut rows = BTreeMap::new();
    let mut derived = BTreeMap::new();
    for line in text.lines().filter(|l| l.starts_with("| `")) {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        // A leading and a trailing empty piece around four cells.
        if cells.len() != 6 {
            bail!("docs/CORPUS.md: a class table row does not have four cells: {line}");
        }
        let class = cells[1].trim_matches('`').to_string();
        rows.insert(class.clone(), split_parenthetical(cells[2]).0);
        let notes = cells[4];
        let marks: Vec<_> = name_re.captures_iter(notes).collect();
        for (i, m) in marks.iter().enumerate() {
            let whole = m.get(0).expect("match");
            let end = marks
                .get(i + 1)
                .map_or(notes.len(), |n| n.get(0).expect("match").start());
            let (_, note) =
                split_parenthetical(notes[whole.end()..end].trim_end_matches([';', ' ']));
            derived.insert(m[1].to_string(), (class.clone(), note));
        }
    }
    if rows.is_empty() {
        bail!("docs/CORPUS.md: no class table found");
    }
    Ok(Descriptions { rows, derived })
}

/// No token of a description may be a figure: a token with a digit and no letter. Number words
/// ("Three working-tree snapshots") stay allowed (R251): the descriptions are the spec's prose, a
/// number word there says how the class is built, not what was measured, and digits stay banned,
/// so no count or size from the prose reaches the page.
fn check_no_figures(class: &str, text: &str) -> Result<()> {
    for token in text.split_whitespace() {
        let t = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if t.chars().any(|c| c.is_ascii_digit()) && !t.chars().any(|c| c.is_ascii_alphabetic()) {
            bail!("the description of class `{class}` holds the figure `{t}`; the page takes no number from docs/CORPUS.md's prose");
        }
    }
    Ok(())
}

impl Descriptions {
    fn describe(&self, class: &str, alias: Option<&String>) -> Result<String> {
        let text = if let Some(d) = self.rows.get(class) {
            d.clone()
        } else if let Some((parent, note)) = self
            .derived
            .get(class)
            .or_else(|| alias.and_then(|a| self.derived.get(a)))
        {
            match note {
                Some(n) => format!("Derived from `{parent}`: {n}"),
                None => format!("Derived from `{parent}`"),
            }
        } else {
            bail!("class `{class}` has no description in docs/CORPUS.md's class table");
        };
        check_no_figures(class, &text)?;
        Ok(text)
    }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\r', '\n'], " ")
}

#[derive(Default, Clone, Copy)]
struct Tally {
    entries: u64,
    bytes: u64,
}

fn corpus_page(
    root: &Path,
    site: &Site,
    inputs: &report::model::Inputs,
    lab: &dyn Fn(&str) -> String,
) -> Result<String> {
    let c = &site.corpus;
    let desc = parse_spec(&read(&root.join(&c.spec))?)?;
    let lock: Lock = serde_json::from_str(&read(&root.join(&c.lock))?)
        .with_context(|| format!("parsing {}", c.lock))?;
    let sources: SourcesFile = toml::from_str(&read(&root.join(&c.sources))?)
        .with_context(|| format!("parsing {}", c.sources))?;
    let by_id: BTreeMap<&str, &Source> =
        sources.source.iter().map(|s| (s.id.as_str(), s)).collect();
    let totals = render::corpus_totals(inputs).context("the published run has no rows")?;

    // Every class of the sources file needs a description, whether or not a lock profile holds it.
    for s in &sources.source {
        desc.describe(&s.class, c.derived.get(&s.class))?;
    }
    let profile_names: Vec<String> = lock.profiles.keys().map(|p| format!("`{p}`")).collect();
    let mut body = String::from("# The benchmark corpus\n\n");
    body.push_str(&format!(
        "The benchmark corpus is built from public sources, each pinned by address and BLAKE3 in \
         `bench/corpus.lock`; the sources, their classes and licences are in \
         `bench/corpus-sources.toml`, and the specification is `docs/CORPUS.md`. The lock lists \
         downloads (archives, files and git commits), not the files of the built corpus: one \
         download can yield many files, and some corpus files are derived locally. The lock has \
         the profiles {}; the published results are of the profile `{}`. A bracketed number is \
         the index of a result file of the published run, listed under Sources.\n\n",
        profile_names.join(", "),
        totals.profile
    ));
    body.push_str("## The built corpus of the published run\n\n");
    body.push_str(&format!(
        "- profile `{}`, manifest BLAKE3 `{}`: {} files, {} bytes, {} classes {}\n\n",
        totals.profile,
        totals.manifest,
        totals
            .files
            .as_ref()
            .map_or("n/a".to_string(), |t| t.show(render::count_text)),
        totals
            .bytes
            .as_ref()
            .map_or("n/a".to_string(), |t| t.show(render::count_text)),
        totals.classes,
        brackets(&BTreeSet::from([totals.src]))
    ));

    for (profile, entries) in &lock.profiles {
        let mut per_source: BTreeMap<&str, Tally> = BTreeMap::new();
        for e in entries {
            let Some(src) = by_id.get(e.source.as_str()) else {
                bail!(
                    "{}: profile `{profile}` has an entry of source `{}`, which {} does not list",
                    c.lock,
                    e.source,
                    c.sources
                );
            };
            let t = per_source.entry(src.id.as_str()).or_default();
            t.entries += 1;
            t.bytes += e.bytes.unwrap_or(0);
        }
        let mut classes: BTreeMap<&str, Tally> = BTreeMap::new();
        for s in sources
            .source
            .iter()
            .filter(|s| s.profiles.contains(profile))
        {
            let t = per_source.get(s.id.as_str()).copied().unwrap_or_default();
            let k = classes.entry(s.class.as_str()).or_default();
            k.entries += t.entries;
            k.bytes += t.bytes;
        }
        for id in per_source.keys() {
            classes.entry(by_id[id].class.as_str()).or_default();
        }
        body.push_str(&format!("## Profile `{profile}`\n\n### Classes\n\n"));
        body.push_str(
            "A pin of a git commit has no size and adds no bytes.\n\n\
             | class | description | downloads (lock entries) | download bytes |\n|---|---|---|---|\n",
        );
        for (class, t) in &classes {
            let text = desc.describe(class, c.derived.get(*class))?;
            body.push_str(&format!(
                "| `{class}` | {} | {} | {} |\n",
                cell(&text),
                t.entries,
                t.bytes
            ));
        }
        body.push_str("\n### Sources\n\n| id | class | licence | downloads (lock entries) | download bytes |\n|---|---|---|---|---|\n");
        for s in sources
            .source
            .iter()
            .filter(|s| s.profiles.contains(profile))
        {
            let t = per_source.get(s.id.as_str()).copied().unwrap_or_default();
            body.push_str(&format!(
                "| `{}` | `{}` | {} | {} | {} |\n",
                s.id,
                s.class,
                cell(s.licence.as_deref().unwrap_or("-")),
                t.entries,
                t.bytes
            ));
        }
        body.push('\n');
    }
    // The citations are those of the totals line, read from its traced values.
    let mut ids: BTreeSet<usize> = BTreeSet::from([totals.src]);
    for t in [&totals.files, &totals.bytes].into_iter().flatten() {
        ids.extend(t.sources.iter().copied());
    }
    let mut s = body;
    s.push_str(&sources_section(&inputs.sources, &ids, lab)?);
    Ok(s)
}

fn finish(text: String) -> String {
    format!(
        "{}\n{FOOTER}",
        text.replace("\r\n", "\n").trim_end_matches('\n')
    )
}

/// Both pages for the inputs named by `<root>/bench/site.toml`.
pub fn generate(root: &Path) -> Result<Pages> {
    let site_text = read(&root.join(SITE_FILE))?;
    let site: Site = toml::from_str(&site_text).with_context(|| format!("parsing {SITE_FILE}"))?;
    let inputs = load_inputs(root, &site)?;
    let host = inputs.baseline.host.host.clone();
    // Every label must have a site form before any page is made.
    for label in &inputs.sources {
        site_label(label, &host)?;
    }
    for d in &inputs.baseline.pooled {
        site_label(&d.label, &host)?;
    }
    let lab = |l: &str| site_label(l, &host).unwrap_or_else(|_| l.to_string());
    let results = finish(results_page(&inputs, &lab)?);
    let corpus = finish(corpus_page(root, &site, &inputs, &lab)?);
    Ok(Pages { results, corpus })
}

fn first_difference(committed: &str, generated: &str) -> String {
    let (mut a, mut b) = (committed.lines(), generated.lines());
    let mut n = 0;
    loop {
        n += 1;
        match (a.next(), b.next()) {
            (None, None) => return "differs in line endings".to_string(),
            (x, y) if x != y => {
                let cut = |l: Option<&str>| {
                    l.map_or("(end of file)".to_string(), |l| {
                        l.chars().take(100).collect()
                    })
                };
                return format!("line {n}: committed `{}`, generated `{}`", cut(x), cut(y));
            }
            _ => {}
        }
    }
}

fn run_inner(args: &BookArgs) -> Result<()> {
    let pages = generate(&args.root)?;
    let out = args
        .out_dir
        .clone()
        .unwrap_or_else(|| args.root.join(OUT_DIR));
    if args.check {
        let mut bad = Vec::new();
        for name in PAGE_NAMES {
            let path = out.join(name);
            match std::fs::read_to_string(&path) {
                Ok(have) if have == pages.get(name) => {}
                Ok(have) => bad.push(format!(
                    "{name}: {}",
                    first_difference(&have, pages.get(name))
                )),
                Err(_) => bad.push(format!("{name}: missing")),
            }
        }
        if !bad.is_empty() {
            bail!(
                "the committed pages differ from the generated ones; run `lpk-bench book` and commit \
                 the result. {}",
                bad.join("; ")
            );
        }
        println!("book pages are up to date");
        return Ok(());
    }
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    for name in PAGE_NAMES {
        let path = out.join(name);
        std::fs::write(&path, pages.get(name))
            .with_context(|| format!("writing {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

pub fn command(args: &BookArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn run_names_follow_the_directory_suffix() {
        assert_eq!(
            run_name("2026-10-30-megatron", "megatron").unwrap(),
            "2026-10-30 run 1"
        );
        assert_eq!(
            run_name("2026-10-30-megatron-3", "megatron").unwrap(),
            "2026-10-30 run 3"
        );
        assert!(run_name("2026-10-30-other", "megatron").is_err());
        assert!(run_name("2026-10-30-megatron-x", "megatron").is_err());
        assert_eq!(
            site_label("bench/results/2026-10-28-megatron-2/host.json", "megatron").unwrap(),
            "2026-10-28 run 2 host.json"
        );
        assert_eq!(
            site_label("bench/report-mixes.toml", "megatron").unwrap(),
            "bench/report-mixes.toml"
        );
    }

    #[test]
    fn citations_are_read_from_brackets() {
        let c = cited("a 1 [3] b [1,5-7] c [[mix]] d [x] e [2,]");
        assert_eq!(c.into_iter().collect::<Vec<_>>(), vec![1, 2, 3, 5, 6, 7]);
    }

    #[test]
    fn spec_rows_and_derived_variants_are_read() {
        let text = "| Class | What | Src | Derived |\n|---|---|---|---|\n\
            | `a` | Plain files (profile `full` only; D-1) | x | `a-edited`: re-save 20% of them (lossy edits) |\n\
            | `b` | More | y | — |\n";
        let d = parse_spec(text).unwrap();
        assert_eq!(d.describe("a", None).unwrap(), "Plain files");
        assert_eq!(
            d.describe("a-edited", None).unwrap(),
            "Derived from `a`: lossy edits"
        );
        assert_eq!(
            d.describe("c-alias", Some(&"a-edited".to_string()))
                .unwrap(),
            "Derived from `a`: lossy edits"
        );
        let err = d.describe("zzz", None).unwrap_err().to_string();
        assert!(err.contains("`zzz`"), "{err}");
    }

    #[test]
    fn a_figure_in_a_description_is_refused() {
        assert!(check_no_figures("a", "up to 200 MB of data").is_err());
        assert!(check_no_figures("a", "fp16 and H.264 and node-v1").is_ok());
    }

    #[test]
    fn fenced_brackets_are_not_citations_and_unknown_indices_fail() {
        let text = "a [3]\n```toml\nx = [99]\n```\nb [1,2]\n";
        assert_eq!(
            cited_outside_fences(text).into_iter().collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        let sources = vec!["one".to_string()];
        let lab = |l: &str| l.to_string();
        for bad in [999usize, 0] {
            let err = sources_section(&sources, &BTreeSet::from([bad]), &lab)
                .unwrap_err()
                .to_string();
            assert!(err.contains(&format!("[{bad}]")), "{err}");
        }
    }

    #[test]
    fn a_difference_is_named_by_line() {
        assert_eq!(
            first_difference("a\nb\n", "a\nc\n"),
            "line 2: committed `b`, generated `c`"
        );
    }
}
