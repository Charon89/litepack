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
pub use win::{
    current_thread_id, handle_path, handle_type, own_handles, own_user_cpu, HandleEntry,
};

#[cfg(windows)]
mod win {
    use super::*;
    use crate::job::werr;
    use std::os::windows::io::FromRawHandle;
    use windows::Wdk::Foundation::{NtQueryObject, OBJECT_INFORMATION_CLASS};
    use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessHandleInformation};
    use windows::Win32::Foundation::{FILETIME, HANDLE, STATUS_INFO_LENGTH_MISMATCH};
    use windows::Win32::System::Memory::{
        VirtualFree, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_RELEASE,
        PAGE_READONLY, PAGE_READWRITE,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

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
        // starting at `addr`, and nothing frees memory during this single-threaded start-up
        // before the slice's last use. A candidate the broker did not take may be any region of
        // ours: its bytes are untrusted until `bootstrap::decode` validates magic, version,
        // length and checksum, and only a validated block is acted on.
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
        object_string(value, 1)
    }

    /// The object type name of a handle the worker holds (`File`, `Process`, `Key`, ...).
    pub fn handle_type(value: u64) -> Option<String> {
        object_string(value, 2)
    }

    /// ObjectNameInformation (1) and ObjectTypeInformation (2) both start with a UNICODE_STRING
    /// {Length, MaximumLength (u16), Buffer} whose characters the kernel places right after the
    /// fixed part; for the type the fixed part is longer, so the characters are found through
    /// the Buffer pointer's offset from the start of our buffer.
    fn object_string(value: u64, class: i32) -> Option<String> {
        let mut buf = [0u64; 512];
        let mut ret = 0u32;
        // SAFETY: `value` is a handle in our own table (from `own_handles`); `buf` is writable
        // for its full size and 8-aligned.
        let st = unsafe {
            NtQueryObject(
                Some(HANDLE(value as usize as *mut _)),
                OBJECT_INFORMATION_CLASS(class),
                Some(buf.as_mut_ptr().cast()),
                std::mem::size_of_val(&buf) as u32,
                Some(&mut ret),
            )
        };
        if st.0 < 0 {
            return None;
        }
        let len = (buf[0] & 0xFFFF) as usize / 2;
        let start = (buf[1] as usize).checked_sub(buf.as_ptr() as usize)?;
        if len == 0 || start % 2 != 0 || start + len * 2 > std::mem::size_of_val(&buf) {
            return None;
        }
        let units: Vec<u16> = buf
            .iter()
            .flat_map(|w| (0..4).map(move |i| (w >> (16 * i)) as u16))
            .skip(start / 2)
            .take(len)
            .collect();
        Some(String::from_utf16_lossy(&units))
    }

    /// The calling thread's OS id.
    pub fn current_thread_id() -> u32 {
        // SAFETY: no preconditions.
        unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
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
