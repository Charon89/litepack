//! E5-9's acceptance in the real window: axe-core (Deque, MPL-2.0; test-only, unmodified, fetched
//! and hash-verified by CI's `gui-e2e` job, never in the repository) is injected through the
//! WebDriver's script execution into every view and dialog state the harness reaches without a
//! native dialog, and a violation of impact `critical` fails the test with the rule, its help text
//! and the failing nodes. `serious` ones are printed. The states the harness cannot reach (the
//! native password prompt, the envelope refusal, a transient progress view) are in
//! `docs/GUI-CHECKLIST.md` for E5-10's pass with Narrator.
//!
//! `LPK_AXE_JS` names the verified `axe.min.js`; without it the test fails (never skips).
//! Ignored unless the crate was built with `LPK_GUI_E2E=1` (build.rs); CI's `gui-e2e` job sets it.
#![cfg(windows)]
#![forbid(unsafe_code)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{connect_args, lpk_exe, page_state, start_driver, Driver, FLAGS, ROWS_WITHIN};
use fantoccini::{Client, Locator};

/// The Edge WebDriver's port for this test (the others use 4444 to 4450 and 4452).
const DRIVER_PORT: u16 = 4451;
const SETTLE: Duration = Duration::from_millis(600);

/// The tags axe-core runs: WCAG 2.0 and 2.1 A and AA, and its best practices.
const AXE_OPTIONS: &str = "{ runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'best-practice'] }, resultTypes: ['violations'] }";

fn axe_source() -> String {
    let path = std::env::var_os("LPK_AXE_JS")
        .map(PathBuf::from)
        .expect("LPK_AXE_JS is not set: CI's gui-e2e job fetches and verifies axe-core 4.14.0");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("LPK_AXE_JS {}: {e}", path.display()));
    assert!(
        text.contains("4.14.0"),
        "{} is not axe-core 4.14.0",
        path.display()
    );
    text
}

/// The text files and the binary one of the fixture; `sub/` for the tree view.
fn write_inputs(input: &Path, extra: &str) {
    std::fs::create_dir_all(input.join("sub")).expect("create the input folder");
    std::fs::write(
        input.join("note.txt"),
        format!("hello preview pane {extra}\n"),
    )
    .expect("write the fixture");
    std::fs::write(input.join("pic.png"), PNG).expect("write the fixture");
    // Not text, not an image: the hex view.
    let bin: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    std::fs::write(input.join("data.bin"), bin).expect("write the fixture");
    std::fs::write(input.join("sub").join("deep.txt"), "deep\n").expect("write the fixture");
}

fn lpk(args: &[&Path], first: &str) {
    let status = Command::new(lpk_exe())
        .arg(first)
        .args(args)
        .status()
        .expect("run lpk");
    assert!(status.success(), "lpk {first} failed: {status}");
}

/// A valid 1x1 PNG.
const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 96, 248, 95, 15, 0, 2,
    135, 1, 128, 235, 71, 186, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

async fn wait_for(
    c: &Client,
    css: &str,
    what: &str,
) -> Result<fantoccini::elements::Element, String> {
    let deadline = Instant::now() + ROWS_WITHIN;
    loop {
        if let Ok(e) = c.find(Locator::Css(css)).await {
            return Ok(e);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no {what} within {ROWS_WITHIN:?}; page: {}",
                page_state(c).await
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A script (a function body) until it returns a truthy value.
async fn wait_script(c: &Client, what: &str, script: &str) -> Result<(), String> {
    let deadline = Instant::now() + ROWS_WITHIN;
    loop {
        let v = c
            .execute(script, vec![])
            .await
            .map_err(|e| format!("{what}: {e}"))?;
        if v.as_bool().unwrap_or(false) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("{what}: not true; page: {}", page_state(c).await));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The dialog is open and holds no running progress bar.
async fn dialog_settled(c: &Client) -> Result<(), String> {
    wait_script(
        c,
        "a settled dialog",
        "const d = document.querySelector('dialog[open]'); return !!d && !d.querySelector('progress');",
    )
    .await
}

async fn click(c: &Client, css: &str) -> Result<(), String> {
    wait_for(c, css, css)
        .await?
        .click()
        .await
        .map_err(|e| format!("click {css}: {e}"))
}

async fn click_row(c: &Client, name: &str) -> Result<(), String> {
    let deadline = Instant::now() + ROWS_WITHIN;
    loop {
        for row in c
            .find_all(Locator::Css("#rows [role=row]"))
            .await
            .unwrap_or_default()
        {
            if let Ok(cell) = row.find(Locator::Css("[role=gridcell]")).await {
                if cell.text().await.unwrap_or_default() == name {
                    return row.click().await.map_err(|e| format!("click {name}: {e}"));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(format!("no row {name}; page: {}", page_state(c).await));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The page's flags must be clean (no script error, rejection, CSP violation or failure).
async fn flags_clean(c: &Client, at: &str) -> Result<(), String> {
    let html = c
        .find(Locator::Css("html"))
        .await
        .map_err(|e| format!("{at}: no <html>: {e}"))?;
    for name in FLAGS {
        let v = html
            .attr(name)
            .await
            .map_err(|e| format!("{at}: reading {name}: {e}"))?;
        if v.as_deref() != Some("0") {
            return Err(format!(
                "{at}: page flag {name} is {v:?}; page: {}",
                page_state(c).await
            ));
        }
    }
    Ok(())
}

/// Inject axe-core (once per session) and run it over the page as it stands; any `critical`
/// violation is an error, the `serious` ones are printed. The page's flags are clean before the
/// injection and after the run.
async fn axe(c: &Client, source: &str, at: &str) -> Result<(), String> {
    tokio::time::sleep(SETTLE).await;
    flags_clean(c, &format!("{at} (before axe)")).await?;
    let loaded = c
        .execute("return typeof window.axe === 'object';", vec![])
        .await
        .map_err(|e| e.to_string())?;
    if loaded.as_bool() != Some(true) {
        c.execute(source, vec![])
            .await
            .map_err(|e| format!("{at}: injecting axe-core: {e}"))?;
    }
    let script = format!(
        "const done = arguments[arguments.length - 1]; \
         axe.run(document, {AXE_OPTIONS}).then((r) => done(JSON.stringify(r.violations.map((v) => ({{ \
             id: v.id, impact: v.impact, help: v.help, \
             nodes: v.nodes.slice(0, 6).map((n) => ({{ target: n.target, html: (n.html || '').slice(0, 160) }})) \
         }})))), (e) => done(JSON.stringify({{ error: String(e) }})));"
    );
    let out = c
        .execute_async(&script, vec![])
        .await
        .map_err(|e| format!("{at}: axe.run: {e}"))?;
    let text = out
        .as_str()
        .ok_or_else(|| format!("{at}: axe.run returned a non-string: {out}"))?
        .to_string();
    let parsed: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{at}: axe result: {e}"))?;
    if parsed.get("error").is_some() {
        return Err(format!("{at}: axe-core failed: {text}"));
    }
    let violations = parsed
        .as_array()
        .ok_or_else(|| format!("{at}: axe result is not a list: {text}"))?;
    let mut critical = Vec::new();
    for v in violations {
        let impact = v["impact"].as_str().unwrap_or("");
        let line = format!(
            "{at}: [{impact}] {}: {} {}",
            v["id"].as_str().unwrap_or(""),
            v["help"].as_str().unwrap_or(""),
            v["nodes"]
        );
        if impact == "critical" {
            critical.push(line);
        } else {
            eprintln!("{line}");
        }
    }
    eprintln!("{at}: axe-core done, {} critical", critical.len());
    flags_clean(c, &format!("{at} (after axe)")).await?;
    if critical.is_empty() {
        Ok(())
    } else {
        Err(critical.join("\n"))
    }
}

async fn session(driver: &mut Driver, work: &Path, args: Vec<String>) -> Client {
    // The previous window is gone before the next starts (one instance at a time).
    tokio::time::sleep(Duration::from_secs(3)).await;
    connect_args(driver, work, args, DRIVER_PORT).await
}

/// Each state's outcome: every state is attempted, whatever the others did, and the test fails
/// at the end if any failed or was not reached.
#[derive(Default)]
struct Report {
    states: Vec<(String, Result<(), String>)>,
}

impl Report {
    fn record(&mut self, state: &str, r: Result<(), String>) {
        match &r {
            Ok(()) => eprintln!("STATE ok: {state}"),
            Err(e) => eprintln!("STATE FAILED: {state}: {e}"),
        }
        self.states.push((state.to_string(), r));
    }
}

/// Close whatever dialog is open (Esc, a few times), so that the next state starts clean.
async fn recover(c: &Client) {
    for _ in 0..4 {
        let open = c
            .execute("return !!document.querySelector('dialog[open]');", vec![])
            .await
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !open {
            return;
        }
        if let Ok(active) = c.active_element().await {
            let _ = active.send_keys("\u{E00C}").await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[test]
#[cfg_attr(
    not(lpk_gui_e2e),
    ignore = "needs the Edge WebDriver and axe-core; build with LPK_GUI_E2E=1 (CI's gui-e2e job)"
)]
fn no_view_or_dialog_has_a_critical_accessibility_issue() {
    let source = axe_source();
    let temp = tempfile::tempdir().expect("scratch dir");
    let work = temp.path().to_path_buf();
    let input = work.join("in");
    write_inputs(&input, "one");
    let archive = work.join("multi.lpk");
    lpk(&[&archive, &input], "a");
    // A second and a third generation: the history has something to browse and roll back.
    write_inputs(&input, "two");
    lpk(&[&archive, &input], "append");
    write_inputs(&input, "three");
    lpk(&[&archive, &input], "append");
    // An existing file of the same name beside the archive: "This folder" asks about it.
    std::fs::write(work.join("note.txt"), "already here\n").expect("write the fixture");
    // The create wizard's folder.
    let create = work.join("fixture");
    write_inputs(&create, "create");

    let mut driver = start_driver(&work, DRIVER_PORT);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let mut report = Report::default();
    rt.block_on(async {
        // The archive: browser, preview, extract, maintenance dialogs, a generation's view.
        let c = session(
            &mut driver,
            &work,
            vec![format!("--open={}", archive.display())],
        )
        .await;
        browse(&c, &source, &mut report).await;
        let _ = c.close().await;
        // The verb queue's result.
        let c = session(
            &mut driver,
            &work,
            vec![format!("--test={}", archive.display())],
        )
        .await;
        let r = async {
            wait_for(&c, "dialog.wizard[open] .results li p", "a result").await?;
            dialog_settled(&c).await?;
            axe(&c, &source, "the verb queue's result (--test)").await
        }
        .await;
        report.record("the verb queue's result (--test)", r);
        let _ = c.close().await;
        // The create wizard and its summary.
        let c = session(
            &mut driver,
            &work,
            vec![format!("--create={}", create.display())],
        )
        .await;
        wizard(&c, &source, &mut report).await;
        let _ = c.close().await;
        // The empty window, with the recents the first session left.
        let c = session(&mut driver, &work, vec![]).await;
        let r = async {
            wait_for(
                &c,
                "#recents:not([hidden]) #recent-list button",
                "a recent archive",
            )
            .await?;
            axe(&c, &source, "the empty window with recents").await
        }
        .await;
        report.record("the empty window with recents", r);
        let _ = c.close().await;
    });
    eprintln!("states checked ({}):", report.states.len());
    for (name, r) in &report.states {
        eprintln!("  {}: {name}", if r.is_ok() { "ok" } else { "FAILED" });
    }
    let failed: Vec<String> = report
        .states
        .iter()
        .filter_map(|(n, r)| r.as_ref().err().map(|e| format!("{n}: {e}")))
        .collect();
    assert!(
        report.states.len() >= EXPECTED_STATES,
        "only {} of {EXPECTED_STATES} states were reached or recorded",
        report.states.len()
    );
    assert!(
        failed.is_empty(),
        "critical accessibility issues or harness failures:\n{}",
        failed.join("\n")
    );
}

/// The states the test records: browser 2, previews 3, extract 3, test, check, update, history,
/// generation view, the verb queue, the wizard 2, the empty window.
const EXPECTED_STATES: usize = 17;

/// Open the Extract dialog on the whole archive (the clicked row would otherwise make the scope
/// "Selected"), "This folder" as the destination.
async fn extract_here(c: &Client) -> Result<(), String> {
    click(c, "#action-extract:not([disabled])").await?;
    dialog_settled(c).await?;
    click(c, "dialog.wizard input[name=scope][value=all]").await?;
    click(c, "dialog.wizard input[name=dest][value=here]").await
}

async fn browse(c: &Client, source: &str, rep: &mut Report) {
    if let Err(e) = wait_for(c, "#rows [role=row]", "a row").await {
        rep.record("the browser (rows)", Err(e));
        return;
    }
    let r = axe(c, source, "the browser, flat view").await;
    rep.record("the browser, flat view", r);

    let r = async {
        click(c, "#mode-tree").await?;
        wait_script(
            c,
            "the tree view",
            "return document.getElementById('mode-tree').getAttribute('aria-checked') === 'true';",
        )
        .await?;
        axe(c, source, "the browser, tree view").await
    }
    .await;
    rep.record("the browser, tree view", r);

    // The preview pane: text, an image, hex.
    for (name, css, at) in [
        (
            "note.txt",
            "#preview-body pre.preview-text",
            "the preview of text",
        ),
        ("pic.png", "#preview-body img", "the preview of an image"),
        ("data.bin", "#preview-body .hex", "the preview of hex"),
    ] {
        let r = async {
            click_row(c, name).await?;
            wait_for(c, css, at).await?;
            axe(c, source, at).await
        }
        .await;
        rep.record(at, r);
    }

    // The extract wizard: its form, the question, the result.
    let r = async {
        click(c, "#action-extract:not([disabled])").await?;
        dialog_settled(c).await?;
        axe(c, source, "the extract dialog (choose)").await
    }
    .await;
    rep.record("the extract dialog (choose)", r);
    recover(c).await;

    let r = async {
        extract_here(c).await?;
        click(c, "dialog.wizard button.primary").await?;
        wait_for(
            c,
            "dialog.wizard .wizard-buttons button:nth-child(4)",
            "the conflict question",
        )
        .await?;
        dialog_settled(c).await?;
        axe(c, source, "the extract dialog (conflict question)").await
    }
    .await;
    rep.record("the extract dialog (conflict question)", r);
    recover(c).await;

    let r = async {
        extract_here(c).await?;
        click(c, "dialog.wizard button.primary").await?;
        wait_for(
            c,
            "dialog.wizard .wizard-buttons button:nth-child(4)",
            "the conflict question",
        )
        .await?;
        // Skip (the primary button): the rest is written and the result shown.
        click(c, "dialog.wizard button.primary").await?;
        wait_for(c, "dialog.wizard .check", "the extract result").await?;
        dialog_settled(c).await?;
        axe(c, source, "the extract dialog (result)").await
    }
    .await;
    rep.record("the extract dialog (result)", r);
    recover(c).await;

    // The maintenance dialogs.
    let r = async {
        click(c, "#action-test").await?;
        wait_for(c, "dialog.wizard .check", "the test result").await?;
        dialog_settled(c).await?;
        axe(c, source, "the test dialog (result)").await
    }
    .await;
    rep.record("the test dialog (result)", r);
    recover(c).await;

    let r = async {
        click(c, "#action-check").await?;
        dialog_settled(c).await?;
        wait_for(c, "dialog.wizard .wizard-body p", "the check result").await?;
        axe(c, source, "the check dialog (result)").await
    }
    .await;
    rep.record("the check dialog (result)", r);
    recover(c).await;

    let r = async {
        click(c, "#action-update").await?;
        wait_for(c, "dialog.wizard details", "the update dialog").await?;
        dialog_settled(c).await?;
        axe(c, source, "the update-from-folder dialog").await
    }
    .await;
    rep.record("the update-from-folder dialog", r);
    recover(c).await;

    let r = async {
        click(c, "#action-history").await?;
        wait_for(
            c,
            "dialog.wizard input[name=generation]",
            "the history list",
        )
        .await?;
        dialog_settled(c).await?;
        axe(c, source, "the history dialog (list)").await
    }
    .await;
    rep.record("the history dialog (list)", r);

    // Browse an earlier generation: the read-only view with its banner (the history dialog is
    // still open if its state passed or not; the flow opens it again when it is not).
    let r = async {
        let open = c
            .execute(
                "return !!document.querySelector('dialog[open] input[name=generation]');",
                vec![],
            )
            .await
            .map_err(|e| e.to_string())?;
        if open.as_bool() != Some(true) {
            recover(c).await;
            click(c, "#action-history").await?;
            wait_for(c, "dialog.wizard input[name=generation]", "the history list").await?;
        }
        c.execute(
            "const r = [...document.querySelectorAll('dialog input[name=generation]')].find((x) => !x.checked); r.click(); return true;",
            vec![],
        )
        .await
        .map_err(|e| e.to_string())?;
        click(c, "dialog.wizard button.primary").await?;
        wait_script(
            c,
            "the generation's view",
            "return !document.querySelector('dialog[open]') && !document.getElementById('banner').hidden;",
        )
        .await?;
        wait_for(c, "#rows [role=row]", "a row").await?;
        axe(c, source, "the read-only generation view").await
    }
    .await;
    rep.record("the read-only generation view", r);
}

async fn wizard(c: &Client, source: &str, rep: &mut Report) {
    let r = async {
        wait_for(c, "dialog.create[open] .inputs li", "a listed input").await?;
        // Every tier card shows its estimate once Rust's calibration has run.
        wait_script(
            c,
            "the estimates",
            "const cards = document.querySelectorAll('dialog.create .card .estimate'); \
             return cards.length === 3 && [...cards].every((e) => e.textContent.includes('about') || e.textContent.includes('No estimate'));",
        )
        .await?;
        axe(c, source, "the create wizard (with estimates)").await
    }
    .await;
    rep.record("the create wizard (with estimates)", r);

    let r = async {
        click(c, "dialog.create[open] button.primary:not([disabled])").await?;
        wait_for(c, "dialog.create .check", "the summary").await?;
        dialog_settled(c).await?;
        axe(c, source, "the create summary").await
    }
    .await;
    rep.record("the create summary", r);
}
