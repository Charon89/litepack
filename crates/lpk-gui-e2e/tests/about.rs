//! The About box in the real window: the settings dialog's "About LitePack" button opens it from
//! the keyboard, the focus is inside, it shows the running build's version and the five links (by
//! name, each with its fixed address as the tooltip), and Esc closes it with the focus back on the
//! gear. Neither a link nor the profile's removal is pressed (no browser, no profile touched).
//! Ignored unless the crate was built with `LPK_GUI_E2E=1` (build.rs); CI's `gui-e2e` job sets it.
#![cfg(windows)]
#![forbid(unsafe_code)]

mod common;

use std::time::{Duration, Instant};

use common::{connect_args, page_state, start_driver, ROWS_WITHIN};
use fantoccini::{Client, Locator};

/// The Edge WebDriver's port for this test (the others use 4444 to 4452).
const DRIVER_PORT: u16 = 4453;

const ENTER: &str = "\u{E007}";
const ESCAPE: &str = "\u{E00C}";

/// The links the box shows, in order: name and address.
const LINKS: [(&str, &str); 5] = [
    ("docs", "https://charon89.github.io/litepack/"),
    ("privacy", "https://charon89.github.io/litepack/privacy.html"),
    ("security", "https://charon89.github.io/litepack/security.html"),
    ("notices", "https://charon89.github.io/litepack/notices.html"),
    ("repository", "https://github.com/Charon89/litepack"),
];

#[test]
#[cfg_attr(
    not(lpk_gui_e2e),
    ignore = "needs the Edge WebDriver; build with LPK_GUI_E2E=1 (CI's gui-e2e job)"
)]
fn the_about_box_shows_the_version_and_links_and_closes_with_escape() {
    let temp = tempfile::tempdir().expect("scratch dir");
    let work = temp.path().to_path_buf();
    let mut driver = start_driver(&work, DRIVER_PORT);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let c = connect_args(&mut driver, &work, vec![], DRIVER_PORT).await;
        let outcome = run(&c).await;
        let _ = c.close().await;
        if let Err(e) = outcome {
            panic!("{e}");
        }
    });
}

async fn wait_for(c: &Client, css: &str, what: &str) -> Result<(), String> {
    let deadline = Instant::now() + ROWS_WITHIN;
    loop {
        if c.find(Locator::Css(css)).await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("no {what}; page: {}", page_state(c).await));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn script(c: &Client, js: &str) -> Result<serde_json::Value, String> {
    c.execute(js, vec![]).await.map_err(|e| e.to_string())
}

async fn press(c: &Client, keys: &str) -> Result<(), String> {
    c.active_element()
        .await
        .map_err(|e| e.to_string())?
        .send_keys(keys)
        .await
        .map_err(|e| e.to_string())
}

async fn run(c: &Client) -> Result<(), String> {
    wait_for(c, "#settings-open", "the gear").await?;
    c.find(Locator::Css("#settings-open"))
        .await
        .map_err(|e| e.to_string())?
        .send_keys(ENTER)
        .await
        .map_err(|e| e.to_string())?;
    wait_for(c, "dialog#settings[open] #about-open", "the settings dialog").await?;
    c.find(Locator::Css("#about-open"))
        .await
        .map_err(|e| e.to_string())?
        .send_keys(ENTER)
        .await
        .map_err(|e| e.to_string())?;
    wait_for(
        c,
        "dialog#about[open] #about-links button.link",
        "the About box",
    )
    .await?;
    // The settings dialog closed behind it, and the focus is in the box.
    let v = script(
        c,
        "return [document.querySelectorAll('dialog[open]').length, \
         !!document.activeElement.closest('dialog#about')];",
    )
    .await?;
    if v != serde_json::json!([1, true]) {
        return Err(format!("one open dialog with the focus in it, got {v}"));
    }
    let version = script(
        c,
        "return document.getElementById('about-version').textContent;",
    )
    .await?;
    let version = version.as_str().unwrap_or("");
    if !version.contains(env!("CARGO_PKG_VERSION")) {
        return Err(format!(
            "the version line {version:?} lacks {}",
            env!("CARGO_PKG_VERSION")
        ));
    }
    let links = script(
        c,
        "return [...document.querySelectorAll('#about-links button.link')] \
         .map((b) => [b.dataset.link, b.title, b.textContent.length > 0]);",
    )
    .await?;
    let want: Vec<_> = LINKS
        .iter()
        .map(|(n, u)| serde_json::json!([n, u, true]))
        .collect();
    if links != serde_json::Value::Array(want) {
        return Err(format!("the links: {links}"));
    }
    let remove = script(
        c,
        "const b = document.getElementById('about-remove'); \
         return [b.textContent.length > 0, b.getAttribute('aria-describedby')];",
    )
    .await?;
    if remove != serde_json::json!([true, "about-profile-text"]) {
        return Err(format!("the removal button: {remove}"));
    }
    press(c, ESCAPE).await?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let v = script(
            c,
            "return [!document.querySelector('dialog[open]'), document.activeElement.id];",
        )
        .await?;
        if v == serde_json::json!([true, "settings-open"]) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Esc did not close the box with the focus on the gear: {v}; page: {}",
                page_state(c).await
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
