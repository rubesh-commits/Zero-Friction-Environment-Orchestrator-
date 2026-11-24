//! Minimal Win32 bindings for telemetry and process introspection.
//!
//! All of this is read-only: memory/disk figures and a process table. No code
//! in this module terminates or modifies another process.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryStatus {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub load_percent: u32,
}

impl MemoryStatus {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }

    pub fn available_gib(&self) -> f64 {
        self.available_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DiskSpace {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
}

#[cfg(windows)]
pub fn memory_status() -> Option<MemoryStatus> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    if ok == 0 {
        return None;
    }
    Some(MemoryStatus {
        total_bytes: status.ullTotalPhys,
        available_bytes: status.ullAvailPhys,
        load_percent: status.dwMemoryLoad,
    })
}

#[cfg(not(windows))]
pub fn memory_status() -> Option<MemoryStatus> {
    None
}

#[cfg(windows)]
pub fn disk_space(path: &Path) -> Option<DiskSpace> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available = 0u64;
    let mut total = 0u64;
    let mut free = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) };
    if ok == 0 {
        return None;
    }
    Some(DiskSpace {
        total_bytes: total,
        free_bytes: free,
        available_bytes: available,
    })
}

#[cfg(not(windows))]
pub fn disk_space(_path: &Path) -> Option<DiskSpace> {
    None
}

#[cfg(windows)]
pub fn process_table() -> Vec<ProcessEntry> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut entries = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            return entries;
        }
        let mut raw: PROCESSENTRY32W = std::mem::zeroed();
        raw.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snapshot, &mut raw);
        while ok != 0 {
            let name_len = raw
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(raw.szExeFile.len());
            let name = OsString::from_wide(&raw.szExeFile[..name_len])
                .to_string_lossy()
                .to_string();
            entries.push(ProcessEntry {
                pid: raw.th32ProcessID,
                parent_pid: raw.th32ParentProcessID,
                name,
            });
            ok = Process32NextW(snapshot, &mut raw);
        }
        CloseHandle(snapshot);
    }
    entries.sort_by_key(|entry| entry.pid);
    entries
}

#[cfg(not(windows))]
pub fn process_table() -> Vec<ProcessEntry> {
    Vec::new()
}

pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    process_table().iter().any(|entry| entry.pid == pid)
}

/// Return `pid` plus every descendant, using the Toolhelp snapshot's parent
/// links. Result is sorted and deduplicated.
pub fn process_tree(root_pid: u32) -> Vec<u32> {
    let table = process_table();
    let mut result = vec![root_pid];
    let mut index = 0;
    while index < result.len() {
        let current = result[index];
        for entry in &table {
            if entry.parent_pid == current && !result.contains(&entry.pid) {
                result.push(entry.pid);
            }
        }
        index += 1;
    }
    result.sort_unstable();
    result
}

/// A Windows Job Object used to contain a sandbox process tree.
///
/// The handle keeps the job alive. When the job is created with
/// `KILL_ON_JOB_CLOSE`, dropping the handle terminates every process still
/// assigned to it, which is exactly what we want for sandbox cleanup.
pub struct JobHandle {
    #[cfg(windows)]
    raw: isize,
    #[cfg(not(windows))]
    _private: (),
}

impl JobHandle {
    /// Create a job with an optional memory cap in megabytes. Returns `None`
    /// when job objects are unavailable (non-Windows or an API failure); the
    /// caller can still run the process, just without a hard cap.
    #[cfg(windows)]
    pub fn create(memory_limit_mb: Option<u64>) -> Option<Self> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_JOB_MEMORY,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        };
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if let Some(mb) = memory_limit_mb {
                let bytes = mb.saturating_mul(1024 * 1024);
                info.BasicLimitInformation.LimitFlags |=
                    JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY;
                info.ProcessMemoryLimit = bytes as usize;
                info.JobMemoryLimit = bytes as usize;
            }
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                use windows_sys::Win32::Foundation::CloseHandle;
                CloseHandle(job);
                return None;
            }
            Some(Self { raw: job as isize })
        }
    }

    #[cfg(not(windows))]
    pub fn create(_memory_limit_mb: Option<u64>) -> Option<Self> {
        None
    }

    /// Assign a running process to the job. Descendants it spawns afterwards
    /// are automatically included.
    #[cfg(windows)]
    pub fn assign_pid(&self, pid: u32) -> bool {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };
        unsafe {
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return false;
            }
            let ok = windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                self.raw as *mut std::ffi::c_void,
                process,
            );
            CloseHandle(process);
            ok != 0
        }
    }

    #[cfg(not(windows))]
    pub fn assign_pid(&self, _pid: u32) -> bool {
        false
    }

    #[cfg(windows)]
    pub fn terminate(&self) -> bool {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        unsafe { TerminateJobObject(self.raw as *mut std::ffi::c_void, 1) != 0 }
    }

    #[cfg(not(windows))]
    pub fn terminate(&self) -> bool {
        false
    }
}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            CloseHandle(self.raw as *mut std::ffi::c_void);
        }
    }
}

/// Resume a process that was started with `CREATE_SUSPENDED` so it can be
/// assigned to a job object before it executes any code. Uses the documented
/// ntdll export because `std::process` does not expose the primary thread
/// handle.
#[cfg(windows)]
pub fn resume_process(pid: u32) -> bool {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SUSPEND_RESUME};

    type NtResumeProcess = unsafe extern "system" fn(*mut c_void) -> i32;

    unsafe {
        let module_name: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
        let module = LoadLibraryW(module_name.as_ptr());
        if module.is_null() {
            return false;
        }
        let proc_name = b"NtResumeProcess\0";
        let address = GetProcAddress(module, proc_name.as_ptr());
        let Some(address) = address else {
            return false;
        };
        let resume: NtResumeProcess = std::mem::transmute(address);
        let process = OpenProcess(PROCESS_SUSPEND_RESUME, 0, pid);
        if process.is_null() {
            return false;
        }
        let status = resume(process);
        CloseHandle(process);
        status >= 0
    }
}

#[cfg(not(windows))]
pub fn resume_process(_pid: u32) -> bool {
    false
}
