/// The page's commands. Each is its own permission (`AppManifest::commands`, so a window may
/// call only what its capability file lists); `build.rs` reads this list too, and a test checks
/// it against `generate_handler!` in `src/host.rs` and the capability files.
pub const COMMANDS: &[&str] = &[
    "startup",
    "settings",
    "set_settings",
    "open_archive",
    "rows",
    "view",
    "select",
    "open_entry",
    "preview",
    "pick",
    "extract_plan",
    "extract_run",
    "extract_archive",
    "recents",
    "clear_recents",
    "close_archive",
    "create_plan",
    "create_name",
    "create_calibrate",
    "create_password",
    "create_run",
    "cancel",
    "subscribe",
    "listing_shown",
];
