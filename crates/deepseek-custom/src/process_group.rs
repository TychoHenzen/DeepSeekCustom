//! Ties every child process this harness spawns to the harness's own
//! lifetime.
//!
//! Without this, closing the window or crashing left the `claude -p` child
//! running. That is not a small leak: a real orphan from an autopilot run
//! kept working for two and a half hours after its parent was gone, spawned
//! a subtree of 33 processes, and rewrote the repository it had been
//! pointed at, including `git stash` and `git reset --hard` over a running
//! session's edits.
//!
//! `kill_on_drop` alone does not fix that. It only fires when the `Child`
//! value is actually dropped, and on the way out of `main` the agent task
//! that owns the backend is a detached tokio task that never gets dropped
//! at all. A panic or an outside `taskkill` skips every destructor anyway.
//!
//! So the guarantee comes from the operating system instead. On Windows,
//! one job object is created the first time a child is adopted, with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` set. Every adopted child joins it.
//! When this process ends, however it ends, its last handle to the job
//! closes and the kernel terminates everything still inside. A child's own
//! children join the same job automatically, which is what makes this reach
//! a whole subtree rather than only the process spawned here.
//!
//! The job handle is deliberately never closed. Holding it open for the
//! life of the process is the entire mechanism.
//!
//! On any other platform this is a no-op that reports success, so a caller
//! needs no `cfg` of its own. There is no equivalent guarantee there yet.

/// Put `child` under the harness's lifetime, so the operating system kills
/// it if this process goes away.
///
/// Never fails the caller's own work: a failure is logged at `warn` and
/// swallowed. A turn that runs with an unreaped child is worse than a
/// clean turn, but it is much better than no turn at all.
pub fn adopt(child: &tokio::process::Child) {
    #[cfg(windows)]
    if let Err(e) = windows_impl::adopt(child) {
        tracing::warn!("process group: could not adopt child: {e}");
    }
    #[cfg(not(windows))]
    let _ = child;
}

#[cfg(windows)]
mod windows_impl {
    use std::sync::OnceLock;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// The process-wide job, as a raw pointer value. `HANDLE` is neither
    /// `Send` nor `Sync`, so the handle itself cannot live in a static. The
    /// pointer value can, and rebuilding a `HANDLE` around it is free.
    static JOB: OnceLock<Option<isize>> = OnceLock::new();

    /// Create the job on first use and configure it to kill its members
    /// when its last handle closes. Returns `None` when either step fails,
    /// which is cached: a machine that refuses to make a job object will
    /// refuse every time, and retrying per spawn would only repeat the log.
    fn job() -> Option<HANDLE> {
        let raw = JOB.get_or_init(|| match create_job() {
            Ok(handle) => Some(handle.0 as isize),
            Err(e) => {
                tracing::warn!("process group: could not create job object: {e}");
                None
            }
        });
        raw.map(|value| HANDLE(value as *mut core::ffi::c_void))
    }

    fn create_job() -> windows::core::Result<HANDLE> {
        // SAFETY: both calls are plain Win32 FFI. `CreateJobObjectW` takes
        // two optional pointers, both absent here, and returns an owned
        // handle. `SetInformationJobObject` reads `limits` for the length
        // given, which is that value's own size, and does not retain it.
        unsafe {
            let handle = CreateJobObjectW(None, None)?;
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
            Ok(handle)
        }
    }

    pub fn adopt(child: &tokio::process::Child) -> windows::core::Result<()> {
        let Some(job) = job() else {
            return Ok(());
        };
        // A child that has already exited has no handle left to assign.
        // That is not an error: there is nothing to reap.
        let Some(raw) = child.raw_handle() else {
            return Ok(());
        };
        // SAFETY: `raw` is the live process handle tokio holds for a child
        // it has not reaped, and `job` is the handle created above. The
        // call borrows neither past its return.
        unsafe { AssignProcessToJobObject(job, HANDLE(raw)) }
    }
}
