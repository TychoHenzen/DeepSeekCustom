//! Slow Controlled Development work kept outside application actor arbitration.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::agent::events::{RoutedEvent, StreamEvent};
use crate::backend::Backend;
use crate::backend::factory::BackendFactory;
use crate::controlled_development::{
    ControlledDevelopmentCommand, ControlledDevelopmentEffect, work_card_json_schema,
};
use crate::procedure::{
    DisposableDraftWorkspace, VerifierCommandRunner, promote_verified_workspace,
};

/// One actor-authorized effect tagged with the session that owns it.
#[derive(Debug)]
pub struct ControlledDevelopmentEffectRequest {
    pub session_id: String,
    pub effect: ControlledDevelopmentEffect,
}

/// One deterministic completion sent back to the application actor.
#[derive(Debug)]
pub struct ControlledDevelopmentServiceEvent {
    pub session_id: String,
    pub command: ControlledDevelopmentCommand,
}

/// Execute controlled effects in order without holding the application actor lock.
pub async fn run_controlled_development_service(
    factory: Arc<BackendFactory>,
    mut effects: mpsc::UnboundedReceiver<ControlledDevelopmentEffectRequest>,
    events: mpsc::UnboundedSender<ControlledDevelopmentServiceEvent>,
) {
    while let Some(request) = effects.recv().await {
        execute_effect(&factory, &events, request).await;
    }
}

async fn execute_effect(
    factory: &Arc<BackendFactory>,
    events: &mpsc::UnboundedSender<ControlledDevelopmentServiceEvent>,
    request: ControlledDevelopmentEffectRequest,
) {
    let session_id = request.session_id;
    match request.effect {
        ControlledDevelopmentEffect::DispatchPlanning {
            packet_id,
            original_request,
            selection,
            planning_root,
            interrupt,
        } => {
            let (raw_tx, raw_rx) = mpsc::unbounded_channel();
            let backend = factory.build_controlled_planning(
                &selection.backend,
                selection.model.as_deref(),
                raw_tx,
                planning_root,
            );
            let response = match backend {
                Ok(mut backend) => {
                    backend.adopt_interrupt_flag(Arc::clone(&interrupt));
                    run_backend(
                        &mut backend,
                        &planning_prompt(&packet_id, &original_request),
                        raw_rx,
                        events,
                        &session_id,
                        &packet_id,
                    )
                    .await
                }
                Err(error) => Err(error),
            };
            if interrupt.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let command = match response {
                Ok(final_response) => ControlledDevelopmentCommand::PlanningFinished {
                    packet_id,
                    final_response,
                },
                Err(error) => failure(&packet_id, "planning backend failed", error),
            };
            send_event(events, session_id, command);
        }
        ControlledDevelopmentEffect::CreateExecutionWorkspace {
            card_id,
            project_root,
        } => {
            let workspace = tokio::task::spawn_blocking(move || {
                DisposableDraftWorkspace::create_current_state_pair(&project_root)
            })
            .await;
            let command = match workspace {
                Ok(Ok(workspace)) => ControlledDevelopmentCommand::ExecutionWorkspaceReady {
                    card_id,
                    workspace: Box::new(workspace),
                },
                Ok(Err(error)) => failure(
                    &card_id,
                    "execution workspace creation failed",
                    error.to_string(),
                ),
                Err(error) => failure(
                    &card_id,
                    "execution workspace task failed",
                    error.to_string(),
                ),
            };
            send_event(events, session_id, command);
        }
        ControlledDevelopmentEffect::DispatchExecution {
            card_id,
            original_request,
            card,
            selection,
            execution_root,
            interrupt,
        } => {
            let (raw_tx, raw_rx) = mpsc::unbounded_channel();
            let backend = factory.build_controlled_execution(
                &selection.backend,
                selection.model.as_deref(),
                raw_tx,
                execution_root,
            );
            let result = match backend {
                Ok(mut backend) => {
                    backend.adopt_interrupt_flag(Arc::clone(&interrupt));
                    run_backend(
                        &mut backend,
                        &execution_prompt(&original_request, &card),
                        raw_rx,
                        events,
                        &session_id,
                        &card_id,
                    )
                    .await
                    .map(|_| ())
                }
                Err(error) => Err(error),
            };
            if interrupt.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let command = match result {
                Ok(()) => ControlledDevelopmentCommand::ValidateIsolatedChanges { card_id },
                Err(error) => failure(&card_id, "execution backend failed", error),
            };
            send_event(events, session_id, command);
        }
        ControlledDevelopmentEffect::RunProofCommands {
            card_id,
            proof_commands,
            execution_root,
            interrupt,
        } => {
            let run = VerifierCommandRunner::with_interrupt(interrupt)
                .run(&execution_root, &proof_commands)
                .await;
            send_event(
                events,
                session_id,
                ControlledDevelopmentCommand::ProofsFinished {
                    card_id,
                    run: Box::new(run),
                },
            );
        }
        ControlledDevelopmentEffect::PromoteValidatedChanges {
            card_id,
            project_root,
            execution_root,
            baseline,
            targets,
            interrupt,
        } => {
            if interrupt.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let result = tokio::task::spawn_blocking(move || {
                promote_verified_workspace(&project_root, &execution_root, &baseline, &targets)
            })
            .await;
            let command = match result {
                Ok(result) => ControlledDevelopmentCommand::PromotionFinished {
                    card_id,
                    result: result.map(Box::new).map_err(Box::new),
                },
                Err(error) => failure(&card_id, "promotion task failed", error.to_string()),
            };
            send_event(events, session_id, command);
        }
    }
}

async fn run_backend(
    backend: &mut Backend,
    prompt: &str,
    mut raw_events: mpsc::UnboundedReceiver<RoutedEvent>,
    events: &mpsc::UnboundedSender<ControlledDevelopmentServiceEvent>,
    session_id: &str,
    packet_id: &str,
) -> Result<String, String> {
    let result = backend.run(prompt).await.map_err(|error| error.to_string());
    backend.shutdown().await;

    let mut streamed_text = String::new();
    while let Ok(event) = raw_events.try_recv() {
        if let StreamEvent::Text { text, .. } = &event.event {
            streamed_text.push_str(text);
        }
        send_event(
            events,
            session_id.to_string(),
            ControlledDevelopmentCommand::RecordRawEvent {
                packet_id: packet_id.to_string(),
                event,
            },
        );
    }

    let responses = result?;
    let direct_text = responses.join("");
    if direct_text.trim().is_empty() {
        Ok(streamed_text)
    } else {
        Ok(direct_text)
    }
}

fn planning_prompt(packet_id: &str, original_request: &str) -> String {
    format!(
        "Plan the following development request without changing any file. Return exactly one JSON Work Card and no prose. The Work Card id must be {packet_id:?}. Request:\n{original_request}\nJSON Schema:\n{}",
        serde_json::to_string_pretty(&work_card_json_schema())
            .expect("Work Card schema is serializable")
    )
}

fn execution_prompt(
    original_request: &str,
    card: &crate::controlled_development::WorkCard,
) -> String {
    format!(
        "Implement only the approved Work Card in this isolated workspace. Call the available read tool, then call edit or write through the provider's native tool-call channel to make the approved change now. Never print, quote, or fence a tool call as assistant text. Assistant text alone does not change the workspace, so do not claim completion without applying the required file edit. Do not change paths outside the card. Original request:\n{original_request}\nApproved Work Card:\n{}",
        serde_json::to_string_pretty(card).expect("Work Card is serializable")
    )
}

fn failure(packet_id: &str, context: &str, error: String) -> ControlledDevelopmentCommand {
    let blocker = format!("{context}: {error}");
    ControlledDevelopmentCommand::Fail {
        packet_id: packet_id.to_string(),
        blocker: blocker.clone(),
        summary: format!("Controlled Development blocked: {blocker}"),
    }
}

fn send_event(
    events: &mpsc::UnboundedSender<ControlledDevelopmentServiceEvent>,
    session_id: String,
    command: ControlledDevelopmentCommand,
) {
    let _ = events.send(ControlledDevelopmentServiceEvent {
        session_id,
        command,
    });
}
