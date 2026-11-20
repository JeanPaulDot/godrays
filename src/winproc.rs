//! Windows process plumbing for the hosting side: running a program with
//! administrator rights, and tying a child process tree to our own lifetime.
//! Other platforms get stand-ins that report the feature as unavailable.

#[cfg(windows)]
mod imp {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    /// Runs `exe parameters` as administrator (Windows shows its permission
    /// prompt) and waits for it to finish. Returns the exit code.
    pub fn run_elevated(exe: &Path, parameters: &str) -> Result<u32, String> {
        let verb = wide(OsStr::new("runas"));
        let file = wide(exe.as_os_str());
        let parameters = wide(OsStr::new(parameters));

        // SAFETY: SHELLEXECUTEINFOW is plain data; every pointer placed in it
        // refers to a buffer that outlives the ShellExecuteExW call.
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = parameters.as_ptr();
        info.nShow = SW_HIDE;

        // SAFETY: `info` is fully initialised above.
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            let code = unsafe { GetLastError() };
            return Err(if code == ERROR_CANCELLED {
                "Windows permission was declined, so nothing was installed.".to_owned()
            } else {
                format!("Windows could not start the setup (error {code}).")
            });
        }
        if info.hProcess.is_null() {
            return Err("Windows did not report the setup process.".to_owned());
        }
        // SAFETY: hProcess is a valid process handle we own because of
        // SEE_MASK_NOCLOSEPROCESS; it is closed exactly once here.
        unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1u32;
            GetExitCodeProcess(info.hProcess, &mut code);
            CloseHandle(info.hProcess);
            Ok(code)
        }
    }

    /// Keeps a child and everything it starts inside a job object that
    /// Windows terminates when this guard is dropped, or when the launcher
    /// itself dies for any reason.
    pub struct JobGuard(HANDLE);

    // SAFETY: a job handle may be used and closed from any thread.
    unsafe impl Send for JobGuard {}

    impl JobGuard {
        pub fn attach(child: &Child) -> Option<Self> {
            // SAFETY: straightforward Win32 calls; the job handle is closed
            // on every failure path and otherwise owned by the guard.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let configured = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const c_void,
                    std::mem::size_of_val(&limits) as u32,
                ) != 0;
                if !configured
                    || AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0
                {
                    CloseHandle(job);
                    return None;
                }
                Some(Self(job))
            }
        }
    }

    impl JobGuard {
        /// Processes currently in the job, nested jobs included.
        pub fn process_ids(&self) -> Vec<u32> {
            use windows_sys::Win32::System::JobObjects::{
                JobObjectBasicProcessIdList, QueryInformationJobObject,
                JOBOBJECT_BASIC_PROCESS_ID_LIST,
            };
            // Room for 64 processes; Kyber uses a handful.
            #[repr(C)]
            struct List {
                header: JOBOBJECT_BASIC_PROCESS_ID_LIST,
                more: [usize; 63],
            }
            // SAFETY: `List` extends the header's one-element array, as the
            // API expects; Windows writes at most the size we pass.
            unsafe {
                let mut list: List = std::mem::zeroed();
                let ok = QueryInformationJobObject(
                    self.0,
                    JobObjectBasicProcessIdList,
                    &mut list as *mut List as *mut c_void,
                    std::mem::size_of::<List>() as u32,
                    std::ptr::null_mut(),
                ) != 0;
                if !ok {
                    return Vec::new();
                }
                let count = (list.header.NumberOfProcessIdsInList as usize).min(64);
                let first = list.header.ProcessIdList.as_ptr();
                (0..count).map(|i| *first.add(i) as u32).collect()
            }
        }
    }

    /// Puts a process ahead of games for the CPU and the GPU, as game
    /// streaming hosts do: HIGH CPU priority class, and REALTIME GPU
    /// scheduling priority (the call Sunshine uses), falling back to HIGH
    /// when Windows refuses. Measured under full GPU load: REALTIME cuts
    /// Kyber's encode time by about a fifth; HIGH alone changes nothing.
    /// Returns (cpu, gpu) success.
    pub fn boost_process(pid: u32) -> (bool, bool) {
        use windows_sys::Wdk::Graphics::Direct3D::{
            D3DKMTSetProcessSchedulingPriorityClass, D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH,
            D3DKMT_SCHEDULINGPRIORITYCLASS_REALTIME,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, SetPriorityClass, HIGH_PRIORITY_CLASS, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SET_INFORMATION,
        };
        // SAFETY: plain Win32 calls on a handle we open and close here.
        unsafe {
            let process = OpenProcess(
                PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            );
            if process.is_null() {
                return (false, false);
            }
            let cpu = SetPriorityClass(process, HIGH_PRIORITY_CLASS) != 0;
            // GODRAYS_GPU_PRIORITY (0-5) overrides the class, for experiments.
            let wanted = std::env::var("GODRAYS_GPU_PRIORITY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(D3DKMT_SCHEDULINGPRIORITYCLASS_REALTIME);
            let gpu = D3DKMTSetProcessSchedulingPriorityClass(process, wanted) >= 0
                || D3DKMTSetProcessSchedulingPriorityClass(
                    process,
                    D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH,
                ) >= 0;
            CloseHandle(process);
            (cpu, gpu)
        }
    }

    impl Drop for JobGuard {
        fn drop(&mut self) {
            // SAFETY: the handle is valid and closed only here.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// An event the Kyber server watches (through `KYBER_SHUTDOWN_EVENT`) to
    /// shut itself down in an orderly way: it ends its client sessions and
    /// removes its virtual input devices before exiting. Kyber's own Windows
    /// service stops the server the same way.
    pub struct ShutdownEvent(HANDLE);

    // SAFETY: an event handle may be signalled and closed from any thread.
    unsafe impl Send for ShutdownEvent {}

    impl ShutdownEvent {
        pub fn new() -> Option<Self> {
            let attributes = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()
                    as u32,
                lpSecurityDescriptor: std::ptr::null_mut(),
                // The server process must inherit the handle.
                bInheritHandle: 1,
            };
            // SAFETY: `attributes` is valid for the call; manual-reset,
            // initially unsignalled, unnamed.
            let handle = unsafe {
                windows_sys::Win32::System::Threading::CreateEventW(
                    &attributes,
                    1,
                    0,
                    std::ptr::null(),
                )
            };
            (!handle.is_null()).then_some(Self(handle))
        }

        /// The handle value, as the server expects it in its environment.
        pub fn env_value(&self) -> String {
            (self.0 as isize).to_string()
        }

        pub fn signal(&self) {
            // SAFETY: the handle is valid until drop.
            unsafe { windows_sys::Win32::System::Threading::SetEvent(self.0) };
        }
    }

    impl Drop for ShutdownEvent {
        fn drop(&mut self) {
            // SAFETY: the handle is valid and closed only here.
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;
    use std::process::Child;

    pub fn run_elevated(_exe: &Path, _parameters: &str) -> Result<u32, String> {
        Err("Automatic setup is only available on Windows.".to_owned())
    }

    pub struct JobGuard;

    impl JobGuard {
        pub fn attach(_child: &Child) -> Option<Self> {
            None
        }

        pub fn process_ids(&self) -> Vec<u32> {
            Vec::new()
        }
    }

    pub fn boost_process(_pid: u32) -> (bool, bool) {
        (false, false)
    }

    pub struct ShutdownEvent;

    impl ShutdownEvent {
        pub fn new() -> Option<Self> {
            None
        }

        pub fn env_value(&self) -> String {
            String::new()
        }

        pub fn signal(&self) {}
    }
}

pub use imp::{boost_process, run_elevated, JobGuard, ShutdownEvent};
