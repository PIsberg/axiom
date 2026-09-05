//! Kernel-enforced sandboxing for process-isolated snippet and test execution.
//!
//! On Windows, execution is wrapped in a Windows Job Object configured with:
//! - `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: Guarantees kernel-level cleanup of all child
//!   and grandchild processes when the handle closes or parent terminates.
//! - `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`: Prevents crash dialogs from blocking execution.
//! - Memory ceilings via `AXIOM_SANDBOX_MEMORY_LIMIT_MB` (`JOB_OBJECT_LIMIT_PROCESS_MEMORY`).
//! - Active process ceilings via `AXIOM_SANDBOX_MAX_PROCESSES` (`JOB_OBJECT_LIMIT_ACTIVE_PROCESS`).
//! - Peak memory accounting via `QueryInformationJobObject`.
//!
//! On Unix, execution uses process group isolation (`setpgid`) and `setrlimit`
//! ceilings (`RLIMIT_AS`, `RLIMIT_NPROC`) with negative-PID group termination.

use std::process::{Child, Command};

/// Memory limit in bytes parsed from `AXIOM_SANDBOX_MEMORY_LIMIT_MB`, if configured.
pub fn configured_memory_limit_bytes() -> Option<usize> {
    std::env::var("AXIOM_SANDBOX_MEMORY_LIMIT_MB")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&mb| mb > 0)
        .map(|mb| mb * 1024 * 1024)
}

/// Max process count parsed from `AXIOM_SANDBOX_MAX_PROCESSES`, if configured.
pub fn configured_max_processes() -> Option<u32> {
    std::env::var("AXIOM_SANDBOX_MAX_PROCESSES")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|&p| p > 0)
}

#[cfg(windows)]
#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]
mod win32 {
    use std::ffi::c_void;

    pub type HANDLE = *mut c_void;
    pub type BOOL = i32;
    pub type DWORD = u32;
    pub type ULONG_PTR = usize;

    pub const FALSE: BOOL = 0;
    pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

    pub const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: DWORD = 0x2000;
    pub const JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION: DWORD = 0x0400;
    pub const JOB_OBJECT_LIMIT_ACTIVE_PROCESS: DWORD = 0x0008;
    pub const JOB_OBJECT_LIMIT_PROCESS_MEMORY: DWORD = 0x0100;
    pub const JOB_OBJECT_LIMIT_JOB_MEMORY: DWORD = 0x0200;

    pub const JobObjectExtendedLimitInformation: i32 = 9;

    #[repr(C)]
    #[derive(Default)]
    pub struct IO_COUNTERS {
        pub ReadOperationCount: u64,
        pub WriteOperationCount: u64,
        pub OtherOperationCount: u64,
        pub ReadTransferCount: u64,
        pub WriteTransferCount: u64,
        pub OtherTransferCount: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct JOBOBJECT_BASIC_LIMIT_INFORMATION {
        pub PerProcessUserTimeLimit: i64,
        pub PerJobUserTimeLimit: i64,
        pub LimitFlags: DWORD,
        pub MinimumWorkingSetSize: ULONG_PTR,
        pub MaximumWorkingSetSize: ULONG_PTR,
        pub ActiveProcessLimit: DWORD,
        pub Affinity: ULONG_PTR,
        pub PriorityClass: DWORD,
        pub SchedulingClass: DWORD,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
        pub BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION,
        pub IoInfo: IO_COUNTERS,
        pub ProcessMemoryLimit: ULONG_PTR,
        pub JobMemoryLimit: ULONG_PTR,
        pub PeakProcessMemoryUsed: ULONG_PTR,
        pub PeakJobMemoryUsed: ULONG_PTR,
    }

    #[repr(C)]
    pub struct SECURITY_ATTRIBUTES {
        pub nLength: DWORD,
        pub lpSecurityDescriptor: *mut c_void,
        pub bInheritHandle: BOOL,
    }

    unsafe extern "system" {
        pub fn CreateJobObjectW(
            lpJobAttributes: *mut SECURITY_ATTRIBUTES,
            lpName: *const u16,
        ) -> HANDLE;

        pub fn SetInformationJobObject(
            hJob: HANDLE,
            JobObjectInformationClass: i32,
            lpJobObjectInformation: *const c_void,
            cbJobObjectInformationLength: DWORD,
        ) -> BOOL;

        pub fn QueryInformationJobObject(
            hJob: HANDLE,
            JobObjectInformationClass: i32,
            lpJobObjectInformation: *mut c_void,
            cbJobObjectInformationLength: DWORD,
            lpReturnLength: *mut DWORD,
        ) -> BOOL;

        pub fn AssignProcessToJobObject(
            hJob: HANDLE,
            hProcess: HANDLE,
        ) -> BOOL;

        pub fn TerminateJobObject(
            hJob: HANDLE,
            uExitCode: u32,
        ) -> BOOL;

        pub fn CloseHandle(hObject: HANDLE) -> BOOL;
    }
}

#[cfg(windows)]
pub struct SandboxGuard {
    job_handle: win32::HANDLE,
}

#[cfg(windows)]
impl SandboxGuard {
    /// Create a new Windows Job Object with kill-on-close and exception suppression.
    pub fn new() -> Option<Self> {
        let handle = unsafe { win32::CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
        if handle.is_null() || handle == win32::INVALID_HANDLE_VALUE {
            return None;
        }

        let mut info = win32::JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let mut flags = win32::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | win32::JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;

        if let Some(mem_limit) = configured_memory_limit_bytes() {
            flags |= win32::JOB_OBJECT_LIMIT_PROCESS_MEMORY | win32::JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.ProcessMemoryLimit = mem_limit;
            info.JobMemoryLimit = mem_limit;
        }

        if let Some(max_procs) = configured_max_processes() {
            flags |= win32::JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            info.BasicLimitInformation.ActiveProcessLimit = max_procs;
        }

        info.BasicLimitInformation.LimitFlags = flags;

        let res = unsafe {
            win32::SetInformationJobObject(
                handle,
                win32::JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<win32::JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as win32::DWORD,
            )
        };

        if res == win32::FALSE {
            unsafe { win32::CloseHandle(handle) };
            return None;
        }

        Some(Self { job_handle: handle })
    }

    /// Assign a child process to this Job Object.
    pub fn assign_child(&mut self, child: &Child) -> bool {
        use std::os::windows::io::AsRawHandle;
        let p_handle = child.as_raw_handle() as win32::HANDLE;
        let res = unsafe { win32::AssignProcessToJobObject(self.job_handle, p_handle) };
        res != win32::FALSE
    }

    /// Query peak memory used by the processes in the job object.
    pub fn peak_memory_bytes(&self) -> Option<u64> {
        let mut info = win32::JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let mut ret_len: win32::DWORD = 0;
        let res = unsafe {
            win32::QueryInformationJobObject(
                self.job_handle,
                win32::JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<win32::JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as win32::DWORD,
                &mut ret_len,
            )
        };

        if res != win32::FALSE {
            let peak = info.PeakJobMemoryUsed.max(info.PeakProcessMemoryUsed) as u64;
            if peak > 0 {
                return Some(peak);
            }
        }
        None
    }

    /// Terminate all processes in the job object immediately.
    pub fn terminate(&self) {
        unsafe {
            win32::TerminateJobObject(self.job_handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for SandboxGuard {
    fn drop(&mut self) {
        if !self.job_handle.is_null() && self.job_handle != win32::INVALID_HANDLE_VALUE {
            unsafe {
                win32::CloseHandle(self.job_handle);
            }
        }
    }
}

#[cfg(unix)]
pub struct SandboxGuard {
    pid: u32,
}

#[cfg(unix)]
impl SandboxGuard {
    pub fn new() -> Option<Self> {
        Some(Self { pid: 0 })
    }

    pub fn assign_child(&mut self, child: &Child) -> bool {
        self.pid = child.id();
        true
    }

    pub fn peak_memory_bytes(&self) -> Option<u64> {
        if self.pid == 0 {
            return None;
        }
        let status_path = format!("/proc/{}/status", self.pid);
        if let Ok(content) = std::fs::read_to_string(status_path) {
            for line in content.lines() {
                if line.starts_with("VmPeak:") || line.starts_with("VmHWM:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(kb) = parts[1].parse::<u64>() {
                            return Some(kb * 1024);
                        }
                    }
                }
            }
        }
        None
    }

    pub fn terminate(&self) {
        if self.pid > 0 {
            let pid = self.pid as i32;
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
}

/// Apply Unix rlimits and process group isolation to a command before spawning.
#[cfg(unix)]
pub fn configure_command_isolation(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    let mem_limit = configured_memory_limit_bytes();
    let max_procs = configured_max_processes();

    unsafe {
        command.pre_exec(move || {
            // Set process group leader
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }

            // Virtual address space limit
            if let Some(mem) = mem_limit {
                let rlim = libc::rlimit {
                    rlim_cur: mem as libc::rlim_t,
                    rlim_max: mem as libc::rlim_t,
                };
                libc::setrlimit(libc::RLIMIT_AS, &rlim);
            }

            // Process count limit
            if let Some(procs) = max_procs {
                let rlim = libc::rlimit {
                    rlim_cur: procs as libc::rlim_t,
                    rlim_max: procs as libc::rlim_t,
                };
                libc::setrlimit(libc::RLIMIT_NPROC, &rlim);
            }

            // Disable core dumps
            let rlim_core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::setrlimit(libc::RLIMIT_CORE, &rlim_core);

            Ok(())
        });
    }
}

/// Apply Windows or Unix process isolation configuration to `Command`.
pub fn prepare_command(command: &mut Command) {
    #[cfg(unix)]
    configure_command_isolation(command);

    #[cfg(windows)]
    {
        let _ = command;
    }
}
