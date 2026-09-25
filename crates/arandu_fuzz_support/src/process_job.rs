//! Shared process isolation helpers for Windows (Job Objects) and Unix (Process Groups).

#[cfg(unix)]
unsafe extern "C" {
    fn kill(process_group: i32, signal: i32) -> i32;
}

/// Terminate all processes belonging to the dedicated process group whose ID is `process_id`.
#[cfg(unix)]
pub fn terminate_process_group(process_id: u32) {
    const SIGKILL: i32 = 9;
    let Ok(process_group) = i32::try_from(process_id) else {
        return;
    };
    // SAFETY: caller contract: child processes were spawned in a dedicated process
    // group where the group ID matches `process_id`. A negative PID targets only that
    // process group.
    unsafe {
        let _ = kill(-process_group, SIGKILL);
    }
}

/// Windows Job Object guard configured with `KILL_ON_JOB_CLOSE`.
#[cfg(windows)]
pub struct ProcessJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl ProcessJob {
    /// Creates an unnamed Job Object configured with `KILL_ON_JOB_CLOSE`.
    pub fn new() -> Result<Self, String> {
        use std::ptr;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        // SAFETY: null security attributes and name request an unnamed job
        // object with default security for this process.
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            return Err(format!("cannot create job object: Windows error {error}"));
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = match u32::try_from(std::mem::size_of_val(&limits)) {
            Ok(size) => size,
            Err(_) => {
                // SAFETY: handle is the live job handle returned above.
                unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
                return Err("job information size overflow".to_owned());
            }
        };
        // SAFETY: `limits` has the layout required by the information class and
        // remains alive for this synchronous call.
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size,
            )
        };
        if configured == 0 {
            // SAFETY: handle is the live job handle returned above.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            return Err(format!(
                "cannot configure job object: Windows error {error}"
            ));
        }
        Ok(Self(handle))
    }

    /// Assign a process to this Job Object by its PID.
    pub fn assign(&self, process_id: u32) -> Result<(), String> {
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };
        // SAFETY: OpenProcess accepts a process id and requested access mask.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, process_id) };
        if process.is_null() {
            let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            return Err(format!(
                "cannot open process for job assignment: Windows error {error}"
            ));
        }
        // SAFETY: both handles are live; `process` has the rights requested
        // above and `self.0` is a valid job object.
        let assigned = unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(self.0, process)
        };
        let error =
            (assigned == 0).then(|| unsafe { windows_sys::Win32::Foundation::GetLastError() });
        // SAFETY: process is the live handle returned by OpenProcess.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(process) };
        match error {
            Some(error) => Err(format!(
                "cannot assign process to job: Windows error {error}"
            )),
            None => Ok(()),
        }
    }

    /// Explicitly terminate all processes in this job.
    pub fn terminate(&self) {
        // SAFETY: self.0 is the live job handle owned by this guard.
        unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1) };
    }
}

#[cfg(windows)]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        // KILL_ON_JOB_CLOSE terminates any remaining descendants.
        // SAFETY: self.0 is the live handle owned by this guard.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}
