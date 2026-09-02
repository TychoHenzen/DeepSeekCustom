use std::fmt;

use super::WorkCardValidationErrors;

/// A rejected session-local Controlled Development transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlledDevelopmentTransitionError {
    Disabled,
    InvalidPacketId,
    NotPlanning,
    CardIdMismatch,
    InvalidWorkCard(WorkCardValidationErrors),
}

impl fmt::Display for ControlledDevelopmentTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Controlled Development is disabled"),
            Self::InvalidPacketId => formatter.write_str("packet id must be nonempty"),
            Self::NotPlanning => formatter.write_str("session is not planning a Work Card"),
            Self::CardIdMismatch => {
                formatter.write_str("Work Card id does not match the current packet")
            }
            Self::InvalidWorkCard(errors) => errors.fmt(formatter),
        }
    }
}

impl std::error::Error for ControlledDevelopmentTransitionError {}
