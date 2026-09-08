use std::fmt;

/// A deterministic reason that isolated changes cannot advance to proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlledChangeGateError {
    UnauthorizedPaths(Vec<String>),
    TooManyProductionPaths {
        maximum: usize,
        changed_paths: Vec<String>,
    },
    DependencyInspectionFailed {
        path: String,
        reason: String,
    },
    MissingDependencyException {
        path: String,
        changed_dependencies: Vec<String>,
    },
}

impl fmt::Display for ControlledChangeGateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnauthorizedPaths(paths) => write!(
                formatter,
                "isolated changes include paths outside the approved Work Card: {}",
                paths.join(", ")
            ),
            Self::TooManyProductionPaths {
                maximum,
                changed_paths,
            } => write!(
                formatter,
                "isolated changes include {} production paths, exceeding the limit of {maximum}: {}",
                changed_paths.len(),
                changed_paths.join(", ")
            ),
            Self::DependencyInspectionFailed { path, reason } => write!(
                formatter,
                "could not inspect changed dependency file {path}: {reason}"
            ),
            Self::MissingDependencyException {
                path,
                changed_dependencies,
            } => write!(
                formatter,
                "changed dependency file {path} has no matching named complexity exception; changed dependencies: {}",
                changed_dependencies.join(", ")
            ),
        }
    }
}

impl std::error::Error for ControlledChangeGateError {}
