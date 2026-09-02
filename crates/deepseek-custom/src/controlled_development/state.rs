use serde::{Deserialize, Serialize};

use super::{
    ControlledDevelopmentPhase, ControlledDevelopmentTransitionError, WorkCard,
    WorkCardValidationError, WorkCardValidationErrors,
};

/// Browser-safe state owned by one top-level session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ControlledDevelopmentState {
    enabled: bool,
    phase: ControlledDevelopmentPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    packet_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approved_card_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    card: Option<WorkCard>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    structural_errors: Vec<WorkCardValidationError>,
}

impl ControlledDevelopmentState {
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub const fn phase(&self) -> ControlledDevelopmentPhase {
        self.phase
    }

    pub fn packet_id(&self) -> Option<&str> {
        self.packet_id.as_deref()
    }

    pub fn approved_card_id(&self) -> Option<&str> {
        self.approved_card_id.as_deref()
    }

    pub const fn work_card(&self) -> Option<&WorkCard> {
        self.card.as_ref()
    }

    pub fn structural_errors(&self) -> &[WorkCardValidationError] {
        &self.structural_errors
    }

    /// Enables or disables the mode for this session.
    ///
    /// Enabling does not start work. Disabling removes every packet-local
    /// authority and returns the lifecycle to `Off`.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.phase = ControlledDevelopmentPhase::Off;
            self.packet_id = None;
            self.approved_card_id = None;
            self.card = None;
            self.structural_errors.clear();
        }
    }

    /// Starts a new packet before a planning backend can be dispatched.
    pub fn begin_packet(
        &mut self,
        packet_id: impl Into<String>,
    ) -> Result<(), ControlledDevelopmentTransitionError> {
        if !self.enabled {
            return Err(ControlledDevelopmentTransitionError::Disabled);
        }
        let packet_id = packet_id.into();
        if packet_id.trim().is_empty() {
            return Err(ControlledDevelopmentTransitionError::InvalidPacketId);
        }
        self.packet_id = Some(packet_id);
        self.approved_card_id = None;
        self.card = None;
        self.structural_errors.clear();
        self.phase = ControlledDevelopmentPhase::Planning;
        Ok(())
    }

    /// Accepts only a complete JSON Work Card from the planning final response.
    ///
    /// This deliberately deserializes the complete input. It never scans prose,
    /// markdown fences, or substrings for an embedded card.
    pub fn accept_planning_result(
        &mut self,
        final_response: &str,
    ) -> Result<(), ControlledDevelopmentTransitionError> {
        if self.phase != ControlledDevelopmentPhase::Planning {
            return Err(ControlledDevelopmentTransitionError::NotPlanning);
        }
        let card = match serde_json::from_str::<WorkCard>(final_response) {
            Ok(card) => card,
            Err(error) => {
                let errors = WorkCardValidationErrors::new(vec![WorkCardValidationError::new(
                    "work_card",
                    format!("Work Card JSON is structurally invalid: {error}"),
                )]);
                self.block_with_structural_errors(&errors);
                return Err(ControlledDevelopmentTransitionError::InvalidWorkCard(
                    errors,
                ));
            }
        };
        self.accept_work_card(card)
    }

    /// Stores the complete card only after structural and packet checks pass.
    pub fn accept_work_card(
        &mut self,
        card: WorkCard,
    ) -> Result<(), ControlledDevelopmentTransitionError> {
        if self.phase != ControlledDevelopmentPhase::Planning {
            return Err(ControlledDevelopmentTransitionError::NotPlanning);
        }
        if let Err(errors) = card.validate() {
            self.block_with_structural_errors(&errors);
            return Err(ControlledDevelopmentTransitionError::InvalidWorkCard(
                errors,
            ));
        }
        if self.packet_id.as_deref() != Some(card.id.as_str()) {
            return Err(ControlledDevelopmentTransitionError::CardIdMismatch);
        }
        self.approved_card_id = None;
        self.structural_errors.clear();
        self.card = Some(card);
        self.phase = ControlledDevelopmentPhase::AwaitingApproval;
        Ok(())
    }

    fn block_with_structural_errors(&mut self, errors: &WorkCardValidationErrors) {
        self.approved_card_id = None;
        self.card = None;
        self.structural_errors = errors.errors().to_vec();
        self.phase = ControlledDevelopmentPhase::Blocked;
    }
}
