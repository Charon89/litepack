//! The Windows implementation (see the crate documentation).

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::core::PWSTR;
use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::{
    NtCreateFile, FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
    FILE_OPEN_IF, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    NTCREATEFILE_CREATE_DISPOSITION, NTCREATEFILE_CREATE_OPTIONS,
};
use windows::Win32::Foundation::{
    GetLastError, RtlNtStatusToDosError, ERROR_NOT_ALL_ASSIGNED, HANDLE, LUID, NTSTATUS,
    OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, STATUS_REPARSE_POINT_ENCOUNTERED,
    STATUS_STOPPED_ON_SYMLINK, UNICODE_STRING,
};
use windows::Win32::Security::{
    AdjustTokenPrivileges, DuplicateTokenEx, LookupPrivilegeValueW, RevertToSelf,
    SecurityImpersonation, TokenImpersonation, LUID_AND_ATTRIBUTES, SE_CREATE_SYMBOLIC_LINK_NAME,
    SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_DUPLICATE, TOKEN_IMPERSONATE,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{
    FileAttributeTagInfo, FileDispositionInfo, FileDispositionInfoEx, FileIdInfo,
    GetFileInformationByHandle, GetFileInformationByHandleEx, SetFileInformationByHandle,
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO,
    FILE_DISPOSITION_INFO_EX, FILE_DISPOSITION_INFO_EX_FLAGS, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
    FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, SYNCHRONIZE,
};
use windows::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
use windows::Win32::System::SystemServices::IO_REPARSE_TAG_SYMLINK;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken, SetThreadToken,
};
use windows::Win32::System::IO::{DeviceIoControl, IO_STATUS_BLOCK};

/// Why an operation was refused or failed.
#[derive(Debug)]
pub enum FsError {
    /// A symlink, junction or mount point (a name-surrogate reparse tag) where a directory to
    /// go through, or a file to reopen, was expected: D-54's "linked parent".
    Linked,
    /// The name now holds another file or directory than the one recorded (file IDs differ).
    Changed,
    /// Any other failure, as the system reported it.
    Io(io::Error),
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FsError::Linked => f.write_str("a symlink, junction or mount point is in the way"),
            FsError::Changed => f.write_str("replaced during the extraction (file ID changed)"),
            FsError::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for FsError {}

impl From<io::Error> for FsError {
    fn from(e: io::Error) -> Self {
        FsError::Io(e)
    }
}

impl From<windows::core::Error> for FsError {
    fn from(e: windows::core::Error) -> Self {
        // A Win32 error carried as an HRESULT (`0x8007xxxx`) keeps its code, so its kind.
        let code = e.code().0 as u32;
        if code & 0xFFFF_0000 == 0x8007_0000 {
            FsError::Io(io::Error::from_raw_os_error((code & 0xFFFF) as i32))
        } else {
            FsError::Io(io::Error::other(e.to_string()))
        }
    }
}

/// A file's identity on its volume: the volume serial number and the 128-bit file ID
/// (`FILE_ID_INFO`, unique on NTFS and ReFS), or, where that query fails (FAT, exFAT), the
/// 64-bit file index of `BY_HANDLE_FILE_INFORMATION` widened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId {
    volume: u64,
    id: [u8; 16],
}

/// A name-surrogate reparse tag (`IsReparseTagNameSurrogate`): symlinks, junctions, mount
/// points; not cloud placeholders or dedup files.
const NAME_SURROGATE: u32 = 0x2000_0000;
/// `SYMLINK_FLAG_RELATIVE` of a symlink reparse buffer.
const SYMLINK_FLAG_RELATIVE: u32 = 1;

thread_local! {
    static SHARE_DELETE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test builds only: on this thread, add `FILE_SHARE_DELETE` to the directory handles opened
/// from now on (so a held directory can be renamed, as it could without this crate).
#[cfg(feature = "test-hooks")]
pub fn set_share_delete_for_tests(on: bool) {
    SHARE_DELETE.with(|c| c.set(on));
}

fn dir_share() -> FILE_SHARE_MODE {
    if SHARE_DELETE.with(std::cell::Cell::get) {
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
    } else {
        FILE_SHARE_READ | FILE_SHARE_WRITE
    }
}

const FILE_SHARE_ALL: FILE_SHARE_MODE =
    FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0);

fn dir_access() -> FILE_ACCESS_RIGHTS {
    FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE
}

fn status_error(st: NTSTATUS) -> FsError {
    if st == STATUS_REPARSE_POINT_ENCOUNTERED || st == STATUS_STOPPED_ON_SYMLINK {
        return FsError::Linked;
    }
    // SAFETY: a pure translation of a status code; no pointer is involved.
    let code = unsafe { RtlNtStatusToDosError(st) };
    FsError::Io(io::Error::from_raw_os_error(code as i32))
}

/// One component, as UTF-16, refused when it could name anything but a child of the parent.
fn component(name: &OsStr) -> Result<Vec<u16>, FsError> {
    let w: Vec<u16> = name.encode_wide().collect();
    let bad = w.is_empty()
        || w == [u16::from(b'.')]
        || w == [u16::from(b'.'), u16::from(b'.')]
        || w.iter().any(|&c| {
            c == 0 || c == u16::from(b'\\') || c == u16::from(b'/') || c == u16::from(b':')
        });
    if bad || w.len() > 32_767 {
        return Err(FsError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a single file name component",
        )));
    }
    Ok(w)
}

/// `NtCreateFile` of `name` (one component) relative to `root`, with `OBJ_DONT_REPARSE` and
/// `OBJ_CASE_INSENSITIVE`, synchronous I/O, and `FILE_OPEN_REPARSE_POINT` always (the final
/// component is opened as itself, never followed).
fn nt_create(
    root: &OwnedHandle,
    name: &[u16],
    access: FILE_ACCESS_RIGHTS,
    share: FILE_SHARE_MODE,
    disposition: NTCREATEFILE_CREATE_DISPOSITION,
    options: NTCREATEFILE_CREATE_OPTIONS,
) -> Result<OwnedHandle, FsError> {
    let bytes = u16::try_from(name.len() * 2)
        .map_err(|_| FsError::Io(io::Error::new(io::ErrorKind::InvalidInput, "name too long")))?;
    let us = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        // The buffer is only read by the call (the type is `PWSTR` for historical reasons).
        Buffer: PWSTR(name.as_ptr().cast_mut()),
    };
    let oa = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: HANDLE(root.as_raw_handle()),
        ObjectName: &us,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut iosb = IO_STATUS_BLOCK::default();
    let mut h = HANDLE::default();
    // SAFETY: every pointer is to a live local (`h`, `oa`, `us` and the `name` buffer it points
    // to, `iosb`) for the duration of the call; `root` is an open handle borrowed for the call;
    // no allocation size and no extended attributes are passed.
    let st = unsafe {
        NtCreateFile(
            &mut h,
            access,
            &oa,
            &mut iosb,
            None,
            FILE_ATTRIBUTE_NORMAL,
            share,
            disposition,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            None,
            0,
        )
    };
    if st.0 < 0 {
        return Err(status_error(st));
    }
    // SAFETY: the call succeeded, so `h` is a new handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(h.0) })
}

/// The attributes and the reparse tag of an open handle.
fn tag_info(h: &OwnedHandle) -> Result<(u32, u32), FsError> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: `info` is a live, correctly sized output buffer for this class; `h` is open.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(h.as_raw_handle()),
            FileAttributeTagInfo,
            (&raw mut info).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    }?;
    Ok((info.FileAttributes, info.ReparseTag))
}

fn is_name_surrogate(attrs: u32, tag: u32) -> bool {
    attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 && tag & NAME_SURROGATE != 0
}

fn file_id(h: &OwnedHandle) -> Result<FileId, FsError> {
    let mut wide = FILE_ID_INFO::default();
    // SAFETY: `wide` is a live, correctly sized output buffer for this class; `h` is open.
    let r = unsafe {
        GetFileInformationByHandleEx(
            HANDLE(h.as_raw_handle()),
            FileIdInfo,
            (&raw mut wide).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if r.is_ok() {
        return Ok(FileId {
            volume: wide.VolumeSerialNumber,
            id: wide.FileId.Identifier,
        });
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `info` is a live output structure; `h` is open.
    unsafe { GetFileInformationByHandle(HANDLE(h.as_raw_handle()), &mut info) }?;
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&index.to_le_bytes());
    Ok(FileId {
        volume: u64::from(info.dwVolumeSerialNumber),
        id,
    })
}

/// Delete what `h` is open on (opened with `DELETE`): POSIX semantics where the volume has
/// them (the name goes at once), else the classic disposition (the name goes at the close).
fn dispose(h: &OwnedHandle) -> Result<(), FsError> {
    let ex = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
            FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0,
        ),
    };
    // SAFETY: `ex` is a live, correctly sized input buffer for this class; `h` is open.
    let r = unsafe {
        SetFileInformationByHandle(
            HANDLE(h.as_raw_handle()),
            FileDispositionInfoEx,
            (&raw const ex).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if r.is_ok() {
        return Ok(());
    }
    let classic = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `classic` is a live, correctly sized input buffer for this class; `h` is open.
    unsafe {
        SetFileInformationByHandle(
            HANDLE(h.as_raw_handle()),
            FileDispositionInfo,
            (&raw const classic).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }?;
    Ok(())
}

/// A verified directory handle, held without `FILE_SHARE_DELETE` (in a test build with the
/// `test-hooks` switch on, with it).
#[derive(Debug)]
pub struct Dir {
    h: OwnedHandle,
    id: FileId,
}

impl Dir {
    /// Open the destination by its path, as the user chose it (a link on the way is
    /// followed: that is the choice); everything below it goes through [`Dir::open_dir`].
    pub fn open_path(path: &Path) -> Result<Dir, FsError> {
        let f = OpenOptions::new()
            .access_mode((dir_access()).0)
            .share_mode(dir_share().0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(path)?;
        let h = OwnedHandle::from(f);
        let (attrs, _) = tag_info(&h)?;
        if attrs & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
            return Err(FsError::Io(io::Error::new(
                io::ErrorKind::NotADirectory,
                "the destination is not a directory",
            )));
        }
        let id = file_id(&h)?;
        Ok(Dir { h, id })
    }

    /// This directory's file ID.
    pub fn id(&self) -> FileId {
        self.id
    }

    /// Open the child directory `name` (with `create`, create it when absent), relative to
    /// this one; a name-surrogate reparse point there is [`FsError::Linked`].
    pub fn open_dir(&self, name: &OsStr, create: bool) -> Result<Dir, FsError> {
        let w = component(name)?;
        let disposition = if create { FILE_OPEN_IF } else { FILE_OPEN };
        let h = nt_create(
            &self.h,
            &w,
            dir_access(),
            dir_share(),
            disposition,
            FILE_DIRECTORY_FILE,
        )?;
        let (attrs, tag) = tag_info(&h)?;
        if is_name_surrogate(attrs, tag) {
            return Err(FsError::Linked);
        }
        let id = file_id(&h)?;
        Ok(Dir { h, id })
    }

    /// Create the file `name` in this directory (`FILE_CREATE`: fails when anything is
    /// there), open for writing; with its file ID.
    pub fn create_file(&self, name: &OsStr) -> Result<(File, FileId), FsError> {
        let w = component(name)?;
        let h = nt_create(
            &self.h,
            &w,
            FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES,
            FILE_SHARE_ALL,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
        )?;
        let id = file_id(&h)?;
        Ok((File::from(h), id))
    }

    /// Open the file `name` in this directory, for writing (no create, no truncation) or for
    /// reading, when it is still the file `id`.
    pub fn open_file(&self, name: &OsStr, id: FileId, write: bool) -> Result<File, FsError> {
        let w = component(name)?;
        let access = if write {
            FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES
        } else {
            FILE_GENERIC_READ
        };
        let h = nt_create(
            &self.h,
            &w,
            access,
            FILE_SHARE_ALL,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
        )?;
        let (attrs, tag) = tag_info(&h)?;
        if is_name_surrogate(attrs, tag) {
            return Err(FsError::Linked);
        }
        if file_id(&h)? != id {
            return Err(FsError::Changed);
        }
        Ok(File::from(h))
    }

    /// Remove what is at `name` in this directory, never following it: with `Some(id)`, only
    /// when it is still the file `id`; with `None`, a file or a link (a directory link or a
    /// junction removed as the link), while a real directory is an `AlreadyExists` error.
    pub fn remove(&self, name: &OsStr, id: Option<FileId>) -> Result<(), FsError> {
        let w = component(name)?;
        let h = nt_create(
            &self.h,
            &w,
            DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_ALL,
            FILE_OPEN,
            NTCREATEFILE_CREATE_OPTIONS(0),
        )?;
        let (attrs, tag) = tag_info(&h)?;
        match id {
            Some(id) if file_id(&h)? != id => return Err(FsError::Changed),
            Some(_) => {}
            None => {
                // Only a name surrogate (symlink, junction, mount point) is a link; any other
                // directory, a cloud placeholder or a dedup one included, is in the way.
                if attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0 && !is_name_surrogate(attrs, tag) {
                    return Err(FsError::Io(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "a directory is in the way",
                    )));
                }
            }
        }
        dispose(&h)
    }

    /// Create the symlink `name` in this directory with the relative `target` (already in
    /// Windows form: `\` separators), a directory link when `dir`. The node is created
    /// relative to this directory (`FILE_CREATE`) and given the link's reparse data; on
    /// failure it is removed again. Needs `SeCreateSymbolicLinkPrivilege` in the token.
    pub fn symlink(&self, name: &OsStr, target: &str, dir: bool) -> Result<(), FsError> {
        let w = component(name)?;
        let buf = symlink_buffer(target)?;
        let options = if dir {
            FILE_DIRECTORY_FILE
        } else {
            FILE_NON_DIRECTORY_FILE
        };
        let h = nt_create(
            &self.h,
            &w,
            FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | FILE_READ_ATTRIBUTES | DELETE | SYNCHRONIZE,
            FILE_SHARE_ALL,
            FILE_CREATE,
            options,
        )?;
        let set = with_symlink_privilege(|| {
            let mut returned = 0u32;
            // SAFETY: `buf` is a live input buffer of the length passed; no output buffer; the
            // call is synchronous (no OVERLAPPED); `h` is open with write access.
            unsafe {
                DeviceIoControl(
                    HANDLE(h.as_raw_handle()),
                    FSCTL_SET_REPARSE_POINT,
                    Some(buf.as_ptr().cast()),
                    buf.len() as u32,
                    None,
                    0,
                    Some(&mut returned),
                    None,
                )
            }
            .map_err(FsError::from)
        });
        if let Err(e) = set {
            let _ = dispose(&h);
            return Err(e);
        }
        Ok(())
    }
}

/// A symlink reparse buffer (`REPARSE_DATA_BUFFER`, `SymbolicLinkReparseBuffer`) for a
/// relative `target`: the substitute name and the print name are both the target.
fn symlink_buffer(target: &str) -> Result<Vec<u8>, FsError> {
    let t: Vec<u16> = target.encode_utf16().collect();
    let too_long = || {
        FsError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target too long",
        ))
    };
    let n = u16::try_from(t.len() * 2).map_err(|_| too_long())?;
    // Four u16 offsets and lengths, the flags, then both names.
    let data_len = u16::try_from(12 + 2 * usize::from(n)).map_err(|_| too_long())?;
    let mut b = Vec::with_capacity(8 + usize::from(data_len));
    b.extend_from_slice(&IO_REPARSE_TAG_SYMLINK.to_le_bytes());
    b.extend_from_slice(&data_len.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
    b.extend_from_slice(&n.to_le_bytes()); // SubstituteNameLength
    b.extend_from_slice(&n.to_le_bytes()); // PrintNameOffset
    b.extend_from_slice(&n.to_le_bytes()); // PrintNameLength
    b.extend_from_slice(&SYMLINK_FLAG_RELATIVE.to_le_bytes());
    for _ in 0..2 {
        for c in &t {
            b.extend_from_slice(&c.to_le_bytes());
        }
    }
    Ok(b)
}

/// An impersonation copy of the process token with `SeCreateSymbolicLinkPrivilege` enabled,
/// or `None` when the token does not hold it.
fn privileged_token() -> Result<Option<OwnedHandle>, FsError> {
    let mut process = HANDLE::default();
    // SAFETY: the pseudo-handle of this process; `process` is a live output.
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY,
            &mut process,
        )
    }?;
    // SAFETY: the call succeeded, so `process` is a new handle that nothing else owns.
    let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
    let mut dup = HANDLE::default();
    // SAFETY: `process` is open with TOKEN_DUPLICATE; no security attributes; `dup` is a live
    // output.
    unsafe {
        DuplicateTokenEx(
            HANDLE(process.as_raw_handle()),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_IMPERSONATE | TOKEN_QUERY,
            None,
            SecurityImpersonation,
            TokenImpersonation,
            &mut dup,
        )
    }?;
    // SAFETY: the call succeeded, so `dup` is a new handle that nothing else owns.
    let dup = unsafe { OwnedHandle::from_raw_handle(dup.0) };
    let mut luid = LUID::default();
    // SAFETY: a constant, NUL-terminated privilege name; `luid` is a live output.
    unsafe { LookupPrivilegeValueW(None, SE_CREATE_SYMBOLIC_LINK_NAME, &mut luid) }?;
    let tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: `dup` (our own copy, never the process token) is open with
    // TOKEN_ADJUST_PRIVILEGES; `tp` is a live, one-entry input; no previous state is asked
    // for; the last error is read at once, as the call's contract requires.
    let held = unsafe {
        AdjustTokenPrivileges(HANDLE(dup.as_raw_handle()), false, Some(&tp), 0, None, None)
            .map(|()| GetLastError() != ERROR_NOT_ALL_ASSIGNED)
    }?;
    Ok(held.then_some(dup))
}

/// Whether the process token holds `SeCreateSymbolicLinkPrivilege` (enabled or not).
/// Developer Mode alone does not give it; an elevated administrator's token has it.
pub fn symlink_privilege_held() -> bool {
    matches!(privileged_token(), Ok(Some(_)))
}

/// Run `f` on this thread under an impersonation copy of the process token with
/// `SeCreateSymbolicLinkPrivilege` enabled (when the token holds it; otherwise `f` runs as
/// is and its own call reports the missing privilege). The thread's token is restored after.
fn with_symlink_privilege<T>(f: impl FnOnce() -> Result<T, FsError>) -> Result<T, FsError> {
    let Some(token) = privileged_token()? else {
        return f();
    };
    // The thread's own token, if it impersonates already: restored afterwards, so an earlier
    // identity always comes back (none: the thread reverts to the process token).
    let mut before = HANDLE::default();
    // SAFETY: the pseudo-handle of this thread; `before` is a live output; the access check
    // is made against the process (open_as_self), so it works whatever the thread holds.
    let previous = unsafe {
        OpenThreadToken(
            GetCurrentThread(),
            TOKEN_IMPERSONATE | TOKEN_QUERY,
            true,
            &mut before,
        )
    }
    .ok()
    // SAFETY: the call succeeded, so `before` is a new handle that nothing else owns.
    .map(|()| unsafe { OwnedHandle::from_raw_handle(before.0) });
    // SAFETY: `token` is an open impersonation token with TOKEN_IMPERSONATE; `None` is the
    // calling thread.
    unsafe { SetThreadToken(None, Some(HANDLE(token.as_raw_handle()))) }?;
    struct Restore(Option<OwnedHandle>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.0 {
                Some(t) => {
                    // SAFETY: `t` is the thread's earlier impersonation token, open with
                    // TOKEN_IMPERSONATE; `None` is the calling thread.
                    let _ = unsafe { SetThreadToken(None, Some(HANDLE(t.as_raw_handle()))) };
                }
                None => {
                    // SAFETY: ends this thread's impersonation; no pointer is involved.
                    let _ = unsafe { RevertToSelf() };
                }
            }
        }
    }
    let _restore = Restore(previous);
    f()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn junction(link: &Path, target: &Path) {
        let st = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(st.status.success(), "{st:?}");
    }

    #[test]
    fn directories_and_files_go_through_the_held_handle() {
        let d = tempfile::tempdir().unwrap();
        let root = Dir::open_path(d.path()).unwrap();
        let sub = root.open_dir(OsStr::new("sub"), true).unwrap();
        // Opening again finds the same directory, whatever the case of the name.
        let again = root.open_dir(OsStr::new("SUB"), false).unwrap();
        assert_eq!(sub.id(), again.id());
        drop(again);
        let (mut f, id) = sub.create_file(OsStr::new("f.txt")).unwrap();
        io::Write::write_all(&mut f, b"abc").unwrap();
        drop(f);
        // FILE_CREATE: an existing name is refused.
        let e = sub.create_file(OsStr::new("f.txt")).unwrap_err();
        assert!(
            matches!(&e, FsError::Io(e) if e.kind() == io::ErrorKind::AlreadyExists),
            "{e:?}"
        );
        let mut r = sub.open_file(OsStr::new("f.txt"), id, false).unwrap();
        let mut s = String::new();
        io::Read::read_to_string(&mut r, &mut s).unwrap();
        assert_eq!(s, "abc");
        drop(r);
        // Another file under the same name is not the recorded one.
        sub.remove(OsStr::new("f.txt"), Some(id)).unwrap();
        let (f2, _) = sub.create_file(OsStr::new("f.txt")).unwrap();
        drop(f2);
        assert!(matches!(
            sub.open_file(OsStr::new("f.txt"), id, true),
            Err(FsError::Changed)
        ));
        assert!(matches!(
            sub.remove(OsStr::new("f.txt"), Some(id)),
            Err(FsError::Changed)
        ));
        // A real directory is never removed as "what is in the way".
        sub.open_dir(OsStr::new("inner"), true).unwrap();
        let e = sub.remove(OsStr::new("inner"), None).unwrap_err();
        assert!(
            matches!(&e, FsError::Io(e) if e.kind() == io::ErrorKind::AlreadyExists),
            "{e:?}"
        );
        assert!(d.path().join("sub/inner").is_dir());
    }

    #[test]
    fn a_held_directory_cannot_be_renamed() {
        let d = tempfile::tempdir().unwrap();
        let root = Dir::open_path(d.path()).unwrap();
        let sub = root.open_dir(OsStr::new("sub"), true).unwrap();
        assert!(std::fs::rename(d.path().join("sub"), d.path().join("moved")).is_err());
        drop(sub);
        drop(root);
        std::fs::rename(d.path().join("sub"), d.path().join("moved")).unwrap();
    }

    #[test]
    fn a_junction_is_refused_as_linked_and_removed_as_a_link() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("canary"), b"keep").unwrap();
        junction(&d.path().join("j"), outside.path());
        let root = Dir::open_path(d.path()).unwrap();
        assert!(matches!(
            root.open_dir(OsStr::new("j"), false),
            Err(FsError::Linked)
        ));
        assert!(matches!(
            root.open_dir(OsStr::new("J"), true),
            Err(FsError::Linked)
        ));
        root.remove(OsStr::new("j"), None).unwrap();
        assert!(std::fs::symlink_metadata(d.path().join("j")).is_err());
        assert_eq!(
            std::fs::read(outside.path().join("canary")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn names_that_are_not_one_component_are_refused() {
        let d = tempfile::tempdir().unwrap();
        let root = Dir::open_path(d.path()).unwrap();
        for bad in ["", ".", "..", "a\\b", "a/b", "a:s", "a\0b"] {
            assert!(root.open_dir(OsStr::new(bad), true).is_err(), "{bad:?}");
            assert!(root.create_file(OsStr::new(bad)).is_err(), "{bad:?}");
        }
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn deep_trees_need_no_max_path() {
        let d = tempfile::tempdir().unwrap();
        let mut cur = Dir::open_path(d.path()).unwrap();
        let name = "n".repeat(100);
        for _ in 0..40 {
            cur = cur.open_dir(OsStr::new(&name), true).unwrap();
        }
        let (f, _) = cur.create_file(OsStr::new("deep.txt")).unwrap();
        drop(f);
    }

    /// The probe against the real token: `whoami /priv` lists the privilege exactly when the
    /// probe says it is held.
    #[test]
    fn the_privilege_probe_matches_the_token() {
        let out = std::process::Command::new("whoami")
            .arg("/priv")
            .output()
            .unwrap();
        let listed = String::from_utf8_lossy(&out.stdout).contains("SeCreateSymbolicLinkPrivilege");
        assert_eq!(symlink_privilege_held(), listed);
    }

    #[test]
    fn symlinks_are_created_relative_to_the_held_directory_or_fail_without_privilege() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("t.txt"), b"x").unwrap();
        std::fs::create_dir(d.path().join("td")).unwrap();
        let root = Dir::open_path(d.path()).unwrap();
        let r = root.symlink(OsStr::new("l"), "t.txt", false);
        if symlink_privilege_held() {
            r.unwrap();
            root.symlink(OsStr::new("ld"), "td", true).unwrap();
            drop(root);
            assert_eq!(
                std::fs::read_link(d.path().join("l")).unwrap(),
                Path::new("t.txt")
            );
            assert_eq!(std::fs::read(d.path().join("l")).unwrap(), b"x");
            use std::os::windows::fs::FileTypeExt;
            let t = std::fs::symlink_metadata(d.path().join("ld"))
                .unwrap()
                .file_type();
            assert!(t.is_symlink_dir());
        } else {
            assert!(r.is_err());
            // The node made for the link is removed again.
            assert!(std::fs::symlink_metadata(d.path().join("l")).is_err());
            println!("SKIPPED: no SeCreateSymbolicLinkPrivilege; the failure path was checked");
        }
    }

    #[test]
    fn the_reparse_buffer_has_the_documented_layout() {
        let b = symlink_buffer("a\\b").unwrap();
        assert_eq!(b.len(), 8 + 12 + 12);
        assert_eq!(&b[0..4], &IO_REPARSE_TAG_SYMLINK.to_le_bytes());
        assert_eq!(u16::from_le_bytes([b[4], b[5]]), 24);
        assert_eq!(u16::from_le_bytes([b[10], b[11]]), 6);
        assert_eq!(u16::from_le_bytes([b[12], b[13]]), 6);
        assert_eq!(u32::from_le_bytes([b[16], b[17], b[18], b[19]]), 1);
    }
}
