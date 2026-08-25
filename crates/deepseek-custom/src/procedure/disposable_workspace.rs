//! Disposable source snapshots for isolated patch drafting.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use thiserror::Error;

const EXCLUDED_NAMES: &[&str] = &[".git", "target", ".deepseek"];
const BINARY_SCAN_BUFFER_BYTES: usize = 8 * 1024;
const BINARY_EXTENSIONS: &[&str] = &[
    "7z", "a", "avi", "bin", "bmp", "class", "dll", "dylib", "exe", "flac", "gif", "gz", "ico",
    "jar", "jpeg", "jpg", "lib", "mkv", "mov", "mp3", "mp4", "o", "obj", "onnx", "otf", "pdb",
    "pdf", "png", "pyc", "so", "tar", "ttf", "wav", "webm", "webp", "woff", "woff2", "xz", "zip",
];

/// Failure while creating or removing an isolated source snapshot.
#[derive(Debug, Error)]
pub enum DisposableWorkspaceError {
    #[error("draft workspace source is not a directory: {0}")]
    SourceNotDirectory(PathBuf),
    #[error("draft workspace source must not be a link or reparse point: {0}")]
    LinkedSource(PathBuf),
    #[error("could not {action} draft workspace path {path}: {source}")]
    FileSystem {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// An owned source snapshot that removes itself when it leaves scope.
#[derive(Debug)]
pub struct DisposableDraftWorkspace {
    root: Option<PathBuf>,
}

impl DisposableDraftWorkspace {
    /// Copy the current source state into a new directory under the system temp directory.
    pub fn create(source_root: &Path) -> Result<Self, DisposableWorkspaceError> {
        validate_source_root(source_root)?;
        let root =
            std::env::temp_dir().join(format!("deepseek-draft-workspace-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).map_err(|source| file_error("create", &root, source))?;

        let workspace = Self { root: Some(root) };
        copy_directory(source_root, workspace.path())?;
        Ok(workspace)
    }

    /// Root directory supplied as the drafting subagent's working directory.
    pub fn path(&self) -> &Path {
        self.root
            .as_deref()
            .expect("a live disposable workspace always has a root")
    }

    /// Remove the snapshot now and report cleanup errors to the caller.
    pub fn close(mut self) -> Result<(), DisposableWorkspaceError> {
        let root = self
            .root
            .take()
            .expect("a live disposable workspace always has a root");
        match remove_workspace(&root) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.root = Some(root);
                Err(error)
            }
        }
    }
}

impl Drop for DisposableDraftWorkspace {
    fn drop(&mut self) {
        if let Some(root) = self.root.take() {
            let _ = remove_workspace(&root);
        }
    }
}

fn validate_source_root(source_root: &Path) -> Result<(), DisposableWorkspaceError> {
    let metadata = fs::symlink_metadata(source_root)
        .map_err(|source| file_error("inspect", source_root, source))?;
    if metadata.file_type().is_symlink() || is_reparse_point_from_metadata(&metadata) {
        return Err(DisposableWorkspaceError::LinkedSource(
            source_root.to_path_buf(),
        ));
    }
    if !metadata.is_dir() {
        return Err(DisposableWorkspaceError::SourceNotDirectory(
            source_root.to_path_buf(),
        ));
    }
    Ok(())
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), DisposableWorkspaceError> {
    let mut entries = fs::read_dir(source)
        .map_err(|error| file_error("read", source, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| file_error("read", source, error))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        if is_excluded_name(&name) {
            continue;
        }
        let source_path = entry.path();
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| file_error("inspect", &source_path, error))?;
        if metadata.file_type().is_symlink() || is_reparse_point_from_metadata(&metadata) {
            continue;
        }

        let destination_path = destination.join(&name);
        if metadata.is_dir() {
            fs::create_dir(&destination_path)
                .map_err(|error| file_error("create", &destination_path, error))?;
            copy_directory(&source_path, &destination_path)?;
        } else if metadata.is_file() && !is_binary_file(&source_path)? {
            fs::copy(&source_path, &destination_path)
                .map_err(|error| file_error("copy", &source_path, error))?;
        }
    }
    Ok(())
}

fn is_excluded_name(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    EXCLUDED_NAMES
        .iter()
        .any(|excluded| name.eq_ignore_ascii_case(excluded))
}

fn is_binary_file(path: &Path) -> Result<bool, DisposableWorkspaceError> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            BINARY_EXTENSIONS
                .iter()
                .any(|binary| extension.eq_ignore_ascii_case(binary))
        })
    {
        return Ok(true);
    }

    let mut file = fs::File::open(path).map_err(|error| file_error("read", path, error))?;
    let mut buffer = [0; BINARY_SCAN_BUFFER_BYTES];
    let mut incomplete_utf8 = Vec::new();
    loop {
        let length = file
            .read(&mut buffer)
            .map_err(|error| file_error("read", path, error))?;
        if length == 0 {
            return Ok(!incomplete_utf8.is_empty());
        }
        if buffer[..length].contains(&0) {
            return Ok(true);
        }

        incomplete_utf8.extend_from_slice(&buffer[..length]);
        match std::str::from_utf8(&incomplete_utf8) {
            Ok(_) => incomplete_utf8.clear(),
            Err(error) if error.error_len().is_some() => return Ok(true),
            Err(error) => {
                incomplete_utf8.drain(..error.valid_up_to());
            }
        }
    }
}

fn remove_workspace(root: &Path) -> Result<(), DisposableWorkspaceError> {
    if !root.exists() {
        return Ok(());
    }
    make_tree_writable(root)?;
    fs::remove_dir_all(root).map_err(|source| file_error("remove", root, source))
}

#[cfg(windows)]
#[allow(
    clippy::permissions_set_readonly_false,
    reason = "clearing the Windows read-only attribute does not grant Unix world-write access"
)]
fn make_tree_writable(root: &Path) -> Result<(), DisposableWorkspaceError> {
    for entry in walkdir::WalkDir::new(root).contents_first(true) {
        let entry = entry.map_err(|error| {
            file_error("inspect", root, std::io::Error::other(error.to_string()))
        })?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| file_error("inspect", entry.path(), error))?;
        if metadata.is_file() && metadata.permissions().readonly() {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            fs::set_permissions(entry.path(), permissions)
                .map_err(|error| file_error("make writable", entry.path(), error))?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn make_tree_writable(_root: &Path) -> Result<(), DisposableWorkspaceError> {
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point_from_metadata(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point_from_metadata(_metadata: &fs::Metadata) -> bool {
    false
}

fn file_error(
    action: &'static str,
    path: &Path,
    source: std::io::Error,
) -> DisposableWorkspaceError {
    DisposableWorkspaceError::FileSystem {
        action,
        path: path.to_path_buf(),
        source,
    }
}
