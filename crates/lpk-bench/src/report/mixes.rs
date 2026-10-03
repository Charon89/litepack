//! The disk mixes (`bench/report-mixes.toml`): parsed and checked.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde::Deserialize;

/// The corpus classes (docs/CORPUS.md as adjusted by D-15).
pub const KNOWN_CLASSES: [&str; 17] = [
    "archives-nested",
    "audio",
    "backup-versions",
    "encrypted-random",
    "game-assets",
    "logs-text",
    "model-weights",
    "office-pdf",
    "photo-jpeg",
    "photo-jpeg-edited",
    "photo-raw-png",
    "small-files",
    "software-installed",
    "source-git",
    "text-prose",
    "video",
    "vm-image",
];

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mix {
    pub name: String,
    /// Class name to share of bytes (whole percent).
    pub weights: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mixes {
    pub mix: Vec<Mix>,
}

impl Mixes {
    /// Parse and check: at least one mix, unique names, known classes, weights summing to 100.
    pub fn parse(text: &str) -> Result<Mixes> {
        let m: Mixes = toml::from_str(text)?;
        if m.mix.is_empty() {
            bail!("the mixes file defines no mix");
        }
        let mut seen: Vec<&str> = Vec::new();
        for mix in &m.mix {
            if seen.contains(&mix.name.as_str()) {
                bail!("mix `{}` is defined twice", mix.name);
            }
            seen.push(&mix.name);
            for (class, w) in &mix.weights {
                if !KNOWN_CLASSES.contains(&class.as_str()) {
                    bail!("mix `{}`: unknown class `{class}`", mix.name);
                }
                if *w == 0 {
                    bail!("mix `{}`: class `{class}` has weight 0", mix.name);
                }
            }
            let total: u32 = mix.weights.values().sum();
            if total != 100 {
                bail!(
                    "mix `{}`: weights sum to {total}, they must sum to 100",
                    mix.name
                );
            }
        }
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_file_parses_and_checks() {
        let text = include_str!("../../../../bench/report-mixes.toml");
        let m = Mixes::parse(text).expect("committed mixes");
        let names: Vec<&str> = m.mix.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["photo-doc", "developer", "video-heavy"]);
    }

    #[test]
    fn bad_mixes_are_refused() {
        let ok = "[[mix]]\nname = \"a\"\nweights = { video = 60, audio = 40 }\n";
        assert!(Mixes::parse(ok).is_ok());
        for (bad, why) in [
            ("[[mix]]\nname = \"a\"\nweights = { video = 60, audio = 30 }\n", "sum"),
            ("[[mix]]\nname = \"a\"\nweights = { video = 60, nope = 40 }\n", "unknown class"),
            ("[[mix]]\nname = \"a\"\nweights = { video = 100, audio = 0 }\n", "zero weight"),
            (
                "[[mix]]\nname = \"a\"\nweights = { video = 100 }\n[[mix]]\nname = \"a\"\nweights = { audio = 100 }\n",
                "twice",
            ),
            ("", "empty"),
        ] {
            assert!(Mixes::parse(bad).is_err(), "{why}");
        }
    }
}
