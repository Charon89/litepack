//! Builds libarchive 3.8.9's read side from `vendor/libarchive-3.8.9/` with `cc` against the
//! committed `config.h` of the target (design `docs/notes/e4-design.md` §3).
//!
//! Only the files in [`SOURCES`] are compiled: the read core, the readers `rar`, `rar5`,
//! `iso9660`, `cab`, `lha`, `ar`, `cpio`, `raw` and `tar`, and the filters `gzip`, `xz`, `zstd`,
//! `compress`, `rpm` and `uu`. Never compiled: any writer, any `*_disk*` file, the
//! external-program filter (so no fallback to a program can link), `bzip2`, `lrzip`, `lz4`,
//! `lzop`, `grzip`, `mtree`, `zip`, `7zip`, `xar` and `warc`.
//!
//! When the vendored sources are absent (they wait for the owner's approval to fetch the release
//! tarball), the crate builds a stub whose every open fails with a named error, and this script
//! says so with a cargo warning; the `lpk_libarchive` cfg marks a real build.

use std::path::{Path, PathBuf};

/// The vendored release, relative to the workspace root.
const VENDOR: &str = "vendor/libarchive-3.8.9";

/// The C files compiled, relative to `VENDOR/libarchive/`. To be checked against the vendored
/// tree when it lands (a file missing from the tree fails the build, never silently).
const SOURCES: &[&str] = &[
    // The read core and what it needs.
    "archive_acl.c",
    "archive_check_magic.c",
    "archive_entry.c",
    "archive_entry_sparse.c",
    "archive_entry_stat.c",
    "archive_entry_strmode.c",
    "archive_entry_xattr.c",
    "archive_options.c",
    "archive_read.c",
    "archive_read_add_passphrase.c",
    "archive_read_set_options.c",
    "archive_string.c",
    "archive_string_sprintf.c",
    "archive_time.c",
    "archive_util.c",
    "archive_version_details.c",
    "archive_virtual.c",
    // Helpers of the readers: PPMd (RAR), BLAKE2sp (RAR5), the red-black tree (ISO 9660).
    "archive_ppmd7.c",
    "archive_blake2s_ref.c",
    "archive_blake2sp_ref.c",
    "archive_rb.c",
    // Readers.
    "archive_read_support_format_rar.c",
    "archive_read_support_format_rar5.c",
    "archive_read_support_format_iso9660.c",
    "archive_read_support_format_cab.c",
    "archive_read_support_format_lha.c",
    "archive_read_support_format_ar.c",
    "archive_read_support_format_cpio.c",
    "archive_read_support_format_raw.c",
    "archive_read_support_format_tar.c",
    // Filters.
    "archive_read_support_filter_none.c",
    "archive_read_support_filter_gzip.c",
    "archive_read_support_filter_xz.c",
    "archive_read_support_filter_zstd.c",
    "archive_read_support_filter_compress.c",
    "archive_read_support_filter_rpm.c",
    "archive_read_support_filter_uu.c",
];

/// Windows-only helpers of the core.
const SOURCES_WINDOWS: &[&str] = &["archive_windows.c"];

fn main() {
    println!("cargo::rustc-check-cfg=cfg(lpk_libarchive)");
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let vendor = root.join(VENDOR);
    println!("cargo::rerun-if-changed={}", vendor.display());
    println!("cargo::rerun-if-env-changed=LPK_LIBARCHIVE_REQUIRE");
    let lib = vendor.join("libarchive");
    if !lib.join("archive.h").is_file() {
        if std::env::var_os("LPK_LIBARCHIVE_REQUIRE").is_some() {
            panic!(
                "LPK_LIBARCHIVE_REQUIRE is set but {} has no libarchive sources",
                vendor.display()
            );
        }
        println!(
            "cargo::warning=lpk-libarchive-sys: {VENDOR} is absent, so this build has a stub \
             libarchive that refuses every archive (RAR, ISO, CAB, LZH, AR, CPIO, RPM, .Z, UUE)"
        );
        return;
    }
    let target = std::env::var("TARGET").unwrap_or_default();
    let config_dir = vendor.join("litepack-config").join(&target);
    if !config_dir.join("config.h").is_file() {
        panic!(
            "no committed config.h for target {target} under {}",
            config_dir.display()
        );
    }
    let mut build = cc::Build::new();
    build
        .include(&config_dir)
        .include(&lib)
        .define("HAVE_CONFIG_H", None)
        .define("LIBARCHIVE_STATIC", None)
        .warnings(false);
    for dep in ["Z", "LZMA", "ZSTD"] {
        if let Ok(inc) = std::env::var(format!("DEP_{dep}_INCLUDE")) {
            for p in inc.split(',') {
                build.include(p);
            }
        } else if let Ok(r) = std::env::var(format!("DEP_{dep}_ROOT")) {
            build.include(Path::new(&r).join("include"));
        }
    }
    let windows = target.contains("windows");
    let files = SOURCES
        .iter()
        .chain(if windows { SOURCES_WINDOWS } else { &[] }.iter());
    for f in files {
        let p = lib.join(f);
        assert!(p.is_file(), "libarchive source missing: {}", p.display());
        build.file(p);
    }
    build.compile("archive");
    println!("cargo::rustc-cfg=lpk_libarchive");
}
