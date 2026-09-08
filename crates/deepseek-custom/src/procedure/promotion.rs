//! Promotion targets and the concurrent-edit baseline gate.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    BoundaryValidatedPatch, ProcedureFingerprintError, ProcedurePathFingerprint,
    capture_path_fingerprint, capture_path_fingerprints,
};

/// The file operation represented by one promotion target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionTargetKind {
    Create,
    Update,
    Delete,
    Rename,
}

/// One typed file operation that a verified patch may promote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromotionTarget {
    Create { path: String },
    Update { path: String },
    Delete { path: String },
    Rename { from: String, to: String },
}

impl PromotionTarget {
    pub const fn kind(&self) -> PromotionTargetKind {
        match self {
            Self::Create { .. } => PromotionTargetKind::Create,
            Self::Update { .. } => PromotionTargetKind::Update,
            Self::Delete { .. } => PromotionTargetKind::Delete,
            Self::Rename { .. } => PromotionTargetKind::Rename,
        }
    }

    /// Return every real path whose baseline must be checked.
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Self::Create { path } | Self::Update { path } | Self::Delete { path } => {
                vec![path]
            }
            Self::Rename { from, to } => vec![from, to],
        }
    }
}

/// Failure to model a validated patch's file operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PromotionTargetError {
    #[error("promotion patch section {section} is malformed: {reason}")]
    Malformed { section: usize, reason: String },
}

/// Model every create, update, delete, and rename section in a patch.
pub fn model_promotion_targets(
    patch: &BoundaryValidatedPatch,
) -> Result<Vec<PromotionTarget>, PromotionTargetError> {
    let lines = patch.envelope().unified_diff.lines().collect::<Vec<_>>();
    let starts = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.starts_with("diff --git ").then_some(index))
        .collect::<Vec<_>>();
    if starts.is_empty() {
        return Err(malformed(1, "no diff sections were found"));
    }

    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(lines.len());
            model_section(index + 1, &lines[*start..end])
        })
        .collect()
}

/// A captured preview baseline for every real endpoint in a promotion plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionBaseline {
    fingerprints: Vec<ProcedurePathFingerprint>,
}

impl PromotionBaseline {
    /// Capture the current real-workspace state for every target endpoint.
    pub fn capture(
        project_root: &Path,
        targets: &[PromotionTarget],
    ) -> Result<Self, ProcedureFingerprintError> {
        let paths = targets
            .iter()
            .flat_map(PromotionTarget::paths)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let fingerprints = capture_path_fingerprints(project_root, paths)?;
        Ok(Self { fingerprints })
    }

    /// Build a baseline from previously captured preview fingerprints.
    pub fn from_fingerprints(fingerprints: Vec<ProcedurePathFingerprint>) -> Self {
        Self { fingerprints }
    }

    pub fn fingerprints(&self) -> &[ProcedurePathFingerprint] {
        &self.fingerprints
    }

    /// Compare every baseline endpoint with the current real workspace.
    pub fn compare(
        &self,
        project_root: &Path,
    ) -> Result<PromotionBaselineComparison, ProcedureFingerprintError> {
        let current = capture_path_fingerprints(
            project_root,
            self.fingerprints
                .iter()
                .map(|fingerprint| fingerprint.path.clone()),
        )?;
        let current_by_path = current
            .into_iter()
            .map(|fingerprint| (fingerprint.path.clone(), fingerprint))
            .collect::<BTreeMap<_, _>>();
        let stale_paths = self
            .fingerprints
            .iter()
            .filter_map(|expected| {
                let actual = current_by_path
                    .get(&expected.path)
                    .expect("current fingerprints contain every baseline path");
                (actual != expected).then(|| StalePromotionPath {
                    path: expected.path.clone(),
                    expected: expected.clone(),
                    actual: actual.clone(),
                })
            })
            .collect();
        Ok(PromotionBaselineComparison {
            checked_paths: self
                .fingerprints
                .iter()
                .map(|fingerprint| fingerprint.path.clone())
                .collect(),
            stale_paths,
        })
    }

    /// Refuse the promotion gate when any real endpoint changed.
    pub fn ensure_current(
        &self,
        project_root: &Path,
    ) -> Result<PromotionBaselineComparison, PromotionBaselineCheckError> {
        let comparison = self.compare(project_root)?;
        if !comparison.can_promote() {
            return Err(PromotionBaselineCheckError::Stale {
                stale_paths: comparison.stale_paths.clone(),
            });
        }
        Ok(comparison)
    }
}

/// One real path whose state differs from the preview baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StalePromotionPath {
    pub path: String,
    pub expected: ProcedurePathFingerprint,
    pub actual: ProcedurePathFingerprint,
}

/// Result of the immediate pre-promotion baseline comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionBaselineComparison {
    pub checked_paths: Vec<String>,
    pub stale_paths: Vec<StalePromotionPath>,
}

impl PromotionBaselineComparison {
    pub fn can_promote(&self) -> bool {
        self.stale_paths.is_empty()
    }

    pub fn is_current(&self) -> bool {
        self.can_promote()
    }
}

/// Failure of the pre-promotion baseline gate.
#[derive(Debug, Error)]
pub enum PromotionBaselineCheckError {
    #[error(transparent)]
    Fingerprint(#[from] ProcedureFingerprintError),
    #[error("promotion baseline is stale for paths: {stale_paths:?}")]
    Stale {
        stale_paths: Vec<StalePromotionPath>,
    },
}

#[derive(Clone, Copy)]
enum EndpointSide {
    Old,
    New,
    Rename,
}

fn model_section(section: usize, lines: &[&str]) -> Result<PromotionTarget, PromotionTargetError> {
    let header = lines
        .first()
        .and_then(|line| line.strip_prefix("diff --git "))
        .ok_or_else(|| malformed(section, "missing diff header"))?;
    let (old, new) = parse_diff_header(header).map_err(|reason| malformed(section, reason))?;
    let old_header = matching_header(lines, "--- ", section)?;
    let new_header = matching_header(lines, "+++ ", section)?;
    let old_is_missing = old_header.as_deref().is_some_and(is_dev_null);
    let new_is_missing = new_header.as_deref().is_some_and(is_dev_null);
    let rename_from = matching_header(lines, "rename from ", section)?
        .map(|value| required_endpoint(&value, EndpointSide::Rename))
        .transpose()
        .map_err(|reason| malformed(section, reason))?;
    let rename_to = matching_header(lines, "rename to ", section)?
        .map(|value| required_endpoint(&value, EndpointSide::Rename))
        .transpose()
        .map_err(|reason| malformed(section, reason))?;

    if rename_from.is_some() != rename_to.is_some() {
        return Err(malformed(
            section,
            "rename source and destination must be paired",
        ));
    }
    if let (Some(from), Some(to)) = (rename_from, rename_to) {
        if old.as_deref() != Some(from.as_str()) || new.as_deref() != Some(to.as_str()) {
            return Err(malformed(
                section,
                "rename metadata does not match the diff endpoints",
            ));
        }
        return Ok(PromotionTarget::Rename { from, to });
    }

    match (old_is_missing, new_is_missing, old, new) {
        (true, false, _, Some(path)) => Ok(PromotionTarget::Create { path }),
        (false, true, Some(path), _) => Ok(PromotionTarget::Delete { path }),
        (false, false, Some(old), Some(new)) if old == new => {
            Ok(PromotionTarget::Update { path: new })
        }
        _ => Err(malformed(
            section,
            "file endpoints do not describe a create, update, delete, or rename",
        )),
    }
}

/// The result of a successful verified promotion transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionResult {
    pub baseline: PromotionBaselineComparison,
    pub final_fingerprints: Vec<ProcedurePathFingerprint>,
    pub cleanup: PromotionCleanupEvidence,
}

/// Post-commit cleanup evidence returned without changing promotion success.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromotionCleanupEvidence {
    pub errors: Vec<String>,
    pub retained_paths: Vec<PathBuf>,
}

impl PromotionCleanupEvidence {
    pub fn completed(&self) -> bool {
        self.errors.is_empty() && self.retained_paths.is_empty()
    }

    pub fn retained_recovery_data(&self) -> bool {
        !self.retained_paths.is_empty()
    }
}

/// Evidence describing whether a failed promotion was rolled back completely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionRecoveryEvidence {
    pub rollback_errors: Vec<String>,
    pub recovery_paths: Vec<PathBuf>,
}

impl PromotionRecoveryEvidence {
    pub fn rollback_succeeded(&self) -> bool {
        self.rollback_errors.is_empty() && self.recovery_paths.is_empty()
    }

    pub fn requires_recovery(&self) -> bool {
        !self.recovery_paths.is_empty()
    }
}

#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromotionFailureInjection {
    /// Fail before installing the zero-based staged-file index.
    pub fail_install_at: Option<usize>,
    /// Fail before the zero-based rollback operation.
    pub fail_rollback_at: Option<usize>,
    /// Edit the endpoint at this zero-based first-destructive-check index.
    pub edit_endpoint_at: Option<usize>,
    /// Retain the backup at this zero-based post-commit cleanup index.
    pub fail_cleanup_at: Option<usize>,
}

/// Failure while installing verified endpoint results.
#[derive(Debug, Error)]
pub enum PromotionError {
    #[error(transparent)]
    Baseline(#[from] PromotionBaselineCheckError),
    #[error("promotion target set is invalid: {reason}")]
    InvalidTargets { reason: String },
    #[error("verified promotion result for {path} is invalid: {reason}")]
    InvalidVerifiedResult { path: String, reason: String },
    #[error("could not fingerprint the verified promotion result: {source}")]
    VerifiedFingerprint {
        #[source]
        source: ProcedureFingerprintError,
    },
    #[error("could not {action} promotion path {path}: {source}; recovery: {recovery:?}")]
    Transaction {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
        recovery: PromotionRecoveryEvidence,
    },
    #[error("could not verify final promotion hashes: {source}; recovery: {recovery:?}")]
    FinalFingerprint {
        #[source]
        source: ProcedureFingerprintError,
        recovery: PromotionRecoveryEvidence,
    },
    #[error(
        "verified promotion result differs from the installed files: {stale_paths:?}; recovery: {recovery:?}"
    )]
    FinalMismatch {
        stale_paths: Vec<StalePromotionPath>,
        recovery: PromotionRecoveryEvidence,
    },
    #[error("promotion endpoint became stale: {stale_paths:?}; recovery: {recovery:?}")]
    ConcurrentEdit {
        stale_paths: Vec<StalePromotionPath>,
        recovery: PromotionRecoveryEvidence,
    },
    #[error("could not recheck promotion endpoint {path}: {source}; recovery: {recovery:?}")]
    EndpointFingerprint {
        path: String,
        #[source]
        source: ProcedureFingerprintError,
        recovery: PromotionRecoveryEvidence,
    },
}

impl PromotionError {
    /// Return durable recovery evidence for failures that touched
    /// transaction paths.
    pub fn recovery(&self) -> Option<PromotionRecoveryEvidence> {
        match self {
            Self::Transaction { recovery, .. }
            | Self::FinalFingerprint { recovery, .. }
            | Self::FinalMismatch { recovery, .. }
            | Self::ConcurrentEdit { recovery, .. } => Some(recovery.clone()),
            Self::EndpointFingerprint { recovery: r, .. } => Some(r.clone()),
            Self::Baseline(_)
            | Self::InvalidTargets { .. }
            | Self::InvalidVerifiedResult { .. }
            | Self::VerifiedFingerprint { .. } => None,
        }
    }
}

/// Install all verified endpoint results after the concurrent-edit gate passes.
///
/// Every file result is staged beside its real target before existing targets
/// move to sibling backups. Installed files are fingerprinted before backups
/// are removed. Any installation or final-hash failure attempts to restore the
/// original targets and retains recovery paths when that rollback is incomplete.
pub fn promote_verified_workspace(
    project_root: &Path,
    verified_workspace: &Path,
    baseline: &PromotionBaseline,
    targets: &[PromotionTarget],
) -> Result<PromotionResult, PromotionError> {
    promote_verified_workspace_inner(
        project_root,
        verified_workspace,
        baseline,
        targets,
        FailureInjection::default(),
    )
}

#[cfg(feature = "test-support")]
pub fn promote_verified_workspace_with_failure_injection(
    project_root: &Path,
    verified_workspace: &Path,
    baseline: &PromotionBaseline,
    targets: &[PromotionTarget],
    injection: PromotionFailureInjection,
) -> Result<PromotionResult, PromotionError> {
    promote_verified_workspace_inner(
        project_root,
        verified_workspace,
        baseline,
        targets,
        FailureInjection {
            fail_install_at: injection.fail_install_at,
            fail_rollback_at: injection.fail_rollback_at,
            edit_endpoint_at: injection.edit_endpoint_at,
            fail_cleanup_at: injection.fail_cleanup_at,
        },
    )
}

#[derive(Debug, Clone, Copy, Default)]
struct FailureInjection {
    fail_install_at: Option<usize>,
    fail_rollback_at: Option<usize>,
    #[cfg(feature = "test-support")]
    edit_endpoint_at: Option<usize>,
    fail_cleanup_at: Option<usize>,
}

fn promote_verified_workspace_inner(
    project_root: &Path,
    verified_workspace: &Path,
    baseline: &PromotionBaseline,
    targets: &[PromotionTarget],
    injection: FailureInjection,
) -> Result<PromotionResult, PromotionError> {
    let paths = promotion_paths(targets)?;
    validate_baseline_paths(baseline, &paths)?;
    let baseline_comparison = baseline.ensure_current(project_root)?;
    let verified = capture_path_fingerprints(verified_workspace, paths.iter().cloned())
        .map_err(|source| PromotionError::VerifiedFingerprint { source })?;
    validate_verified_results(targets, &verified)?;

    let transaction_id = uuid::Uuid::new_v4().to_string();
    let mut staged_paths = Vec::new();
    let prepared = match prepare_targets(
        project_root,
        verified_workspace,
        targets,
        &transaction_id,
        &mut staged_paths,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = remove_paths(&staged_paths);
            return Err(error);
        }
    };

    let mut backups = Vec::new();
    let mut touched_paths = BTreeSet::new();
    let mut endpoint_check_index = 0;
    for fingerprint in baseline
        .fingerprints()
        .iter()
        .filter(|fingerprint| fingerprint.state == super::ProcedurePathState::Present)
    {
        let original = project_root.join(&fingerprint.path);
        check_endpoint_before_destructive_operation(
            project_root,
            baseline,
            &fingerprint.path,
            &touched_paths,
            &mut endpoint_check_index,
            &[],
            &backups,
            &staged_paths,
            injection,
        )?;
        let backup = sibling_path(&original, "backup", &transaction_id);
        if let Err(source) = fs::rename(&original, &backup) {
            return Err(transaction_failure(
                "back up",
                original,
                source,
                &[],
                &backups,
                &staged_paths,
                injection,
            ));
        }
        backups.push(Backup { original, backup });
        touched_paths.insert(fingerprint.path.clone());
    }

    let mut installed_paths = Vec::new();
    let mut install_index = 0;
    for target in &prepared {
        let (action, path, staged) = match target {
            PreparedTarget::Install { path, staged } => ("install", path, Some(staged)),
            PreparedTarget::Delete { path } => ("delete", path, None),
            PreparedTarget::Rename { to, staged } => ("install rename", to, Some(staged)),
        };
        let Some(staged) = staged else {
            continue;
        };
        let relative = path
            .strip_prefix(project_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if !touched_paths.contains(&relative) {
            check_endpoint_before_destructive_operation(
                project_root,
                baseline,
                &relative,
                &touched_paths,
                &mut endpoint_check_index,
                &installed_paths,
                &backups,
                &staged_paths,
                injection,
            )?;
        }
        if injection.fail_install_at == Some(install_index) {
            return Err(transaction_failure(
                action,
                path.clone(),
                std::io::Error::other(format!(
                    "injected promotion install failure at index {install_index}"
                )),
                &installed_paths,
                &backups,
                &staged_paths,
                injection,
            ));
        }
        if let Err(source) = fs::rename(staged, path) {
            return Err(transaction_failure(
                action,
                path.clone(),
                source,
                &installed_paths,
                &backups,
                &staged_paths,
                injection,
            ));
        }
        installed_paths.push(path.clone());
        touched_paths.insert(relative);
        install_index += 1;
    }

    let final_fingerprints = match capture_path_fingerprints(project_root, paths.iter().cloned()) {
        Ok(fingerprints) => fingerprints,
        Err(source) => {
            return Err(final_fingerprint_failure(
                source,
                &installed_paths,
                &backups,
                &staged_paths,
                injection,
            ));
        }
    };
    let stale_paths = mismatched_fingerprints(&verified, &final_fingerprints);
    if !stale_paths.is_empty() {
        return Err(final_mismatch_failure(
            stale_paths,
            &installed_paths,
            &backups,
            &staged_paths,
            injection,
        ));
    }

    let cleanup = cleanup_after_commit(&backups, &staged_paths, injection);

    Ok(PromotionResult {
        baseline: baseline_comparison,
        final_fingerprints,
        cleanup,
    })
}

fn cleanup_after_commit(
    backups: &[Backup],
    staged_paths: &[PathBuf],
    injection: FailureInjection,
) -> PromotionCleanupEvidence {
    let mut errors = Vec::new();
    for (index, backup) in backups.iter().enumerate() {
        if injection.fail_cleanup_at == Some(index) {
            errors.push(format!(
                "injected post-commit cleanup failure for {}",
                backup.backup.display()
            ));
            continue;
        }
        if backup.backup.exists()
            && let Err(source) = fs::remove_file(&backup.backup)
        {
            errors.push(format!("remove {}: {source}", backup.backup.display()));
        }
    }
    errors.extend(remove_paths(staged_paths));
    let retained_paths = backups
        .iter()
        .map(|backup| backup.backup.clone())
        .chain(staged_paths.iter().cloned())
        .filter(|path| path.exists())
        .collect();
    PromotionCleanupEvidence {
        errors,
        retained_paths,
    }
}

#[allow(clippy::too_many_arguments)]
fn check_endpoint_before_destructive_operation(
    project_root: &Path,
    baseline: &PromotionBaseline,
    relative: &str,
    touched_paths: &BTreeSet<String>,
    endpoint_check_index: &mut usize,
    installed_paths: &[PathBuf],
    backups: &[Backup],
    staged_paths: &[PathBuf],
    injection: FailureInjection,
) -> Result<(), PromotionError> {
    #[cfg(feature = "test-support")]
    if injection.edit_endpoint_at == Some(*endpoint_check_index) {
        let path = project_root.join(relative);
        if let Some(parent) = path.parent()
            && let Err(source) = fs::create_dir_all(parent)
        {
            return Err(transaction_failure(
                "inject a concurrent edit for",
                path,
                source,
                installed_paths,
                backups,
                staged_paths,
                injection,
            ));
        }
        if let Err(source) = fs::write(
            &path,
            b"injected concurrent edit after initial promotion validation\n",
        ) {
            return Err(transaction_failure(
                "inject a concurrent edit for",
                path,
                source,
                installed_paths,
                backups,
                staged_paths,
                injection,
            ));
        }
    }
    *endpoint_check_index += 1;

    let expected = baseline
        .fingerprints()
        .iter()
        .find(|fingerprint| fingerprint.path == relative)
        .expect("validated baseline contains every promotion endpoint");
    let actual = capture_path_fingerprint(project_root, relative).map_err(|source| {
        PromotionError::EndpointFingerprint {
            path: relative.to_string(),
            source,
            recovery: rollback_and_collect(installed_paths, backups, staged_paths, injection),
        }
    })?;
    if actual == *expected {
        return Ok(());
    }

    let stale_paths = known_stale_untouched_paths(project_root, baseline, touched_paths);
    let recovery = rollback_and_collect(installed_paths, backups, staged_paths, injection);
    Err(PromotionError::ConcurrentEdit {
        stale_paths,
        recovery,
    })
}

fn known_stale_untouched_paths(
    project_root: &Path,
    baseline: &PromotionBaseline,
    touched_paths: &BTreeSet<String>,
) -> Vec<StalePromotionPath> {
    baseline
        .fingerprints()
        .iter()
        .filter(|expected| !touched_paths.contains(&expected.path))
        .filter_map(|expected| {
            let actual = capture_path_fingerprint(project_root, &expected.path).ok()?;
            (actual != *expected).then(|| StalePromotionPath {
                path: expected.path.clone(),
                expected: expected.clone(),
                actual,
            })
        })
        .collect()
}

#[derive(Debug)]
enum PreparedTarget {
    Install { path: PathBuf, staged: PathBuf },
    Delete { path: PathBuf },
    Rename { to: PathBuf, staged: PathBuf },
}

#[derive(Debug)]
struct Backup {
    original: PathBuf,
    backup: PathBuf,
}

fn promotion_paths(targets: &[PromotionTarget]) -> Result<Vec<String>, PromotionError> {
    let mut paths = BTreeSet::new();
    for path in targets.iter().flat_map(PromotionTarget::paths) {
        if !paths.insert(path.to_string()) {
            return Err(PromotionError::InvalidTargets {
                reason: format!("endpoint appears more than once: {path}"),
            });
        }
    }
    Ok(paths.into_iter().collect())
}

fn validate_baseline_paths(
    baseline: &PromotionBaseline,
    paths: &[String],
) -> Result<(), PromotionError> {
    let baseline_paths = baseline
        .fingerprints()
        .iter()
        .map(|fingerprint| fingerprint.path.clone())
        .collect::<BTreeSet<_>>();
    let target_paths = paths.iter().cloned().collect::<BTreeSet<_>>();
    if baseline_paths != target_paths {
        return Err(PromotionError::InvalidTargets {
            reason: "baseline endpoints do not match promotion targets".to_string(),
        });
    }
    Ok(())
}

fn validate_verified_results(
    targets: &[PromotionTarget],
    verified: &[ProcedurePathFingerprint],
) -> Result<(), PromotionError> {
    let by_path = verified
        .iter()
        .map(|fingerprint| (fingerprint.path.as_str(), fingerprint))
        .collect::<BTreeMap<_, _>>();
    for target in targets {
        match target {
            PromotionTarget::Create { path } | PromotionTarget::Update { path } => {
                require_state(&by_path, path, super::ProcedurePathState::Present)?;
            }
            PromotionTarget::Delete { path } => {
                require_state(&by_path, path, super::ProcedurePathState::Missing)?;
            }
            PromotionTarget::Rename { from, to } => {
                require_state(&by_path, from, super::ProcedurePathState::Missing)?;
                require_state(&by_path, to, super::ProcedurePathState::Present)?;
            }
        }
    }
    Ok(())
}

fn require_state(
    fingerprints: &BTreeMap<&str, &ProcedurePathFingerprint>,
    path: &str,
    expected: super::ProcedurePathState,
) -> Result<(), PromotionError> {
    let actual = fingerprints
        .get(path)
        .map(|fingerprint| fingerprint.state)
        .ok_or_else(|| PromotionError::InvalidVerifiedResult {
            path: path.to_string(),
            reason: "endpoint fingerprint is missing".to_string(),
        })?;
    if actual != expected {
        return Err(PromotionError::InvalidVerifiedResult {
            path: path.to_string(),
            reason: format!("expected {expected:?}, found {actual:?}"),
        });
    }
    Ok(())
}

fn prepare_targets(
    project_root: &Path,
    verified_workspace: &Path,
    targets: &[PromotionTarget],
    transaction_id: &str,
    staged_paths: &mut Vec<PathBuf>,
) -> Result<Vec<PreparedTarget>, PromotionError> {
    targets
        .iter()
        .map(|target| match target {
            PromotionTarget::Create { path } | PromotionTarget::Update { path } => {
                let path = project_root.join(path);
                let staged = stage_file(
                    verified_workspace,
                    path.strip_prefix(project_root).unwrap_or(&path),
                    &path,
                    transaction_id,
                    staged_paths,
                )?;
                Ok(PreparedTarget::Install { path, staged })
            }
            PromotionTarget::Delete { path } => Ok(PreparedTarget::Delete {
                path: project_root.join(path),
            }),
            PromotionTarget::Rename { to, .. } => {
                let to = project_root.join(to);
                let staged = stage_file(
                    verified_workspace,
                    to.strip_prefix(project_root).unwrap_or(&to),
                    &to,
                    transaction_id,
                    staged_paths,
                )?;
                Ok(PreparedTarget::Rename { to, staged })
            }
        })
        .collect()
}

fn stage_file(
    verified_workspace: &Path,
    relative: &Path,
    target: &Path,
    transaction_id: &str,
    staged_paths: &mut Vec<PathBuf>,
) -> Result<PathBuf, PromotionError> {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| PromotionError::InvalidTargets {
            reason: format!("target has no valid file name: {}", target.display()),
        })?;
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| PromotionError::Transaction {
        action: "create staging directory for",
        path: parent.to_path_buf(),
        source,
        recovery: PromotionRecoveryEvidence {
            rollback_errors: Vec::new(),
            recovery_paths: Vec::new(),
        },
    })?;
    let staged = parent.join(format!(
        ".{file_name}.deepseek-promotion-stage-{transaction_id}"
    ));
    staged_paths.push(staged.clone());
    let source = verified_workspace.join(relative);
    fs::copy(&source, &staged).map_err(|source| PromotionError::Transaction {
        action: "stage",
        path: target.to_path_buf(),
        source,
        recovery: PromotionRecoveryEvidence {
            rollback_errors: Vec::new(),
            recovery_paths: vec![staged.clone()],
        },
    })?;
    Ok(staged)
}

fn sibling_path(path: &Path, kind: &str, transaction_id: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("target");
    path.with_file_name(format!(
        ".{name}.deepseek-promotion-{kind}-{transaction_id}"
    ))
}

fn transaction_failure(
    action: &'static str,
    path: PathBuf,
    source: std::io::Error,
    installed_paths: &[PathBuf],
    backups: &[Backup],
    staged_paths: &[PathBuf],
    injection: FailureInjection,
) -> PromotionError {
    let recovery = rollback_and_collect(installed_paths, backups, staged_paths, injection);
    PromotionError::Transaction {
        action,
        path,
        source,
        recovery,
    }
}

fn final_fingerprint_failure(
    source: ProcedureFingerprintError,
    installed_paths: &[PathBuf],
    backups: &[Backup],
    staged_paths: &[PathBuf],
    injection: FailureInjection,
) -> PromotionError {
    let recovery = rollback_and_collect(installed_paths, backups, staged_paths, injection);
    PromotionError::FinalFingerprint { source, recovery }
}

fn final_mismatch_failure(
    stale_paths: Vec<StalePromotionPath>,
    installed_paths: &[PathBuf],
    backups: &[Backup],
    staged_paths: &[PathBuf],
    injection: FailureInjection,
) -> PromotionError {
    let recovery = rollback_and_collect(installed_paths, backups, staged_paths, injection);
    PromotionError::FinalMismatch {
        stale_paths,
        recovery,
    }
}

fn rollback_and_collect(
    installed_paths: &[PathBuf],
    backups: &[Backup],
    staged_paths: &[PathBuf],
    injection: FailureInjection,
) -> PromotionRecoveryEvidence {
    let rollback_errors = rollback(installed_paths, backups, staged_paths, injection);
    let recovery_paths = recovery_paths(backups, staged_paths);
    PromotionRecoveryEvidence {
        rollback_errors,
        recovery_paths,
    }
}

fn rollback(
    installed_paths: &[PathBuf],
    backups: &[Backup],
    staged_paths: &[PathBuf],
    injection: FailureInjection,
) -> Vec<String> {
    let mut errors = Vec::new();
    let mut rollback_index = 0;
    for path in installed_paths.iter().rev() {
        if injection.fail_rollback_at == Some(rollback_index) {
            errors.push(format!(
                "injected rollback failure before removing {}",
                path.display()
            ));
            rollback_index += 1;
            continue;
        }
        if path.exists()
            && let Err(source) = fs::remove_file(path)
        {
            errors.push(format!("remove {}: {source}", path.display()));
        }
        rollback_index += 1;
    }
    for backup in backups.iter().rev() {
        if injection.fail_rollback_at == Some(rollback_index) {
            errors.push(format!(
                "injected rollback failure before restoring {}",
                backup.original.display()
            ));
            rollback_index += 1;
            continue;
        }
        if backup.backup.exists() {
            if backup.original.exists()
                && let Err(source) = fs::remove_file(&backup.original)
            {
                errors.push(format!("remove {}: {source}", backup.original.display()));
                continue;
            }
            if let Err(source) = fs::rename(&backup.backup, &backup.original) {
                errors.push(format!("restore {}: {source}", backup.original.display()));
            }
        }
        rollback_index += 1;
    }
    errors.extend(remove_paths(staged_paths));
    errors
}

fn remove_paths(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| path.exists())
        .filter_map(|path| {
            fs::remove_file(path)
                .err()
                .map(|source| format!("remove {}: {source}", path.display()))
        })
        .collect()
}

fn recovery_paths(backups: &[Backup], staged_paths: &[PathBuf]) -> Vec<PathBuf> {
    backups
        .iter()
        .map(|backup| backup.backup.clone())
        .chain(staged_paths.iter().cloned())
        .filter(|path| path.exists())
        .collect()
}

fn mismatched_fingerprints(
    expected: &[ProcedurePathFingerprint],
    actual: &[ProcedurePathFingerprint],
) -> Vec<StalePromotionPath> {
    let actual_by_path = actual
        .iter()
        .map(|fingerprint| (fingerprint.path.as_str(), fingerprint))
        .collect::<BTreeMap<_, _>>();
    expected
        .iter()
        .filter_map(|expected| {
            let actual = actual_by_path
                .get(expected.path.as_str())
                .expect("final fingerprints contain every expected path");
            (actual != &expected).then(|| StalePromotionPath {
                path: expected.path.clone(),
                expected: expected.clone(),
                actual: (*actual).clone(),
            })
        })
        .collect()
}

fn matching_header(
    lines: &[&str],
    prefix: &str,
    section: usize,
) -> Result<Option<String>, PromotionTargetError> {
    let matches = lines
        .iter()
        .filter_map(|line| line.strip_prefix(prefix))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [value] => Ok(Some(value.trim().to_owned())),
        _ => Err(malformed(
            section,
            format!("found more than one `{prefix}` header"),
        )),
    }
}

fn parse_diff_header(header: &str) -> Result<(Option<String>, Option<String>), String> {
    let (old, rest) = parse_path_token(header)?;
    let (new, trailing) = parse_path_token(rest.trim_start())?;
    if !trailing.trim().is_empty() {
        return Err("unexpected content after the new endpoint".to_string());
    }
    Ok((
        normalize_endpoint(&old, EndpointSide::Old, true)?,
        normalize_endpoint(&new, EndpointSide::New, true)?,
    ))
}

fn parse_path_token(input: &str) -> Result<(String, &str), String> {
    if let Some(quoted) = input.strip_prefix('"') {
        let mut value = String::new();
        let mut escaped = false;
        for (index, character) in quoted.char_indices() {
            if escaped {
                let decoded = match character {
                    '"' => '"',
                    '\\' => '\\',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err(format!("unsupported quoted-path escape `\\{character}`")),
                };
                value.push(decoded);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                return Ok((value, &quoted[index + character.len_utf8()..]));
            } else {
                value.push(character);
            }
        }
        return Err("unterminated quoted path".to_string());
    }

    let input = input.trim_start();
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    if end == 0 {
        return Err("path endpoint is empty".to_string());
    }
    Ok((input[..end].to_string(), &input[end..]))
}

fn normalize_endpoint(
    raw: &str,
    side: EndpointSide,
    allow_dev_null: bool,
) -> Result<Option<String>, String> {
    let (parsed, trailing) = parse_path_token(raw.trim())?;
    if !trailing.trim().is_empty() {
        return Err("unexpected content after the path endpoint".to_string());
    }
    let replaced = parsed.replace('\\', "/");
    if allow_dev_null && replaced == "/dev/null" {
        return Ok(None);
    }
    let stripped = match side {
        EndpointSide::Old => replaced.strip_prefix("a/").unwrap_or(&replaced),
        EndpointSide::New => replaced.strip_prefix("b/").unwrap_or(&replaced),
        EndpointSide::Rename => &replaced,
    };
    if stripped.is_empty() || stripped.starts_with('/') || stripped.starts_with("//") {
        return Err(format!("invalid repository path `{parsed}`"));
    }
    let parts = stripped.split('/').collect::<Vec<_>>();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(format!("repository path is not normalized: `{parsed}`"));
    }
    Ok(Some(parts.join("/")))
}

fn required_endpoint(raw: &str, side: EndpointSide) -> Result<String, String> {
    normalize_endpoint(raw, side, false)?.ok_or_else(|| "endpoint cannot be /dev/null".to_string())
}

fn is_dev_null(raw: &str) -> bool {
    raw.trim().trim_matches('"').replace('\\', "/") == "/dev/null"
}

fn malformed(section: usize, reason: impl Into<String>) -> PromotionTargetError {
    PromotionTargetError::Malformed {
        section,
        reason: reason.into(),
    }
}
