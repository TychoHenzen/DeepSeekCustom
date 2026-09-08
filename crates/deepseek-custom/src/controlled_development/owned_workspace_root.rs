use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

/// A persisted path that has passed the disposable-workspace ownership check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ControlledDevelopmentOwnedWorkspaceRoot(PathBuf);

impl ControlledDevelopmentOwnedWorkspaceRoot {
    pub fn new(path: PathBuf) -> Result<Self, crate::procedure::DisposableWorkspaceError> {
        crate::procedure::validate_retained_workspace_root(&path)?;
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn into_path(self) -> PathBuf {
        self.0
    }
}

impl<'de> Deserialize<'de> for ControlledDevelopmentOwnedWorkspaceRoot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let path = PathBuf::deserialize(deserializer)?;
        Self::new(path).map_err(serde::de::Error::custom)
    }
}
