//! Pre-dispatch validation for one explicitly named localization run.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use super::{
    ContractSelection, OpenSpecInput, ProcedureReportRepository, ProcedureReviewDisposition,
    ProcedureRun, ProcedureRunId, RouteOverride, StoredProcedureReport, ValidatedContractInput,
    capture_path_fingerprints, require_approved_report, sha256_json,
};
use crate::error::HarnessError;

/// The exact localization run and OpenSpec task requested for one preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchPreviewInputRequest {
    pub localization_run_id: ProcedureRunId,
    pub change_id: String,
    pub task_id: String,
    pub route_override: RouteOverride,
}

/// Current, approved input that a route evaluator may consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPatchPreviewInput {
    pub report: ProcedureRun,
    pub contract: ValidatedContractInput,
    pub route_override: RouteOverride,
}

/// Refusal from the preview gate before routing, workspace creation, or dispatch.
#[derive(Debug, PartialEq, Eq)]
pub enum PatchPreviewInputError {
    MissingReport {
        run_id: String,
    },
    ReportLoad {
        run_id: String,
        reason: String,
    },
    ReviewDisposition {
        run_id: String,
        disposition: ProcedureReviewDisposition,
    },
    ChangeMismatch {
        run_id: String,
        requested: String,
        actual: String,
    },
    TaskMismatch {
        run_id: String,
        requested: String,
        actual: String,
    },
    CurrentOpenSpec {
        run_id: String,
        reason: String,
    },
    Stale {
        run_id: String,
        paths: Vec<String>,
    },
}

impl fmt::Display for PatchPreviewInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingReport { run_id } => write!(
                formatter,
                "patch preview rejected for localization run {run_id}: report is missing"
            ),
            Self::ReportLoad { run_id, reason } => write!(
                formatter,
                "patch preview rejected for localization run {run_id}: could not load report: {reason}"
            ),
            Self::ReviewDisposition {
                run_id,
                disposition,
            } => write!(
                formatter,
                "patch preview rejected for localization run {run_id}: review disposition is {disposition}"
            ),
            Self::ChangeMismatch {
                run_id,
                requested,
                actual,
            } => write!(
                formatter,
                "patch preview rejected for localization run {run_id}: change mismatch; requested `{requested}`, report belongs to `{actual}`"
            ),
            Self::TaskMismatch {
                run_id,
                requested,
                actual,
            } => write!(
                formatter,
                "patch preview rejected for localization run {run_id}: task mismatch; requested `{requested}`, report belongs to `{actual}`"
            ),
            Self::CurrentOpenSpec { run_id, reason } => write!(
                formatter,
                "patch preview rejected for localization run {run_id}: could not load current OpenSpec input: {reason}"
            ),
            Self::Stale { run_id, paths } => {
                write!(
                    formatter,
                    "patch preview rejected for localization run {run_id}: localization input is stale:"
                )?;
                for path in paths {
                    write!(formatter, "\n- {path}")?;
                }
                write!(
                    formatter,
                    "\nrun localization again before previewing a patch"
                )
            }
        }
    }
}

impl std::error::Error for PatchPreviewInputError {}

/// Loads and validates preview input without owning any downstream seam.
pub struct PatchPreviewInputGate {
    input: OpenSpecInput,
    project_root: PathBuf,
    reports: ProcedureReportRepository,
}

impl PatchPreviewInputGate {
    pub fn new(
        input: OpenSpecInput,
        project_root: PathBuf,
        reports: ProcedureReportRepository,
    ) -> Self {
        Self {
            input,
            project_root,
            reports,
        }
    }

    /// Return current approved input. No route, workspace, or model seam is
    /// reachable until this function returns successfully.
    pub fn load(
        &self,
        request: &PatchPreviewInputRequest,
    ) -> Result<ValidatedPatchPreviewInput, PatchPreviewInputError> {
        let run_id = request.localization_run_id.as_str();
        let stored = self.load_named_report(request, &run_id)?;
        require_approved_report(&stored.run).map_err(|error| {
            PatchPreviewInputError::ReviewDisposition {
                run_id: error.run_id,
                disposition: error.disposition,
            }
        })?;
        if stored.run.change_id != request.change_id {
            return Err(PatchPreviewInputError::ChangeMismatch {
                run_id,
                requested: request.change_id.clone(),
                actual: stored.run.change_id,
            });
        }
        if stored.run.selected_task.id != request.task_id {
            return Err(PatchPreviewInputError::TaskMismatch {
                run_id,
                requested: request.task_id.clone(),
                actual: stored.run.selected_task.id,
            });
        }

        let contract = self
            .input
            .validate_and_select_task(&request.change_id, &request.task_id)
            .map_err(|error| PatchPreviewInputError::CurrentOpenSpec {
                run_id: run_id.clone(),
                reason: error.to_string(),
            })?;
        let stale_paths = self.stale_paths(&stored, &contract);
        if !stale_paths.is_empty() {
            return Err(PatchPreviewInputError::Stale {
                run_id,
                paths: stale_paths,
            });
        }

        Ok(ValidatedPatchPreviewInput {
            report: stored.run,
            contract,
            route_override: request.route_override,
        })
    }

    fn load_named_report(
        &self,
        request: &PatchPreviewInputRequest,
        run_id: &str,
    ) -> Result<StoredProcedureReport, PatchPreviewInputError> {
        self.reports
            .load_with_fingerprints(&request.localization_run_id)
            .map_err(|error| match &error {
                HarnessError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    PatchPreviewInputError::MissingReport {
                        run_id: run_id.to_string(),
                    }
                }
                _ => PatchPreviewInputError::ReportLoad {
                    run_id: run_id.to_string(),
                    reason: error.to_string(),
                },
            })
    }

    fn stale_paths(
        &self,
        stored: &StoredProcedureReport,
        current: &ValidatedContractInput,
    ) -> Vec<String> {
        let mut stale = BTreeSet::new();
        let expected_openspec = openspec_paths(&current.contract);
        let current_spec = sha256_json(&current.contract).ok();
        let fingerprints_are_current = stored
            .run
            .spec_fingerprint
            .as_deref()
            .is_some_and(|value| value.starts_with("sha256:"));
        if !fingerprints_are_current || stored.input_fingerprints.openspec.is_empty() {
            stale.extend(expected_openspec.iter().cloned());
        }
        let stale_before_artifact_comparison = stale.len();
        compare_paths(
            &self.project_root,
            &expected_openspec,
            &stored.input_fingerprints.openspec,
            &mut stale,
        );
        if stored.run.spec_fingerprint != current_spec
            && stale.len() == stale_before_artifact_comparison
        {
            stale.extend(expected_openspec.iter().cloned());
        }

        let target_paths = localized_paths(&stored.run);
        compare_paths(
            &self.project_root,
            &target_paths,
            &stored.input_fingerprints.targets,
            &mut stale,
        );
        stale.into_iter().collect()
    }
}

fn compare_paths(
    project_root: &Path,
    expected_paths: &[String],
    stored: &[super::ProcedurePathFingerprint],
    stale: &mut BTreeSet<String>,
) {
    let stored_by_path = stored
        .iter()
        .map(|fingerprint| (fingerprint.path.clone(), fingerprint))
        .collect::<BTreeMap<_, _>>();
    let current = capture_path_fingerprints(project_root, expected_paths.iter().cloned());
    let Ok(current) = current else {
        stale.extend(expected_paths.iter().cloned());
        return;
    };
    let current_by_path = current
        .iter()
        .map(|fingerprint| (fingerprint.path.clone(), fingerprint))
        .collect::<BTreeMap<_, _>>();

    for path in expected_paths {
        if stored_by_path.get(path) != current_by_path.get(path) {
            stale.insert(path.clone());
        }
    }
    for path in stored_by_path.keys() {
        if !current_by_path.contains_key(path) {
            stale.insert(path.clone());
        }
    }
}

fn localized_paths(report: &ProcedureRun) -> Vec<String> {
    let mut paths = report
        .attempts
        .iter()
        .rev()
        .find(|attempt| attempt.disposition == super::ProcedureAttemptDisposition::Accepted)
        .map(|attempt| {
            attempt
                .targets
                .iter()
                .map(|target| target.path.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    paths.sort();
    paths.dedup();
    paths
}

fn openspec_paths(contract: &super::SelectedContractSlice) -> Vec<String> {
    let prefix = format!("openspec/changes/{}", contract.change_id);
    let mut paths = vec![
        format!("{prefix}/proposal.md"),
        format!("{prefix}/tasks.md"),
    ];
    match &contract.selection {
        ContractSelection::Bound { capability, .. } => {
            paths.push(format!("{prefix}/specs/{capability}/spec.md"));
        }
        ContractSelection::Unbound { capability_delta } => {
            paths.push(format!(
                "{prefix}/specs/{}/spec.md",
                capability_delta.capability
            ));
        }
    }
    paths.sort();
    paths.dedup();
    paths
}
