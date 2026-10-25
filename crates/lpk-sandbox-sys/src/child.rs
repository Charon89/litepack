//! The worker's side: take the handles the broker passed, and (for tests and diagnostics) report
//! the token and handle table the worker runs with.

use crate::bootstrap::{self, BootstrapError};
use std::time::Duration;

/// A handle (Windows) or fd (Linux) the worker owns.
#[cfg(windows)]
pub type Owned = std::os::windows::io::OwnedHandle;
/// A handle (Windows) or fd (Linux) the worker owns.
#[cfg(unix)]
pub type Owned = std::os::fd::OwnedFd;

fn find_arg(prefix: &str) -> Result<String, BootstrapError> {
    std::env::args_os()
        .skip(1)
        .find_map(|a| {
            a.to_str()
                .filter(|s| s.starts_with(prefix))
                .map(str::to_string)
        })
        .ok_or(BootstrapError::MissingArgument)
}

/// Take the passed handles as `(role, handle)`. The worker must refuse to start on `Err`.
#[cfg(windows)]
pub fn take_handles() -> Result<Vec<(u32, Owned)>, BootstrapError> {
    let addrs = bootstrap::parse_addresses(&find_arg(bootstrap::ARG_PREFIX)?)?;
    win::take(&addrs)
}

/// Take the passed fds as `(role, fd)`. The worker must refuse to start on `Err`.
#[cfg(unix)]
pub fn take_handles() -> Result<Vec<(u32, Owned)>, BootstrapError> {
    use std::os::fd::FromRawFd;
    let arg = find_arg(bootstrap::FDS_PREFIX)?;
    let list = &arg[bootstrap::FDS_PREFIX.len()..];
    let bad = || BootstrapError::BadArgument(arg.clone());
    let mut out = Vec::new();
    for part in list.split(',').filter(|p| !p.is_empty()) {
        let (role, fd) = part.split_once(':').ok_or_else(bad)?;
        let role: u32 = role.parse().map_err(|_| bad())?;
        let fd: i32 = fd.parse().map_err(|_| bad())?;
        if fd < 3 || out.len() == bootstrap::MAX_HANDLES {
            return Err(bad());
        }
        // SAFETY: F_GETFD only inspects the descriptor table.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            return Err(BootstrapError::Malformed("fd not open"));
        }
        // SAFETY: the launcher moved this open fd here for us alone; we take ownership once.
        out.push((role, unsafe { Owned::from_raw_fd(fd) }));
    }
    Ok(out)
}

/// The worker's own user-mode CPU time.
#[cfg(unix)]
pub fn own_user_cpu() -> Duration {
    // SAFETY: zeroed rusage is a valid out buffer.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `ru` is writable.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    Duration::from_secs(ru.ru_utime.tv_sec as u64)
        + Duration::from_micros(ru.ru_utime.tv_usec as u64)
}

#[cfg(windows)]
pub use win::{handle_path, own_handles, own_user_cpu, token_report, HandleEntry, TokenReport};

#[cfg(windows)]
mod win {
    use super::*;
    use crate::job::werr;
    use crate::windows_impl::{sid_to_string, token_info};
    use std::os::windows::io::FromRawHandle;
    use windows::Wdk::Foundation::{NtQueryObject, OBJECT_INFORMATION_CLASS};
    use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessHandleInformation};
    use windows::Win32::Foundation::{FILETIME, HANDLE, STATUS_INFO_LENGTH_MISMATCH};
    use windows::Win32::Security::{
        IsTokenRestricted, TokenAppContainerSid, TokenIntegrityLevel, TokenIsAppContainer,
        TokenPrivileges, TokenRestrictedSids, PSID, TOKEN_APPCONTAINER_INFORMATION,
        TOKEN_MANDATORY_LABEL,
    };
    use windows::Win32::System::Memory::{
        VirtualFree, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_RELEASE,
        PAGE_READONLY, PAGE_READWRITE,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    /// `GetCurrentProcessToken()`: a pseudo-handle valid for queries, never closed.
    fn current_token() -> HANDLE {
        HANDLE(-4isize as *mut _)
    }

    /// The page at `addr` if it is committed and readable as a whole.
    fn committed_page(addr: u64) -> Option<&'static [u8]> {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: querying our own address space; `mbi` is writable for its size.
        let n = unsafe {
            VirtualQuery(
                Some(addr as *const _),
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        let ok = n == std::mem::size_of::<MEMORY_BASIC_INFORMATION>()
            && mbi.BaseAddress as u64 == addr
            && mbi.State == MEM_COMMIT
            && (mbi.Protect == PAGE_READONLY || mbi.Protect == PAGE_READWRITE)
            && mbi.RegionSize >= bootstrap::PAGE;
        if !ok {
            return None;
        }
        // SAFETY: VirtualQuery just reported a committed, readable region of at least a page
        // starting at `addr`; the broker's page is never freed except by `take` below, after the
        // last use of this slice.
        Some(unsafe { std::slice::from_raw_parts(addr as *const u8, bootstrap::PAGE) })
    }

    pub(super) fn take(addrs: &[u64; 4]) -> Result<Vec<(u32, Owned)>, BootstrapError> {
        let mut malformed = None;
        for &a in addrs {
            let Some(page) = committed_page(a) else {
                continue;
            };
            match bootstrap::decode(page) {
                Ok(entries) => {
                    let out = entries
                        .into_iter()
                        .map(|(role, v)| {
                            // SAFETY: the broker duplicated this handle into us for our sole
                            // ownership and named it in a block that validated.
                            (role, unsafe {
                                Owned::from_raw_handle(v as usize as *mut _)
                            })
                        })
                        .collect();
                    // SAFETY: releasing the broker's page; `page` is not used after this.
                    let _ = unsafe { VirtualFree(a as *mut _, 0, MEM_RELEASE) };
                    return Ok(out);
                }
                Err(e) => malformed = Some(e),
            }
        }
        Err(malformed.unwrap_or(BootstrapError::NoPage))
    }

    /// The shape of the worker's token.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TokenReport {
        /// `TokenIsAppContainer`.
        pub appcontainer: bool,
        /// The AppContainer SID, if any.
        pub package_sid: Option<String>,
        /// The integrity RID (0x1000 Low, 0x2000 Medium, ...).
        pub integrity: u32,
        /// `IsTokenRestricted`.
        pub restricted: bool,
        /// Number of restricting SIDs.
        pub restricted_sids: u32,
        /// Number of privileges present.
        pub privileges: u32,
    }

    fn u32_info(class: windows::Win32::Security::TOKEN_INFORMATION_CLASS) -> std::io::Result<u32> {
        let b = token_info(current_token(), class)?;
        Ok(b[0] as u32)
    }

    /// Describe the worker's own token.
    pub fn token_report() -> std::io::Result<TokenReport> {
        let appcontainer = u32_info(TokenIsAppContainer)? != 0;
        let package_sid = if appcontainer {
            let b = token_info(current_token(), TokenAppContainerSid)?;
            // SAFETY: the kernel wrote a TOKEN_APPCONTAINER_INFORMATION at the start of `b`.
            let sid: PSID = unsafe {
                (*(b.as_ptr() as *const TOKEN_APPCONTAINER_INFORMATION)).TokenAppContainer
            };
            Some(sid_to_string(sid)?)
        } else {
            None
        };
        let b = token_info(current_token(), TokenIntegrityLevel)?;
        // SAFETY: the kernel wrote a TOKEN_MANDATORY_LABEL whose SID lies inside `b`.
        let label: PSID = unsafe { (*(b.as_ptr() as *const TOKEN_MANDATORY_LABEL)).Label.Sid };
        let s = sid_to_string(label)?;
        let integrity = s
            .rsplit('-')
            .next()
            .and_then(|r| r.parse().ok())
            .unwrap_or(u32::MAX);
        // SAFETY: query on a valid pseudo-handle.
        let restricted = unsafe { IsTokenRestricted(current_token()) }.is_ok();
        // TOKEN_GROUPS and TOKEN_PRIVILEGES both start with a u32 count.
        let restricted_sids = u32_info(TokenRestrictedSids)?;
        let privileges = u32_info(TokenPrivileges)?;
        Ok(TokenReport {
            appcontainer,
            package_sid,
            integrity,
            restricted,
            restricted_sids,
            privileges,
        })
    }

    /// One entry of the worker's handle table.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct HandleEntry {
        /// The handle value.
        pub value: u64,
        /// The object type index (same index = same type).
        pub type_index: u32,
        /// The granted access mask.
        pub access: u32,
    }

    /// The worker's handle table (`ProcessHandleInformation`).
    pub fn own_handles() -> std::io::Result<Vec<HandleEntry>> {
        let mut buf = vec![0u64; 1024];
        loop {
            let mut ret = 0u32;
            let len = (buf.len() * 8) as u32;
            // SAFETY: querying our own process into a writable, 8-aligned buffer of `len` bytes.
            let st = unsafe {
                NtQueryInformationProcess(
                    GetCurrentProcess(),
                    ProcessHandleInformation,
                    buf.as_mut_ptr().cast(),
                    len,
                    &mut ret,
                )
            };
            if st == STATUS_INFO_LENGTH_MISMATCH {
                buf.resize((ret as usize).div_ceil(8).max(buf.len() * 2), 0);
                continue;
            }
            if st.0 < 0 {
                return Err(std::io::Error::other(format!(
                    "NtQueryInformationProcess {:#x}",
                    st.0
                )));
            }
            break;
        }
        // PROCESS_HANDLE_SNAPSHOT_INFORMATION: NumberOfHandles, Reserved (usize), then entries of
        // {HandleValue, HandleCount, PointerCount (usize); GrantedAccess, ObjectTypeIndex,
        // HandleAttributes, Reserved (u32)}: 5 u64 words each.
        let n = buf[0] as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let at = 2 + i * 5;
            let Some(e) = buf.get(at..at + 5) else { break };
            out.push(HandleEntry {
                value: e[0],
                access: e[3] as u32,
                type_index: (e[3] >> 32) as u32,
            });
        }
        Ok(out)
    }

    /// The NT path of a file or device handle the worker holds (`None` when it has none, as for
    /// pipes). Do not call it on a synchronous pipe handle another thread is blocked on.
    pub fn handle_path(value: u64) -> Option<String> {
        // ObjectNameInformation: a UNICODE_STRING {Length, MaximumLength (u16), Buffer} whose
        // characters the kernel places right after it.
        let mut buf = [0u64; 260];
        let mut ret = 0u32;
        // SAFETY: `value` is a handle in our own table (from `own_handles`); `buf` is writable
        // for its full size and 8-aligned.
        let st = unsafe {
            NtQueryObject(
                Some(HANDLE(value as usize as *mut _)),
                OBJECT_INFORMATION_CLASS(1),
                Some(buf.as_mut_ptr().cast()),
                std::mem::size_of_val(&buf) as u32,
                Some(&mut ret),
            )
        };
        if st.0 < 0 {
            return None;
        }
        let len = (buf[0] & 0xFFFF) as usize / 2;
        let units: Vec<u16> = buf[2..]
            .iter()
            .flat_map(|w| (0..4).map(move |i| (w >> (16 * i)) as u16))
            .take(len)
            .collect();
        (len > 0).then(|| String::from_utf16_lossy(&units))
    }

    /// The worker's own user-mode CPU time.
    pub fn own_user_cpu() -> Duration {
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: our own pseudo-handle; four writable FILETIMEs.
        if unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }
            .map_err(werr)
            .is_err()
        {
            return Duration::ZERO;
        }
        let t = (u64::from(u.dwHighDateTime) << 32) | u64::from(u.dwLowDateTime);
        Duration::from_nanos(t.saturating_mul(100))
    }
}
