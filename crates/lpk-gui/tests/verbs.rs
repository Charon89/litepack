//! E5-7's acceptance: the verbs `lpk-gui.exe` accepts are the table of `docs/API.md`
//! ("Activation verbs (E5-7)"). Every row is parsed and its command line run through the
//! binary's parser (`lpk_gui::cli`, which `src/host.rs` calls for the first instance and for a
//! second one alike); no verb outside the table is accepted.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;

use lpk_gui::cli::{parse, Verb, NAME, VERBS};

struct Row {
    verb: String,
    modifier: Option<String>,
    bare_path: bool,
    arguments: String,
}

fn table() -> Vec<Row> {
    let api =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/API.md"))
            .unwrap();
    let start = api.find("## Activation verbs").expect("the section");
    let section = &api[start..];
    let section = &section[..section[3..].find("\n## ").map_or(section.len(), |e| e + 3)];
    let mut rows = Vec::new();
    for line in section.lines().filter(|l| l.starts_with("| `--")) {
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        assert_eq!(cells.len(), 4, "{line}");
        // `--open` (or a bare path); `--create … --name`.
        let ticked = cells[0].split('`').nth(1).expect("a verb in backticks");
        let words: Vec<&str> = ticked.split_whitespace().collect();
        rows.push(Row {
            verb: words[0].to_owned(),
            modifier: (words.len() > 1).then(|| words[words.len() - 1].to_owned()),
            bare_path: cells[0].contains("bare path"),
            arguments: cells[1].trim_matches('`').to_owned(),
        });
    }
    rows
}

/// A command line for a row's arguments, and whether it names several paths.
fn sample(row: &Row) -> (Vec<String>, bool) {
    let mut args = vec![row.verb.clone()];
    let mut several = false;
    for word in row.arguments.split_whitespace() {
        match word {
            "<archive>" => args.push("a.lpk".into()),
            "<archive>..." => {
                args.extend(["a.lpk".into(), "b.zip".into()]);
                several = true;
            }
            "<path>..." => {
                args.extend(["x".into(), "y.txt".into()]);
                several = true;
            }
            "<file>.lpk" => args.push("n.lpk".into()),
            w if w.starts_with("--") => args.push(w.into()),
            w => panic!("an argument form the test does not know: {w}"),
        }
    }
    (args, several)
}

fn run(args: &[String]) -> Result<Option<Verb>, String> {
    parse(args.iter().map(OsString::from), None).map(|s| s.verb)
}

#[test]
fn the_parser_accepts_exactly_the_documented_verbs() {
    let rows = table();
    assert_eq!(rows.len(), 7, "the table's rows");
    let mut documented = BTreeSet::new();
    for row in &rows {
        documented.insert(row.verb.clone());
        if let Some(m) = &row.modifier {
            documented.insert(m.clone());
        }
        let (args, several) = sample(row);
        let verb = run(&args)
            .unwrap_or_else(|e| panic!("{args:?} refused: {e}"))
            .unwrap();
        assert_eq!(verb.flag(), row.verb, "{args:?}");
        assert_eq!(verb.paths().len(), if several { 2 } else { 1 }, "{args:?}");
        match (&verb, &row.modifier) {
            (Verb::Create { name, .. }, Some(_)) => assert_eq!(name.as_deref(), Some("n.lpk")),
            (Verb::Create { name, .. }, None) => assert_eq!(*name, None),
            _ => {}
        }
        if !several {
            // One archive: a second is refused.
            let mut two = args.clone();
            two.push("c.lpk".into());
            assert!(run(&two).is_err(), "{two:?} accepted");
        }
        // A row's verb with nothing after it is refused.
        assert!(run(std::slice::from_ref(&row.verb)).is_err());
        if row.bare_path {
            assert_eq!(
                run(&["a.lpk".into()]).unwrap().map(|v| v.flag()),
                Some("--open")
            );
            let eq = format!("{}=a.lpk", row.verb);
            assert_eq!(run(&[eq]).unwrap().map(|v| v.flag()), Some("--open"));
        }
    }
    let accepted: BTreeSet<String> = VERBS
        .iter()
        .chain([NAME].iter())
        .map(|v| v.to_string())
        .collect();
    assert_eq!(accepted, documented, "the parser's verbs and the table");
    // No other verb: the menu's other words, near misses, Chromium's switches.
    for extra in [
        "--add",
        "--extract",
        "--repair",
        "--check",
        "--rollback",
        "--OPEN",
        "--open-folder",
        "--register-shell",
        "--remote-debugging-port=9222",
    ] {
        assert!(
            run(&[extra.into(), "a.lpk".into()]).is_err(),
            "{extra} accepted"
        );
    }
}

#[test]
fn the_response_file_note_is_documented() {
    let api =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/API.md"))
            .unwrap();
    assert!(api.contains("`@file` response files"));
}
