use std::collections::BTreeSet;
use std::path::Path;

use crate::procedure::{
    ProcedurePathFingerprint, ProcedurePathState, PromotionBaseline, PromotionTarget,
    capture_path_fingerprint, capture_path_fingerprints,
};

use super::WorkCard;

pub const PROJECT_STATE_PATH: &str = "PROJECT_STATE.md";

pub fn capture_card_baseline(
    project_root: &Path,
    card: &WorkCard,
) -> Result<PromotionBaseline, String> {
    let paths = card
        .production_paths
        .iter()
        .chain(&card.supporting_paths)
        .cloned()
        .chain(std::iter::once(PROJECT_STATE_PATH.to_string()));
    capture_path_fingerprints(project_root, paths)
        .map(PromotionBaseline::from_fingerprints)
        .map_err(|error| error.to_string())
}

pub fn build_promotion_plan(
    execution_root: &Path,
    baseline: &PromotionBaseline,
    changed_paths: &[String],
) -> Result<(PromotionBaseline, Vec<PromotionTarget>), String> {
    let mut paths = changed_paths.iter().cloned().collect::<BTreeSet<_>>();
    let project_state = baseline_fingerprint(baseline, PROJECT_STATE_PATH)?;
    let execution_project_state = capture_path_fingerprint(execution_root, PROJECT_STATE_PATH)
        .map_err(|error| error.to_string())?;
    if execution_project_state.state == ProcedurePathState::Present
        && execution_project_state != *project_state
    {
        paths.insert(PROJECT_STATE_PATH.to_string());
    }

    let mut selected = Vec::with_capacity(paths.len());
    let mut targets = Vec::with_capacity(paths.len());
    for path in paths {
        let before = baseline_fingerprint(baseline, &path)?.clone();
        let after =
            capture_path_fingerprint(execution_root, &path).map_err(|error| error.to_string())?;
        targets.push(target_for_states(&path, before.state, after.state)?);
        selected.push(before);
    }
    Ok((PromotionBaseline::from_fingerprints(selected), targets))
}

fn baseline_fingerprint<'a>(
    baseline: &'a PromotionBaseline,
    path: &str,
) -> Result<&'a ProcedurePathFingerprint, String> {
    baseline
        .fingerprints()
        .iter()
        .find(|fingerprint| fingerprint.path == path)
        .ok_or_else(|| format!("promotion baseline is missing approved path {path}"))
}

fn target_for_states(
    path: &str,
    before: ProcedurePathState,
    after: ProcedurePathState,
) -> Result<PromotionTarget, String> {
    match (before, after) {
        (ProcedurePathState::Missing, ProcedurePathState::Present) => {
            Ok(PromotionTarget::Create { path: path.into() })
        }
        (ProcedurePathState::Present, ProcedurePathState::Present) => {
            Ok(PromotionTarget::Update { path: path.into() })
        }
        (ProcedurePathState::Present, ProcedurePathState::Missing) => {
            Ok(PromotionTarget::Delete { path: path.into() })
        }
        (ProcedurePathState::Missing, ProcedurePathState::Missing) => Err(format!(
            "validated changed path {path} is missing from both execution baseline and result"
        )),
    }
}
