//! Self-contained CUDA runtime DLL discovery and registration.
//!
//! kokoro-en asks ort for the CUDA execution provider. ort's CUDA provider
//! plugin (`onnxruntime_providers_cuda.dll`) imports several CUDA runtime
//! DLLs. Which exact file names those are can drift: ort-sys downloads a
//! prebuilt provider bundle at build time rather than vendoring one, and
//! this crate measured two different sets of required names within the
//! same day. Some of those DLLs ship as pip wheels (nvidia-cudnn-cu12 and
//! friends), landing under `nvidia/<component>/bin` inside whatever Python
//! site-packages directory pip installed them into. Others, on this
//! machine, only exist inside the CUDA Toolkit install (cublas, cublasLt,
//! cudart currently carry CUDA-13 names there, while the pip wheels are
//! still versioned for CUDA 12). Relying on `PATH` to contain all of those
//! directories made GPU speed depend on which shell or IDE launched the
//! process. This module finds them on its own and registers them with the
//! Windows loader before ort ever tries to load a provider, so `PATH` stops
//! mattering.
//!
//! Because the exact required DLL names drift, [`log_real_cuda_provider_status`]
//! does not hardcode a list to check. It attempts the same CUDA EP
//! registration kokoro-en performs, against a throwaway session, with
//! `error_on_failure()` turned on so a failure comes back as a real `Err`
//! instead of the silent-fallback-to-CPU behavior kokoro-en itself uses
//! (which is also why kokoro-en's own "using CUDA execution provider" log
//! line cannot be trusted: it only means the registration *call* returned
//! without a Rust-level error, not that the provider is actually usable).
//!
//! One failure mode this module cannot fix from inside the process. `ort-sys`
//! copies `onnxruntime_providers_cuda.dll` (and its neighbors) into
//! `target/debug/`, `target/debug/examples/`, and `target/debug/deps/`
//! separately, once per build-script invocation. It only copies when the
//! destination file does not already exist (see `ort-sys`'s
//! `build/dynamic_link.rs`, `copy_dylibs`). Say an earlier build copied a
//! DLL from a different, incompatible prebuilt bundle into one of those
//! directories. This can happen while `Cargo.lock` was still resolving
//! between `ort-sys` 2.0.0-rc.11/12/13, before this crate's exact version
//! pin was added. A later build with the corrected dependency version will
//! never overwrite that stale file, even though a fresh directory gets the
//! correct one. That produces exactly this symptom. The CUDA provider DLL
//! loads, since the file is found, but its `DllMain` fails during real CUDA
//! runtime init: Windows error 1114 (`ERROR_DLL_INIT_FAILED`). The cause is
//! a cuDNN/cuBLAS ABI mismatch against the installed CUDA runtime DLLs. It
//! reproduces in one build output directory (say the main binary's
//! `target/debug/`) but not another (say `target/debug/examples/`) built
//! moments apart from the identical source. To diagnose, compare the file
//! size and hash of `onnxruntime_providers_cuda.dll` across `target/debug/`
//! and `target/debug/examples/`. A mismatch confirms this. To fix, delete
//! the stale copies (`onnxruntime_providers_cuda.dll` and its sibling
//! `onnxruntime_providers_*`/`DirectML.dll` files) from the affected
//! `target/debug*` directory, then rebuild. `ort-sys` re-copies the current,
//! checksum-verified bundle since the destination no longer exists.
//! `target/` is gitignored, so this is never a source change.

#[cfg(windows)]
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;

#[cfg(windows)]
use ort::execution_providers::CUDAExecutionProvider;
#[cfg(windows)]
use ort::session::Session;
use tracing::debug;
#[cfg(windows)]
use tracing::{info, warn};
#[cfg(windows)]
use windows::Win32::System::LibraryLoader::{
    AddDllDirectory, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_USER_DIRS,
    SetDefaultDllDirectories,
};
#[cfg(windows)]
use windows::core::HSTRING;

/// Find every directory this machine's Python installations and CUDA
/// Toolkit install put CUDA runtime DLLs in. Register each with the Windows
/// loader so DLLs there resolve regardless of `PATH`. A no-op, not an
/// error, on a machine with neither.
///
/// Must run before the first ort session is built: `AddDllDirectory` only
/// affects loads that happen after it returns.
pub fn register_cuda_dll_dirs() {
    #[cfg(not(windows))]
    {
        debug!("cuda dll dirs: Windows DLL registration is unavailable on this platform");
    }

    #[cfg(windows)]
    {
        let dirs = find_cuda_dll_dirs();
        if dirs.is_empty() {
            debug!("cuda dll dirs: no CUDA runtime DLL dirs found, PATH left unchanged");
            return;
        }
        if !enable_dll_directory_search() {
            return;
        }
        let registered = dirs.iter().filter(|dir| add_dll_directory(dir)).count();
        debug!(
            "cuda dll dirs: registered {registered}/{} dir(s)",
            dirs.len()
        );
    }
}

/// Log the true answer to "is the CUDA execution provider actually usable",
/// instead of trusting kokoro-en's own log line (see module docs).
pub fn log_real_cuda_provider_status() {
    #[cfg(not(windows))]
    {
        debug!("cuda dll dirs: CUDA provider probing is unavailable on this platform");
    }

    #[cfg(windows)]
    {
        match probe_cuda_registration() {
            Ok(()) => info!(
                "cuda dll dirs: CUDA execution provider registered successfully, GPU is genuinely in use"
            ),
            Err(e) => warn!(
                "cuda dll dirs: CUDA execution provider registration failed ({e}), actually running on CPU regardless of what kokoro-en logged"
            ),
        }
    }
}

/// Every directory reachable from this machine's Python installations and
/// CUDA Toolkit install that might hold a CUDA runtime DLL. Empty, never an
/// error, when neither is present.
#[cfg(windows)]
fn find_cuda_dll_dirs() -> Vec<PathBuf> {
    let mut dirs = nvidia_bin_dirs_under(&candidate_site_packages_dirs());
    dirs.extend(cuda_toolkit_bin_dirs());
    dirs
}

/// For each candidate site-packages directory, glob `nvidia/*/bin` and
/// collect every directory found. `pub`: the moved discovery tests call
/// this directly against a temp directory tree; it is also used
/// unconditionally by `find_cuda_dll_dirs` above.
pub fn nvidia_bin_dirs_under(site_packages_dirs: &[PathBuf]) -> Vec<PathBuf> {
    site_packages_dirs
        .iter()
        .flat_map(|dir| nvidia_bin_dirs_in_one(dir))
        .collect()
}

/// `nvidia/*/bin` under one site-packages directory. Empty if there is no
/// `nvidia` directory there, or none of its children has a `bin`.
fn nvidia_bin_dirs_in_one(site_packages: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(site_packages.join("nvidia")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().join("bin"))
        .filter(|bin| bin.is_dir())
        .collect()
}

/// Every Python site-packages directory this machine plausibly has. Asks
/// Python itself first, then falls back to probing the standard per-user
/// install locations Windows Python installers use. Never fails: an absent
/// Python, or a Python with no such directories, just yields fewer or zero
/// candidates.
#[cfg(windows)]
fn candidate_site_packages_dirs() -> Vec<PathBuf> {
    let mut dirs = site_packages_from_python();
    dirs.extend(site_packages_from_known_locations());
    dirs
}

#[cfg(windows)]
const SITE_PACKAGES_SCRIPT: &str =
    "import site\nfor p in site.getsitepackages() + [site.getusersitepackages()]:\n    print(p)";

/// Ask Python for its site-packages directories, both the interpreter's own
/// and the per-user one `pip install --user` uses. Tries `py` (the Windows
/// launcher, always registered by the official installer) before `python`.
#[cfg(windows)]
fn site_packages_from_python() -> Vec<PathBuf> {
    for exe in ["py", "python", "python3"] {
        if let Some(dirs) = query_python_site_packages(exe) {
            return dirs;
        }
    }
    Vec::new()
}

#[cfg(windows)]
fn query_python_site_packages(exe: &str) -> Option<Vec<PathBuf>> {
    let output = Command::new(exe)
        .args(["-c", SITE_PACKAGES_SCRIPT])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let dirs: Vec<PathBuf> = stdout.lines().map(PathBuf::from).collect();
    (!dirs.is_empty()).then_some(dirs)
}

/// Standard per-user site-packages roots on Windows, used only when Python
/// could not be asked directly. `%APPDATA%\Python\Python3*\site-packages`
/// covers `pip install --user`.
/// `%LOCALAPPDATA%\Programs\Python\Python3*\Lib\site-packages` covers the
/// per-user official installer. Neither hardcodes a Python version.
#[cfg(windows)]
fn site_packages_from_known_locations() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(appdata) = env::var("APPDATA") {
        let root = PathBuf::from(appdata).join("Python");
        dirs.extend(
            child_dirs_starting_with(&root, "Python")
                .into_iter()
                .map(|d| d.join("site-packages")),
        );
    }
    if let Ok(local) = env::var("LOCALAPPDATA") {
        let root = PathBuf::from(local).join("Programs").join("Python");
        dirs.extend(
            child_dirs_starting_with(&root, "Python")
                .into_iter()
                .map(|d| d.join("Lib").join("site-packages")),
        );
    }
    dirs
}

/// CUDA Toolkit `bin` directories for every version installed under the
/// standard Program Files location. Used only when Python's pip wheels
/// don't cover a DLL the provider needs (see module docs).
#[cfg(windows)]
fn cuda_toolkit_bin_dirs() -> Vec<PathBuf> {
    let Ok(program_files) = env::var("ProgramFiles") else {
        return Vec::new();
    };
    let root = PathBuf::from(program_files)
        .join("NVIDIA GPU Computing Toolkit")
        .join("CUDA");
    cuda_toolkit_bin_dirs_under(&root)
}

/// `toolkit_bin_dir` applied to every `v*` version directory directly under
/// `cuda_root`. `pub`: the moved discovery tests call this directly
/// against a temp directory tree; it is also used unconditionally by
/// `cuda_toolkit_bin_dirs` above.
pub fn cuda_toolkit_bin_dirs_under(cuda_root: &Path) -> Vec<PathBuf> {
    child_dirs_starting_with(cuda_root, "v")
        .into_iter()
        .filter_map(|v| toolkit_bin_dir(&v))
        .collect()
}

/// The directory that actually holds a CUDA Toolkit version's DLLs.
/// `bin\x64` if it exists: CUDA 13 and newer moved the DLLs there.
/// Otherwise plain `bin`, for CUDA 12 and older. `None` if neither exists.
/// `pub`: the moved discovery tests call this directly; it is also used
/// unconditionally by [`cuda_toolkit_bin_dirs_under`] above.
pub fn toolkit_bin_dir(version_dir: &Path) -> Option<PathBuf> {
    let bin = version_dir.join("bin");
    let bin_x64 = bin.join("x64");
    if bin_x64.is_dir() {
        return Some(bin_x64);
    }
    bin.is_dir().then_some(bin)
}

fn child_dirs_starting_with(parent: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            p.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|n| n.starts_with(prefix))
        })
        .collect()
}

/// Turn on `AddDllDirectory`-based search for this process. Must happen
/// before `add_dll_directory` calls take effect.
#[cfg(windows)]
fn enable_dll_directory_search() -> bool {
    let flags = LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_USER_DIRS;
    // SAFETY: SetDefaultDllDirectories takes a plain flags bitmask, no
    // pointers or lifetimes involved. It is documented safe to call at any
    // point in the process, any number of times.
    let result = unsafe { SetDefaultDllDirectories(flags) };
    if let Err(e) = result {
        warn!(
            "cuda dll dirs: SetDefaultDllDirectories failed: {e}, PATH-independent CUDA DLL loading disabled"
        );
        return false;
    }
    true
}

/// Register one directory with the loader. Returns whether it succeeded.
#[cfg(windows)]
fn add_dll_directory(dir: &Path) -> bool {
    let wide = HSTRING::from(dir.to_string_lossy().as_ref());
    // SAFETY: AddDllDirectory takes a wide string naming a directory to
    // search and returns a cookie or null on failure. `wide` is a valid
    // HSTRING that outlives the call, and the returned cookie is only
    // checked for null here, never dereferenced.
    let cookie = unsafe { AddDllDirectory(&wide) };
    if cookie.is_null() {
        warn!(
            "cuda dll dirs: AddDllDirectory failed for {}",
            dir.display()
        );
        return false;
    }
    true
}

/// Attempt the same CUDA EP registration kokoro-en performs, against a
/// throwaway session builder, with failure reporting turned on.
/// `Ok(())` means the provider genuinely registered. `Err(message)` carries
/// ort's own diagnostic. That diagnostic already names the specific missing
/// DLL when that is the cause (see module docs). Building the session
/// itself failing also comes back as `Err`, since that too means no CUDA
/// session is possible.
#[cfg(windows)]
fn probe_cuda_registration() -> Result<(), String> {
    let builder = Session::builder().map_err(|e| e.to_string())?;
    let cuda = CUDAExecutionProvider::default().build().error_on_failure();
    builder
        .with_execution_providers([cuda])
        .map_err(|e| e.to_string())?;
    Ok(())
}
