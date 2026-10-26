//! The safe wrapper over the vendored libarchive (a real build).
//!
//! Each volume is a heap [`Volume`] whose raw pointer is libarchive's client data for that
//! dataset; the callbacks read and seek its file. The volumes are freed only after
//! `archive_read_free`, so no callback can see a freed volume. Callbacks catch panics (a panic
//! never unwinds into C) and report I/O errors back through the volume.

use std::ffi::{c_int, c_void, CStr, CString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;

use crate::ffi;
use crate::{Entry, EntryKind, Error, ErrorKind, OpenSpec, ReadFilter, ReadFormat};

/// Bytes handed to libarchive per read callback.
const READ_CHUNK: usize = 64 * 1024;

/// One volume: its file, the buffer the last read callback filled, and the last I/O error.
struct Volume {
    file: File,
    buf: Vec<u8>,
    err: Option<std::io::Error>,
}

/// An open archive.
pub struct Archive {
    a: NonNull<ffi::archive>,
    /// From `Box::into_raw`; freed in `Drop` after the archive.
    volumes: Vec<*mut Volume>,
}

impl std::fmt::Debug for Archive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive")
            .field("volumes", &self.volumes.len())
            .finish()
    }
}

impl Drop for Archive {
    fn drop(&mut self) {
        // SAFETY: `self.a` came from `archive_read_new` and is freed exactly once, here.
        unsafe { ffi::archive_read_free(self.a.as_ptr()) };
        for &v in &self.volumes {
            // SAFETY: each pointer came from `Box::into_raw` and libarchive, freed above, can no
            // longer call back with it.
            drop(unsafe { Box::from_raw(v) });
        }
    }
}

/// Copy a C string libarchive owns (`None` for NULL).
///
/// # Safety
/// `p` is NULL or a NUL-terminated string valid for the duration of the call.
unsafe fn copy_cstr(p: *const std::ffi::c_char) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a valid NUL-terminated string.
    Some(unsafe { CStr::from_ptr(p) }.to_bytes().to_vec())
}

/// The volume behind a callback's client data.
///
/// # Safety
/// `data` is one of [`Archive::volumes`]' pointers and no other reference to it is live.
unsafe fn volume<'a>(data: *mut c_void) -> Option<&'a mut Volume> {
    // SAFETY: the caller guarantees `data` is a live, unaliased `Volume`.
    unsafe { data.cast::<Volume>().as_mut() }
}

unsafe extern "C" fn read_cb(
    _a: *mut ffi::archive,
    data: *mut c_void,
    out: *mut *const c_void,
) -> ffi::la_ssize_t {
    // SAFETY: libarchive passes back the client data registered for this dataset.
    let Some(v) = (unsafe { volume(data) }) else {
        return ffi::ARCHIVE_FATAL as ffi::la_ssize_t;
    };
    let r = catch_unwind(AssertUnwindSafe(|| loop {
        match v.file.read(&mut v.buf) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            r => return r,
        }
    }));
    match r {
        Ok(Ok(n)) => {
            if !out.is_null() {
                // SAFETY: `out` is libarchive's non-NULL out-parameter; the buffer stays valid
                // until the next callback on this volume.
                unsafe { *out = v.buf.as_ptr().cast() };
            }
            n as ffi::la_ssize_t
        }
        Ok(Err(e)) => {
            v.err = Some(e);
            ffi::ARCHIVE_FATAL as ffi::la_ssize_t
        }
        Err(_) => ffi::ARCHIVE_FATAL as ffi::la_ssize_t,
    }
}

unsafe extern "C" fn seek_cb(
    _a: *mut ffi::archive,
    data: *mut c_void,
    offset: ffi::la_int64_t,
    whence: c_int,
) -> ffi::la_int64_t {
    // SAFETY: libarchive passes back the client data registered for this dataset.
    let Some(v) = (unsafe { volume(data) }) else {
        return ffi::ARCHIVE_FATAL as ffi::la_int64_t;
    };
    let to = match whence {
        0 if offset >= 0 => SeekFrom::Start(offset as u64),
        1 => SeekFrom::Current(offset),
        2 => SeekFrom::End(offset),
        _ => return ffi::ARCHIVE_FATAL as ffi::la_int64_t,
    };
    match catch_unwind(AssertUnwindSafe(|| v.file.seek(to))) {
        Ok(Ok(p)) => i64::try_from(p).unwrap_or(ffi::ARCHIVE_FATAL as i64),
        Ok(Err(e)) => {
            v.err = Some(e);
            ffi::ARCHIVE_FATAL as ffi::la_int64_t
        }
        Err(_) => ffi::ARCHIVE_FATAL as ffi::la_int64_t,
    }
}

unsafe extern "C" fn skip_cb(
    _a: *mut ffi::archive,
    data: *mut c_void,
    request: ffi::la_int64_t,
) -> ffi::la_int64_t {
    // SAFETY: libarchive passes back the client data registered for this dataset.
    let Some(v) = (unsafe { volume(data) }) else {
        return 0;
    };
    if request <= 0 {
        return 0;
    }
    let r = catch_unwind(AssertUnwindSafe(|| -> std::io::Result<i64> {
        let len = v.file.metadata()?.len();
        let cur = v.file.stream_position()?;
        let to = cur.saturating_add(request as u64).min(len);
        v.file.seek(SeekFrom::Start(to))?;
        Ok((to - cur) as i64)
    }));
    match r {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => {
            v.err = Some(e);
            0
        }
        Err(_) => 0,
    }
}

unsafe extern "C" fn close_cb(_a: *mut ffi::archive, _data: *mut c_void) -> c_int {
    ffi::ARCHIVE_OK
}

unsafe extern "C" fn switch_cb(
    _a: *mut ffi::archive,
    _old: *mut c_void,
    new: *mut c_void,
) -> c_int {
    // SAFETY: libarchive passes back the client data registered for a dataset (or NULL).
    let Some(v) = (unsafe { volume(new) }) else {
        return ffi::ARCHIVE_OK;
    };
    match catch_unwind(AssertUnwindSafe(|| v.file.seek(SeekFrom::Start(0)))) {
        Ok(Ok(_)) => ffi::ARCHIVE_OK,
        Ok(Err(e)) => {
            v.err = Some(e);
            ffi::ARCHIVE_FATAL
        }
        Err(_) => ffi::ARCHIVE_FATAL,
    }
}

type Register = unsafe extern "C" fn(*mut ffi::archive) -> c_int;

fn format_fn(f: ReadFormat) -> Register {
    match f {
        ReadFormat::Rar => ffi::archive_read_support_format_rar,
        ReadFormat::Rar5 => ffi::archive_read_support_format_rar5,
        ReadFormat::Iso9660 => ffi::archive_read_support_format_iso9660,
        ReadFormat::Cab => ffi::archive_read_support_format_cab,
        ReadFormat::Lha => ffi::archive_read_support_format_lha,
        ReadFormat::Ar => ffi::archive_read_support_format_ar,
        ReadFormat::Cpio => ffi::archive_read_support_format_cpio,
        ReadFormat::Raw => ffi::archive_read_support_format_raw,
        ReadFormat::Tar => ffi::archive_read_support_format_tar,
    }
}

fn filter_fn(f: ReadFilter) -> Register {
    match f {
        ReadFilter::Gzip => ffi::archive_read_support_filter_gzip,
        ReadFilter::Xz => ffi::archive_read_support_filter_xz,
        ReadFilter::Zstd => ffi::archive_read_support_filter_zstd,
        ReadFilter::Compress => ffi::archive_read_support_filter_compress,
        ReadFilter::Rpm => ffi::archive_read_support_filter_rpm,
        ReadFilter::Uu => ffi::archive_read_support_filter_uu,
    }
}

impl Archive {
    /// Open the archive of `spec`: register its readers and filters, its volumes as datasets,
    /// and read the first header block. Errors carry libarchive's message.
    pub fn open(spec: OpenSpec<'_>) -> Result<Archive, Error> {
        if spec.volumes.is_empty() {
            return Err(Error {
                kind: ErrorKind::Fatal,
                errno: 0,
                message: "no volume to open".into(),
            });
        }
        // SAFETY: a plain constructor; NULL (out of memory) is handled below.
        let raw = unsafe { ffi::archive_read_new() };
        let a = NonNull::new(raw).ok_or_else(|| Error {
            kind: ErrorKind::Fatal,
            errno: 0,
            message: "libarchive could not allocate an archive object".into(),
        })?;
        let mut this = Archive {
            a,
            volumes: Vec::with_capacity(spec.volumes.len()),
        };
        for file in spec.volumes {
            this.volumes.push(Box::into_raw(Box::new(Volume {
                file,
                buf: vec![0; READ_CHUNK],
                err: None,
            })));
        }
        let p = this.a.as_ptr();
        // SAFETY: `p` is a live archive object.
        this.check(unsafe { ffi::archive_read_support_filter_none(p) })?;
        for &f in spec.filters {
            let reg = filter_fn(f);
            // SAFETY: `p` is a live archive object; `reg` is one of libarchive's registrars.
            this.check(unsafe { reg(p) })?;
        }
        for &f in spec.formats {
            let reg = format_fn(f);
            // SAFETY: as above.
            this.check(unsafe { reg(p) })?;
        }
        // SAFETY: `p` is live; the callbacks match libarchive's documented signatures.
        this.check(unsafe { ffi::archive_read_set_read_callback(p, read_cb) })?;
        // SAFETY: as above.
        this.check(unsafe { ffi::archive_read_set_seek_callback(p, seek_cb) })?;
        // SAFETY: as above.
        this.check(unsafe { ffi::archive_read_set_skip_callback(p, skip_cb) })?;
        // SAFETY: as above.
        this.check(unsafe { ffi::archive_read_set_close_callback(p, close_cb) })?;
        // SAFETY: as above.
        this.check(unsafe { ffi::archive_read_set_switch_callback(p, switch_cb) })?;
        let vols = this.volumes.clone();
        for (i, &v) in vols.iter().enumerate() {
            let rc = if i == 0 {
                // SAFETY: `v` is a live `Volume` that outlives the archive (freed in `Drop`).
                unsafe { ffi::archive_read_set_callback_data2(p, v.cast(), 0) }
            } else {
                // SAFETY: as above.
                unsafe { ffi::archive_read_append_callback_data(p, v.cast()) }
            };
            this.check(rc)?;
        }
        if let Some(pw) = spec.passphrase {
            let c = CString::new(pw).map_err(|_| Error {
                kind: ErrorKind::Failed,
                errno: 0,
                message: "the password contains a NUL byte".into(),
            })?;
            // SAFETY: `p` is live; libarchive copies the string.
            this.check(unsafe { ffi::archive_read_add_passphrase(p, c.as_ptr()) })?;
        }
        // SAFETY: `p` is live and fully configured.
        this.check(unsafe { ffi::archive_read_open1(p) })?;
        Ok(this)
    }

    /// `Ok` for `ARCHIVE_OK`, `ARCHIVE_WARN` and `ARCHIVE_EOF`; otherwise the error.
    fn check(&mut self, rc: c_int) -> Result<(), Error> {
        match rc {
            ffi::ARCHIVE_OK | ffi::ARCHIVE_WARN | ffi::ARCHIVE_EOF => Ok(()),
            _ => Err(self.error(rc)),
        }
    }

    /// The error for return code `rc`: an I/O error a callback recorded wins over libarchive's
    /// message.
    fn error(&mut self, rc: c_int) -> Error {
        for &v in &self.volumes {
            // SAFETY: the volumes are live; no callback runs during this method (libarchive is
            // single-threaded and not inside a call).
            if let Some(vol) = unsafe { v.as_mut() } {
                if let Some(e) = vol.err.take() {
                    return Error {
                        kind: ErrorKind::Io,
                        errno: e.raw_os_error().unwrap_or(0),
                        message: e.to_string(),
                    };
                }
            }
        }
        let kind = match rc {
            ffi::ARCHIVE_FATAL => ErrorKind::Fatal,
            ffi::ARCHIVE_FAILED => ErrorKind::Failed,
            _ => ErrorKind::Other,
        };
        let p = self.a.as_ptr();
        // SAFETY: `p` is live.
        let errno = unsafe { ffi::archive_errno(p) };
        // SAFETY: `p` is live; the string is owned by libarchive and copied at once.
        let msg = unsafe { copy_cstr(ffi::archive_error_string(p)) };
        Error {
            kind,
            errno,
            message: msg
                .map(|m| String::from_utf8_lossy(&m).into_owned())
                .unwrap_or_else(|| format!("libarchive error {rc}")),
        }
    }

    /// The next entry's header, or `None` at the end. The previous entry's unread data is
    /// skipped by libarchive.
    pub fn next_entry(&mut self) -> Result<Option<Entry>, Error> {
        let p = self.a.as_ptr();
        let mut e: *mut ffi::archive_entry = std::ptr::null_mut();
        // SAFETY: `p` is live; `e` is a valid out-parameter.
        let rc = unsafe { ffi::archive_read_next_header(p, &mut e) };
        let warning = match rc {
            ffi::ARCHIVE_EOF => return Ok(None),
            ffi::ARCHIVE_OK => None,
            ffi::ARCHIVE_WARN => Some(self.error(rc).message),
            _ => return Err(self.error(rc)),
        };
        if e.is_null() {
            return Err(Error {
                kind: ErrorKind::Fatal,
                errno: 0,
                message: "libarchive returned no entry".into(),
            });
        }
        // SAFETY: `e` is the entry libarchive just returned, valid until
        // the next header call; strings are copied at once.
        let path = unsafe { copy_cstr(ffi::archive_entry_pathname_utf8(e)) };
        // SAFETY: as above.
        let ft = u32::from(unsafe { ffi::archive_entry_filetype(e) }) & ffi::AE_IFMT;
        let kind = match ft {
            ffi::AE_IFREG => EntryKind::File,
            ffi::AE_IFDIR => EntryKind::Directory,
            ffi::AE_IFLNK => EntryKind::Symlink,
            other => EntryKind::Other(other),
        };
        // SAFETY: as above.
        let size = if unsafe { ffi::archive_entry_size_is_set(e) } != 0 {
            // SAFETY: as above.
            u64::try_from(unsafe { ffi::archive_entry_size(e) }).ok()
        } else {
            None
        };
        // SAFETY: as above.
        let mtime = if unsafe { ffi::archive_entry_mtime_is_set(e) } != 0 {
            // SAFETY: as above.
            let s = unsafe { ffi::archive_entry_mtime(e) };
            // SAFETY: as above.
            let ns = i64::from(unsafe { ffi::archive_entry_mtime_nsec(e) });
            Some((s, ns))
        } else {
            None
        };
        // SAFETY: as above.
        let symlink = unsafe { copy_cstr(ffi::archive_entry_symlink_utf8(e)) };
        // SAFETY: as above.
        let hardlink = unsafe { copy_cstr(ffi::archive_entry_hardlink_utf8(e)) };
        // SAFETY: as above.
        let encrypted = unsafe { ffi::archive_entry_is_encrypted(e) } != 0;
        // SAFETY: `p` is live.
        let header_position = unsafe { ffi::archive_read_header_position(p) };
        Ok(Some(Entry {
            path,
            kind,
            size,
            mtime,
            symlink,
            hardlink,
            encrypted,
            header_position,
            warning,
        }))
    }

    /// Read the current entry's data into `buf`; `Ok(0)` at its end.
    pub fn read_data(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let p = self.a.as_ptr();
        // SAFETY: `p` is live; `buf` is writable for its length.
        let n = unsafe { ffi::archive_read_data(p, buf.as_mut_ptr().cast(), buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        Err(self.error(c_int::try_from(n).unwrap_or(ffi::ARCHIVE_FATAL)))
    }

    /// The detected format's name.
    pub fn format_name(&self) -> String {
        // SAFETY: `self.a` is live; the string is copied at once.
        let s = unsafe { copy_cstr(ffi::archive_format_name(self.a.as_ptr())) };
        s.map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    }

    /// The filters in use, outermost first (without `none`).
    pub fn filter_names(&self) -> Vec<String> {
        let p = self.a.as_ptr();
        // SAFETY: `p` is live.
        let n = unsafe { ffi::archive_filter_count(p) };
        let mut out = Vec::new();
        for i in 0..n.max(0) {
            // SAFETY: `p` is live and `i` is below the filter count; the string is copied.
            let s = unsafe { copy_cstr(ffi::archive_filter_name(p, i)) };
            if let Some(s) = s {
                let s = String::from_utf8_lossy(&s).into_owned();
                if s != "none" {
                    out.push(s);
                }
            }
        }
        out
    }
}
