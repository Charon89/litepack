//! E5-11's WebDriver harness for `lpk-gui.exe` (tests only, in `tests/`); the library is empty.
//!
//! Run on Windows with `LPK_GUI_E2E=1 cargo test -p lpk-gui-e2e` after building `lpk` and
//! `lpk-gui` in release (CI's `gui-e2e` job). The environment the tests read:
//! - `LPK_E2E_NATIVE_DRIVER`: `msedgedriver.exe` (default: `%EdgeWebDriver%\msedgedriver.exe`,
//!   the runner's preinstalled Edge WebDriver);
//! - `LPK_E2E_LPK`, `LPK_E2E_GUI`: the executables (default: `target/release/lpk.exe` and
//!   `target/release/lpk-gui.exe` of this workspace).
//!
//! The page (`apps/gui/main.js`) counts `error`, `unhandledrejection` and
//! `securitypolicyviolation` events, and its own failures, in `data-lpk-*` attributes of
//! `<html>`; the tests read them as attributes (no script is injected, the CSP is unchanged) and
//! fail on any non-zero count.
#![forbid(unsafe_code)]
