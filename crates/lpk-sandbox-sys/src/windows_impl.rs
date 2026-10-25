//! Windows launch: LPAC or restricted token, job list, mitigations, bootstrap page.

use crate::bootstrap;
use crate::job::{werr, Job};
use crate::{Access, Launch, Mode};
use std::ffi::{c_void, OsStr};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::time::Duration;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    DuplicateHandle, GetLastError, LocalFree, DUPLICATE_HANDLE_OPTIONS, ERROR_FILE_NOT_FOUND,
    GENERIC_ALL, HANDLE, HLOCAL, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW,
    SetNamedSecurityInfoW, SetSecurityInfo, EXPLICIT_ACCESS_W, GRANT_ACCESS, SE_FILE_OBJECT,
    SE_KERNEL_OBJECT, TRUSTEE_IS_SID, TRUSTEE_W,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    CopySid, CreateRestrictedToken, EqualSid, FreeSid, GetLengthSid, GetTokenInformation,
    InitializeSecurityDescriptor, SetSecurityDescriptorDacl, SetTokenInformation,
    TokenAppContainerSid, TokenCapabilities, TokenDefaultDacl, TokenGroups, TokenIntegrityLevel,
    TokenIsAppContainer, TokenPrivileges, TokenRestrictedSids, TokenUser, ACL,
    DACL_SECURITY_INFORMATION, DISABLE_MAX_PRIVILEGE, NO_INHERITANCE,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    SECURITY_CAPABILITIES, SECURITY_DESCRIPTOR, SID_AND_ATTRIBUTES,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_ACCESS_MASK, TOKEN_ADJUST_DEFAULT,
    TOKEN_APPCONTAINER_INFORMATION, TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE,
    TOKEN_GROUPS, TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, READ_CONTROL, WRITE_DAC,
};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, VirtualProtectEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
    PAGE_PROTECTION_FLAGS, PAGE_READONLY, PAGE_READWRITE,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ,
};
use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows::Win32::System::SystemServices::{SECURITY_DESCRIPTOR_REVISION, SE_GROUP_INTEGRITY};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
    GetExitCodeProcess, InitializeProcThreadAttributeList, OpenProcess, OpenProcessToken,
    OpenThread, ResumeThread, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_ACCESS_RIGHTS, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
    PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, THREAD_ACCESS_RIGHTS,
};
use windows::Win32::System::WindowsProgramming::{
    PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT, PROCESS_CREATION_CHILD_PROCESS_RESTRICTED,
};

// PROCESS_CREATION_MITIGATION_POLICY_*_ALWAYS_ON (winnt.h; not exported by `windows` 0.62).
const MIT_STRICT_HANDLE_CHECKS: u64 = 1 << 24;
const MIT_WIN32K_SYSTEM_CALL_DISABLE: u64 = 1 << 28;
const MIT_EXTENSION_POINT_DISABLE: u64 = 1 << 32;
const MIT_PROHIBIT_DYNAMIC_CODE: u64 = 1 << 36;
const MIT_IMAGE_LOAD_NO_REMOTE: u64 = 1 << 52;
const MIT_IMAGE_LOAD_NO_LOW_LABEL: u64 = 1 << 56;
/// Every mitigation the worker starts with.
pub(crate) const MITIGATIONS: u64 = MIT_STRICT_HANDLE_CHECKS
    | MIT_WIN32K_SYSTEM_CALL_DISABLE
    | MIT_EXTENSION_POINT_DISABLE
    | MIT_PROHIBIT_DYNAMIC_CODE
    | MIT_IMAGE_LOAD_NO_REMOTE
    | MIT_IMAGE_LOAD_NO_LOW_LABEL;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

fn owned(h: HANDLE) -> OwnedHandle {
    // SAFETY: every caller passes a handle the OS just returned to us and that nothing else owns.
    unsafe { OwnedHandle::from_raw_handle(h.0) }
}

fn raw(h: &impl AsRawHandle) -> HANDLE {
    HANDLE(h.as_raw_handle())
}

fn current_process() -> HANDLE {
    // SAFETY: returns a pseudo-handle; no preconditions.
    unsafe { GetCurrentProcess() }
}

/// A SID allocated by the OS, freed on drop with the matching function.
struct Sid {
    ptr: PSID,
    by_free_sid: bool,
}

impl Drop for Sid {
    fn drop(&mut self) {
        if self.by_free_sid {
            // SAFETY: allocated by DeriveAppContainerSidFromAppContainerName, freed once.
            unsafe { FreeSid(self.ptr) };
        } else {
            // SAFETY: allocated by ConvertStringSidToSidW with LocalAlloc, freed once.
            unsafe { LocalFree(Some(HLOCAL(self.ptr.0))) };
        }
    }
}

impl Sid {
    fn from_string(s: &str) -> io::Result<Sid> {
        let w = wide(OsStr::new(s));
        let mut p = PSID::default();
        // SAFETY: `w` is NUL-terminated and outlives the call; `p` is writable.
        unsafe { ConvertStringSidToSidW(PCWSTR(w.as_ptr()), &mut p) }.map_err(werr)?;
        Ok(Sid {
            ptr: p,
            by_free_sid: false,
        })
    }

    fn appcontainer(name: &str) -> io::Result<Sid> {
        let w = wide(OsStr::new(name));
        // SAFETY: `w` is NUL-terminated and outlives the call.
        let p = unsafe { DeriveAppContainerSidFromAppContainerName(PCWSTR(w.as_ptr())) }
            .map_err(werr)?;
        Ok(Sid {
            ptr: p,
            by_free_sid: true,
        })
    }

    fn to_string_sid(&self) -> io::Result<String> {
        sid_to_string(self.ptr)
    }
}

pub(crate) fn sid_to_string(sid: PSID) -> io::Result<String> {
    let mut s = PWSTR::null();
    // SAFETY: `sid` is a valid SID for the duration of the call; `s` is writable.
    unsafe { ConvertSidToStringSidW(sid, &mut s) }.map_err(werr)?;
    // SAFETY: on success `s` is a NUL-terminated string allocated with LocalAlloc.
    let out = unsafe { s.to_string() }.map_err(|e| io::Error::other(e.to_string()));
    // SAFETY: freeing the LocalAlloc'd string once.
    unsafe { LocalFree(Some(HLOCAL(s.0 as *mut c_void))) };
    out
}

/// The string SID of the AppContainer derived from `name` (no profile is created).
pub fn appcontainer_sid_string(name: &str) -> io::Result<String> {
    Sid::appcontainer(name)?.to_string_sid()
}

/// Read a variable-length token information class into an 8-aligned buffer.
pub(crate) fn token_info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
    let mut len = 0u32;
    // SAFETY: a null buffer of length 0 asks for the required length only.
    let _ = unsafe { GetTokenInformation(token, class, None, 0, &mut len) };
    if len == 0 {
        // SAFETY: plain thread-local error read.
        return Err(io::Error::from_raw_os_error(
            unsafe { GetLastError() }.0 as i32,
        ));
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` is writable for at least `len` bytes and 8-aligned.
    unsafe { GetTokenInformation(token, class, Some(buf.as_mut_ptr().cast()), len, &mut len) }
        .map_err(werr)?;
    Ok(buf)
}

/// The broker's user SID, in its own buffer.
pub(crate) struct UserSid(Vec<u64>);

impl UserSid {
    pub(crate) fn of_current_process() -> io::Result<UserSid> {
        Ok(UserSid(token_info(current_token(), TokenUser)?))
    }

    pub(crate) fn ptr(&self) -> PSID {
        // SAFETY: the kernel wrote a TOKEN_USER (whose SID lies inside the buffer) at its start.
        unsafe { (*(self.0.as_ptr() as *const TOKEN_USER)).User.Sid }
    }
}

/// `GetCurrentProcessToken()`: a pseudo-handle valid for queries, never closed.
pub(crate) fn current_token() -> HANDLE {
    HANDLE(-4isize as *mut _)
}

/// A random SID, unique to one launch, used as the restricted worker's third restricting SID
/// (Chromium's `unique_restricted_sid`): objects granted to it are reachable by that worker only.
fn unique_sid() -> io::Result<(Sid, String)> {
    use std::hash::BuildHasher;
    let state = std::hash::RandomState::new();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let r: Vec<u32> = (0..3u8)
        .map(|i| state.hash_one((i, std::process::id(), nanos)) as u32)
        .collect();
    let s = format!("S-1-5-21-{}-{}-{}", r[0], r[1], r[2]);
    Ok((Sid::from_string(&s)?, s))
}

/// Set a token's integrity level to Low, with the label and its SID in one buffer.
fn set_low_integrity(token: HANDLE) -> io::Result<()> {
    let low = Sid::from_string("S-1-16-4096")?;
    // SAFETY: `low.ptr` is a valid SID.
    let sid_len = unsafe { GetLengthSid(low.ptr) } as usize;
    let head = std::mem::size_of::<TOKEN_MANDATORY_LABEL>();
    let mut buf = vec![0u64; (head + sid_len).div_ceil(8)];
    let base = buf.as_mut_ptr().cast::<u8>();
    // SAFETY: `buf` holds `head + sid_len` bytes, so offset `head` is inside it.
    let sid_dst = unsafe { base.add(head) };
    // SAFETY: the destination has exactly `sid_len` bytes left in `buf`; the source is valid.
    unsafe { CopySid(sid_len as u32, PSID(sid_dst.cast()), low.ptr) }.map_err(werr)?;
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: PSID(sid_dst.cast()),
            Attributes: SE_GROUP_INTEGRITY as u32,
        },
    };
    // SAFETY: the 8-aligned start of `buf` has room for the header, which the SID follows.
    unsafe { std::ptr::write(base.cast::<TOKEN_MANDATORY_LABEL>(), label) };
    // SAFETY: `buf` is one allocation holding the label and the SID it points to; the length
    // covers both and `buf` outlives the call.
    unsafe {
        SetTokenInformation(
            token,
            TokenIntegrityLevel,
            base.cast_const().cast(),
            (head + sid_len) as u32,
        )
    }
    .map_err(werr)
}

/// Replace a token's default DACL (the DACL of every object it creates without one).
fn set_default_dacl(token: HANDLE, acl: &LocalAcl) -> io::Result<()> {
    let dd = TOKEN_DEFAULT_DACL { DefaultDacl: acl.0 };
    // SAFETY: `dd` points at a valid ACL; both outlive the call.
    unsafe {
        SetTokenInformation(
            token,
            TokenDefaultDacl,
            &dd as *const _ as *const c_void,
            std::mem::size_of::<TOKEN_DEFAULT_DACL>() as u32,
        )
    }
    .map_err(werr)
}

/// Replace the (protected) DACL of a kernel object we hold with WRITE_DAC.
fn set_object_dacl(h: HANDLE, acl: &LocalAcl) -> io::Result<()> {
    // SAFETY: `h` is a valid handle with WRITE_DAC; `acl` is valid through the call.
    let r = unsafe {
        SetSecurityInfo(
            h,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(acl.0),
            None,
        )
    };
    if r.0 != 0 {
        return Err(io::Error::from_raw_os_error(r.0 as i32));
    }
    Ok(())
}

/// The caller's token restricted as the opt-in mode requires, with `unique` as the third
/// restricting SID in place of RESTRICTED (shared by every restricted process of the session).
fn restricted_token(unique: &Sid, user: &UserSid) -> io::Result<OwnedHandle> {
    let mut cur = HANDLE::default();
    // SAFETY: opening our own process token; `cur` is writable.
    unsafe {
        OpenProcessToken(
            current_process(),
            TOKEN_DUPLICATE
                | TOKEN_QUERY
                | TOKEN_ASSIGN_PRIMARY
                | TOKEN_ADJUST_DEFAULT
                | TOKEN_ACCESS_MASK(WRITE_DAC.0 | READ_CONTROL.0),
            &mut cur,
        )
    }
    .map_err(werr)?;
    let cur = owned(cur);
    let groups_buf = token_info(raw(&cur), TokenGroups)?;
    let groups = group_entries(&groups_buf);
    // Every group deny-only except Everyone and BUILTIN\Users: the restricted token passes an
    // access check only when both the normal and the restricting-SID checks grant it, so with
    // those two deny-only as well nothing (System32 included) is readable and the loader fails
    // with STATUS_ACCESS_DENIED (measured). This is Chromium's USER_LIMITED shape.
    let everyone = Sid::from_string("S-1-1-0")?;
    let users = Sid::from_string("S-1-5-32-545")?;
    let is = |a: PSID, b: &Sid| {
        // SAFETY: both are valid SIDs for the duration of the call.
        unsafe { EqualSid(a, b.ptr) }.is_ok()
    };
    let deny_only: Vec<SID_AND_ATTRIBUTES> = groups
        .iter()
        .filter(|g| g.Attributes & (SE_GROUP_INTEGRITY as u32) == 0)
        .filter(|g| !is(g.Sid, &everyone) && !is(g.Sid, &users))
        .map(|g| SID_AND_ATTRIBUTES {
            Sid: g.Sid,
            Attributes: 0,
        })
        .collect();
    let restrict: Vec<SID_AND_ATTRIBUTES> = [everyone.ptr, users.ptr, unique.ptr]
        .iter()
        .map(|&s| SID_AND_ATTRIBUTES {
            Sid: s,
            Attributes: 0,
        })
        .collect();
    let mut new = HANDLE::default();
    // SAFETY: `cur` is a valid token with TOKEN_DUPLICATE; both arrays and the SIDs they point to
    // outlive the call; `new` is writable.
    unsafe {
        CreateRestrictedToken(
            raw(&cur),
            DISABLE_MAX_PRIVILEGE,
            Some(&deny_only),
            None,
            Some(&restrict),
            &mut new,
        )
    }
    .map_err(werr)?;
    let new = owned(new);
    set_low_integrity(raw(&new))?;
    // The worker's own objects and its token: the user, SYSTEM and this launch's SID only.
    let system = Sid::from_string("S-1-5-18")?;
    let acl = LocalAcl::granting(&[user.ptr(), system.ptr, unique.ptr], GENERIC_ALL.0)?;
    set_default_dacl(raw(&new), &acl)?;
    set_object_dacl(raw(&new), &acl)?;
    Ok(new)
}

/// The SID_AND_ATTRIBUTES entries of a TOKEN_GROUPS the kernel wrote into `buf`.
fn group_entries(buf: &[u64]) -> &[SID_AND_ATTRIBUTES] {
    let gp = buf.as_ptr() as *const TOKEN_GROUPS;
    // SAFETY: the kernel wrote a TOKEN_GROUPS at the start of `buf` (8-aligned).
    let count = unsafe { (*gp).GroupCount } as usize;
    // SAFETY: `count` entries follow in the buffer the kernel sized for them; the SIDs they point
    // to live inside `buf`, which the returned slice borrows.
    unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!((*gp).Groups).cast::<SID_AND_ATTRIBUTES>(),
            count,
        )
    }
}

/// A token as the host sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenView {
    /// `TokenIsAppContainer`.
    pub appcontainer: bool,
    /// The AppContainer SID, if any.
    pub package_sid: Option<String>,
    /// Capability SIDs.
    pub capabilities: Vec<String>,
    /// The integrity RID (0x1000 Low, 0x2000 Medium, ...).
    pub integrity: u32,
    /// Groups as (SID, attributes).
    pub groups: Vec<(String, u32)>,
    /// Restricting SIDs.
    pub restricting: Vec<String>,
    /// Number of privileges present.
    pub privileges: u32,
}

fn sids_of(buf: &[u64]) -> io::Result<Vec<(String, u32)>> {
    group_entries(buf)
        .iter()
        .map(|g| Ok((sid_to_string(g.Sid)?, g.Attributes)))
        .collect()
}

pub(crate) fn token_view(token: HANDLE) -> io::Result<TokenView> {
    let appcontainer = token_info(token, TokenIsAppContainer)?[0] as u32 != 0;
    let package_sid = if appcontainer {
        let b = token_info(token, TokenAppContainerSid)?;
        // SAFETY: the kernel wrote a TOKEN_APPCONTAINER_INFORMATION (SID inside `b`) at its start.
        let sid =
            unsafe { (*(b.as_ptr() as *const TOKEN_APPCONTAINER_INFORMATION)).TokenAppContainer };
        Some(sid_to_string(sid)?)
    } else {
        None
    };
    let capabilities = sids_of(&token_info(token, TokenCapabilities)?)?
        .into_iter()
        .map(|s| s.0)
        .collect();
    let b = token_info(token, TokenIntegrityLevel)?;
    // SAFETY: the kernel wrote a TOKEN_MANDATORY_LABEL whose SID lies inside `b`.
    let label = unsafe { (*(b.as_ptr() as *const TOKEN_MANDATORY_LABEL)).Label.Sid };
    let integrity = sid_to_string(label)?
        .rsplit('-')
        .next()
        .and_then(|r| r.parse().ok())
        .unwrap_or(u32::MAX);
    let groups = sids_of(&token_info(token, TokenGroups)?)?;
    let restricting = sids_of(&token_info(token, TokenRestrictedSids)?)?
        .into_iter()
        .map(|s| s.0)
        .collect();
    // TOKEN_PRIVILEGES starts with a u32 count.
    let privileges = token_info(token, TokenPrivileges)?[0] as u32;
    Ok(TokenView {
        appcontainer,
        package_sid,
        capabilities,
        integrity,
        groups,
        restricting,
        privileges,
    })
}

/// Try `OpenProcess(access, pid)`; the handle is closed at once. For the isolation tests.
pub fn probe_open_process(pid: u32, access: u32) -> io::Result<()> {
    // SAFETY: plain open; the result is checked and closed.
    let h = unsafe { OpenProcess(PROCESS_ACCESS_RIGHTS(access), false, pid) }.map_err(werr)?;
    drop(owned(h));
    Ok(())
}

/// Try `OpenThread(access, tid)`; the handle is closed at once. For the isolation tests.
pub fn probe_open_thread(tid: u32, access: u32) -> io::Result<()> {
    // SAFETY: plain open; the result is checked and closed.
    let h = unsafe { OpenThread(THREAD_ACCESS_RIGHTS(access), false, tid) }.map_err(werr)?;
    drop(owned(h));
    Ok(())
}

/// Try to open `HKCU\path` (`hklm` false) or `HKLM\path` for reading. For the residual tests.
pub fn probe_registry_read(hklm: bool, path: &str) -> io::Result<()> {
    let w = wide(OsStr::new(path));
    let root = if hklm {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    };
    let mut key = HKEY::default();
    // SAFETY: `w` is NUL-terminated and outlives the call; `key` is writable.
    let r = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), None, KEY_READ, &mut key) };
    if r.0 != 0 {
        return Err(io::Error::from_raw_os_error(r.0 as i32));
    }
    // SAFETY: closing the key just opened.
    let _ = unsafe { RegCloseKey(key) };
    Ok(())
}

/// The minimal environment of a worker: `SystemRoot` only, so no host variable (tokens, proxy
/// credentials) reaches it.
fn worker_environment(mode: Mode) -> io::Result<Vec<u16>> {
    let mut env: Vec<u16> = Vec::new();
    let mut put = |name: &str, value: &[u16]| {
        env.extend(OsStr::new(name).encode_wide());
        env.push(b'=' as u16);
        env.extend_from_slice(value);
        env.push(0);
    };
    // SystemRoot from the system, not from the host's (caller-controlled) environment.
    let mut buf = [0u16; 512];
    // SAFETY: `buf` is writable for its length.
    let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    if n == 0 || n >= buf.len() {
        return Err(io::Error::last_os_error());
    }
    put("SystemRoot", &buf[..n]);
    if mode == Mode::Lpac {
        // The AppContainer launch rewrites LOCALAPPDATA (and TEMP/TMP) to the package's own
        // folders and fails with ERROR_ENVVAR_NOT_FOUND when the block lacks it (measured).
        let v: Vec<u16> = std::env::var_os("LOCALAPPDATA")
            .unwrap_or_default()
            .encode_wide()
            .collect();
        put("LOCALAPPDATA", &v);
    }
    env.push(0);
    Ok(env)
}

/// An ACL allocated by `SetEntriesInAclW`, freed on drop.
struct LocalAcl(*mut ACL);

impl LocalAcl {
    /// An ACL with one non-inherited allow ACE of `mask` per SID.
    fn granting(sids: &[PSID], mask: u32) -> io::Result<LocalAcl> {
        let entries: Vec<EXPLICIT_ACCESS_W> = sids
            .iter()
            .map(|s| EXPLICIT_ACCESS_W {
                grfAccessPermissions: mask,
                grfAccessMode: GRANT_ACCESS,
                grfInheritance: NO_INHERITANCE,
                Trustee: TRUSTEE_W {
                    TrusteeForm: TRUSTEE_IS_SID,
                    ptstrName: PWSTR(s.0.cast()),
                    ..Default::default()
                },
            })
            .collect();
        let mut acl: *mut ACL = std::ptr::null_mut();
        // SAFETY: `entries` and the SIDs they point to live through the call; no old ACL.
        let r = unsafe { SetEntriesInAclW(Some(&entries), None, &mut acl) };
        if r.0 != 0 {
            return Err(io::Error::from_raw_os_error(r.0 as i32));
        }
        Ok(LocalAcl(acl))
    }
}

impl Drop for LocalAcl {
    fn drop(&mut self) {
        // SAFETY: allocated by SetEntriesInAclW with LocalAlloc; freed once.
        unsafe { LocalFree(Some(HLOCAL(self.0.cast()))) };
    }
}

/// A process/thread attribute list, deleted on drop.
struct AttrList {
    buf: Vec<u64>,
}

impl AttrList {
    fn new(count: u32) -> io::Result<AttrList> {
        let mut size = 0usize;
        // SAFETY: a null list asks for the required size only.
        let _ = unsafe { InitializeProcThreadAttributeList(None, count, None, &mut size) };
        let mut buf = vec![0u64; size.div_ceil(8).max(1)];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr().cast());
        // SAFETY: `buf` is writable for `size` bytes, 8-aligned, and never moves (heap).
        unsafe { InitializeProcThreadAttributeList(Some(list), count, None, &mut size) }
            .map_err(werr)?;
        Ok(AttrList { buf })
    }

    fn list(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buf.as_mut_ptr().cast())
    }

    /// # Safety
    /// `value` must point to `size` readable bytes that stay valid and unmoved until the list
    /// has been used by `CreateProcess*` and dropped.
    unsafe fn set(&mut self, attr: u32, value: *const c_void, size: usize) -> io::Result<()> {
        let list = self.list();
        // SAFETY: the list is initialised; the caller guarantees `value`'s lifetime.
        unsafe { UpdateProcThreadAttribute(list, 0, attr as usize, Some(value), size, None, None) }
            .map_err(werr)
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        let list = self.list();
        // SAFETY: the list was initialised in `new` and is deleted once.
        unsafe { DeleteProcThreadAttributeList(list) };
    }
}

/// Append `arg` to a command line with the MSVC quoting rules.
fn push_quoted(out: &mut Vec<u16>, arg: &OsStr) {
    let units: Vec<u16> = arg.encode_wide().collect();
    let needs = units.is_empty()
        || units.iter().any(|&c| {
            c == b' ' as u16 || c == b'\t' as u16 || c == b'"' as u16 || c == b'\n' as u16
        });
    if !out.is_empty() {
        out.push(b' ' as u16);
    }
    if !needs {
        out.extend_from_slice(&units);
        return;
    }
    out.push(b'"' as u16);
    let mut backslashes = 0usize;
    for &c in &units {
        if c == b'\\' as u16 {
            backslashes += 1;
        } else if c == b'"' as u16 {
            out.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
            out.push(c);
            backslashes = 0;
        } else {
            out.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            out.push(c);
            backslashes = 0;
        }
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
    out.push(b'"' as u16);
}

/// Four 64 KiB-aligned candidate addresses in [64 GiB, 112 TiB), fixed before creation.
fn candidate_addresses() -> [u64; 4] {
    use std::hash::BuildHasher;
    const LO: u64 = 0x10_0000_0000;
    const HI: u64 = 0x7000_0000_0000;
    let state = std::hash::RandomState::new();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut out = [0u64; 4];
    for (i, a) in out.iter_mut().enumerate() {
        let r = state.hash_one((i, std::process::id(), nanos));
        *a = LO + ((r % ((HI - LO) >> 16)) << 16);
    }
    out
}

/// A running sandboxed worker. Dropping it closes the job, which kills the worker.
#[derive(Debug)]
pub struct Worker {
    process: OwnedHandle,
    job: Job,
    pid: u32,
    tid: u32,
    restricting_sid: Option<String>,
    cpu_ceiling: Duration,
}

impl Worker {
    pub(crate) fn spawn(l: &Launch<'_>) -> io::Result<Worker> {
        // A missing image also fails CreateProcess with ERROR_FILE_NOT_FOUND: name it here, so
        // that error is never mistaken for a missing AppContainer profile.
        if !l.exe.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("worker executable not found: {}", l.exe.display()),
            ));
        }
        let job = Job::sandbox(l.limits.memory, l.limits.cpu_time)?;
        let addrs = candidate_addresses();

        let mut cmd: Vec<u16> = Vec::new();
        push_quoted(&mut cmd, l.exe.as_os_str());
        push_quoted(&mut cmd, OsStr::new(&bootstrap::format_addresses(&addrs)));
        for a in &l.args {
            push_quoted(&mut cmd, a);
        }
        cmd.push(0);
        let exe_w = wide(l.exe.as_os_str());
        let cwd = l
            .current_dir
            .clone()
            .or_else(|| l.exe.parent().map(Path::to_path_buf))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "exe has no parent"))?;
        let cwd_w = wide(cwd.as_os_str());
        let env = worker_environment(l.mode)?;

        // The worker's process and thread objects grant the broker's user and SYSTEM only: no
        // RESTRICTED, no package SID, no ALL APPLICATION PACKAGES, so no other worker opens them.
        let user = UserSid::of_current_process()?;
        let system = Sid::from_string("S-1-5-18")?;
        let broker_acl = LocalAcl::granting(&[user.ptr(), system.ptr], GENERIC_ALL.0)?;
        let mut sd = SECURITY_DESCRIPTOR::default();
        let psd = PSECURITY_DESCRIPTOR((&mut sd as *mut SECURITY_DESCRIPTOR).cast());
        // SAFETY: `sd` is a writable SECURITY_DESCRIPTOR that outlives every use of `psd`.
        unsafe { InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION) }.map_err(werr)?;
        // SAFETY: `psd` is initialised; `broker_acl` outlives the descriptor's use.
        unsafe { SetSecurityDescriptorDacl(psd, true, Some(broker_acl.0), false) }.map_err(werr)?;
        let object_attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: psd.0,
            bInheritHandle: false.into(),
        };

        // Attribute values: all locals that outlive CreateProcess* below.
        let job_list = [job.raw()];
        let mitigations: u64 = MITIGATIONS;
        let child_policy: u32 = PROCESS_CREATION_CHILD_PROCESS_RESTRICTED;
        let aap_policy: u32 = PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT;
        let (sid, token, unique) = match l.mode {
            Mode::Lpac => (Some(Sid::appcontainer(&l.appcontainer_name)?), None, None),
            Mode::RestrictedToken => {
                let (unique, text) = unique_sid()?;
                let token = restricted_token(&unique, &user)?;
                (None, Some(token), Some((unique, text)))
            }
        };
        // What the worker's own objects (threads, events, sections) are created with.
        let child_acl = match &unique {
            Some((u, _)) => LocalAcl::granting(&[user.ptr(), system.ptr, u.ptr], GENERIC_ALL.0)?,
            None => LocalAcl::granting(&[user.ptr(), system.ptr], GENERIC_ALL.0)?,
        };
        let caps = SECURITY_CAPABILITIES {
            AppContainerSid: sid.as_ref().map(|s| s.ptr).unwrap_or_default(),
            ..Default::default()
        };
        let mut attrs = AttrList::new(if sid.is_some() { 5 } else { 3 })?;
        // SAFETY: each value passed to `set` (here and below) is a local declared above that
        // lives, unmoved, until after CreateProcess* returns and `attrs` is dropped.
        unsafe {
            attrs.set(
                PROC_THREAD_ATTRIBUTE_JOB_LIST,
                job_list.as_ptr().cast(),
                std::mem::size_of_val(&job_list),
            )?;
        }
        // SAFETY: as above.
        unsafe {
            attrs.set(
                PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY,
                (&mitigations as *const u64).cast(),
                8,
            )?;
        }
        // SAFETY: as above.
        unsafe {
            attrs.set(
                PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
                (&child_policy as *const u32).cast(),
                4,
            )?;
        }
        if sid.is_some() {
            // SAFETY: as above; `caps` points at `sid`, which also outlives the call.
            unsafe {
                attrs.set(
                    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                    (&caps as *const SECURITY_CAPABILITIES).cast(),
                    std::mem::size_of::<SECURITY_CAPABILITIES>(),
                )?;
            }
            // SAFETY: as above.
            unsafe {
                attrs.set(
                    PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
                    (&aap_policy as *const u32).cast(),
                    4,
                )?;
            }
        }
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // Null standard handles: the worker gets none of ours.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = attrs.list();
        let flags = CREATE_SUSPENDED
            | DETACHED_PROCESS
            | EXTENDED_STARTUPINFO_PRESENT
            | CREATE_UNICODE_ENVIRONMENT;
        let mut pi = PROCESS_INFORMATION::default();
        let mut create = || match &token {
            // SAFETY: every pointer is to a live local (NUL-terminated strings, the environment
            // block, the security attributes, the startup info with its attribute list); no
            // handle is inherited.
            None => unsafe {
                CreateProcessW(
                    PCWSTR(exe_w.as_ptr()),
                    Some(PWSTR(cmd.as_mut_ptr())),
                    Some(&object_attrs),
                    Some(&object_attrs),
                    false,
                    flags,
                    Some(env.as_ptr().cast()),
                    PCWSTR(cwd_w.as_ptr()),
                    &si.StartupInfo,
                    &mut pi,
                )
            },
            // SAFETY: as above; `t` is a primary token derived from our own.
            Some(t) => unsafe {
                CreateProcessAsUserW(
                    Some(raw(t)),
                    PCWSTR(exe_w.as_ptr()),
                    Some(PWSTR(cmd.as_mut_ptr())),
                    Some(&object_attrs),
                    Some(&object_attrs),
                    false,
                    flags,
                    Some(env.as_ptr().cast()),
                    PCWSTR(cwd_w.as_ptr()),
                    &si.StartupInfo,
                    &mut pi,
                )
            },
        };
        let mut created = create();
        if l.mode == Mode::Lpac
            && matches!(&created, Err(e) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult())
        {
            // Another process may be creating the profile right now: retry once.
            std::thread::sleep(Duration::from_millis(100));
            created = create();
            if l.exe.is_file()
                && matches!(&created, Err(e) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult())
            {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    crate::ProfileMissing(l.appcontainer_name.clone()),
                ));
            }
        }
        created.map_err(werr)?;
        drop(attrs);
        let thread = owned(pi.hThread);
        let worker = Worker {
            process: owned(pi.hProcess),
            job,
            pid: pi.dwProcessId,
            tid: pi.dwThreadId,
            restricting_sid: unique.map(|u| u.1),
            cpu_ceiling: l.limits.cpu_ceiling,
        };
        // From here every failure kills the suspended child (the job also does on drop).
        let setup = (|| {
            // The kernel may add ACEs for an AppContainer's own SID: set the broker-only DACL
            // again, protected, on the created objects.
            set_object_dacl(raw(&worker.process), &broker_acl)?;
            set_object_dacl(raw(&thread), &broker_acl)?;
            worker.set_child_default_dacl(&child_acl)?;
            worker.bootstrap(l, &addrs)
        })();
        if let Err(e) = setup {
            let _ = worker.job.terminate(1);
            return Err(e);
        }
        // SAFETY: `thread` is the child's initial thread with full access.
        if unsafe { ResumeThread(raw(&thread)) } == u32::MAX {
            let e = io::Error::last_os_error();
            let _ = worker.job.terminate(1);
            return Err(e);
        }
        Ok(worker)
    }

    /// Replace the suspended child's default DACL (LPAC's would grant its package SID, so every
    /// worker of the package could open the objects another creates).
    fn set_child_default_dacl(&self, acl: &LocalAcl) -> io::Result<()> {
        let mut t = HANDLE::default();
        // SAFETY: the child's process handle has full access; `t` is writable.
        unsafe {
            OpenProcessToken(
                raw(&self.process),
                TOKEN_ADJUST_DEFAULT | TOKEN_QUERY,
                &mut t,
            )
        }
        .map_err(werr)?;
        let t = owned(t);
        set_default_dacl(raw(&t), acl)
    }

    /// The worker's token as the host sees it.
    pub fn token_view(&self) -> io::Result<TokenView> {
        let mut t = HANDLE::default();
        // SAFETY: the worker's process handle has full access; `t` is writable.
        unsafe { OpenProcessToken(raw(&self.process), TOKEN_QUERY, &mut t) }.map_err(werr)?;
        let t = owned(t);
        token_view(raw(&t))
    }

    /// The id of the worker's initial thread.
    pub fn thread_id(&self) -> u32 {
        self.tid
    }

    /// The per-launch restricting SID (restricted mode only).
    pub fn restricting_sid(&self) -> Option<&str> {
        self.restricting_sid.as_deref()
    }

    /// Duplicate the lent handles into the child and commit the bootstrap page.
    fn bootstrap(&self, l: &Launch<'_>, addrs: &[u64; 4]) -> io::Result<()> {
        let proc = raw(&self.process);
        let mut entries = Vec::with_capacity(l.handles.len());
        for (role, h, access) in &l.handles {
            let (mask, opts) = match access {
                Access::Read => (FILE_GENERIC_READ.0, DUPLICATE_HANDLE_OPTIONS(0)),
                Access::Write => (FILE_GENERIC_WRITE.0, DUPLICATE_HANDLE_OPTIONS(0)),
            };
            let mut out = HANDLE::default();
            // SAFETY: `h` is a valid handle borrowed for the launch; `proc` is the child's
            // process handle with PROCESS_DUP_HANDLE; `out` is writable.
            unsafe {
                DuplicateHandle(current_process(), raw(h), proc, &mut out, mask, false, opts)
            }
            .map_err(werr)?;
            entries.push((*role, out.0 as u64));
        }
        let block = bootstrap::encode(&entries).map_err(|e| io::Error::other(e.to_string()))?;
        for &a in addrs {
            // SAFETY: allocating in the suspended child we created; the address is checked below.
            let p = unsafe {
                VirtualAllocEx(
                    proc,
                    Some(a as *const c_void),
                    bootstrap::PAGE,
                    MEM_RESERVE | MEM_COMMIT,
                    PAGE_READWRITE,
                )
            };
            if p.is_null() {
                continue;
            }
            if p as u64 != a {
                // SAFETY: releasing the region just allocated in the child.
                let _ = unsafe { VirtualFreeEx(proc, p, 0, MEM_RELEASE) };
                continue;
            }
            // SAFETY: `p` is a committed, writable page of the child; `block` is readable and
            // shorter than a page.
            unsafe { WriteProcessMemory(proc, p, block.as_ptr().cast(), block.len(), None) }
                .map_err(werr)?;
            let mut old = PAGE_PROTECTION_FLAGS::default();
            // SAFETY: changing the protection of the page committed above.
            unsafe { VirtualProtectEx(proc, p, bootstrap::PAGE, PAGE_READONLY, &mut old) }
                .map_err(werr)?;
            return Ok(());
        }
        Err(io::Error::other(
            "no candidate bootstrap address was free in the child",
        ))
    }

    /// The worker's process id.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// The worker's job.
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// Wait for the worker to exit; `None` timeout waits forever. Returns its exit code, or
    /// `None` when the timeout passed first.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<Option<u32>> {
        if !wait_handle(self.process.as_handle(), timeout)? {
            return Ok(None);
        }
        let mut code = 0u32;
        // SAFETY: valid process handle; `code` is writable.
        unsafe { GetExitCodeProcess(raw(&self.process), &mut code) }.map_err(werr)?;
        Ok(Some(code))
    }

    /// Kill the worker (every process of its job) with exit code 1.
    pub fn kill(&self) -> io::Result<()> {
        self.job.terminate(1)
    }

    /// Raise (or lower) the worker's user-time limit while it runs; at most the ceiling.
    pub fn set_cpu_limit(&self, cpu: Duration) -> io::Result<()> {
        if cpu > self.cpu_ceiling {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cpu limit above the ceiling",
            ));
        }
        self.job.set_process_time_limit(cpu)
    }

    /// What the job has observed of the worker: bytes read, user time, peak commit.
    pub fn usage(&self) -> io::Result<crate::Usage> {
        let a = self.job.accounting()?;
        let e = self.job.extended_limits()?;
        let ticks = u64::try_from(a.BasicInfo.TotalUserTime).unwrap_or(0);
        Ok(crate::Usage {
            read_bytes: a.IoInfo.ReadTransferCount,
            user_time: Duration::from_nanos(ticks.saturating_mul(100)),
            peak_memory: e.PeakProcessMemoryUsed as u64,
        })
    }

    /// A second handle to the worker's process (for waiting on it after the `Worker` is gone).
    pub fn try_clone_process(&self) -> io::Result<OwnedHandle> {
        self.process.try_clone()
    }
}

/// Wait until `h` is signalled; `true` when it was, `false` on timeout.
pub fn wait_handle(h: BorrowedHandle<'_>, timeout: Option<Duration>) -> io::Result<bool> {
    let ms = match timeout {
        Some(t) => t.as_millis().min(u128::from(INFINITE - 1)) as u32,
        None => INFINITE,
    };
    // SAFETY: `h` is a valid, borrowed waitable handle.
    let w = unsafe { WaitForSingleObject(raw(&h), ms) };
    if w == WAIT_OBJECT_0 {
        Ok(true)
    } else if w == WAIT_TIMEOUT {
        Ok(false)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Add an allow ACE for `sid` (string form) with `mask` to the DACL of `path`, inherited by
/// children when `inherit`. Tests use it to grant the AppContainer SID read and execute on a
/// copy of the worker and to make a file readable by BUILTIN\Users.
pub fn grant_path_access(path: &Path, sid: &str, mask: u32, inherit: bool) -> io::Result<()> {
    let path_w = wide(path.as_os_str());
    let sid = Sid::from_string(sid)?;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `path_w` is NUL-terminated; the out pointers are writable.
    let r = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(path_w.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl),
            None,
            &mut sd,
        )
    };
    if r.0 != 0 {
        return Err(io::Error::from_raw_os_error(r.0 as i32));
    }
    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: if inherit {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            ptstrName: PWSTR(sid.ptr.0.cast()),
            ..Default::default()
        },
    };
    let mut new_acl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `ea` and its SID live through the call; `dacl` points into `sd`, still alive.
    let r = unsafe { SetEntriesInAclW(Some(&[ea]), Some(dacl), &mut new_acl) };
    let result = if r.0 != 0 {
        Err(io::Error::from_raw_os_error(r.0 as i32))
    } else {
        // SAFETY: `path_w` is NUL-terminated; `new_acl` is the valid ACL just built.
        let r = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(path_w.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(new_acl),
                None,
            )
        };
        // SAFETY: `new_acl` was allocated by SetEntriesInAclW with LocalAlloc; freed once.
        unsafe { LocalFree(Some(HLOCAL(new_acl.cast()))) };
        if r.0 != 0 {
            Err(io::Error::from_raw_os_error(r.0 as i32))
        } else {
            Ok(())
        }
    };
    // SAFETY: `sd` was allocated by GetNamedSecurityInfoW with LocalAlloc; freed once.
    unsafe { LocalFree(Some(HLOCAL(sd.0))) };
    result
}

/// Create the AppContainer profile `name` (no capability). `Ok(false)` when it already existed.
pub fn create_appcontainer_profile(name: &str) -> io::Result<bool> {
    let w = wide(OsStr::new(name));
    // SAFETY: `w` is NUL-terminated and outlives the call; no capability array.
    let r = unsafe {
        CreateAppContainerProfile(
            PCWSTR(w.as_ptr()),
            PCWSTR(w.as_ptr()),
            PCWSTR(w.as_ptr()),
            None,
        )
    };
    match r {
        Ok(sid) => {
            drop(Sid {
                ptr: sid,
                by_free_sid: true,
            });
            Ok(true)
        }
        Err(e) if e.code().0 as u32 == 0x8007_00B7 => Ok(false),
        Err(e) => Err(werr(e)),
    }
}

/// Delete the AppContainer profile `name` (its folders and registry keys).
pub fn delete_appcontainer_profile(name: &str) -> io::Result<()> {
    let w = wide(OsStr::new(name));
    // SAFETY: `w` is NUL-terminated and outlives the call.
    unsafe { DeleteAppContainerProfile(PCWSTR(w.as_ptr())) }.map_err(werr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(args: &[&str]) -> String {
        let mut v = Vec::new();
        for a in args {
            push_quoted(&mut v, OsStr::new(a));
        }
        String::from_utf16(&v).expect("utf16")
    }

    #[test]
    fn quoting_follows_msvc_rules() {
        assert_eq!(quoted(&["a", "b c", ""]), r#"a "b c" """#);
        assert_eq!(quoted(&[r#"x"y"#]), r#""x\"y""#);
        assert_eq!(quoted(&[r"C:\a b\"]), r#""C:\a b\\""#);
        assert_eq!(quoted(&[r"C:\ab\"]), r"C:\ab\");
    }

    #[test]
    fn candidates_are_aligned_and_in_range() {
        for a in candidate_addresses() {
            assert_eq!(a % 0x1_0000, 0);
            assert!((0x10_0000_0000..0x7000_0000_0000).contains(&a));
        }
    }

    #[test]
    fn appcontainer_sid_is_derived_without_a_profile() {
        let s = appcontainer_sid_string(crate::APPCONTAINER_NAME).expect("derive");
        assert!(s.starts_with("S-1-15-2-"), "{s}");
        assert_eq!(
            s,
            appcontainer_sid_string(crate::APPCONTAINER_NAME).expect("derive")
        );
    }
}
