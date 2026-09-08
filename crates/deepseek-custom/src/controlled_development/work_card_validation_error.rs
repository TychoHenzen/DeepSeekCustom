use serde::{Deserialize, Serialize};

/// One structural problem in a proposed Work Card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkCardValidationError {
    pub field: String,
    pub message: String,
}

impl WorkCardValidationError {
    pub(crate) fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}
