//! Validation of model-selected localization targets against a current index.

use std::fmt;
use std::path::{Component, Path};

use super::{LocalizationTarget, RepositoryIndexEntry};

/// One rejected target and the deterministic reason it was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizationTargetRejection {
    pub target_index: usize,
    pub path: String,
    pub symbol: Option<String>,
    pub reason: String,
}

/// Every invalid target returned by one localization response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizationTargetValidationError {
    rejections: Vec<LocalizationTargetRejection>,
}

impl LocalizationTargetValidationError {
    /// Return all rejected targets in response order.
    pub fn rejections(&self) -> &[LocalizationTargetRejection] {
        &self.rejections
    }
}

impl fmt::Display for LocalizationTargetValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "localization target validation failed:")?;
        for rejection in &self.rejections {
            write!(
                formatter,
                "- target[{}] path={:?}",
                rejection.target_index, rejection.path
            )?;
            if let Some(symbol) = &rejection.symbol {
                write!(formatter, " symbol={symbol:?}")?;
            }
            writeln!(formatter, ": {}", rejection.reason)?;
        }
        Ok(())
    }
}

impl std::error::Error for LocalizationTargetValidationError {}

/// Accept all targets unchanged, or reject the whole response with every
/// invalid target and reason.
pub fn validate_localization_targets(
    targets: Vec<LocalizationTarget>,
    index: &[RepositoryIndexEntry],
) -> Result<Vec<LocalizationTarget>, LocalizationTargetValidationError> {
    let mut rejections = Vec::new();

    for (target_index, target) in targets.iter().enumerate() {
        let entry = match target_path_error(&target.path) {
            Some(reason) => {
                rejections.push(rejection(target_index, target, reason));
                continue;
            }
            None => index.iter().find(|entry| entry.path == target.path),
        };

        let Some(entry) = entry else {
            rejections.push(rejection(
                target_index,
                target,
                "path is not present in the repository index",
            ));
            continue;
        };

        if let Some(symbol) = &target.symbol
            && !entry.symbols.iter().any(|indexed| indexed == symbol)
        {
            rejections.push(rejection(
                target_index,
                target,
                "symbol is not present under the indexed path",
            ));
        }
    }

    if rejections.is_empty() {
        Ok(targets)
    } else {
        Err(LocalizationTargetValidationError { rejections })
    }
}

fn rejection(
    target_index: usize,
    target: &LocalizationTarget,
    reason: impl Into<String>,
) -> LocalizationTargetRejection {
    LocalizationTargetRejection {
        target_index,
        path: target.path.clone(),
        symbol: target.symbol.clone(),
        reason: reason.into(),
    }
}

fn target_path_error(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("path must not be empty");
    }
    if Path::new(path).is_absolute() || is_windows_absolute(path) {
        return Some("path must be repository-relative");
    }
    if path.contains('\\') {
        return Some("path must use normalized '/' separators");
    }
    if path.starts_with('/') || path.ends_with('/') || path.contains("//") {
        return Some("path must be a normalized repository-relative path");
    }
    if Path::new(path)
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Some("path must not contain traversal or dot components");
    }
    None
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}
