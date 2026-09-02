use std::fmt;

use super::WorkCardValidationErrors;

/// A rejected session-local Controlled Development transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlledDevelopmentTransitionError {
    Disabled,
    InvalidPacketId,
    InvalidRequest,
    InvalidBackendName,
    NotPlanning,
    NotAwaitingApproval,
    NotExecuting,
    NotActivePacket,
    PacketIdMismatch,
    CardIdMismatch,
    NoCurrentCard,
    MissingPacketContext,
    MissingExecutionWorkspace,
    ExecutionWorkspaceAlreadyAttached,
    InvalidWorkCard(WorkCardValidationErrors),
}

impl fmt::Display for ControlledDevelopmentTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Controlled Development is disabled"),
            Self::InvalidPacketId => formatter.write_str("packet id must be nonempty"),
            Self::InvalidRequest => formatter.write_str("development request must be nonempty"),
            Self::InvalidBackendName => formatter.write_str("backend name must be nonempty"),
            Self::NotPlanning => formatter.write_str("session is not planning a Work Card"),
            Self::NotAwaitingApproval => {
                formatter.write_str("session is not awaiting Work Card approval")
            }
            Self::NotExecuting => formatter.write_str("session is not executing an approved card"),
            Self::NotActivePacket => formatter.write_str("current packet is already terminal"),
            Self::PacketIdMismatch => {
                formatter.write_str("command packet id does not match the current packet")
            }
            Self::CardIdMismatch => {
                formatter.write_str("Work Card id does not match the current packet")
            }
            Self::NoCurrentCard => formatter.write_str("session has no current Work Card"),
            Self::MissingPacketContext => {
                formatter.write_str("current packet is missing its backend or request context")
            }
            Self::MissingExecutionWorkspace => {
                formatter.write_str("current card has no isolated execution workspace")
            }
            Self::ExecutionWorkspaceAlreadyAttached => {
                formatter.write_str("current card already owns an execution workspace")
            }
            Self::InvalidWorkCard(errors) => errors.fmt(formatter),
        }
    }
}

impl std::error::Error for ControlledDevelopmentTransitionError {}
