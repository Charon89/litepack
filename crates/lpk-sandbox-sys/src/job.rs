//! Job objects (Windows): the sandbox job and the queries the tests and the broker need.
//!
//! The create/set/query calls follow `lpk-procstat-sys/src/windows_impl.rs` (its kill-on-close job,
//! process-id list and accounting queries); this module adds the sandbox's limits.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::time::Duration;
use windows::Win32::Foundation::{ERROR_MORE_DATA, HANDLE};
use windows::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectBasicAndIoAccountingInformation, JobObjectBasicProcessIdList,
    JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject, JOBOBJECTINFOCLASS,
    JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_UI_RESTRICTIONS,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOB_OBJECT_LIMIT_PROCESS_TIME, JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS,
    JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES,
    JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
    JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};

const _: () = assert!(std::mem::size_of::<usize>() == 8, "64-bit Windows only");

pub(crate) fn werr(e: windows::core::Error) -> io::Error {
    let code = e.code().0 as u32;
    if code & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xFFFF) as i32)
    } else {
        io::Error::other(e.to_string())
    }
}

/// CPU time in the job's 100 ns ticks.
pub(crate) fn ticks(d: Duration) -> i64 {
    i64::try_from(d.as_nanos() / 100).unwrap_or(i64::MAX)
}

/// A job object, closed on drop (kill-on-close when the sandbox limits are set).
#[derive(Debug)]
pub struct Job(OwnedHandle);

impl Job {
    /// A job with no limits.
    pub fn new() -> io::Result<Job> {
        // SAFETY: null attributes and name are allowed; the result is checked.
        let h = unsafe { CreateJobObjectW(None, None) }.map_err(werr)?;
        // SAFETY: `h` was just returned by the OS and nothing else owns it.
        Ok(Job(unsafe { OwnedHandle::from_raw_handle(h.0) }))
    }

    /// A job with the sandbox's limits: one active process, `memory` bytes of commit per process
    /// and for the job, `cpu` of user time per process, every UI restriction, kill-on-close and
    /// die-on-unhandled-exception; no breakaway.
    pub fn sandbox(memory: u64, cpu: Duration) -> io::Result<Job> {
        let job = Job::new()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let b = &mut info.BasicLimitInformation;
        b.LimitFlags = JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_PROCESS_TIME
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        b.ActiveProcessLimit = 1;
        b.PerProcessUserTimeLimit = ticks(cpu);
        let mem = usize::try_from(memory).unwrap_or(usize::MAX);
        info.ProcessMemoryLimit = mem;
        info.JobMemoryLimit = mem;
        job.set_extended_limits(&info)?;
        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_EXITWINDOWS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
        };
        job.set(JobObjectBasicUIRestrictions, &ui)?;
        Ok(job)
    }

    /// The raw handle (borrowed; valid while `self` lives).
    pub fn raw(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }

    fn set<T>(&self, class: JOBOBJECTINFOCLASS, value: &T) -> io::Result<()> {
        // SAFETY: `value` is a valid `T` of the stated size, the structure `class` expects (each
        // caller pairs the class with its structure).
        unsafe {
            SetInformationJobObject(
                self.raw(),
                class,
                value as *const T as *const _,
                std::mem::size_of::<T>() as u32,
            )
        }
        .map_err(werr)
    }

    fn query<T: Default>(&self, class: JOBOBJECTINFOCLASS) -> io::Result<T> {
        let mut out = T::default();
        // SAFETY: `out` is a writable `T` of the stated size, the structure `class` returns.
        unsafe {
            QueryInformationJobObject(
                Some(self.raw()),
                class,
                &mut out as *mut T as *mut _,
                std::mem::size_of::<T>() as u32,
                None,
            )
        }
        .map_err(werr)?;
        Ok(out)
    }

    /// Set the extended limits.
    pub fn set_extended_limits(
        &self,
        info: &JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    ) -> io::Result<()> {
        self.set(JobObjectExtendedLimitInformation, info)
    }

    /// The extended limits, with the peak commit figures.
    pub fn extended_limits(&self) -> io::Result<JOBOBJECT_EXTENDED_LIMIT_INFORMATION> {
        self.query(JobObjectExtendedLimitInformation)
    }

    /// The UI restriction flags.
    pub fn ui_restrictions(&self) -> io::Result<u32> {
        let ui: JOBOBJECT_BASIC_UI_RESTRICTIONS = self.query(JobObjectBasicUIRestrictions)?;
        Ok(ui.UIRestrictionsClass.0)
    }

    /// Change the per-process user-time limit, also for the processes already in the job.
    pub fn set_process_time_limit(&self, cpu: Duration) -> io::Result<()> {
        let mut info = self.extended_limits()?;
        info.BasicLimitInformation.PerProcessUserTimeLimit = ticks(cpu);
        self.set_extended_limits(&info)
    }

    /// Accounting totals (CPU times, process counts, I/O).
    pub fn accounting(&self) -> io::Result<JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION> {
        self.query(JobObjectBasicAndIoAccountingInformation)
    }

    /// Ids of the processes in the job (at most 254; more is reported as an error).
    pub fn process_ids(&self) -> io::Result<Vec<u32>> {
        // Header (two u32 in the first u64) plus 255 ids.
        let mut buf = [0u64; 256];
        // SAFETY: `buf` is writable for its full size and 8-byte aligned, which satisfies the
        // alignment of JOBOBJECT_BASIC_PROCESS_ID_LIST.
        let q = unsafe {
            QueryInformationJobObject(
                Some(self.raw()),
                JobObjectBasicProcessIdList,
                buf.as_mut_ptr() as *mut _,
                std::mem::size_of_val(&buf) as u32,
                None,
            )
        };
        match q {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => {
                return Err(io::Error::other("more than 255 processes in the job"))
            }
            Err(e) => return Err(werr(e)),
        }
        // Little-endian layout: NumberOfAssignedProcesses, NumberOfProcessIdsInList (u32 each),
        // then the ids as usize.
        let n = ((buf[0] >> 32) as usize).min(buf.len() - 1);
        Ok(buf[1..=n].iter().map(|&id| id as u32).collect())
    }

    /// Terminate every process in the job with `code`.
    pub fn terminate(&self, code: u32) -> io::Result<()> {
        // SAFETY: valid job handle owned by `self`.
        unsafe { TerminateJobObject(self.raw(), code) }.map_err(werr)
    }
}
