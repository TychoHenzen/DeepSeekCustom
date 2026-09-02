use std::fmt;

use serde::{Deserialize, Serialize};

use super::WorkCardValidationError;

/// Every structural problem found during one bounded validation pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkCardValidationErrors(Vec<WorkCardValidationError>);

impl WorkCardValidationErrors {
    pub(crate) fn from_errors(errors: Vec<WorkCardValidationError>) -> Result<(), Self> {
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Self(errors))
        }
    }

    pub fn errors(&self) -> &[WorkCardValidationError] {
        &self.0
    }
}

impl fmt::Display for WorkCardValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Work Card has {} structural error(s)",
            self.0.len()
        )
    }
}

impl std::error::Error for WorkCardValidationErrors {}
