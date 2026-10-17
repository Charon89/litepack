//! `supersedes.toml`: a later results directory names the rows of an earlier directory that its
//! own re-measured rows replace when the two are pooled (`lpk-bench report --results A --results
//! B`). Nothing is deleted or edited on disk: the earlier row stays in its directory and the
//! report lists every replacement with its reason.
//!
//! ```toml
//! [[supersede]]
//! tool = "7z"
//! setting = "mx5"
//! class = "game-assets"
//! from = "2026-10-17-megatron-15"
//! reason = "measured while builds ran on the machine (21:59:57-22:31 UTC)"
//! ```
//!
//! Every row is named in full: no wildcard (`*`) in `tool`, `setting` or `class`. `from` is the
//! earlier directory as the report labels it (its name, or its `bench/results/<name>` label).

use anyhow::{bail, Result};
use serde::Deserialize;

pub const FILE_NAME: &str = "supersedes.toml";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Supersede {
    pub tool: String,
    pub setting: String,
    pub class: String,
    /// The earlier directory whose row is replaced.
    pub from: String,
    pub reason: String,
}

impl Supersede {
    /// `tool/setting/class`.
    pub fn combination(&self) -> String {
        format!("{}/{}/{}", self.tool, self.setting, self.class)
    }

    /// Does `from` name the directory labelled `label` (the label itself or its last component)?
    pub fn names(&self, label: &str) -> bool {
        label == self.from || label.rsplit('/').next() == Some(self.from.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Supersedes {
    #[serde(default)]
    pub supersede: Vec<Supersede>,
}

impl Supersedes {
    /// Parse and check the file's own rules: at least one entry, every field given and none a
    /// wildcard, no combination listed twice.
    pub fn parse(text: &str) -> Result<Supersedes> {
        let file: Supersedes = toml::from_str(text)?;
        if file.supersede.is_empty() {
            bail!("the file lists no [[supersede]] entry");
        }
        let mut seen: Vec<String> = Vec::new();
        for e in &file.supersede {
            let name = e.combination();
            for (field, value) in [
                ("tool", &e.tool),
                ("setting", &e.setting),
                ("class", &e.class),
            ] {
                if value.is_empty() {
                    bail!("entry {name}: `{field}` is empty; every row is named in full");
                }
                if value.contains('*') {
                    bail!(
                        "entry {name}: `{field}` is a wildcard; every row is named in full \
                         (one [[supersede]] entry per combination)"
                    );
                }
            }
            if e.from.is_empty() || e.from.contains('*') {
                bail!("entry {name}: `from` must name one earlier results directory");
            }
            if e.reason.trim().is_empty() {
                bail!("entry {name}: `reason` is empty");
            }
            if seen.contains(&name) {
                bail!("combination {name} is listed twice");
            }
            seen.push(name);
        }
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "[[supersede]]\ntool = \"7z\"\nsetting = \"mx5\"\nclass = \"audio\"\n\
                        from = \"2026-10-17-megatron-15\"\nreason = \"re-measured idle\"\n";

    #[test]
    fn a_full_entry_parses_and_names_its_directory_by_label_or_name() {
        let f = Supersedes::parse(GOOD).expect("parse");
        assert_eq!(f.supersede.len(), 1);
        let e = &f.supersede[0];
        assert_eq!(e.combination(), "7z/mx5/audio");
        assert!(e.names("2026-10-17-megatron-15"));
        assert!(e.names("bench/results/2026-10-17-megatron-15"));
        assert!(!e.names("bench/results/2026-10-17-megatron-16"));
        assert!(!e.names("2026-10-17-megatron-15/x"));
    }

    #[test]
    fn wildcards_empty_fields_duplicates_and_unknown_keys_are_refused() {
        let err = |text: &str| format!("{:#}", Supersedes::parse(text).expect_err("refused"));
        assert!(err(&GOOD.replace("\"mx5\"", "\"*\"")).contains("wildcard"));
        assert!(err(&GOOD.replace("\"7z\"", "\"\"")).contains("is empty"));
        assert!(err(&GOOD.replace("re-measured idle", " ")).contains("`reason` is empty"));
        assert!(err(&format!("{GOOD}{GOOD}")).contains("listed twice"));
        assert!(err("").contains("no [[supersede]]"));
        assert!(err(&format!("{GOOD}extra = 1\n")).contains("extra"));
        assert!(err(&GOOD.replace("from = \"2026-10-17-megatron-15\"\n", "")).contains("from"));
    }
}
