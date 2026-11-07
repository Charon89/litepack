//! The user-visible strings, one table for the page and the native dialogs
//! (`apps/gui/strings/en.json`, `docs/notes/e5-design.md` section 5). The page receives the
//! table from `startup` (its CSP allows no fetch of its own files).

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// The English table, as shipped.
pub const EN: &str = include_str!("../../../apps/gui/strings/en.json");

/// The table, parsed.
pub fn table() -> &'static BTreeMap<String, String> {
    static T: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(EN).unwrap_or_default())
}

/// The string `key`, or the key itself when missing (a test keeps the table complete).
pub fn get(key: &str) -> &str {
    table().get(key).map_or(key, String::as_str)
}

/// The native password dialog's texts; `confirm` asks twice (creating an archive).
pub fn password_texts(confirm: bool) -> lpk_gui_sys::PasswordTexts {
    lpk_gui_sys::PasswordTexts {
        title: get("password.title").to_string(),
        prompt: get("password.prompt").to_string(),
        confirm: confirm.then(|| get("password.confirm").to_string()),
        ok: get("password.ok").to_string(),
        cancel: get("password.cancel").to_string(),
        mismatch: get("password.mismatch").to_string(),
    }
}

/// The rollback confirmation's title and text (a native box, `lpk_gui_sys::confirm`).
pub fn rollback_texts() -> (&'static str, &'static str) {
    (get("rollback.title"), get("rollback.text"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_parses_and_is_flat() {
        let v: serde_json::Value = serde_json::from_str(EN).unwrap();
        let m = v.as_object().unwrap();
        assert!(!m.is_empty());
        assert!(m.values().all(serde_json::Value::is_string));
        assert_eq!(table().len(), m.len());
        assert_eq!(get("app.title"), "LitePack");
    }
}
