//! Hand-written declarations of the part of libarchive 3.8's public read API this crate uses,
//! written from the documented interface (`archive_read(3)`, `archive_read_open(3)`,
//! `archive_read_data(3)`, `archive_entry(3)` and friends). Nothing here is generated or copied.
//!
//! Types: `la_int64_t` is a 64-bit integer and `la_ssize_t` a pointer-sized signed integer on
//! every 64-bit target this project builds (`isize`); `time_t` is 64-bit on MSVC and on
//! x86_64 Linux; `__LA_MODE_T` is `unsigned short` on Windows and `mode_t` (`u32`) on Linux.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_long, c_uint, c_void};

/// `struct archive` (opaque).
#[repr(C)]
#[derive(Debug)]
pub struct archive {
    _private: [u8; 0],
}

/// `struct archive_entry` (opaque).
#[repr(C)]
#[derive(Debug)]
pub struct archive_entry {
    _private: [u8; 0],
}

/// `la_int64_t`.
pub type la_int64_t = i64;
/// `la_ssize_t`.
pub type la_ssize_t = isize;
/// `time_t`.
pub type time_t = i64;
/// `__LA_MODE_T`.
#[cfg(windows)]
pub type la_mode_t = u16;
/// `__LA_MODE_T`.
#[cfg(not(windows))]
pub type la_mode_t = u32;

/// `ARCHIVE_EOF`.
pub const ARCHIVE_EOF: c_int = 1;
/// `ARCHIVE_OK`.
pub const ARCHIVE_OK: c_int = 0;
/// `ARCHIVE_RETRY`.
pub const ARCHIVE_RETRY: c_int = -10;
/// `ARCHIVE_WARN`.
pub const ARCHIVE_WARN: c_int = -20;
/// `ARCHIVE_FAILED`.
pub const ARCHIVE_FAILED: c_int = -25;
/// `ARCHIVE_FATAL`.
pub const ARCHIVE_FATAL: c_int = -30;

/// `AE_IFMT` and the file types of `archive_entry_filetype`.
pub const AE_IFMT: u32 = 0o170000;
/// Regular file.
pub const AE_IFREG: u32 = 0o100000;
/// Symbolic link.
pub const AE_IFLNK: u32 = 0o120000;
/// Directory.
pub const AE_IFDIR: u32 = 0o040000;

/// `archive_read_callback`.
pub type archive_read_callback =
    unsafe extern "C" fn(*mut archive, *mut c_void, *mut *const c_void) -> la_ssize_t;
/// `archive_skip_callback`.
pub type archive_skip_callback =
    unsafe extern "C" fn(*mut archive, *mut c_void, la_int64_t) -> la_int64_t;
/// `archive_seek_callback`.
pub type archive_seek_callback =
    unsafe extern "C" fn(*mut archive, *mut c_void, la_int64_t, c_int) -> la_int64_t;
/// `archive_open_callback` and `archive_close_callback`.
pub type archive_open_callback = unsafe extern "C" fn(*mut archive, *mut c_void) -> c_int;
/// `archive_close_callback`.
pub type archive_close_callback = unsafe extern "C" fn(*mut archive, *mut c_void) -> c_int;
/// `archive_switch_callback`.
pub type archive_switch_callback =
    unsafe extern "C" fn(*mut archive, *mut c_void, *mut c_void) -> c_int;

extern "C" {
    pub fn archive_read_new() -> *mut archive;
    pub fn archive_read_free(a: *mut archive) -> c_int;

    pub fn archive_read_support_filter_none(a: *mut archive) -> c_int;
    pub fn archive_read_support_filter_gzip(a: *mut archive) -> c_int;
    pub fn archive_read_support_filter_xz(a: *mut archive) -> c_int;
    pub fn archive_read_support_filter_zstd(a: *mut archive) -> c_int;
    pub fn archive_read_support_filter_compress(a: *mut archive) -> c_int;
    pub fn archive_read_support_filter_rpm(a: *mut archive) -> c_int;
    pub fn archive_read_support_filter_uu(a: *mut archive) -> c_int;

    pub fn archive_read_support_format_rar(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_rar5(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_iso9660(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_cab(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_lha(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_ar(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_cpio(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_raw(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_tar(a: *mut archive) -> c_int;

    pub fn archive_read_set_open_callback(a: *mut archive, cb: archive_open_callback) -> c_int;
    pub fn archive_read_set_read_callback(a: *mut archive, cb: archive_read_callback) -> c_int;
    pub fn archive_read_set_seek_callback(a: *mut archive, cb: archive_seek_callback) -> c_int;
    pub fn archive_read_set_skip_callback(a: *mut archive, cb: archive_skip_callback) -> c_int;
    pub fn archive_read_set_close_callback(a: *mut archive, cb: archive_close_callback) -> c_int;
    pub fn archive_read_set_switch_callback(a: *mut archive, cb: archive_switch_callback) -> c_int;
    pub fn archive_read_set_callback_data2(a: *mut archive, data: *mut c_void, i: c_uint) -> c_int;
    pub fn archive_read_append_callback_data(a: *mut archive, data: *mut c_void) -> c_int;
    pub fn archive_read_add_passphrase(a: *mut archive, p: *const c_char) -> c_int;
    pub fn archive_read_open1(a: *mut archive) -> c_int;

    pub fn archive_read_next_header(a: *mut archive, e: *mut *mut archive_entry) -> c_int;
    pub fn archive_read_header_position(a: *mut archive) -> la_int64_t;
    pub fn archive_read_data(a: *mut archive, buf: *mut c_void, len: usize) -> la_ssize_t;
    pub fn archive_read_data_skip(a: *mut archive) -> c_int;

    pub fn archive_errno(a: *mut archive) -> c_int;
    pub fn archive_error_string(a: *mut archive) -> *const c_char;
    pub fn archive_format_name(a: *mut archive) -> *const c_char;
    pub fn archive_filter_count(a: *mut archive) -> c_int;
    pub fn archive_filter_name(a: *mut archive, n: c_int) -> *const c_char;

    pub fn archive_entry_pathname_utf8(e: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_symlink_utf8(e: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_hardlink_utf8(e: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_size(e: *mut archive_entry) -> la_int64_t;
    pub fn archive_entry_size_is_set(e: *mut archive_entry) -> c_int;
    pub fn archive_entry_mtime(e: *mut archive_entry) -> time_t;
    pub fn archive_entry_mtime_nsec(e: *mut archive_entry) -> c_long;
    pub fn archive_entry_mtime_is_set(e: *mut archive_entry) -> c_int;
    pub fn archive_entry_filetype(e: *mut archive_entry) -> la_mode_t;
    pub fn archive_entry_is_encrypted(e: *mut archive_entry) -> c_int;
}
