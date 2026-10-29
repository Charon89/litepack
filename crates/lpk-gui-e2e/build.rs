//! `LPK_GUI_E2E=1` turns the WebDriver tests on (`cfg(lpk_gui_e2e)`); without it they are
//! `#[ignore]`d, so a local `cargo nextest run --workspace` skips them (they need
//! the Edge WebDriver that matches the WebView2 runtime).
fn main() {
    println!("cargo::rustc-check-cfg=cfg(lpk_gui_e2e)");
    println!("cargo::rerun-if-env-changed=LPK_GUI_E2E");
    if std::env::var_os("LPK_GUI_E2E").is_some_and(|v| v == "1") {
        println!("cargo::rustc-cfg=lpk_gui_e2e");
    }
}
