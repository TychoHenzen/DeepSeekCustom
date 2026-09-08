//! SHA-256 identities for procedure inputs.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Whether a path existed when its identity was captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcedurePathState {
    Present,
    Missing,
}

/// Stable path identity plus the content state observed for that path.
///
/// The path hash is independent from existence and content. It therefore
/// identifies create destinations, delete sources, and both rename endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedurePathFingerprint {
    pub path: String,
    pub identity_sha256: String,
    pub state: ProcedurePathState,
    pub content_sha256: Option<String>,
}

/// SHA-256 evidence stored beside one localization run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureInputFingerprints {
    pub openspec: Vec<ProcedurePathFingerprint>,
    pub targets: Vec<ProcedurePathFingerprint>,
}

impl ProcedureInputFingerprints {
    pub fn is_empty(&self) -> bool {
        self.openspec.is_empty() && self.targets.is_empty()
    }
}

/// Failure to capture a deterministic repository-relative path identity.
#[derive(Debug, Error)]
pub enum ProcedureFingerprintError {
    #[error("procedure fingerprint path must be repository-relative and normalized: {0}")]
    InvalidPath(String),
    #[error("procedure fingerprint path is not a regular file or a missing path: {0}")]
    UnsupportedPath(String),
    #[error("could not read procedure fingerprint path {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not serialize procedure fingerprint input: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Hash one serializable value using its deterministic JSON representation.
pub fn sha256_json(value: &impl Serialize) -> Result<String, ProcedureFingerprintError> {
    Ok(sha256_bytes(&serde_json::to_vec(value)?))
}

/// Capture one normalized repository-relative path.
pub fn capture_path_fingerprint(
    project_root: &Path,
    path: &str,
) -> Result<ProcedurePathFingerprint, ProcedureFingerprintError> {
    let normalized = normalize_relative_path(path)?;
    let absolute = project_root.join(Path::new(&normalized));
    let identity_sha256 = sha256_bytes(format!("path\0{normalized}").as_bytes());

    match std::fs::symlink_metadata(&absolute) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let bytes =
                std::fs::read(&absolute).map_err(|source| ProcedureFingerprintError::Read {
                    path: normalized.clone(),
                    source,
                })?;
            Ok(ProcedurePathFingerprint {
                path: normalized,
                identity_sha256,
                state: ProcedurePathState::Present,
                content_sha256: Some(sha256_bytes(&bytes)),
            })
        }
        Ok(_) => Err(ProcedureFingerprintError::UnsupportedPath(normalized)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ProcedurePathFingerprint {
                path: normalized,
                identity_sha256,
                state: ProcedurePathState::Missing,
                content_sha256: None,
            })
        }
        Err(source) => Err(ProcedureFingerprintError::Read {
            path: normalized,
            source,
        }),
    }
}

/// Capture a sorted, duplicate-free set of repository-relative paths.
pub fn capture_path_fingerprints(
    project_root: &Path,
    paths: impl IntoIterator<Item = String>,
) -> Result<Vec<ProcedurePathFingerprint>, ProcedureFingerprintError> {
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
        .iter()
        .map(|path| capture_path_fingerprint(project_root, path))
        .collect()
}

fn normalize_relative_path(path: &str) -> Result<String, ProcedureFingerprintError> {
    let replaced = path.replace('\\', "/");
    let parsed = Path::new(&replaced);
    if replaced.is_empty() || parsed.is_absolute() {
        return Err(ProcedureFingerprintError::InvalidPath(path.to_string()));
    }

    let mut parts = Vec::new();
    for component in parsed.components() {
        match component {
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Err(ProcedureFingerprintError::InvalidPath(path.to_string()));
                };
                parts.push(part);
            }
            _ => return Err(ProcedureFingerprintError::InvalidPath(path.to_string())),
        }
    }
    if parts.is_empty() {
        return Err(ProcedureFingerprintError::InvalidPath(path.to_string()));
    }
    Ok(parts.join("/"))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}
