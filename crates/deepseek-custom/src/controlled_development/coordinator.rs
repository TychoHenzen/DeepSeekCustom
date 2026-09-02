//! Session-owned authority for one Controlled Development packet lifecycle.

use crate::agent::events::RoutedEvent;
use crate::procedure::{
    DisposableWorkspacePair, PromotionBaseline, PromotionResult, VerifierCommandDisposition,
    VerifierGateEvidence, VerifierRun,
};

use super::{
    ControlledBackendSelection, ControlledDevelopmentCommand, ControlledDevelopmentEffect,
    ControlledDevelopmentPhase, ControlledDevelopmentState, ControlledDevelopmentTransitionError,
    authorize_workspace_changes, promotion_plan,
};

/// Owns all runtime authority and evidence for one top-level session.
#[derive(Default)]
pub struct ControlledDevelopmentCoordinator {
    state: ControlledDevelopmentState,
    original_request: Option<String>,
    selection: Option<ControlledBackendSelection>,
    workspace_root: Option<std::path::PathBuf>,
    workspace_pair: Option<DisposableWorkspacePair>,
    raw_events: Vec<RoutedEvent>,
    proof_evidence: Vec<VerifierGateEvidence>,
    changed_paths: Vec<String>,
    dependency_matches: Vec<super::DependencyExceptionMatch>,
    promotion_baseline: Option<PromotionBaseline>,
    promotion_evidence: Option<PromotionResult>,
    interrupt: std::sync::Arc<std::sync::atomic::AtomicBool>,
    proofs_dispatched: bool,
    promotion_dispatched: bool,
    blocker: Option<String>,
    compact_summary: Option<String>,
}

impl ControlledDevelopmentCoordinator {
    pub const fn state(&self) -> &ControlledDevelopmentState {
        &self.state
    }

    pub fn raw_events(&self) -> &[RoutedEvent] {
        &self.raw_events
    }

    pub fn proof_evidence(&self) -> &[VerifierGateEvidence] {
        &self.proof_evidence
    }

    pub fn changed_paths(&self) -> &[String] {
        &self.changed_paths
    }

    pub fn dependency_matches(&self) -> &[super::DependencyExceptionMatch] {
        &self.dependency_matches
    }

    pub const fn promotion_evidence(&self) -> Option<&PromotionResult> {
        self.promotion_evidence.as_ref()
    }

    pub fn interrupt_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.interrupt)
    }

    pub fn blocker(&self) -> Option<&str> {
        self.blocker.as_deref()
    }

    pub fn compact_summary(&self) -> Option<&str> {
        self.compact_summary.as_deref()
    }

    pub const fn has_execution_workspace(&self) -> bool {
        self.workspace_pair.is_some()
    }

    pub fn execution_workspace_path(&self) -> Option<&std::path::Path> {
        self.workspace_pair
            .as_ref()
            .map(DisposableWorkspacePair::execution_path)
    }

    /// Apply one typed command and return at most one slow service effect.
    pub fn handle(
        &mut self,
        command: ControlledDevelopmentCommand,
    ) -> Result<Option<ControlledDevelopmentEffect>, ControlledDevelopmentTransitionError> {
        match command {
            ControlledDevelopmentCommand::SetEnabled { enabled } => {
                self.state.set_enabled(enabled);
                if !enabled {
                    self.clear_packet_data();
                }
                Ok(None)
            }
            ControlledDevelopmentCommand::Plan {
                packet_id,
                original_request,
                selection,
                workspace_root,
            } => self.plan(packet_id, original_request, selection, workspace_root),
            ControlledDevelopmentCommand::PlanningFinished {
                packet_id,
                final_response,
            } => {
                self.require_packet(&packet_id)?;
                self.state.accept_planning_result(&final_response)?;
                Ok(None)
            }
            ControlledDevelopmentCommand::Approve { card_id } => self.approve(&card_id),
            ControlledDevelopmentCommand::Reject { card_id } => {
                self.state.reject_current_card(&card_id)?;
                self.blocker = Some("Work Card rejected by user".to_string());
                self.compact_summary = Some("Work Card rejected without execution".to_string());
                Ok(None)
            }
            ControlledDevelopmentCommand::ExecutionWorkspaceReady { card_id, workspace } => {
                self.execution_workspace_ready(&card_id, *workspace)
            }
            ControlledDevelopmentCommand::RecordRawEvent { packet_id, event } => {
                self.require_packet(&packet_id)?;
                self.raw_events.push(event);
                Ok(None)
            }
            ControlledDevelopmentCommand::RecordProofEvidence { card_id, evidence } => {
                self.require_executing_card(&card_id)?;
                self.proof_evidence.push(*evidence);
                Ok(None)
            }
            ControlledDevelopmentCommand::ValidateIsolatedChanges { card_id } => {
                self.validate_isolated_changes(&card_id)
            }
            ControlledDevelopmentCommand::ProofsFinished { card_id, run } => {
                self.proofs_finished(&card_id, *run)
            }
            ControlledDevelopmentCommand::PromotionFinished { card_id, result } => {
                self.promotion_finished(&card_id, result.map(|result| *result))
            }
            ControlledDevelopmentCommand::Complete { card_id, summary } => {
                self.state.complete_current_card(&card_id)?;
                self.workspace_pair = None;
                self.blocker = None;
                self.compact_summary = Some(summary);
                Ok(None)
            }
            ControlledDevelopmentCommand::Fail {
                packet_id,
                blocker,
                summary,
            } => {
                self.state.block_current_packet(&packet_id)?;
                self.blocker = Some(blocker);
                self.compact_summary = Some(summary);
                Ok(None)
            }
            ControlledDevelopmentCommand::Interrupt { packet_id, summary } => {
                self.state.interrupt_current_packet(&packet_id)?;
                self.blocker = None;
                self.compact_summary = Some(summary);
                Ok(None)
            }
        }
    }

    fn plan(
        &mut self,
        packet_id: String,
        original_request: String,
        selection: ControlledBackendSelection,
        workspace_root: std::path::PathBuf,
    ) -> Result<Option<ControlledDevelopmentEffect>, ControlledDevelopmentTransitionError> {
        if original_request.trim().is_empty() {
            return Err(ControlledDevelopmentTransitionError::InvalidRequest);
        }
        if selection.backend.trim().is_empty() {
            return Err(ControlledDevelopmentTransitionError::InvalidBackendName);
        }
        self.state.begin_packet(packet_id.clone())?;
        self.clear_packet_data();
        self.original_request = Some(original_request.clone());
        self.selection = Some(selection.clone());
        self.workspace_root = Some(workspace_root.clone());
        Ok(Some(ControlledDevelopmentEffect::DispatchPlanning {
            packet_id,
            original_request,
            selection,
            planning_root: workspace_root,
        }))
    }

    fn approve(
        &mut self,
        card_id: &str,
    ) -> Result<Option<ControlledDevelopmentEffect>, ControlledDevelopmentTransitionError> {
        let project_root = self.packet_context()?.2.clone();
        self.state.approve_current_card(card_id)?;
        let card = self
            .state
            .work_card()
            .ok_or(ControlledDevelopmentTransitionError::NoCurrentCard)?;
        self.promotion_baseline = match promotion_plan::capture_card_baseline(&project_root, card) {
            Ok(baseline) => Some(baseline),
            Err(error) => {
                return self.block_pre_proof_gate(
                    card_id,
                    format!("could not capture promotion baseline: {error}"),
                );
            }
        };
        Ok(Some(
            ControlledDevelopmentEffect::CreateExecutionWorkspace {
                card_id: card_id.to_string(),
                project_root,
            },
        ))
    }

    fn execution_workspace_ready(
        &mut self,
        card_id: &str,
        workspace: DisposableWorkspacePair,
    ) -> Result<Option<ControlledDevelopmentEffect>, ControlledDevelopmentTransitionError> {
        self.require_executing_card(card_id)?;
        if self.workspace_pair.is_some() {
            return Err(ControlledDevelopmentTransitionError::ExecutionWorkspaceAlreadyAttached);
        }
        let (request, selection, _) = self.packet_context()?;
        let request = request.clone();
        let selection = selection.clone();
        let card = self
            .state
            .work_card()
            .cloned()
            .ok_or(ControlledDevelopmentTransitionError::NoCurrentCard)?;
        let execution_root = workspace.execution_path().to_path_buf();
        self.workspace_pair = Some(workspace);
        if self.promotion_baseline.is_none() {
            return Err(ControlledDevelopmentTransitionError::MissingPromotionBaseline);
        }
        Ok(Some(ControlledDevelopmentEffect::DispatchExecution {
            card_id: card_id.to_string(),
            original_request: request,
            card,
            selection,
            execution_root,
        }))
    }

    fn validate_isolated_changes(
        &mut self,
        card_id: &str,
    ) -> Result<Option<ControlledDevelopmentEffect>, ControlledDevelopmentTransitionError> {
        self.require_executing_card(card_id)?;
        if self.proofs_dispatched {
            return Err(ControlledDevelopmentTransitionError::ProofsAlreadyDispatched);
        }
        let card = self
            .state
            .work_card()
            .cloned()
            .ok_or(ControlledDevelopmentTransitionError::NoCurrentCard)?;
        let changes = match self.workspace_pair.as_ref() {
            Some(workspace) => match workspace.changes() {
                Ok(changes) => changes,
                Err(error) => {
                    return self.block_pre_proof_gate(
                        card_id,
                        format!("could not inventory isolated changes: {error}"),
                    );
                }
            },
            None => return Err(ControlledDevelopmentTransitionError::MissingExecutionWorkspace),
        };

        let workspace = self
            .workspace_pair
            .as_ref()
            .ok_or(ControlledDevelopmentTransitionError::MissingExecutionWorkspace)?;
        self.changed_paths = changes.changed_paths();
        match authorize_workspace_changes(
            &changes,
            workspace.baseline_path(),
            workspace.execution_path(),
            &card.production_paths,
            &card.supporting_paths,
            &card.complexity_exceptions,
        ) {
            Ok(authorized) => {
                self.dependency_matches = authorized.dependency_matches().to_vec();
                self.proofs_dispatched = true;
                Ok(Some(ControlledDevelopmentEffect::RunProofCommands {
                    card_id: card_id.to_string(),
                    proof_commands: card.proof_commands,
                    execution_root: workspace.execution_path().to_path_buf(),
                    interrupt: std::sync::Arc::clone(&self.interrupt),
                }))
            }
            Err(error) => self.block_pre_proof_gate(card_id, error.to_string()),
        }
    }

    fn proofs_finished(
        &mut self,
        card_id: &str,
        run: VerifierRun,
    ) -> Result<Option<ControlledDevelopmentEffect>, ControlledDevelopmentTransitionError> {
        self.require_executing_card(card_id)?;
        if !self.proofs_dispatched {
            return Err(ControlledDevelopmentTransitionError::ProofsNotDispatched);
        }
        let expected_commands = &self
            .state
            .work_card()
            .ok_or(ControlledDevelopmentTransitionError::NoCurrentCard)?
            .proof_commands;
        if run.gate_results.len() != expected_commands.len()
            || run
                .gate_results
                .iter()
                .zip(expected_commands)
                .any(|(gate, expected)| gate.command != *expected)
        {
            return Err(ControlledDevelopmentTransitionError::ProofEvidenceMismatch);
        }
        self.proof_evidence = run.gate_evidence();
        if !run.all_commands_succeeded() {
            let failed = run
                .commands
                .last()
                .expect("a failed proof run records its first failed command");
            let summary = format!(
                "Proof command did not pass: {} ({:?})",
                failed.command, failed.disposition
            );
            if failed.disposition == VerifierCommandDisposition::Interrupted {
                self.state.interrupt_current_packet(card_id)?;
                self.blocker = None;
            } else {
                self.state.block_current_packet(card_id)?;
                self.blocker = Some(summary.clone());
            }
            self.compact_summary = Some(summary);
            return Ok(None);
        }

        let workspace = self
            .workspace_pair
            .as_ref()
            .ok_or(ControlledDevelopmentTransitionError::MissingExecutionWorkspace)?;
        let baseline = self
            .promotion_baseline
            .as_ref()
            .ok_or(ControlledDevelopmentTransitionError::MissingPromotionBaseline)?;
        let (baseline, targets) = match promotion_plan::build_promotion_plan(
            workspace.execution_path(),
            baseline,
            &self.changed_paths,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                return self.block_pre_proof_gate(
                    card_id,
                    format!("could not build promotion plan: {error}"),
                );
            }
        };
        let project_root = self
            .workspace_root
            .clone()
            .ok_or(ControlledDevelopmentTransitionError::MissingPacketContext)?;
        self.promotion_dispatched = true;
        Ok(Some(ControlledDevelopmentEffect::PromoteValidatedChanges {
            card_id: card_id.to_string(),
            project_root,
            execution_root: workspace.execution_path().to_path_buf(),
            baseline,
            targets,
        }))
    }

    fn promotion_finished(
        &mut self,
        card_id: &str,
        result: Result<PromotionResult, Box<crate::procedure::PromotionError>>,
    ) -> Result<Option<ControlledDevelopmentEffect>, ControlledDevelopmentTransitionError> {
        self.require_executing_card(card_id)?;
        if !self.promotion_dispatched {
            return Err(ControlledDevelopmentTransitionError::PromotionNotDispatched);
        }
        match result {
            Ok(evidence) => {
                self.promotion_evidence = Some(evidence);
                self.state.complete_current_card(card_id)?;
                self.workspace_pair = None;
                self.blocker = None;
                self.compact_summary = Some(format!(
                    "Controlled Development completed after {} proof command(s)",
                    self.proof_evidence.len()
                ));
            }
            Err(error) => {
                let blocker = format!("promotion failed: {error}");
                self.state.block_current_packet(card_id)?;
                self.blocker = Some(blocker.clone());
                self.compact_summary = Some(format!("Controlled Development blocked: {blocker}"));
            }
        }
        Ok(None)
    }

    fn block_pre_proof_gate(
        &mut self,
        card_id: &str,
        blocker: String,
    ) -> Result<Option<ControlledDevelopmentEffect>, ControlledDevelopmentTransitionError> {
        self.state.block_current_packet(card_id)?;
        self.blocker = Some(blocker.clone());
        self.compact_summary = Some(format!("Controlled Development blocked: {blocker}"));
        Ok(None)
    }

    fn packet_context(
        &self,
    ) -> Result<
        (&String, &ControlledBackendSelection, &std::path::PathBuf),
        ControlledDevelopmentTransitionError,
    > {
        match (
            self.original_request.as_ref(),
            self.selection.as_ref(),
            self.workspace_root.as_ref(),
        ) {
            (Some(request), Some(selection), Some(root)) => Ok((request, selection, root)),
            _ => Err(ControlledDevelopmentTransitionError::MissingPacketContext),
        }
    }

    fn require_packet(&self, packet_id: &str) -> Result<(), ControlledDevelopmentTransitionError> {
        if self.state.packet_id() == Some(packet_id) {
            Ok(())
        } else {
            Err(ControlledDevelopmentTransitionError::PacketIdMismatch)
        }
    }

    fn require_executing_card(
        &self,
        card_id: &str,
    ) -> Result<(), ControlledDevelopmentTransitionError> {
        if self.state.phase() != ControlledDevelopmentPhase::Executing {
            return Err(ControlledDevelopmentTransitionError::NotExecuting);
        }
        if self.state.approved_card_id() != Some(card_id)
            || self.state.work_card().map(|card| card.id.as_str()) != Some(card_id)
        {
            return Err(ControlledDevelopmentTransitionError::CardIdMismatch);
        }
        Ok(())
    }

    fn clear_packet_data(&mut self) {
        self.original_request = None;
        self.selection = None;
        self.workspace_root = None;
        self.workspace_pair = None;
        self.raw_events.clear();
        self.proof_evidence.clear();
        self.changed_paths.clear();
        self.dependency_matches.clear();
        self.promotion_baseline = None;
        self.promotion_evidence = None;
        self.proofs_dispatched = false;
        self.promotion_dispatched = false;
        self.interrupt
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.blocker = None;
        self.compact_summary = None;
    }
}
