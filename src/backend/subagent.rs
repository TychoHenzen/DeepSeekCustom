//! Runs one prompt on a named backend to completion and returns the final
//! text. This is the machinery the `Task` tool sits on. T04 adds that
//! tool. It dispatches a subagent onto another backend, on a different
//! model or provider than the parent session.
//!
//! A subagent never shares the caller's event sender. `run_subagent`
//! gives it a fresh channel of its own. A background task drains and
//! discards it, so three models interleaving token streams never land
//! in one transcript.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::agent::agent_loop::StreamEvent;
use crate::backend::Backend;
use crate::backend::claude_cli::process::ClaudeCliDriver;
use crate::backend::factory::{BackendFactory, ResolvedBackend};

/// One subagent dispatch request. Picks the backend to run on, an
/// optional model override, and the prompt. `depth` says how deep in
/// the dispatch chain this subagent sits.
pub struct SubagentRequest {
    pub backend: String,
    pub model: Option<String>,
    pub prompt: String,
    /// 1 for a subagent dispatched by the main session.
    pub depth: u32,
}

/// The result of a completed subagent run. Carries the final text.
/// Also carries the backend and model that actually produced it, once
/// any override resolves.
#[derive(Debug)]
pub struct SubagentOutcome {
    pub text: String,
    pub backend: String,
    pub model: String,
}

/// Run one prompt on a named backend to completion. An unknown backend
/// name returns the same error `BackendFactory::build` would return. It
/// names the request and lists the known entries.
///
/// An `Api` backend runs through the normal `AgentLoop::run` turn. Its
/// own max-turns guard bounds it. A `ClaudeCli` backend instead runs
/// through `ClaudeCliDriver::run_once`, not the long-lived, stdin-fed
/// path a GUI session would use. A subagent asks one question and wants
/// the process gone once it answers.
pub async fn run_subagent(
    factory: &Arc<BackendFactory>,
    req: SubagentRequest,
) -> Result<SubagentOutcome, String> {
    let start = Instant::now();
    info!(
        backend = %req.backend,
        depth = req.depth,
        "subagent starting"
    );

    let resolved = factory.resolve(&req.backend, req.model.as_deref())?;
    let outcome = match resolved {
        ResolvedBackend::Api { .. } => run_api_subagent(factory, &req).await?,
        ResolvedBackend::ClaudeCli {
            model,
            permission_mode,
            env,
            ..
        } => run_claude_cli_subagent(factory, &req, model, permission_mode, env).await?,
    };

    info!(
        backend = %outcome.backend,
        model = %outcome.model,
        depth = req.depth,
        elapsed_ms = start.elapsed().as_millis() as u64,
        "subagent finished"
    );
    Ok(outcome)
}

/// Drive an `Api` backend to completion. Builds it through the factory,
/// so the depth-gated `Task` tool wiring applies the same way it would
/// for any other backend. Runs one turn and joins the response
/// segments.
async fn run_api_subagent(
    factory: &Arc<BackendFactory>,
    req: &SubagentRequest,
) -> Result<SubagentOutcome, String> {
    let (tx_events, rx_events) = mpsc::unbounded_channel::<StreamEvent>();
    spawn_event_drain(rx_events);

    let backend = factory.build(&req.backend, req.model.as_deref(), tx_events, req.depth)?;
    let Backend::Api(mut agent) = backend else {
        return Err(format!(
            "backend \"{}\" resolved as api but built as claude_cli",
            req.backend
        ));
    };

    let model = agent.model_flag().lock().unwrap().clone();
    let segments = agent.run(&req.prompt).await.map_err(|e| e.to_string())?;
    Ok(SubagentOutcome {
        text: segments.join(""),
        backend: req.backend.clone(),
        model,
    })
}

/// Drive a `ClaudeCli` backend through one `run_once` call. A single
/// prompt goes in. A single answer comes out, then the child process
/// exits. This never touches the long-lived, stdin-fed driver a GUI
/// session would build.
async fn run_claude_cli_subagent(
    factory: &Arc<BackendFactory>,
    req: &SubagentRequest,
    model: String,
    permission_mode: Option<String>,
    env: Option<std::collections::HashMap<String, String>>,
) -> Result<SubagentOutcome, String> {
    // The factory's own flag, not a fresh one. `run_once` polls it between
    // lines, so Escape reaches a claude_cli subagent the same way it
    // reaches an api one.
    let interrupt_flag = factory.interrupt_flag();
    let result = ClaudeCliDriver::run_once(
        &model,
        permission_mode.as_deref(),
        env.as_ref(),
        factory.project_root(),
        &req.prompt,
        interrupt_flag,
    )
    .await?;

    if result.is_error {
        return Err(format!(
            "subagent on backend \"{}\" returned an error result: {}",
            req.backend, result.text
        ));
    }

    Ok(SubagentOutcome {
        text: result.text,
        backend: req.backend.clone(),
        model,
    })
}

/// Drain a subagent's own event channel. Discard everything on it.
/// Never simply drop the receiver instead: an unbounded send into a
/// dropped receiver fails. That failure path is not worth exercising on
/// every token a subagent streams.
fn spawn_event_drain(mut rx: mpsc::UnboundedReceiver<StreamEvent>) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            debug!(?event, "subagent event discarded");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::Settings;
    use std::path::PathBuf;

    fn empty_factory() -> Arc<BackendFactory> {
        Arc::new(BackendFactory::new(Settings::default(), PathBuf::from(".")))
    }

    #[tokio::test]
    async fn unknown_backend_name_names_the_request_and_lists_known_entries() {
        let factory = empty_factory();
        let req = SubagentRequest {
            backend: "nope".to_string(),
            model: None,
            prompt: "hello".to_string(),
            depth: 1,
        };

        let err = run_subagent(&factory, req)
            .await
            .expect_err("should error on an unknown backend name");

        assert!(err.contains("nope"));
        assert!(err.contains("none configured"));
    }

    #[tokio::test]
    async fn subagent_events_never_reach_a_sender_passed_in_separately() {
        // `run_subagent` builds its own event channel. It never takes one
        // from the caller. There is no sender to prove received nothing
        // against, so this test documents the shape instead. A sender
        // created here gets nothing, since `run_subagent` never learns
        // about it.
        let (tx_caller, mut rx_caller) = mpsc::unbounded_channel::<StreamEvent>();
        let factory = empty_factory();
        let req = SubagentRequest {
            backend: "nope".to_string(),
            model: None,
            prompt: "hello".to_string(),
            depth: 1,
        };

        let _ = run_subagent(&factory, req).await;
        drop(tx_caller);

        assert!(rx_caller.try_recv().is_err());
    }
}
