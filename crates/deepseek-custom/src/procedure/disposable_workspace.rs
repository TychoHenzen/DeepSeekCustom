//! Disposable source snapshots for isolated patch drafting.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const EXCLUDED_NAMES: &[&str] = &[".git", "target", ".deepseek"];
const OWNED_WORKSPACE_PREFIX: &str = "deepseek-draft-workspace-";
const BINARY_SCAN_BUFFER_BYTES: usize = 8 * 1024;
/// Default maximum number of bytes copied into one disposable workspace.
pub const DEFAULT_DISPOSABLE_WORKSPACE_MAX_BYTES: u64 = 512 * 1024 * 1024;
const BINARY_EXTENSIONS: &[&str] = &[
    "7z", "a", "avi", "bin", "bmp", "class", "dll", "dylib", "exe", "flac", "gif", "gz", "ico",
    "jar", "jpeg", "jpg", "lib", "mkv", "mov", "mp3", "mp4", "o", "obj", "onnx", "otf", "pdb",
    "pdf", "png", "pyc", "so", "tar", "ttf", "wav", "webm", "webp", "woff", "woff2", "xz", "zip",
];

/// Extra source-relative trees that a disposable snapshot must not copy.
///
/// The built-in exclusions remain active for every snapshot. These paths let
/// a caller add project-specific output trees such as `dist` or `coverage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableWorkspaceOptions {
    pub excluded_paths: Vec<PathBuf>,
    pub max_bytes: u64,
}

/// Progress emitted after each copied regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotProgress {
    pub files_copied: u64,
    pub bytes_copied: u64,
    pub total_bytes: u64,
}

impl Default for DisposableWorkspaceOptions {
    fn default() -> Self {
        Self {
            excluded_paths: Vec::new(),
            max_bytes: DEFAULT_DISPOSABLE_WORKSPACE_MAX_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum SnapshotContents {
    DraftText,
    CurrentState,
}

/// Failure while creating or removing an isolated source snapshot.
#[derive(Debug, Error)]
pub enum DisposableWorkspaceError {
    #[error("draft workspace source is not a directory: {0}")]
    SourceNotDirectory(PathBuf),
    #[error("draft workspace source must not be a link or reparse point: {0}")]
    LinkedSource(PathBuf),
    #[error("draft workspace exclusion must be a relative path without `.` or `..`: {0}")]
    InvalidExclusion(PathBuf),
    #[error("refusing to clean a path that is not an owned disposable workspace: {0}")]
    UnownedCleanupRoot(PathBuf),
    #[error(
        "draft workspace source is {required_bytes} bytes, which exceeds the {max_bytes}-byte limit"
    )]
    SnapshotTooLarge { max_bytes: u64, required_bytes: u64 },
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

/// Deterministic identity of one regular file in an isolated workspace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WorkspaceFileFingerprint {
    pub byte_len: u64,
    pub sha256: String,
}

/// A path-sorted inventory of every regular repository file in a workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFileInventory {
    files: BTreeMap<String, WorkspaceFileFingerprint>,
}

impl WorkspaceFileInventory {
    pub fn capture(root: &Path) -> Result<Self, DisposableWorkspaceError> {
        Self::capture_with_options(root, &DisposableWorkspaceOptions::default())
    }

    fn capture_with_options(
        root: &Path,
        options: &DisposableWorkspaceOptions,
    ) -> Result<Self, DisposableWorkspaceError> {
        validate_source_root(root)?;
        let exclusions = SnapshotExclusions::new(options)?;
        let mut files = BTreeMap::new();
        inventory_directory(root, &exclusions, &mut Vec::new(), &mut files)?;
        Ok(Self { files })
    }

    pub fn files(&self) -> &BTreeMap<String, WorkspaceFileFingerprint> {
        &self.files
    }

    /// Compare path and byte identities without parsing a display diff.
    pub fn compare(&self, current: &Self) -> WorkspaceFileChanges {
        compare_inventories(self, current)
    }
}

/// One deterministic rename display pair. Authorization still uses both endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFileRename {
    pub from: String,
    pub to: String,
}

/// Every endpoint difference between an execution baseline and its current root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFileChanges {
    pub created: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    pub renamed: Vec<WorkspaceFileRename>,
}

impl WorkspaceFileChanges {
    /// Every changed endpoint in deterministic path order.
    pub fn changed_paths(&self) -> Vec<String> {
        self.created
            .iter()
            .chain(&self.modified)
            .chain(&self.deleted)
            .chain(
                self.renamed
                    .iter()
                    .flat_map(|rename| [&rename.from, &rename.to]),
            )
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// One immutable execution baseline and its writable fork.
#[derive(Debug)]
pub struct DisposableWorkspacePair {
    baseline: DisposableDraftWorkspace,
    execution: DisposableDraftWorkspace,
    baseline_inventory: WorkspaceFileInventory,
    inventory_options: DisposableWorkspaceOptions,
}

impl DisposableWorkspacePair {
    pub fn baseline_path(&self) -> &Path {
        self.baseline.path()
    }

    pub fn execution_path(&self) -> &Path {
        self.execution.path()
    }

    pub const fn baseline_inventory(&self) -> &WorkspaceFileInventory {
        &self.baseline_inventory
    }

    pub fn execution_inventory(&self) -> Result<WorkspaceFileInventory, DisposableWorkspaceError> {
        WorkspaceFileInventory::capture_with_options(self.execution_path(), &self.inventory_options)
    }

    pub fn changes(&self) -> Result<WorkspaceFileChanges, DisposableWorkspaceError> {
        Ok(self
            .baseline_inventory
            .compare(&self.execution_inventory()?))
    }

    /// Transfer both owned roots to diagnostic retention.
    pub fn retain_for_diagnostics(self) -> RetainedDisposableWorkspacePair {
        RetainedDisposableWorkspacePair {
            baseline: self.baseline.retain_for_recovery(),
            execution: self.execution.retain_for_recovery(),
        }
    }
}

/// Explicit ownership of baseline and execution roots retained for diagnostics.
#[derive(Debug)]
pub struct RetainedDisposableWorkspacePair {
    baseline: RetainedRecoveryWorkspace,
    execution: RetainedRecoveryWorkspace,
}

impl RetainedDisposableWorkspacePair {
    pub fn baseline_path(&self) -> &Path {
        self.baseline.path()
    }

    pub fn execution_path(&self) -> &Path {
        self.execution.path()
    }

    pub fn cleanup(self) -> Result<(), DisposableWorkspaceError> {
        let baseline_result = self.baseline.cleanup();
        let execution_result = self.execution.cleanup();
        baseline_result.and(execution_result)
    }
}

/// Snapshot data intentionally retained after rollback could not complete.
///
/// Ordinary disposable workspaces do not become this type. A caller should
/// retain one only after a rollback failure so recovery files remain inspectable.
#[derive(Debug)]
pub struct RetainedRecoveryWorkspace {
    root: PathBuf,
}

impl RetainedRecoveryWorkspace {
    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn cleanup(self) -> Result<(), DisposableWorkspaceError> {
        remove_workspace(&self.root)
    }
}

impl DisposableDraftWorkspace {
    /// Copy the current source state into a new directory under the system temp directory.
    pub fn create(source_root: &Path) -> Result<Self, DisposableWorkspaceError> {
        let mut progress = |_| {};
        Self::create_with_contents(
            source_root,
            SnapshotContents::DraftText,
            &DisposableWorkspaceOptions::default(),
            &mut progress,
        )
    }

    /// Copy every regular file from the current source state into a disposable directory.
    pub fn create_current_state(source_root: &Path) -> Result<Self, DisposableWorkspaceError> {
        Self::create_current_state_with_options(source_root, &DisposableWorkspaceOptions::default())
    }

    /// Read the live source once, then fork that exact snapshot for execution.
    pub fn create_current_state_pair(
        source_root: &Path,
    ) -> Result<DisposableWorkspacePair, DisposableWorkspaceError> {
        Self::create_current_state_pair_with_options(
            source_root,
            &DisposableWorkspaceOptions::default(),
        )
    }

    /// Create an immutable baseline and writable execution root from one live snapshot.
    pub fn create_current_state_pair_with_options(
        source_root: &Path,
        options: &DisposableWorkspaceOptions,
    ) -> Result<DisposableWorkspacePair, DisposableWorkspaceError> {
        let baseline = Self::create_current_state_with_options(source_root, options)?;
        let baseline_inventory =
            WorkspaceFileInventory::capture_with_options(baseline.path(), options)?;
        let execution = Self::create_current_state_with_options(baseline.path(), options)?;
        make_tree_read_only(baseline.path())?;
        Ok(DisposableWorkspacePair {
            baseline,
            execution,
            baseline_inventory,
            inventory_options: options.clone(),
        })
    }

    /// Copy the current source state with project-specific output exclusions.
    pub fn create_current_state_with_options(
        source_root: &Path,
        options: &DisposableWorkspaceOptions,
    ) -> Result<Self, DisposableWorkspaceError> {
        let mut progress = |_| {};
        Self::create_with_contents(
            source_root,
            SnapshotContents::CurrentState,
            options,
            &mut progress,
        )
    }

    /// Copy the current source state and report bounded copy progress.
    pub fn create_current_state_with_progress(
        source_root: &Path,
        options: &DisposableWorkspaceOptions,
        progress: &mut impl FnMut(SnapshotProgress),
    ) -> Result<Self, DisposableWorkspaceError> {
        Self::create_with_contents(
            source_root,
            SnapshotContents::CurrentState,
            options,
            progress,
        )
    }

    fn create_with_contents(
        source_root: &Path,
        contents: SnapshotContents,
        options: &DisposableWorkspaceOptions,
        progress: &mut impl FnMut(SnapshotProgress),
    ) -> Result<Self, DisposableWorkspaceError> {
        validate_source_root(source_root)?;
        let exclusions = SnapshotExclusions::new(options)?;
        let total_bytes = snapshot_size(source_root, contents, &exclusions, &mut Vec::new())?;
        if total_bytes > options.max_bytes {
            return Err(DisposableWorkspaceError::SnapshotTooLarge {
                max_bytes: options.max_bytes,
                required_bytes: total_bytes,
            });
        }
        let root =
            std::env::temp_dir().join(format!("{OWNED_WORKSPACE_PREFIX}{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).map_err(|source| file_error("create", &root, source))?;

        let workspace = Self { root: Some(root) };
        progress(SnapshotProgress {
            files_copied: 0,
            bytes_copied: 0,
            total_bytes,
        });
        let mut copy_state = CopyState {
            files_copied: 0,
            bytes_copied: 0,
            total_bytes,
            max_bytes: options.max_bytes,
            progress,
        };
        if let Err(error) = copy_directory(
            source_root,
            workspace.path(),
            contents,
            &exclusions,
            &mut Vec::new(),
            &mut copy_state,
        ) {
            drop(workspace);
            return Err(error);
        }
        Ok(workspace)
    }

    /// Transfer ownership without cleanup for rollback recovery inspection.
    pub fn retain_for_recovery(mut self) -> RetainedRecoveryWorkspace {
        RetainedRecoveryWorkspace {
            root: self
                .root
                .take()
                .expect("a live disposable workspace always has a root"),
        }
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

fn copy_directory(
    source: &Path,
    destination: &Path,
    contents: SnapshotContents,
    exclusions: &SnapshotExclusions,
    relative: &mut Vec<String>,
    copy_state: &mut CopyState<'_>,
) -> Result<(), DisposableWorkspaceError> {
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
        let component = name.to_string_lossy().into_owned();
        relative.push(component);
        if exclusions.matches(relative) {
            relative.pop();
            continue;
        }
        let source_path = entry.path();
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| file_error("inspect", &source_path, error))?;
        if metadata.file_type().is_symlink() || is_reparse_point_from_metadata(&metadata) {
            relative.pop();
            continue;
        }

        let destination_path = destination.join(&name);
        if metadata.is_dir() {
            fs::create_dir(&destination_path)
                .map_err(|error| file_error("create", &destination_path, error))?;
            copy_directory(
                &source_path,
                &destination_path,
                contents,
                exclusions,
                relative,
                copy_state,
            )?;
        } else if metadata.is_file()
            && (matches!(contents, SnapshotContents::CurrentState)
                || !is_binary_file(&source_path)?)
        {
            let copied_bytes = fs::copy(&source_path, &destination_path)
                .map_err(|error| file_error("copy", &source_path, error))?;
            copy_state.record_file(copied_bytes)?;
        }
        relative.pop();
    }
    Ok(())
}

struct CopyState<'a> {
    files_copied: u64,
    bytes_copied: u64,
    total_bytes: u64,
    max_bytes: u64,
    progress: &'a mut dyn FnMut(SnapshotProgress),
}

impl CopyState<'_> {
    fn record_file(&mut self, bytes: u64) -> Result<(), DisposableWorkspaceError> {
        self.bytes_copied = self.bytes_copied.checked_add(bytes).ok_or(
            DisposableWorkspaceError::SnapshotTooLarge {
                max_bytes: self.max_bytes,
                required_bytes: u64::MAX,
            },
        )?;
        if self.bytes_copied > self.max_bytes {
            return Err(DisposableWorkspaceError::SnapshotTooLarge {
                max_bytes: self.max_bytes,
                required_bytes: self.bytes_copied,
            });
        }
        self.files_copied += 1;
        (self.progress)(SnapshotProgress {
            files_copied: self.files_copied,
            bytes_copied: self.bytes_copied,
            total_bytes: self.total_bytes,
        });
        Ok(())
    }
}

fn snapshot_size(
    source: &Path,
    contents: SnapshotContents,
    exclusions: &SnapshotExclusions,
    relative: &mut Vec<String>,
) -> Result<u64, DisposableWorkspaceError> {
    let mut total = 0_u64;
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
        relative.push(name.to_string_lossy().into_owned());
        if exclusions.matches(relative) {
            relative.pop();
            continue;
        }
        let source_path = entry.path();
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| file_error("inspect", &source_path, error))?;
        if metadata.file_type().is_symlink() || is_reparse_point_from_metadata(&metadata) {
            relative.pop();
            continue;
        }
        if metadata.is_dir() {
            total = checked_snapshot_size_add(
                total,
                snapshot_size(&source_path, contents, exclusions, relative)?,
                u64::MAX,
            )?;
        } else if metadata.is_file()
            && (matches!(contents, SnapshotContents::CurrentState)
                || !is_binary_file(&source_path)?)
        {
            total = checked_snapshot_size_add(total, metadata.len(), u64::MAX)?;
        }
        relative.pop();
    }
    Ok(total)
}

fn checked_snapshot_size_add(
    current: u64,
    additional: u64,
    max_bytes: u64,
) -> Result<u64, DisposableWorkspaceError> {
    let total =
        current
            .checked_add(additional)
            .ok_or(DisposableWorkspaceError::SnapshotTooLarge {
                max_bytes,
                required_bytes: u64::MAX,
            })?;
    if total > max_bytes {
        return Err(DisposableWorkspaceError::SnapshotTooLarge {
            max_bytes,
            required_bytes: total,
        });
    }
    Ok(total)
}

#[derive(Debug)]
struct SnapshotExclusions {
    paths: Vec<Vec<String>>,
}

impl SnapshotExclusions {
    fn new(options: &DisposableWorkspaceOptions) -> Result<Self, DisposableWorkspaceError> {
        options
            .excluded_paths
            .iter()
            .map(|path| normalize_exclusion(path))
            .collect::<Result<Vec<_>, _>>()
            .map(|paths| Self { paths })
    }

    fn matches(&self, relative: &[String]) -> bool {
        self.paths
            .iter()
            .any(|excluded| relative.starts_with(excluded))
    }
}

fn normalize_exclusion(path: &Path) -> Result<Vec<String>, DisposableWorkspaceError> {
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>();
    match components {
        Some(components) if !components.is_empty() => Ok(components),
        _ => Err(DisposableWorkspaceError::InvalidExclusion(
            path.to_path_buf(),
        )),
    }
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

fn inventory_directory(
    root: &Path,
    exclusions: &SnapshotExclusions,
    relative: &mut Vec<String>,
    files: &mut BTreeMap<String, WorkspaceFileFingerprint>,
) -> Result<(), DisposableWorkspaceError> {
    let directory = relative
        .iter()
        .fold(root.to_path_buf(), |path, component| path.join(component));
    let mut entries = fs::read_dir(&directory)
        .map_err(|error| file_error("read", &directory, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| file_error("read", &directory, error))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if is_excluded_name(&name) {
            continue;
        }
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| file_error("inspect", &path, error))?;
        if metadata.file_type().is_symlink() || is_reparse_point_from_metadata(&metadata) {
            continue;
        }
        relative.push(name.to_string_lossy().into_owned());
        if exclusions.matches(relative) {
            relative.pop();
            continue;
        }
        if metadata.is_dir() {
            inventory_directory(root, exclusions, relative, files)?;
        } else if metadata.is_file() {
            files.insert(relative.join("/"), fingerprint_file(&path, metadata.len())?);
        }
        relative.pop();
    }
    Ok(())
}

fn fingerprint_file(
    path: &Path,
    byte_len: u64,
) -> Result<WorkspaceFileFingerprint, DisposableWorkspaceError> {
    let mut file = fs::File::open(path).map_err(|error| file_error("read", path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| file_error("read", path, error))?;
        if count == 0 {
            break;
        }
        hasher
            .write_all(&buffer[..count])
            .expect("SHA-256 writes cannot fail");
    }
    Ok(WorkspaceFileFingerprint {
        byte_len,
        sha256: format!("sha256:{:x}", hasher.finalize()),
    })
}

fn compare_inventories(
    baseline: &WorkspaceFileInventory,
    current: &WorkspaceFileInventory,
) -> WorkspaceFileChanges {
    let modified = baseline
        .files
        .iter()
        .filter(|(path, identity)| {
            current
                .files
                .get(*path)
                .is_some_and(|current| current != *identity)
        })
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let mut deleted = baseline
        .files
        .keys()
        .filter(|path| !current.files.contains_key(*path))
        .cloned()
        .collect::<Vec<_>>();
    let mut created = current
        .files
        .keys()
        .filter(|path| !baseline.files.contains_key(*path))
        .cloned()
        .collect::<Vec<_>>();
    let mut renamed = Vec::new();
    let mut remaining_created = BTreeSet::from_iter(created.iter().cloned());
    let mut remaining_deleted = Vec::new();
    for from in &deleted {
        let identity = baseline
            .files
            .get(from)
            .expect("deleted path comes from the baseline");
        let to = remaining_created
            .iter()
            .find(|candidate| current.files.get(*candidate) == Some(identity))
            .cloned();
        if let Some(to) = to {
            remaining_created.remove(&to);
            renamed.push(WorkspaceFileRename {
                from: from.clone(),
                to,
            });
        } else {
            remaining_deleted.push(from.clone());
        }
    }
    created = remaining_created.into_iter().collect();
    deleted = remaining_deleted;
    WorkspaceFileChanges {
        created,
        modified,
        deleted,
        renamed,
    }
}

fn remove_workspace(root: &Path) -> Result<(), DisposableWorkspaceError> {
    validate_owned_workspace_root(root)?;
    if !root.exists() {
        return Ok(());
    }
    make_tree_writable(root)?;
    fs::remove_dir_all(root).map_err(|source| file_error("remove", root, source))
}

fn validate_owned_workspace_root(root: &Path) -> Result<(), DisposableWorkspaceError> {
    let file_name = root.file_name().and_then(|name| name.to_str());
    let owned_uuid = file_name
        .and_then(|name| name.strip_prefix(OWNED_WORKSPACE_PREFIX))
        .and_then(|value| uuid::Uuid::parse_str(value).ok());
    let expected_parent = fs::canonicalize(std::env::temp_dir())
        .map_err(|error| file_error("inspect", &std::env::temp_dir(), error))?;
    let actual_parent = root
        .parent()
        .and_then(|parent| fs::canonicalize(parent).ok());
    if owned_uuid.is_none() || actual_parent.as_deref() != Some(expected_parent.as_path()) {
        return Err(DisposableWorkspaceError::UnownedCleanupRoot(
            root.to_path_buf(),
        ));
    }
    Ok(())
}

#[allow(
    clippy::permissions_set_readonly_false,
    reason = "the baseline is made read-only and cleanup restores writability"
)]
fn make_tree_read_only(root: &Path) -> Result<(), DisposableWorkspaceError> {
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.map_err(|error| {
            file_error("inspect", root, std::io::Error::other(error.to_string()))
        })?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| file_error("inspect", entry.path(), error))?;
        if metadata.is_file() && !metadata.permissions().readonly() {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(true);
            fs::set_permissions(entry.path(), permissions)
                .map_err(|error| file_error("make read-only", entry.path(), error))?;
        }
    }
    Ok(())
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
