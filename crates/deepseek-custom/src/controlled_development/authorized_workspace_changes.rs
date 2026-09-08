use super::DependencyExceptionMatch;

/// Inventory-derived paths that passed every pre-proof authorization gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedWorkspaceChanges {
    paths: Vec<String>,
    dependency_matches: Vec<DependencyExceptionMatch>,
}

impl AuthorizedWorkspaceChanges {
    pub(crate) fn new(paths: Vec<String>) -> Self {
        Self {
            paths,
            dependency_matches: Vec::new(),
        }
    }

    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    pub fn dependency_matches(&self) -> &[DependencyExceptionMatch] {
        &self.dependency_matches
    }

    pub(crate) fn set_dependency_matches(&mut self, matches: Vec<DependencyExceptionMatch>) {
        self.dependency_matches = matches;
    }
}
