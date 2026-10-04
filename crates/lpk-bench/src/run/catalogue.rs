//! The baseline-tool catalogue (`bench/tools.toml`) and the per-machine override file
//! (`bench/tools.local.toml`, untracked). Format described at the top of `bench/tools.toml`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Placeholders allowed in `create` / `extract` templates.
const PLACEHOLDERS: [&str; 7] = [
    "{archive}",
    "{input}",
    "{outdir}",
    "{settings}",
    "{threads}",
    "{list}",
    "{sep}",
];

/// The OS key used in `exe.<os>` and `hints.<os>`.
pub fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionProbe {
    pub args: Vec<String>,
    /// Regular expression; capture group 1 is the version.
    pub pattern: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Install {
    pub winget: Option<String>,
    pub apt: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// The tool archives a directory itself.
    Directory,
    /// The runner pipes a tar stream into the tool.
    TarStream,
}

/// Where the extracted tree appears relative to `{outdir}` (directory mode).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    /// The files of the input directory.
    #[default]
    Flat,
    /// Inside a directory named like `{input}`.
    Nested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub id: String,
    #[serde(default)]
    pub compress: Vec<String>,
    #[serde(default)]
    pub extract: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub exe: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub hints: BTreeMap<String, Vec<String>>,
    /// Try the hints before PATH.
    #[serde(default)]
    pub hints_first: bool,
    pub version: VersionProbe,
    pub extension: String,
    pub mode: Mode,
    #[serde(default)]
    pub layout: Layout,
    #[serde(default)]
    pub create: Vec<String>,
    /// Create template for a private corpus (a list file of relative paths, `{list}`); absent:
    /// the tool is skipped for private corpora.
    #[serde(default)]
    pub create_list: Option<Vec<String>>,
    #[serde(default)]
    pub extract: Vec<String>,
    #[serde(default)]
    pub threads: Vec<String>,
    /// The thread count changes the archive size, not only the speed.
    #[serde(default)]
    pub ratio_depends_on_threads: bool,
    /// The tool deduplicates across files (D-43): the report compares gate G2 against the best
    /// tool without this flag and prints the flagged tools as reference only.
    #[serde(default)]
    pub dedup: bool,
    pub licence: String,
    #[serde(default)]
    pub install: Install,
    #[serde(default)]
    pub manual: bool,
    #[serde(default = "yes")]
    pub verified: bool,
    pub note: Option<String>,
    #[serde(default, rename = "setting")]
    pub settings: Vec<Setting>,
}

fn yes() -> bool {
    true
}

/// A `[[tool]]` entry of `bench/tools.local.toml`: replaces the catalogue's fields of the same
/// name for the tool with the same `id`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolOverride {
    pub id: String,
    /// Executable of this machine (any path, including absolute ones: it is never written to a
    /// tracked file or a result file).
    pub path: Option<String>,
    pub exe: Option<BTreeMap<String, Vec<String>>>,
    pub hints: Option<BTreeMap<String, Vec<String>>>,
    pub hints_first: Option<bool>,
    pub version: Option<VersionProbe>,
    pub extension: Option<String>,
    pub mode: Option<Mode>,
    pub layout: Option<Layout>,
    pub create: Option<Vec<String>>,
    pub create_list: Option<Vec<String>>,
    pub extract: Option<Vec<String>>,
    pub threads: Option<Vec<String>>,
    pub ratio_depends_on_threads: Option<bool>,
    #[serde(default, rename = "setting")]
    pub settings: Vec<Setting>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueFile {
    #[serde(default, rename = "tool")]
    tools: Vec<Tool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalFile {
    #[serde(default, rename = "tool")]
    tools: Vec<ToolOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalogue {
    pub tools: Vec<Tool>,
}

impl Catalogue {
    pub fn parse(text: &str) -> Result<Catalogue> {
        let file: CatalogueFile = toml::from_str(text).context("parsing the tool catalogue")?;
        let cat = Catalogue { tools: file.tools };
        cat.validate()?;
        Ok(cat)
    }

    pub fn load(path: &Path) -> Result<Catalogue> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Catalogue::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn get(&self, id: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.id == id)
    }

    fn validate(&self) -> Result<()> {
        let mut seen = BTreeSet::new();
        for tool in &self.tools {
            if !valid_id(&tool.id) {
                bail!("tool id `{}` must match [a-z0-9-]+", tool.id);
            }
            if !seen.insert(tool.id.as_str()) {
                bail!("duplicate tool id `{}`", tool.id);
            }
            tool.validate()?;
        }
        Ok(())
    }

    /// Merge the local override file's entries over the catalogue. Unknown ids are errors (a typo
    /// must not silently drop a tool).
    pub fn with_local(mut self, text: &str) -> Result<(Catalogue, Local)> {
        let file: LocalFile = toml::from_str(text).context("parsing the local tool overrides")?;
        let mut local = Local::default();
        for ov in file.tools {
            let Some(tool) = self.tools.iter_mut().find(|t| t.id == ov.id) else {
                bail!("local override for unknown tool `{}`", ov.id);
            };
            local.overridden.insert(ov.id.clone());
            if let Some(p) = ov.path {
                local.paths.insert(ov.id.clone(), p);
            }
            if let Some(v) = ov.exe {
                tool.exe = v;
            }
            if let Some(v) = ov.hints {
                tool.hints = v;
            }
            if let Some(v) = ov.hints_first {
                tool.hints_first = v;
            }
            if let Some(v) = ov.layout {
                tool.layout = v;
            }
            if let Some(v) = ov.ratio_depends_on_threads {
                tool.ratio_depends_on_threads = v;
            }
            if let Some(v) = ov.version {
                tool.version = v;
            }
            if let Some(v) = ov.extension {
                tool.extension = v;
            }
            if let Some(v) = ov.mode {
                tool.mode = v;
            }
            if let Some(v) = ov.create {
                tool.create = v;
            }
            if let Some(v) = ov.create_list {
                tool.create_list = Some(v);
            }
            if let Some(v) = ov.extract {
                tool.extract = v;
            }
            if let Some(v) = ov.threads {
                tool.threads = v;
            }
            for s in ov.settings {
                match tool.settings.iter_mut().find(|x| x.id == s.id) {
                    Some(existing) => *existing = s,
                    None => tool.settings.push(s),
                }
            }
        }
        self.validate()?;
        Ok((self, local))
    }
}

/// What the local override file said besides field replacements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Local {
    /// Ids that have an entry in the file.
    pub overridden: BTreeSet<String>,
    /// Explicit executable paths by tool id.
    pub paths: BTreeMap<String, String>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn check_template(tool: &str, what: &str, template: &[String]) -> Result<()> {
    for arg in template {
        let mut rest = arg.as_str();
        while let Some(start) = rest.find('{') {
            let Some(len) = rest[start..].find('}') else {
                bail!("tool `{tool}`: unbalanced `{{` in {what} argument `{arg}`");
            };
            let ph = &rest[start..start + len + 1];
            if !PLACEHOLDERS.contains(&ph) {
                bail!("tool `{tool}`: unknown placeholder `{ph}` in {what} argument `{arg}`");
            }
            rest = &rest[start + len + 1..];
        }
    }
    Ok(())
}

impl Tool {
    fn validate(&self) -> Result<()> {
        let id = &self.id;
        if !self.extension.starts_with('.') {
            bail!(
                "tool `{id}`: extension `{}` must start with `.`",
                self.extension
            );
        }
        regex::Regex::new(&self.version.pattern)
            .with_context(|| format!("tool `{id}`: version pattern"))?;
        for os in self.exe.keys().chain(self.hints.keys()) {
            if !["windows", "linux", "macos"].contains(&os.as_str()) {
                bail!("tool `{id}`: unknown OS key `{os}`");
            }
        }
        for (os, hints) in &self.hints {
            for h in hints {
                // Hints start with an environment variable, or are relative paths (this
                // repository's own build output, resolved from the current directory); never an
                // absolute location.
                let absolute = h.starts_with(['/', '\\']) || h.get(1..2) == Some(":");
                if !h.starts_with('%') && absolute {
                    bail!(
                        "tool `{id}`: {os} hint `{h}` must start with an environment variable or be a relative path"
                    );
                }
            }
        }
        check_template(id, "create", &self.create)?;
        check_template(id, "extract", &self.extract)?;
        if let Some(list) = &self.create_list {
            check_template(id, "create_list", list)?;
            if !list.iter().any(|a| a.contains("{list}")) {
                bail!("tool `{id}`: create_list needs `{{list}}`");
            }
        }
        if !self.threads.is_empty() && !self.threads.iter().any(|a| a.contains("{n}")) {
            bail!("tool `{id}`: threads template needs `{{n}}`");
        }
        let mut seen = BTreeSet::new();
        for s in &self.settings {
            if !valid_id(&s.id) {
                bail!("tool `{id}`: setting id `{}` must match [a-z0-9-]+", s.id);
            }
            if !seen.insert(s.id.as_str()) {
                bail!("tool `{id}`: duplicate setting `{}`", s.id);
            }
        }
        if !self.manual {
            if self.create.is_empty() || self.extract.is_empty() {
                bail!("tool `{id}`: create and extract templates are required");
            }
            if self.settings.is_empty() {
                bail!("tool `{id}`: at least one setting is required");
            }
        }
        Ok(())
    }

    /// Executable names for this OS.
    pub fn exe_names(&self, os: &str) -> &[String] {
        self.exe.get(os).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn hint_list(&self, os: &str) -> &[String] {
        self.hints.get(os).map(Vec::as_slice).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = include_str!("../../../../bench/tools.toml");

    #[test]
    fn committed_catalogue_parses_and_has_the_planned_tools() {
        let cat = Catalogue::parse(REAL).expect("catalogue");
        let ids: Vec<&str> = cat.tools.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "store",
                "7z",
                "rar",
                "zstd",
                "xz",
                "zpaqfranz",
                "lpk",
                "tsaur",
                "wzzip",
                "pacl"
            ]
        );
        let settings = |id: &str| -> Vec<String> {
            cat.get(id)
                .map(|t| t.settings.iter().map(|s| s.id.clone()).collect())
                .unwrap_or_default()
        };
        assert_eq!(settings("7z"), ["mx5", "ultra"]);
        assert_eq!(settings("rar"), ["m3", "best", "best-solid", "best-rr3"]);
        assert_eq!(settings("zstd"), ["3", "19", "ultra22-long27"]);
        assert_eq!(settings("xz"), ["6", "9"]);
        assert_eq!(settings("zpaqfranz"), ["m1", "m5"]);
        assert_eq!(settings("tsaur"), ["default", "max"]);
        for id in ["wzzip", "pacl"] {
            let t = cat.get(id).expect("manual tool");
            assert!(t.manual && !t.verified && t.settings.is_empty() && t.create.is_empty());
        }
        assert!(!cat.get("tsaur").expect("tool").verified);
        for id in ["7z", "rar", "zstd", "xz", "zpaqfranz", "store"] {
            assert!(cat.get(id).expect("tool").verified, "{id}");
        }
    }

    #[test]
    fn rar_settings_differ_only_where_intended() {
        let cat = Catalogue::parse(REAL).expect("catalogue");
        let rar = cat.get("rar").expect("rar");
        let args = |id: &str| -> Vec<&str> {
            let s = rar.settings.iter().find(|s| s.id == id).expect("setting");
            s.compress.iter().map(String::as_str).collect()
        };
        assert_eq!(args("m3"), ["-m3"]);
        assert_eq!(args("best"), ["-m5", "-md256m"]);
        assert_eq!(args("best-solid"), ["-m5", "-md256m", "-s"]);
        assert_eq!(args("best-rr3"), ["-m5", "-md256m", "-rr3%"]);
    }

    #[test]
    fn dedup_flag_is_parsed_defaults_to_false_and_is_set_on_the_dedup_tools() {
        let cat = Catalogue::parse(REAL).expect("catalogue");
        for (id, dedup) in [
            ("zpaqfranz", true),
            ("tsaur", true),
            ("7z", false),
            ("rar", false),
            ("zstd", false),
            ("xz", false),
            ("store", false),
        ] {
            assert_eq!(cat.get(id).expect("tool").dedup, dedup, "{id}");
        }
    }

    #[test]
    fn thread_templates_and_layouts_follow_what_the_real_tools_accept() {
        let cat = Catalogue::parse(REAL).expect("catalogue");
        // zpaqfranz rejects `-t N`; `-t4` is one argument.
        assert_eq!(cat.get("zpaqfranz").expect("t").threads, ["-t{n}"]);
        // xz decompresses with threads too.
        assert!(cat
            .get("xz")
            .expect("xz")
            .extract
            .contains(&"{threads}".to_string()));
        // Thread dependence of the ratio is stated for every non-manual tool.
        for (id, dep) in [
            ("7z", true),
            ("rar", true),
            ("zstd", true),
            ("xz", true),
            ("zpaqfranz", false),
        ] {
            assert_eq!(
                cat.get(id).expect("t").ratio_depends_on_threads,
                dep,
                "{id}"
            );
        }
        assert_eq!(cat.get("7z").expect("t").layout, Layout::Nested);
        assert_eq!(cat.get("zpaqfranz").expect("t").layout, Layout::Nested);
        assert_eq!(cat.get("rar").expect("t").layout, Layout::Flat);
        assert!(cat.get("store").expect("t").hints_first);
        // No directory-mode template passes an absolute-looking input: `{input}` only.
        for t in cat.tools.iter().filter(|t| t.mode == Mode::Directory) {
            for a in t.create.iter().chain(&t.extract) {
                assert!(!a.starts_with('/') && !a.contains(":\\"), "{}: {a}", t.id);
            }
        }
    }

    #[test]
    fn committed_catalogue_has_no_literal_locations() {
        // Every hint starts with an environment variable (checked by validate), and no
        // drive letter appears anywhere in the file.
        for line in REAL.lines().filter(|l| !l.trim_start().starts_with('#')) {
            let b = line.as_bytes();
            for i in 1..b.len().saturating_sub(1) {
                assert!(
                    !(b[i] == b':'
                        && b[i - 1].is_ascii_alphabetic()
                        && matches!(b[i + 1], b'/' | b'\\')
                        && (i < 2 || !b[i - 2].is_ascii_alphanumeric())),
                    "drive path in: {line}"
                );
            }
        }
    }

    #[test]
    fn bad_catalogues_are_rejected() {
        let base = |extra: &str| {
            format!(
                "[[tool]]\nid='a'\nname='A'\nversion={{args=[],pattern='(\\d+)'}}\nextension='.a'\n\
                 mode='directory'\nlicence='x'\ncreate=['a','{{archive}}']\nextract=['x']\n{extra}\n\
                 [[tool.setting]]\nid='s'\n"
            )
        };
        assert!(Catalogue::parse(&base("")).is_ok());
        assert!(Catalogue::parse(&base("bogus=1")).is_err());
        assert!(Catalogue::parse(&base("hints={windows=['C:/x/a.exe']}")).is_err());
        assert!(Catalogue::parse(&base("hints={windows=['%ProgramFiles%/x/a.exe']}")).is_ok());
        assert!(Catalogue::parse(&base("threads=['-t']")).is_err());
        assert!(Catalogue::parse(&base("exe={beos=['a']}")).is_err());
        let dup = format!("{}{}", base(""), base(""));
        assert!(Catalogue::parse(&dup).is_err());
        let bad_ph = base("").replace("{archive}", "{nope}");
        assert!(Catalogue::parse(&bad_ph).is_err());
    }

    #[test]
    fn local_override_replaces_fields_and_adds_settings() {
        let cat = Catalogue::parse(REAL).expect("catalogue");
        let local = r#"
[[tool]]
id = "wzzip"
path = "somewhere/wzzip"
create = ["-a", "{archive}", "{input}", "{settings}"]
extract = ["-e", "{archive}", "{outdir}"]
[[tool.setting]]
id = "max"
compress = ["-ex"]
"#;
        let (cat, paths) = cat.with_local(local).expect("merge");
        let t = cat.get("wzzip").expect("wzzip");
        assert_eq!(t.settings.len(), 1);
        assert_eq!(t.settings[0].compress, ["-ex"]);
        assert_eq!(t.create[0], "-a");
        assert_eq!(paths.paths["wzzip"], "somewhere/wzzip");
        assert!(paths.overridden.contains("wzzip"));

        let cat = Catalogue::parse(REAL).expect("catalogue");
        assert!(cat.with_local("[[tool]]\nid='nope'\n").is_err());
    }
}
