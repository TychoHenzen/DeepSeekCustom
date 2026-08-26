//! Cut the process `PATH` back to a length `cmd.exe` can actually search,
//! and fill in anything the registry says should be there.
//!
//! Every child this harness spawns inherits the process environment, so a
//! `PATH` the launcher handed over in an unusable form breaks every tool
//! call at once. A real autopilot run hit exactly that: `cmd.exe` reported
//! `'node' is not recognized` for a program that sits at
//! `C:\Program Files\nodejs\node.exe`, on a list that named that very
//! directory. The model then spent four turns guessing quoting fixes for a
//! problem that was never about quoting.
//!
//! The cause is a hard limit in the shell, measured on this machine rather
//! than taken from documentation. `cmd.exe` reads at most 8191 characters
//! of `PATH` and silently drops the rest. A run with 8064 characters found
//! `node`. The same run with 8262 did not. The harness had inherited 184
//! entries, well past that line, so every directory after the cut was
//! invisible to every command the Bash tool ran.
//!
//! Length is what matters, so the repair shortens the list rather than
//! growing it:
//!
//! 1. Drop repeats. A launcher that prepends the same directories on each
//!    nested shell is where the bloat comes from, and on this machine an
//!    85-entry list held only 64 distinct directories.
//! 2. Append registry directories the process is missing, from `HKLM` and
//!    then `HKCU`, in the order a normal login joins them.
//! 3. While the list is still too long, drop from the end, and drop a
//!    directory the registry does not name before one it does. A run-only
//!    directory is worth keeping, but not at the price of hiding
//!    `System32`.
//!
//! Order is otherwise left alone. An inherited `PATH` may name a directory
//! that exists only for this run, and it stays ahead of the registry list
//! the way the launcher meant it to.

use std::path::Path;

/// Longest `PATH` `cmd.exe` will read. Anything past this is dropped by the
/// shell before it searches, so the repair keeps the list under it. Measured
/// on this machine: 8064 characters resolved `node`, 8262 did not.
pub const CMD_PATH_LIMIT: usize = 8191;

/// What one repair pass did. `main` logs this once the logging layer is up.
/// The repair itself has to run before that. It writes the environment, and
/// `env::set_var` is unsound once other threads read.
pub struct PathReport {
    /// Directories the process already had.
    pub before: usize,
    /// Characters the list held before, which is what the shell limit is
    /// against.
    pub before_len: usize,
    /// Directories the process has now.
    pub after: usize,
    /// Characters the list holds now.
    pub after_len: usize,
    /// Where `node.exe` was found, if it was found at all.
    pub node: Option<String>,
}

/// Shorten `PATH` to something `cmd.exe` can search, add whatever the
/// registry names and the process lacks, and report whether `node.exe` is
/// reachable afterwards.
///
/// # Safety
///
/// Writes the process environment through `std::env::set_var`, which is
/// unsound with concurrent readers. Call this from `main` before any thread
/// of this application starts.
pub unsafe fn repair_path() -> PathReport {
    let current = std::env::var("PATH").unwrap_or_default();
    let before = split_path(&current);

    let registry = registry_path_entries();
    let mut entries = dedupe(&before);
    append_missing(&mut entries, &registry);
    trim_to_limit(&mut entries, &registry);

    let joined = entries.join(SEPARATOR);
    if joined != current {
        // SAFETY: the caller guarantees no other thread is running.
        unsafe {
            std::env::set_var("PATH", &joined);
        }
    }

    PathReport {
        before: before.len(),
        before_len: current.len(),
        after: entries.len(),
        after_len: joined.len(),
        node: find_on_path(&entries, "node.exe"),
    }
}

/// Keep the first appearance of each directory and drop every later one.
/// This is the whole fix on a machine whose launcher stacks the same
/// directories on every nested shell.
pub fn dedupe(entries: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries {
        if !kept.iter().any(|k| same_dir(k, entry)) {
            kept.push(entry.clone());
        }
    }
    kept
}

/// Add every registry directory the list does not already name, in registry
/// order, so a process started with a stripped `PATH` still gets one.
pub fn append_missing(entries: &mut Vec<String>, registry: &[String]) {
    for dir in registry {
        if !entries.iter().any(|e| same_dir(e, dir)) {
            entries.push(dir.clone());
        }
    }
}

/// Drop entries from the end until the joined list fits the shell limit.
/// A directory the registry does not name goes first, since the registry
/// list is the one a login would have produced and holds `System32`.
pub fn trim_to_limit(entries: &mut Vec<String>, registry: &[String]) {
    while joined_len(entries) > CMD_PATH_LIMIT {
        let Some(index) = last_index_to_drop(entries, registry) else {
            return;
        };
        entries.remove(index);
    }
}

/// Length of the list once joined, without building the string.
pub fn joined_len(entries: &[String]) -> usize {
    let separators = entries.len().saturating_sub(1);
    entries.iter().map(String::len).sum::<usize>() + separators
}

/// The entry to give up next: the last one the registry does not name, or
/// the last one of all when the registry names every one of them. `None`
/// means the list is empty and there is nothing left to drop.
pub fn last_index_to_drop(entries: &[String], registry: &[String]) -> Option<usize> {
    let extra = entries
        .iter()
        .rposition(|e| !registry.iter().any(|r| same_dir(r, e)));
    extra.or_else(|| entries.len().checked_sub(1))
}

#[cfg(windows)]
const SEPARATOR: &str = ";";
#[cfg(not(windows))]
const SEPARATOR: &str = ":";

pub fn split_path(value: &str) -> Vec<String> {
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
pub fn same_dir(a: &str, b: &str) -> bool {
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
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.display().to_string())
}

/// The machine list followed by the user list, in the order a normal login
/// joins them. This is empty on any platform without a Windows registry.
/// A failed read also yields nothing. That leaves the inherited `PATH`
/// exactly as it was.
#[cfg(windows)]
fn registry_path_entries() -> Vec<String> {
    const MACHINE_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
    const USER_KEY: &str = "Environment";

    let mut entries = Vec::new();
    for (root, key) in [
        (
            windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
            MACHINE_KEY,
        ),
        (
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            USER_KEY,
        ),
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
