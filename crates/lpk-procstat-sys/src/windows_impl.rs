//! Windows backend: Job Object accounting.
//!
//! The child is spawned with `CREATE_SUSPENDED`, assigned to a kill-on-close job, and only then
//! has its initial thread resumed (found through a Toolhelp thread snapshot taken before the wall
//! clock starts), so no instruction of the child runs outside the job. Nested jobs work on Windows 8 and later.

use crate::{Measurement, Spec};
use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_MORE_DATA, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAndIoAccountingInformation,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{
    OpenThread, ResumeThread, WaitForSingleObject, CREATE_SUSPENDED, DETACHED_PROCESS, INFINITE,
    THREAD_SUSPEND_RESUME,
};

/// Owned Win32 handle, closed on drop.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this wrapper and closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn werr(e: windows::core::Error) -> io::Error {
    io::Error::other(e.to_string())
}

fn create_job() -> io::Result<Owned> {
    // SAFETY: null attributes and name are allowed; the result is checked.
    let h = unsafe { CreateJobObjectW(None, None) }.map_err(werr)?;
    let job = Owned(h);
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `info` is a valid, correctly sized JOBOBJECT_EXTENDED_LIMIT_INFORMATION.
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    }
    .map_err(werr)?;
    Ok(job)
}

/// Open every thread of `pid` (a freshly created suspended process has exactly one). Threads that
/// cannot be opened (e.g. short-lived ones injected by security software) are skipped.
fn open_threads(pid: u32) -> io::Result<Vec<Owned>> {
    // SAFETY: plain snapshot call; result checked.
    let snap = Owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(werr)?);
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut threads = Vec::new();
    // SAFETY: `entry.dwSize` is set and `entry` is valid for writes.
    let mut ok = unsafe { Thread32First(snap.0, &mut entry) }.is_ok();
    while ok {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: opening a thread by id with the minimal right; failure is skipped.
            if let Ok(h) = unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) } {
                threads.push(Owned(h));
            }
        }
        // SAFETY: as for Thread32First.
        ok = unsafe { Thread32Next(snap.0, &mut entry) }.is_ok();
    }
    if threads.is_empty() {
        Err(io::Error::other(
            "no thread of the suspended child could be opened",
        ))
    } else {
        Ok(threads)
    }
}

/// Peak working set of a process, valid after it has exited as long as the handle is open.
fn peak_working_set(proc: HANDLE) -> io::Result<u64> {
    let mut c = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: `c` is a valid buffer of `cb` bytes; `proc` is a valid process handle.
    unsafe { GetProcessMemoryInfo(proc, &mut c, c.cb) }.map_err(werr)?;
    Ok(c.PeakWorkingSetSize as u64)
}

/// True if the job holds any process other than `main_pid`. The main process itself is ignored
/// by id, so the moment at which the job drops it does not matter.
fn has_other_processes(job: HANDLE, main_pid: u32) -> io::Result<bool> {
    // Room for the header plus well over 200 ids; a bigger job makes the call fail with
    // ERROR_MORE_DATA, which certainly means there are other processes.
    let mut buf = [0u64; 256];
    // SAFETY: `buf` is valid for writes of its full size and 8-byte aligned, which satisfies the
    // alignment of JOBOBJECT_BASIC_PROCESS_ID_LIST.
    let q = unsafe {
        QueryInformationJobObject(
            Some(job),
            JobObjectBasicProcessIdList,
            buf.as_mut_ptr() as *mut _,
            std::mem::size_of_val(&buf) as u32,
            None,
        )
    };
    match q {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => return Ok(true),
        Err(e) => return Err(werr(e)),
    }
    let base = buf.as_ptr() as *const u8;
    // SAFETY: the kernel filled a JOBOBJECT_BASIC_PROCESS_ID_LIST header at the start of `buf`.
    let n = unsafe { (*(base as *const JOBOBJECT_BASIC_PROCESS_ID_LIST)).NumberOfProcessIdsInList }
        as usize;
    let off = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList);
    let cap = (std::mem::size_of_val(&buf) - off) / std::mem::size_of::<usize>();
    // SAFETY: `off` is the offset of the id array inside `buf`; `n.min(cap)` ids lie within it
    // and were written by the kernel.
    let ids = unsafe { std::slice::from_raw_parts(base.add(off) as *const usize, n.min(cap)) };
    Ok(ids.iter().any(|&id| id != main_pid as usize))
}

fn query_accounting(job: HANDLE) -> io::Result<JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION> {
    let mut acct = JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION::default();
    // SAFETY: `acct` is a valid buffer of the stated size for this information class.
    unsafe {
        QueryInformationJobObject(
            Some(job),
            JobObjectBasicAndIoAccountingInformation,
            &mut acct as *mut _ as *mut _,
            std::mem::size_of::<JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION>() as u32,
            None,
        )
    }
    .map_err(werr)?;
    Ok(acct)
}

pub(crate) fn run(spec: &Spec) -> io::Result<Measurement> {
    let job = create_job()?;
    let mut cmd = spec.command()?;
    // With no inherited stream the child needs no console at all. DETACHED_PROCESS gives it none,
    // so no conhost.exe joins the job (CREATE_NO_WINDOW would add one, measured). With an inherited
    // stream keep the default console behaviour.
    let mut flags = CREATE_SUSPENDED.0;
    if spec.stdout != crate::Output::Inherit && spec.stderr != crate::Output::Inherit {
        flags |= DETACHED_PROCESS.0;
    }
    cmd.creation_flags(flags);
    let mut child = cmd.spawn()?;
    let proc = HANDLE(child.as_raw_handle());

    let setup = (|| {
        // SAFETY: both handles are valid; `child` keeps its process handle open.
        unsafe { AssignProcessToJobObject(job.0, proc) }.map_err(werr)?;
        open_threads(child.id())
    })();
    let threads = match setup {
        Ok(t) => t,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };

    // The child does nothing while suspended, so the clock starts right before the resume.
    let start = Instant::now();
    let mut resumed = 0;
    for t in &threads {
        // SAFETY: `t` is a valid thread handle with SUSPEND_RESUME access. Resuming a thread that
        // is not suspended is harmless.
        if unsafe { ResumeThread(t.0) } != u32::MAX {
            resumed += 1;
        }
    }
    drop(threads);
    if resumed == 0 {
        let e = io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }

    let wait_ms = match spec.timeout {
        Some(t) => t.as_millis().min(u128::from(INFINITE - 1)) as u32,
        None => INFINITE,
    };
    // SAFETY: valid process handle owned by `child`.
    let w = unsafe { WaitForSingleObject(proc, wait_ms) };
    let mut timed_out = false;
    let mut descendants_killed = false;
    if w == WAIT_TIMEOUT {
        timed_out = true;
        // The main process is still alive, so more than one active process means descendants.
        descendants_killed = has_other_processes(job.0, child.id())?;
        // SAFETY: valid job handle. Exit code 1 for killed processes.
        unsafe { TerminateJobObject(job.0, 1) }.map_err(werr)?;
        // SAFETY: as above; the process is dying, wait for it so accounting is final.
        unsafe { WaitForSingleObject(proc, INFINITE) };
    } else if w != WAIT_OBJECT_0 {
        let e = io::Error::last_os_error();
        let _ = child.kill();
        return Err(e);
    }
    // Everything below is read at the same moment as `wall`, before anything can wind down.
    let wall = start.elapsed();
    if !timed_out {
        descendants_killed = has_other_processes(job.0, child.id())?;
    }
    let acct = query_accounting(job.0)?;
    let status = child.wait()?;
    let peak_rss = peak_working_set(proc)?;
    let mut ext = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // SAFETY: `ext` is a valid buffer of the stated size for this information class.
    unsafe {
        QueryInformationJobObject(
            Some(job.0),
            JobObjectExtendedLimitInformation,
            &mut ext as *mut _ as *mut _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            None,
        )
    }
    .map_err(werr)?;

    // Times are in 100 ns units.
    let ticks = |t: i64| Duration::from_nanos((t.max(0) as u64).saturating_mul(100));
    Ok(Measurement {
        wall,
        user_cpu: ticks(acct.BasicInfo.TotalUserTime),
        kernel_cpu: ticks(acct.BasicInfo.TotalKernelTime),
        peak_rss,
        peak_commit_process: Some(ext.PeakProcessMemoryUsed as u64),
        peak_commit_job: Some(ext.PeakJobMemoryUsed as u64),
        exit_code: if timed_out { None } else { status.code() },
        timed_out,
        descendants_killed,
    })
    // `job` drops here: kill-on-close terminates anything still alive.
}
