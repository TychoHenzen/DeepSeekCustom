use std::collections::BTreeSet;

use std::path::Path;

use crate::procedure::WorkspaceFileChanges;

use super::dependency_change_inspector::authorize_dependency_file_changes;
use super::{AuthorizedWorkspaceChanges, ControlledChangeGateError, MAX_PRODUCTION_PATHS};

/// Authorize inventory-derived endpoints against raw gate inputs.
///
/// This lower-level boundary deliberately does not accept a validated
/// `WorkCard`. Its production-path limit remains independently enforced if
/// an upstream constructor changes later.
pub fn authorize_changed_paths(
    changed_paths: impl IntoIterator<Item = String>,
    production_paths: &[String],
    supporting_paths: &[String],
) -> Result<AuthorizedWorkspaceChanges, ControlledChangeGateError> {
    let changed_paths = changed_paths.into_iter().collect::<BTreeSet<_>>();
    let approved_paths = production_paths
        .iter()
        .chain(supporting_paths)
        .cloned()
        .collect::<BTreeSet<_>>();
    let unauthorized = changed_paths
        .difference(&approved_paths)
        .cloned()
        .collect::<Vec<_>>();
    if !unauthorized.is_empty() {
        return Err(ControlledChangeGateError::UnauthorizedPaths(unauthorized));
    }

    let approved_production = production_paths.iter().cloned().collect::<BTreeSet<_>>();
    let changed_production = changed_paths
        .intersection(&approved_production)
        .cloned()
        .collect::<Vec<_>>();
    if changed_production.len() > MAX_PRODUCTION_PATHS {
        return Err(ControlledChangeGateError::TooManyProductionPaths {
            maximum: MAX_PRODUCTION_PATHS,
            changed_paths: changed_production,
        });
    }

    Ok(AuthorizedWorkspaceChanges::new(
        changed_paths.into_iter().collect(),
    ))
}

/// Run the complete pre-proof gate against inventory data and isolated bytes.
pub fn authorize_workspace_changes(
    changes: &WorkspaceFileChanges,
    baseline_root: &Path,
    execution_root: &Path,
    production_paths: &[String],
    supporting_paths: &[String],
    complexity_exceptions: &[String],
) -> Result<AuthorizedWorkspaceChanges, ControlledChangeGateError> {
    let mut authorized =
        authorize_changed_paths(changes.changed_paths(), production_paths, supporting_paths)?;
    let dependency_matches = authorize_dependency_file_changes(
        authorized.paths(),
        baseline_root,
        execution_root,
        complexity_exceptions,
    )?;
    authorized.set_dependency_matches(dependency_matches);
    Ok(authorized)
}
