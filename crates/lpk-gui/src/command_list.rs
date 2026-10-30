/// The page's commands. Each is its own permission (`AppManifest::commands`, so a window may
/// call only what its capability file lists); `build.rs` reads this list too, and a test checks
/// it against `generate_handler!` in `src/host.rs` and the capability files.
pub const COMMANDS: &[&str] = &[
    "startup",
    "open_archive",
    "rows",
    "view",
    "select",
    "open_entry",
    "pick",
    "close_archive",
    "create_archive",
    "cancel",
    "subscribe",
    "listing_shown",
];
