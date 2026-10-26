//! The safe part of the handler (`docs/notes/e6-design.md` sections 1 and 2): the menu's verbs,
//! their texts and the `lpk-gui.exe` flags they send. E6-2 adds the selection kinds, states and
//! computed titles here; E6-1 only fixes the names.

/// The title of the one top-level command, which holds the seven verbs as subcommands.
pub const ROOT_TITLE: &str = "LitePack";

/// The seven verbs of the "LitePack" submenu (decision (b)), in menu order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// `Add to archive…`: the create wizard with the selection as input.
    AddToArchive,
    /// `Add to "<name>.lpk"`: creates at once with the defaults.
    AddToNamed,
    /// `Extract here`.
    ExtractHere,
    /// `Extract to "<name>\"`.
    ExtractTo,
    /// `Open`.
    Open,
    /// `Test`.
    Test,
    /// `Convert to…`: a foreign archive re-created as `.lpk`.
    Convert,
}

impl Verb {
    /// Every verb, in menu order.
    pub const ALL: [Verb; 7] = [
        Verb::AddToArchive,
        Verb::AddToNamed,
        Verb::ExtractHere,
        Verb::ExtractTo,
        Verb::Open,
        Verb::Test,
        Verb::Convert,
    ];

    /// The menu text before `<name>` is computed (E6-2 substitutes it from the selection).
    pub fn text(self) -> &'static str {
        match self {
            Verb::AddToArchive => "Add to archive\u{2026}",
            Verb::AddToNamed => "Add to \"<name>.lpk\"",
            Verb::ExtractHere => "Extract here",
            Verb::ExtractTo => "Extract to \"<name>\\\"",
            Verb::Open => "Open",
            Verb::Test => "Test",
            Verb::Convert => "Convert to\u{2026}",
        }
    }

    /// The `lpk-gui.exe` flag of E5-7 (`docs/API.md`, "Activation verbs"). `Add to "<name>.lpk"`
    /// sends `--create` followed by `--name <name>.lpk`.
    pub fn flag(self) -> &'static str {
        match self {
            Verb::AddToArchive | Verb::AddToNamed => "--create",
            Verb::ExtractHere => "--extract-here",
            Verb::ExtractTo => "--extract-to",
            Verb::Open => "--open",
            Verb::Test => "--test",
            Verb::Convert => "--convert",
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use std::path::Path;

    /// Every verb's text and flag appear together in a row of `docs/API.md`'s table, so the
    /// menu and the GUI's parser cannot drift apart silently.
    #[test]
    fn verbs_match_the_api_table() {
        let api = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/API.md"),
        )
        .expect("docs/API.md");
        for verb in Verb::ALL {
            let text = verb.text().replace('<', "&lt;").replace('>', "&gt;");
            let row = api
                .lines()
                .filter(|l| l.starts_with("| `--"))
                .find(|l| l.contains(&format!("| {text} |")))
                .unwrap_or_else(|| unreachable!("no API row for {text}"));
            assert!(row.contains(&format!("`{}", verb.flag())), "{row}");
            if verb == Verb::AddToNamed {
                assert!(row.contains("--name"), "{row}");
            }
        }
    }
}
