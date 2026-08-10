//! Rebuild the process `PATH` from the Windows registry at startup.
//!
//! Every child this harness spawns inherits the process environment, so a
//! `PATH` the launcher handed over in an unusable form breaks every tool
//! call at once. A real autopilot run hit exactly that: `cmd.exe` reported
//! `'node' is not recognized`, and then `'where' is not recognized` for a
//! program that lives in `System32`. Nothing was missing from the machine.
//! The child shell simply had no directory list it could search.
//!
//! The registry is the authority for what `PATH` should be. `HKLM` holds
//! the machine list and `HKCU` holds the user list, and a normal login
//! joins them in that order. This module reads both and appends whatever
//! the running process is missing, so the fix does not depend on which
//! launcher started the harness.
//!
//! Entries already present are kept and stay first. An inherited `PATH` may
//! carry directories that exist only for this run, and dropping them would
//! trade one broken lookup for another.

/// What one repair pass did. `main` logs this once the logging layer is up.
/// The repair itself has to run before that. It writes the environment, and
/// `env::set_var` is unsound once other threads read.
pub struct PathReport {
    /// Directories the process already had.
    pub before: usize,
    /// Directories the process has now.
    pub after: usize,
    /// Where `node.exe` was found, if it was found at all.
    pub node: Option<String>,
}

/// Read the registry, append every missing directory to `PATH`, and report
/// whether `node.exe` is reachable afterwards.
///
/// # Safety
///
/// Writes the process environment through `std::env::set_var`, which is
/// unsound with concurrent readers. Call this from `main` before any thread
/// of this application starts.
pub unsafe fn repair_path() -> PathReport {
    let current = std::env::var("PATH").unwrap_or_default();
    let mut entries = split_path(&current);
    let before = entries.len();

    for dir in registry_path_entries() {
        if !entries.iter().any(|e| same_dir(e, &dir)) {
            entries.push(dir);
        }
    }

    let joined = entries.join(SEPARATOR);
    if joined != current {
        // SAFETY: the caller guarantees no other thread is running.
        unsafe {
            std::env::set_var("PATH", &joined);
        }
    }

    PathReport {
        after: entries.len(),
        before,
        node: find_on_path(&entries, "node.exe"),
    }
}

#[cfg(windows)]
const SEPARATOR: &str = ";";
#[cfg(not(windows))]
const SEPARATOR: &str = ":";

fn split_path(value: &str) -> Vec<String> {
    value
        .split(SEPARATOR)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Compare two directory strings the way the shell does. Windows ignores
/// case. Both sides ignore a trailing separator, because the registry writes
/// `C:\Program Files\nodejs\` where an inherited list often has no slash.
fn same_dir(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches(['\\', '/']);
    let b = b.trim_end_matches(['\\', '/']);
    if cfg!(windows) {
        return a.eq_ignore_ascii_case(b);
    }
    a == b
}

/// Look for one program across a directory list, so the caller can say in
/// the log whether the repair actually made the tool reachable.
fn find_on_path(entries: &[String], program: &str) -> Option<String> {
    entries
        .iter()
        .map(|dir| std::path::Path::new(dir).join(program))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.display().to_string())
}

/// The machine list followed by the user list, in the order a normal login
/// joins them. This is empty on any platform without a Windows registry.
/// A failed read also yields nothing. That leaves the inherited `PATH`
/// exactly as it was.
#[cfg(windows)]
fn registry_path_entries() -> Vec<String> {
    const MACHINE_KEY: &str =
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
    const USER_KEY: &str = "Environment";

    let mut entries = Vec::new();
    for (root, key) in [
        (windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, MACHINE_KEY),
        (windows::Win32::System::Registry::HKEY_CURRENT_USER, USER_KEY),
    ] {
        if let Some(value) = read_registry_string(root, key, "Path") {
            entries.extend(split_path(&value));
        }
    }
    entries
}

#[cfg(not(windows))]
fn registry_path_entries() -> Vec<String> {
    Vec::new()
}

/// Read one string value and expand any `%VAR%` reference it carries. The
/// machine `Path` is a `REG_EXPAND_SZ` value, and it really does hold such
/// references. An unexpanded read would append literal `%SystemRoot%` text
/// that no shell can search.
#[cfg(windows)]
fn read_registry_string(
    root: windows::Win32::System::Registry::HKEY,
    key: &str,
    value: &str,
) -> Option<String> {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RRF_RT_ANY, RegGetValueW};
    use windows::core::HSTRING;

    let key = HSTRING::from(key);
    let value = HSTRING::from(value);

    // One call, either sizing or reading. `RegGetValueW` reports the size
    // it needs when it gets no buffer. That is how a value as long as
    // `Path` is read without a fixed buffer nobody can size right.
    let read = |buffer: Option<*mut std::ffi::c_void>, size: &mut u32| unsafe {
        RegGetValueW(root, &key, &value, RRF_RT_ANY, None, buffer, Some(size))
    };

    let mut size: u32 = 0;
    if read(None, &mut size) != ERROR_SUCCESS || size == 0 {
        return None;
    }

    let mut buffer = vec![0u16; size as usize / 2 + 1];
    let mut size = (buffer.len() * 2) as u32;
    if read(Some(buffer.as_mut_ptr().cast()), &mut size) != ERROR_SUCCESS {
        return None;
    }

    // Stop at the terminator rather than at the reported size. The size
    // comes back in bytes and counts the terminator, and a rounded-up
    // buffer would otherwise carry stale text past the end of the value.
    let end = buffer
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(buffer.len())
        .min((size as usize / 2).min(buffer.len()));
    Some(String::from_utf16_lossy(&buffer[..end]))
}
