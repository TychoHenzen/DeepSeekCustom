//! Filesystem details used by the procedure report repository.

use super::ProcedureRun;
use std::path::Path;

pub(super) fn openspec_artifact_paths(root: &Path, report: &ProcedureRun) -> Vec<String> {
    let prefix = format!("openspec/changes/{}", report.change_id);
    let mut paths = vec![
        format!("{prefix}/proposal.md"),
        format!("{prefix}/tasks.md"),
    ];
    if let Some(binding) = report.selected_task.covers.as_deref() {
        if let Some(capability) = binding.split("::").next().map(str::trim)
            && !capability.is_empty()
        {
            paths.push(format!("{prefix}/specs/{capability}/spec.md"));
        }
    } else {
        collect_spec_paths(root, &root.join(&prefix).join("specs"), &mut paths);
    }
    paths
}

fn collect_spec_paths(root: &Path, directory: &Path, paths: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries
        .filter_map(std::result::Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_spec_paths(root, &path, paths);
        } else if path.file_name().is_some_and(|name| name == "spec.md")
            && let Ok(relative) = path.strip_prefix(root)
        {
            paths.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[cfg(not(windows))]
pub(super) fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(source, target)
}

#[cfg(windows)]
pub(super) fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const REPLACE: u32 = 0x1;
    const WRITE_THROUGH: u32 = 0x8;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Both pointers reference live, null-terminated UTF-16 buffers.
    let replaced =
        unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), REPLACE | WRITE_THROUGH) };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
