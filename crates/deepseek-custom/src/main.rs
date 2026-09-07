use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;

use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use deepseek_custom::agent::agent_types::grade_to_u8;
use deepseek_custom::agent::events::{AgentCommand, RoutedEvent, StreamEvent};
use deepseek_custom::agent::repeat::RepeatCommand;
use deepseek_custom::application::actor::ChatLifecycle;
use deepseek_custom::application::controlled_development_service::{
    ControlledDevelopmentEffectRequest, ControlledDevelopmentServiceEvent,
    run_controlled_development_service,
};
use deepseek_custom::application::dto::AppSnapshot;
use deepseek_custom::application::services::{
    DomainCommandPort, RuntimeSettingsPort, SettingsController,
};
use deepseek_custom::application::session::ApplicationSession;
use deepseek_custom::application::session_state::{SessionOrigin, SessionState};
use deepseek_custom::backend::SharedFlags;
use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::registry::SubagentRegistry;
use deepseek_custom::config::settings::Settings;
use deepseek_custom::mcp::McpManager;
use deepseek_custom::procedure::ProcedureRunCoordinator;
use deepseek_custom::procedure::ProcedureRunCoordinatorParams;
use deepseek_custom::procedure::{
    FrontierRepairDispatcher, LocalPatchDraftDispatcher, LocalizationAgreementResolver,
    LocalizationDispatcher, LocalizationSampler, OpenSpecInput, PatchPreviewInputGate,
    PatchPreviewRunner, ProcedureApplyRunner, ProcedureCommand, ProcedureProgress,
    ProcedureReportRepository, SampledProcedureOutcome, SampledProcedureRequest,
    SampledProcedureRunner, SampledRepairContext, SamplingInputGate, VerificationInputGate,
    WholeChangeProcedureOutcome, WholeChangeProcedureRequest, WholeChangeProcedureRunner,
    apply_review_decision,
};
use deepseek_custom::search::{CascadeCounters, SearchCommand, run_cascade, run_evolve};
use deepseek_custom::session::SessionStore;
use deepseek_custom::voice::service::{
    RealCaptureFactory, Speaker, Transcriber, VoiceCommand, VoiceEvent, VoiceService,
};
use deepseek_custom::voice::stt::WhisperEngine;
use deepseek_custom::voice::tts::TtsHandle;
use deepseek_custom::voice::{resolve_kokoro_paths, resolve_whisper_model_path};
use deepseek_custom::web::server::{
    BindPolicy, BrowserOpener, SystemBrowser, SystemFolderPicker, WebAppState,
    start_with_policy_and_state,
};

/// Start the MCP servers Claude Code's own config files name, in the
/// background.
///
/// Returns at once, with a manager that may still have every server
/// starting. That is deliberate: a stdio server is a child process, and one
/// launched through `npx` can take minutes to answer its first message,
/// which would be minutes of a window that has not opened. A server's tools
/// reach the model on the first turn after it finishes starting.
fn start_mcp_servers(settings: &Settings, project_root: &Path) -> Arc<McpManager> {
    let manager = McpManager::new();
    if !settings.mcp_enabled() {
        info!("mcp: disabled by settings");
        return manager;
    }
    let disabled = settings.mcp_disabled_servers();
    let servers: Vec<_> = deepseek_custom::mcp::discover_servers(project_root)
        .into_iter()
        .filter(|s| !disabled.contains(&s.name))
        .collect();
    manager.start(servers);
    manager
}

/// Walk up from current directory looking for CLAUDE.md.
/// Falls back to current dir if not found.
fn find_project_root() -> PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if dir.join("CLAUDE.md").exists() {
            return dir;
        }
        if !dir.pop() {
            break;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[tokio::main]
async fn main() {
    // Skip kokoro-en's segment-level espeak-ng call. See
    // `voice::tts::normalize_for_synth` for why this is needed. This must
    // run before any other thread starts, since `env::set_var` is unsound
    // with concurrent readers.
    //
    // SAFETY: this is the first statement in `main`, before this
    // application spawns any thread of its own. Nothing else reads or
    // writes the process environment yet.
    unsafe {
        std::env::set_var("KOKORO_G2P_SEGMENT_ESPEAK", "0");
    }

    // Rebuild PATH from the registry, so every child this harness spawns can
    // find `node`, `npx`, and the rest whatever the launcher handed over.
    // See `path_repair` for the failure this fixes. It runs here, before the
    // logging layer, for the same reason the line above does, and reports
    // back so the result can be logged once logging is up.
    //
    // SAFETY: no thread of this application has started yet.
    let path_report = unsafe { deepseek_custom::path_repair::repair_path() };

    let project_root = find_project_root();

    // ── Logging: stderr + file ────────────────────────────────

    let log_path = project_root.join("deepseek_custom.log");
    let log_file = File::create(&log_path).expect("create log file");

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(fmt::layer().with_writer(log_file).with_ansi(false))
        .with(env_filter)
        .init();

    info!("DeepSeekCustom harness starting");
    info!("project root: {}", project_root.display());
    info!("log file: {}", log_path.display());
    info!(
        "PATH repair: {} entries / {} chars before, {} entries / {} chars after; node.exe: {}",
        path_report.before,
        path_report.before_len,
        path_report.after,
        path_report.after_len,
        path_report.node.as_deref().unwrap_or("not found")
    );

    // ── Config ──────────────────────────────────────────────

    let settings = match Settings::load(&project_root) {
        Ok(s) => s,
        Err(e) => {
            error!("failed to load settings: {e}");
            Settings::default()
        }
    };
    settings.log_redacted();

    // ── Backend factory ──────────────────────────────────────

    // Created up front and handed to the factory, so every backend and
    // subagent it builds, main session or `Task`-tool dispatch, shares
    // this one flag. Escape then reaches a running subagent too, not
    // just the turn in front of the user.
    // The handles the application keeps for the life of the process. Created here
    // rather than read back off the first backend, because the backend can
    // be replaced at runtime and the controls must keep driving whichever
    // one is current. See `SharedFlags`.
    let flags = SharedFlags::new(String::new());
    let mcp = start_mcp_servers(&settings, &project_root);
    let factory = Arc::new(
        BackendFactory::new(settings.clone(), project_root.clone())
            .with_interrupt_flag(Arc::clone(&flags.interrupt))
            .with_session_flags(flags.clone())
            .with_mcp(Arc::clone(&mcp)),
    );

    // The starting backend's own model, so the shared handle names the
    // model the first turn actually runs on rather than an empty string.
    let default_name = factory.default_backend_name();
    if let Some(cfg) = settings.resolve_backend(&default_name) {
        flags.set_model(cfg.model().to_string());
    }

    // The working directory the `Bash`, `Read`, `Write`, and `Cd` tools act
    // against. Starts equal to `project_root`. Seeded here from a saved
    // `working_dir` setting, if one is present and still a real directory.
    // An invalid or missing saved directory is not an error: it just leaves
    // the factory's default of `project_root` in place, logged at `warn`.
    let working_dir_flag = factory.working_dir();
    if let Some(saved_dir) = settings.working_dir() {
        let candidate = PathBuf::from(&saved_dir);
        if candidate.is_dir() {
            *working_dir_flag.lock().unwrap() = candidate;
        } else {
            tracing::warn!(
                "configured working_dir \"{saved_dir}\" is not a directory; \
                 staying at the project root"
            );
        }
    }

    // ── Channels ────────────────────────────────────────────

    let (tx_events, mut rx_events) = mpsc::unbounded_channel::<RoutedEvent>();
    let (tx_input, mut rx_input) = mpsc::unbounded_channel::<AgentCommand>();
    let (tx_repeat, mut rx_repeat) = mpsc::unbounded_channel::<RepeatCommand>();
    let (tx_search, mut rx_search) = mpsc::unbounded_channel::<SearchCommand>();
    let (tx_procedure, mut rx_procedure) = mpsc::unbounded_channel::<ProcedureCommand>();
    let (tx_procedure_progress, mut rx_procedure_progress) =
        mpsc::unbounded_channel::<ProcedureProgress>();
    let (tx_controlled_effects, rx_controlled_effects) =
        mpsc::unbounded_channel::<ControlledDevelopmentEffectRequest>();
    let (tx_controlled_events, mut rx_controlled_events) =
        mpsc::unbounded_channel::<ControlledDevelopmentServiceEvent>();
    let procedure_interrupt = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let controlled_service = tokio::spawn(run_controlled_development_service(
        Arc::clone(&factory),
        rx_controlled_effects,
        tx_controlled_events,
    ));

    // ── Backend construction ─────────────────────────────────

    let mut backend = match factory.build(&default_name, None, tx_events.clone(), 0) {
        Ok(b) => b,
        Err(e) => {
            error!("{e}");
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    // ── Voice ───────────────────────────────────────────────

    let voice = setup_voice(&settings, &project_root);

    // ── Seed agent flags from settings ──────────────────────

    let effort = settings.effort();
    let context_budget = settings.context_budget();
    effort.store(&flags.effort);
    flags.context_budget.store(context_budget, Ordering::SeqCst);
    // The style gate reads these two through the same shared handles, so
    // they are seeded here rather than left to `set_style_config`: a
    // depth-0 backend adopts the session flags after it is built, which
    // would otherwise drop whatever that call had just written.
    let plain_language = settings.style_plain_language_enabled();
    let target_grade = grade_to_u8(settings.style_target_grade());
    flags
        .style_plain_language
        .store(plain_language, Ordering::SeqCst);
    flags
        .style_target_grade
        .store(target_grade, Ordering::SeqCst);
    info!(
        "seeded from settings: effort={effort:?} context_budget={context_budget} \
         plain_language={plain_language} target_grade={target_grade}"
    );

    // ── Spawn agent task ────────────────────────────────────

    // Moved into the agent task so a `SwitchBackend` command can build a
    // replacement backend without going back to the GUI thread. The event
    // sender is cloned rather than moved for the same reason: every
    // backend this process ever builds streams onto the one channel the
    // GUI reads.
    let switch_factory = Arc::clone(&factory);
    let search_factory = Arc::clone(&factory);
    let preview_factory = Arc::clone(&factory);
    let search_interrupt = Arc::clone(&flags.search_interrupt);
    // The same two counters the status bar reads, so its escalation rate
    // covers every run this session made.
    let search_counters = CascadeCounters {
        total: Arc::clone(&flags.cascade_total),
        escalated: Arc::clone(&flags.cascade_escalated),
    };
    let switch_tx_events = tx_events;
    let repeat_project_root = project_root.clone();
    let procedure_project_root = project_root.clone();
    let procedure_settings = settings.clone();
    let procedure_working_dir = Arc::clone(&working_dir_flag);
    let procedure_task_interrupt = Arc::clone(&procedure_interrupt);

    tokio::spawn(async move {
        info!("agent task started");
        loop {
            tokio::select! {
                input = rx_input.recv() => {
                    match input {
                        Some(AgentCommand::UserTurn { text, image }) => {
                            debug_agent_input(&text);
                            match backend.run_with_image(&text, image.as_ref()).await {
                                Ok(responses) => {
                                    info!(
                                        "agent turn complete: {} response segments",
                                        responses.len()
                                    );
                                }
                                Err(e) => {
                                    error!("agent error: {e}");
                                    // A failed turn sends no TurnEnd of its
                                    // own, so nothing told the GUI the turn
                                    // was over: the status bar sat on
                                    // "Running..." and a held session switch
                                    // would have waited forever. Report the
                                    // failure, then close the turn.
                                    report_failed_turn(&switch_tx_events, &e.to_string());
                                }
                            }
                        }
                        Some(AgentCommand::NewSession) => {
                            info!("new session command received");
                            backend.start_new_session().await;
                        }
                        Some(AgentCommand::LoadSession { messages, claude_session_id }) => {
                            info!(
                                message_count = messages.len(),
                                "load session command received"
                            );
                            backend.load_session(messages, claude_session_id).await;
                        }
                        Some(AgentCommand::SwitchBackend { name, model }) => {
                            info!(backend = %name, model = ?model, "backend switch requested");
                            match switch_factory.build(&name, model.as_deref(), switch_tx_events.clone(), 0) {
                                Ok(replacement) => {
                                    // The outgoing backend goes first, so a
                                    // `claude -p` child is killed rather
                                    // than left running with nothing
                                    // reading its output.
                                    backend.shutdown().await;
                                    backend = replacement;
                                    info!(backend = %name, "backend switched");
                                }
                                Err(e) => {
                                    // The running backend is untouched, so
                                    // the session keeps working on the one
                                    // it already had.
                                    error!("backend switch failed: {e}");
                                }
                            }
                        }
                        None => break,
                    }
                }
                repeat = rx_repeat.recv() => {
                    match repeat {
                        Some(RepeatCommand { task, iterations }) => {
                            info!(iterations, "repeat command received");
                            backend.run_repeat(&task, iterations, &repeat_project_root).await;
                        }
                        None => break,
                    }
                }
                search = rx_search.recv() => {
                    match search {
                        // A search runs to completion here, on the same
                        // task that drives ordinary turns, so a run and a
                        // turn can never interleave their dispatches and
                        // one Stop can only mean the run that is going.
                        Some(command) => {
                            search_interrupt.store(false, Ordering::SeqCst);
                            match command {
                                SearchCommand::Cascade(params) => {
                                    info!(n = params.n, "cascade run received");
                                    run_cascade(
                                        &search_factory,
                                        *params,
                                        switch_tx_events.clone(),
                                        Arc::clone(&search_interrupt),
                                        search_counters.clone(),
                                    )
                                    .await;
                                }
                                SearchCommand::Evolve(params) => {
                                    info!(
                                        generations = params.generations,
                                        "evolve run received"
                                    );
                                    run_evolve(
                                        &search_factory,
                                        *params,
                                        switch_tx_events.clone(),
                                        Arc::clone(&search_interrupt),
                                    )
                                    .await;
                                }
                            }
                        }
                        None => break,
                    }
                }
                procedure = rx_procedure.recv() => {
                    match procedure {
                        Some(ProcedureCommand::Run {
                            run_id,
                            backend: selected_backend,
                            request,
                        }) => {
                            let mut run_settings = procedure_settings.clone();
                            run_settings.procedure_mut().localization_backend =
                                Some(selected_backend);
                            match LocalizationDispatcher::from_settings(
                                &run_settings,
                                &procedure_project_root,
                            ) {
                                Ok(dispatcher) => {
                                    let working_dir = procedure_working_dir
                                        .lock()
                                        .unwrap()
                                        .clone();
                                    let limits = run_settings
                                        .procedure()
                                        .map(|procedure| procedure.repository_index.clone())
                                        .unwrap_or_default();
                                    let runner = ProcedureRunCoordinator::new(
                                        ProcedureRunCoordinatorParams {
                                            input: OpenSpecInput::new(&procedure_project_root),
                                            working_dir,
                                            index_limits: limits,
                                            dispatcher,
                                            reports: ProcedureReportRepository::for_project(
                                                &procedure_project_root,
                                            ),
                                            interrupt: Arc::clone(&procedure_task_interrupt),
                                        },
                                    )
                                    .with_progress(tx_procedure_progress.clone());
                                    if let Err(error) = runner.run_with_id(run_id, request).await {
                                        let _ = tx_procedure_progress.send(
                                            ProcedureProgress::RunFailed {
                                                run_id,
                                                message: error.to_string(),
                                            },
                                        );
                                    }
                                }
                                Err(error) => {
                                    let _ = tx_procedure_progress.send(
                                        ProcedureProgress::RunFailed {
                                            run_id,
                                            message: error.to_string(),
                                        },
                                    );
                                }
                            }
                        }
                        Some(ProcedureCommand::Review { run_id, decision }) => {
                            apply_review_decision(
                                &ProcedureReportRepository::for_project(&procedure_project_root),
                                run_id,
                                decision,
                                &tx_procedure_progress,
                            );
                        }
                        Some(ProcedureCommand::Sampled { run_id, request }) => {
                            procedure_task_interrupt.store(false, Ordering::SeqCst);
                            let mut local_settings = procedure_settings.clone();
                            {
                                let procedure = local_settings.procedure_mut();
                                procedure.localization_backend = Some(request.local_backend.clone());
                                procedure.local_patch_backend = Some(request.local_backend.clone());
                                procedure.frontier_patch_backend = Some(request.frontier_backend.clone());
                            }
                            // Localization uses the schema-constrained Ollama adapter.
                            // The selected frontier backend remains reserved for bounded repair.
                            // A second localizer enforces the one-call disagreement cap.
                            let frontier_settings = local_settings.clone();
                            let prepared: Result<_, String> = (|| {
                                let local = LocalizationDispatcher::from_settings(
                                    &local_settings,
                                    &procedure_project_root,
                                ).map_err(|error| error.to_string())?;
                                let frontier = LocalizationDispatcher::from_settings(
                                    &frontier_settings,
                                    &procedure_project_root,
                                ).map_err(|error| error.to_string())?;
                                let patch = LocalPatchDraftDispatcher::from_resolved_backend(
                                    preview_factory.resolve(
                                        &request.local_backend,
                                        Some(&request.local_model),
                                    ).map_err(|error| error.to_string())?,
                                    deepseek_custom::effort::Effort::None,
                                    local_settings.max_tokens(),
                                ).map_err(|error| error.to_string())?;
                                let sampling = local_settings
                                    .validated_procedure_sampling_settings()
                                    .map_err(|error| error.to_string())?;
                                let repair = local_settings
                                    .validated_procedure_repair_policy()
                                    .map_err(|error| error.to_string())?;
                                Ok((
                                    local, frontier, patch, sampling, repair,
                                ))
                            })();
                            match prepared {
                                Ok((local, frontier, patch, sampling, repair_policy)) => {
                                    let limits = procedure_index_limits(&local_settings);
                                    let resolver = LocalizationAgreementResolver::new(
                                        LocalizationSampler::new(local, sampling.clone(), Arc::clone(&procedure_task_interrupt)),
                                        frontier,
                                    );
                                    let runner = SampledProcedureRunner::new(
                                        SamplingInputGate::new(
                                            OpenSpecInput::new(&procedure_project_root),
                                            procedure_project_root.clone(),
                                            ProcedureReportRepository::for_project(
                                                &procedure_project_root,
                                            ),
                                        ),
                                        procedure_project_root.clone(),
                                        limits,
                                        resolver,
                                        sampling,
                                        ProcedureReportRepository::for_project(
                                            &procedure_project_root,
                                        ),
                                        Arc::clone(&procedure_task_interrupt),
                                    );
                                    let registry = Arc::new(SubagentRegistry::new());
                                    let frontier_dispatcher = FrontierRepairDispatcher::new(
                                        Arc::clone(&preview_factory),
                                        procedure_project_root.clone(),
                                        Some(request.frontier_model.clone()),
                                        local_settings.effort(),
                                        switch_tx_events.clone(),
                                        registry,
                                    );
                                    let commands = procedure_verifier_commands(&local_settings);
                                    match runner.run(
                                        SampledProcedureRequest {
                                            baseline_localization_run_id: request.localization_run_id,
                                            change_id: request.change_id,
                                            task_id: request.task_id,
                                            route_override: request.route_override,
                                        },
                                        &patch,
                                        &commands,
                                        Some(SampledRepairContext {
                                            policy: repair_policy,
                                            frontier_dispatcher: Some(&frontier_dispatcher),
                                        }),
                                    ).await {
                                        Ok(outcome) => {
                                            let (disposition, message) = match outcome {
                                                SampledProcedureOutcome::Promoted { candidate_index } => (
                                                    deepseek_custom::procedure::ProcedureTerminalDisposition::Succeeded,
                                                    format!("Sampled candidate {candidate_index} passed and was promoted."),
                                                ),
                                                SampledProcedureOutcome::Repaired => (
                                                    deepseek_custom::procedure::ProcedureTerminalDisposition::Succeeded,
                                                    "All sampled candidates failed. The bounded repair ladder promoted a verified repair.".to_string(),
                                                ),
                                                SampledProcedureOutcome::NeedsBoundedRepair => (
                                                    deepseek_custom::procedure::ProcedureTerminalDisposition::Failed {
                                                        reason: "sampled candidates did not produce a promotable repair".to_string(),
                                                    },
                                                    "Sampled candidates and the bounded repair ladder did not produce a promotable patch.".to_string(),
                                                ),
                                                SampledProcedureOutcome::Interrupted => (
                                                    deepseek_custom::procedure::ProcedureTerminalDisposition::Interrupted,
                                                    "Sampled procedure was interrupted.".to_string(),
                                                ),
                                            };
                                            let _ = tx_procedure_progress.send(ProcedureProgress::SampledFinished { run_id, disposition, message });
                                        }
                                        Err(error) => {
                                            let _ = tx_procedure_progress.send(ProcedureProgress::RunFailed { run_id, message: error.to_string() });
                                        }
                                    }
                                }
                                Err(error) => {
                                    let _ = tx_procedure_progress.send(ProcedureProgress::RunFailed { run_id, message: error.to_string() });
                                }
                            }
                        }
                        Some(ProcedureCommand::WholeChange { run_id, request }) => {
                            procedure_task_interrupt.store(false, Ordering::SeqCst);
                            let mut local_settings = procedure_settings.clone();
                            {
                                let procedure = local_settings.procedure_mut();
                                procedure.localization_backend = Some(request.localization_backend.clone());
                                procedure.local_patch_backend = Some(request.local_backend.clone());
                                procedure.frontier_patch_backend = Some(request.frontier_backend.clone());
                            }
                            let frontier_settings = local_settings.clone();
                            let prepared: Result<_, String> = (|| {
                                let local = Arc::new(LocalizationDispatcher::from_settings(
                                    &local_settings,
                                    &procedure_project_root,
                                ).map_err(|error| error.to_string())?);
                                let frontier = Arc::new(LocalizationDispatcher::from_settings(
                                    &frontier_settings,
                                    &procedure_project_root,
                                ).map_err(|error| error.to_string())?);
                                let patch = LocalPatchDraftDispatcher::from_resolved_backend(
                                    preview_factory.resolve(
                                        &request.local_backend,
                                        Some(&request.local_model),
                                    ).map_err(|error| error.to_string())?,
                                    deepseek_custom::effort::Effort::None,
                                    local_settings.max_tokens(),
                                ).map_err(|error| error.to_string())?;
                                let sampling = local_settings
                                    .validated_procedure_sampling_settings()
                                    .map_err(|error| error.to_string())?;
                                let repair = local_settings
                                    .validated_procedure_repair_policy()
                                    .map_err(|error| error.to_string())?;
                                Ok((local, frontier, patch, sampling, repair))
                            })();
                            match prepared {
                                Ok((local, frontier, patch, sampling, repair_policy)) => {
                                    let limits = procedure_index_limits(&local_settings);
                                    let runner = WholeChangeProcedureRunner::new(
                                        ProcedureRunCoordinatorParams {
                                            input: OpenSpecInput::new(&procedure_project_root),
                                            working_dir: procedure_project_root.clone(),
                                            index_limits: limits,
                                            dispatcher: local,
                                            reports: ProcedureReportRepository::for_project(
                                                &procedure_project_root,
                                            ),
                                            interrupt: Arc::clone(&procedure_task_interrupt),
                                        },
                                        frontier,
                                        sampling,
                                    );
                                    let registry = Arc::new(SubagentRegistry::new());
                                    let frontier_dispatcher = FrontierRepairDispatcher::new(
                                        Arc::clone(&preview_factory),
                                        procedure_project_root.clone(),
                                        Some(request.frontier_model.clone()),
                                        local_settings.effort(),
                                        switch_tx_events.clone(),
                                        registry,
                                    );
                                    let commands = procedure_verifier_commands(&local_settings);
                                    match runner.run(
                                        WholeChangeProcedureRequest {
                                            change_id: request.change_id,
                                            route_override: request.route_override,
                                        },
                                        &patch,
                                        &commands,
                                        Some(SampledRepairContext {
                                            policy: repair_policy,
                                            frontier_dispatcher: Some(&frontier_dispatcher),
                                        }),
                                    ).await {
                                        Ok(outcome) => {
                                            let (disposition, message) = match outcome {
                                                WholeChangeProcedureOutcome::Completed { task_ids } => (
                                                    deepseek_custom::procedure::ProcedureTerminalDisposition::Succeeded,
                                                    format!("Completed {} unchecked task(s): {}.", task_ids.len(), task_ids.join(", ")),
                                                ),
                                                WholeChangeProcedureOutcome::Failed { task_id, reason } => (
                                                    deepseek_custom::procedure::ProcedureTerminalDisposition::Failed { reason: reason.clone() },
                                                    format!("Stopped at task {task_id}: {reason}"),
                                                ),
                                                WholeChangeProcedureOutcome::Interrupted { completed_task_ids } => (
                                                    deepseek_custom::procedure::ProcedureTerminalDisposition::Interrupted,
                                                    format!("Whole-change procedure was interrupted after {} task(s).", completed_task_ids.len()),
                                                ),
                                            };
                                            let _ = tx_procedure_progress.send(ProcedureProgress::SampledFinished { run_id, disposition, message });
                                        }
                                        Err(error) => {
                                            let _ = tx_procedure_progress.send(ProcedureProgress::RunFailed { run_id, message: error.to_string() });
                                        }
                                    }
                                }
                                Err(error) => {
                                    let _ = tx_procedure_progress.send(ProcedureProgress::RunFailed { run_id, message: error.to_string() });
                                }
                            }
                        }
                        Some(ProcedureCommand::Preview {
                            preview_id,
                            request,
                        }) => {
                            procedure_task_interrupt.store(false, Ordering::SeqCst);
                            let _ = tx_procedure_progress.send(
                                ProcedureProgress::PreviewStarted { preview_id },
                            );
                            let reports = ProcedureReportRepository::for_project(
                                &procedure_project_root,
                            );
                            let runner = PatchPreviewRunner::new(
                                PatchPreviewInputGate::new(
                                    OpenSpecInput::new(&procedure_project_root),
                                    procedure_project_root.clone(),
                                    reports,
                                ),
                                procedure_project_root.clone(),
                                Arc::clone(&preview_factory),
                                Arc::clone(&procedure_task_interrupt),
                                procedure_settings.effort(),
                                procedure_settings.max_tokens(),
                            );
                            match runner.run(preview_id, request).await {
                                Ok((preview, report_path)) => {
                                    let _ = tx_procedure_progress.send(
                                        ProcedureProgress::PreviewFinished {
                                            preview_id,
                                            preview: Box::new(preview),
                                            report_path,
                                        },
                                    );
                                }
                                Err(error) => {
                                    let _ = tx_procedure_progress.send(
                                        ProcedureProgress::PreviewFailed {
                                            preview_id,
                                            message: error.to_string(),
                                        },
                                    );
                                }
                            }
                        }
                        Some(ProcedureCommand::Apply { run_id, request }) => {
                            procedure_task_interrupt.store(false, Ordering::SeqCst);
                            let commands = procedure_settings
                                .procedure()
                                .map(|procedure| procedure.verifier_commands.clone())
                                .unwrap_or_default();
                            let runner = ProcedureApplyRunner::new(
                                VerificationInputGate::new(
                                    OpenSpecInput::new(&procedure_project_root),
                                    procedure_project_root.clone(),
                                    ProcedureReportRepository::for_project(
                                        &procedure_project_root,
                                    ),
                                ),
                                procedure_project_root.clone(),
                                Arc::clone(&procedure_task_interrupt),
                            )
                            .with_progress(tx_procedure_progress.clone());
                            if let Err(error) = runner.run(run_id, request, &commands).await {
                                error!(apply_run_id = %run_id.as_str(), "procedure Apply failed: {error}");
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        info!("agent task shutting down");
    });

    // ── Run the loopback web application ────────────────────

    let selected_model = flags.model.lock().unwrap().clone();
    let origin = SessionOrigin {
        backend: default_name.clone(),
        model: selected_model.clone(),
    };
    let session = ApplicationSession::new(SessionState::new(
        SessionStore::for_project(&project_root),
        origin.clone(),
    ));
    let runtime_settings = RuntimeSettingsPort::new(
        project_root.clone(),
        Arc::clone(&flags.effort),
        Arc::clone(&flags.voice_mode),
        Arc::clone(&flags.context_budget),
        Arc::clone(&flags.model),
        Arc::clone(&working_dir_flag),
        Arc::clone(&flags.style_plain_language),
        Arc::clone(&flags.style_target_grade),
    );
    let settings_controller = Arc::new(SettingsController::new(
        project_root.clone(),
        settings.clone(),
        runtime_settings,
        Some(default_name.clone()),
        Some(selected_model),
    ));
    let snapshot = AppSnapshot::initial(settings_controller.visible(), session.session_summary());
    let mut web_state = WebAppState::with_settings(
        snapshot,
        256,
        Arc::clone(&settings_controller),
        Arc::new(SystemFolderPicker),
    )
    .with_chat_lifecycle(ChatLifecycle::new(
        session,
        DomainCommandPort::new(tx_input),
        Arc::clone(&flags.interrupt),
        origin,
    ))
    .with_autopilot_port(
        DomainCommandPort::new(tx_repeat),
        Arc::clone(&flags.repeat_interrupt),
    )
    .with_search_port(
        DomainCommandPort::new(tx_search),
        Arc::clone(&flags.search_interrupt),
    )
    .with_procedure_port(
        DomainCommandPort::new(tx_procedure),
        Arc::clone(&procedure_interrupt),
    )
    .with_controlled_development_port(DomainCommandPort::new(tx_controlled_effects))
    .with_test_control(project_root.clone());

    let (voice_forwarder, voice_event_forwarder) = if let Some(runtime) = voice {
        let VoiceRuntime {
            mut events_rx,
            service,
            tts_worker,
        } = runtime;
        let (tx_voice_cmd, rx_voice_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
        let shutdown_voice = tx_voice_cmd.clone();
        web_state = web_state.with_voice_port(DomainCommandPort::new(tx_voice_cmd));
        let state = web_state.clone();
        let event_forwarder = tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                let _ = state.apply_voice_event(event);
            }
        });
        (
            Some((
                spawn_voice_command_forwarder(rx_voice_cmd, service, tts_worker),
                shutdown_voice,
            )),
            Some(event_forwarder),
        )
    } else {
        (None, None)
    };

    let event_state = web_state.clone();
    let event_forwarder = tokio::spawn(async move {
        while let Some(routed) = rx_events.recv().await {
            let _ = event_state.apply_routed_stream_event(routed);
        }
    });
    let procedure_state = web_state.clone();
    let procedure_forwarder = tokio::spawn(async move {
        while let Some(progress) = rx_procedure_progress.recv().await {
            let _ = procedure_state.apply_procedure_progress(&progress);
        }
    });
    let controlled_state = web_state.clone();
    let controlled_forwarder = tokio::spawn(async move {
        while let Some(event) = rx_controlled_events.recv().await {
            let _ = controlled_state.apply_controlled_development_event(event);
        }
    });
    let model_discovery_state = web_state.clone();
    let model_discovery = tokio::spawn(async move {
        match model_discovery_state.refresh_models().await {
            Ok(revision) => info!(revision = revision.0, "backend model discovery completed"),
            Err(error) => warn!("backend model discovery failed: {}", error.message),
        }
    });

    let browser: Option<Arc<dyn BrowserOpener>> = std::env::var_os("DEEPSEEK_DISABLE_BROWSER")
        .is_none()
        .then(|| Arc::new(SystemBrowser) as Arc<dyn BrowserOpener>);
    let server = start_with_policy_and_state(
        BindPolicy::preferred_loopback(8765, true),
        browser,
        web_state.clone(),
    )
    .await
    .expect("web application failed to start");
    info!(url = server.url(), "web application ready");
    println!("DeepSeekCustom web application: {}", server.url());
    tokio::signal::ctrl_c()
        .await
        .expect("failed to wait for shutdown signal");
    server
        .shutdown()
        .await
        .expect("web application shutdown failed");
    event_forwarder.abort();
    procedure_forwarder.abort();
    controlled_service.abort();
    controlled_forwarder.abort();
    model_discovery.abort();
    let _ = event_forwarder.await;
    let _ = procedure_forwarder.await;
    let _ = controlled_service.await;
    let _ = controlled_forwarder.await;
    let _ = model_discovery.await;
    if let Some(forwarder) = voice_event_forwarder {
        forwarder.abort();
        let _ = forwarder.await;
    }
    drop(web_state);

    if let Some((forwarder, shutdown_voice)) = voice_forwarder {
        let _ = shutdown_voice.send(VoiceCommand::Shutdown);
        drop(shutdown_voice);
        let _ = forwarder.await;
    }

    // Kill the MCP servers on the way out. The job object in
    // `src/process_group.rs` would reap them regardless, and does when this
    // process is killed rather than closed. Doing it here as well makes a
    // normal exit deterministic instead of leaving seven child processes to
    // the kernel's timing.
    mcp.shutdown().await;

    info!("DeepSeekCustom harness shutting down");
}

/// Tell the application actor that a turn failed and is over.
///
/// Two events, because neither one alone says both things. `Error` puts the
/// failure in the transcript, and it is not terminal on its own: it also
/// fires mid-turn for a dropped image attachment. `TurnEnd` is what closes
/// the turn, clears the status bar, and releases a session switch that was
/// waiting on it. The token counts are zero because a turn that failed
/// reported none.
fn report_failed_turn(tx_events: &mpsc::UnboundedSender<RoutedEvent>, message: &str) {
    let _ = tx_events.send(RoutedEvent::own(StreamEvent::Error {
        message: format!("Turn failed: {message}"),
    }));
    let _ = tx_events.send(RoutedEvent::own(StreamEvent::TurnEnd {
        turn: 0,
        finish_reason: "error".to_string(),
        total_tokens: 0,
        prompt_cache_hit_tokens: 0,
        prompt_cache_miss_tokens: 0,
    }));
}

fn debug_agent_input(input: &str) {
    let preview: String = input.chars().take(80).collect();
    let suffix = if input.chars().count() > 80 {
        "..."
    } else {
        ""
    };
    info!(
        "agent received input ({} chars): {preview}{suffix}",
        input.len()
    );
}

/// Voice subsystem pieces `main` wires into the web application. `tts_worker` is
/// `None` whenever text to speech never started, so nothing needs joining
/// at shutdown.
struct VoiceRuntime {
    events_rx: mpsc::UnboundedReceiver<VoiceEvent>,
    service: VoiceService,
    tts_worker: Option<thread::JoinHandle<()>>,
}

/// Build the voice subsystem from `settings`, or return `None` when voice
/// is disabled. Speech to text and text to speech resolve independently.
/// A missing model disables only its half, never the whole subsystem.
/// `VoiceService::start` cannot fail, so the app never fails to start
/// because of voice.
fn setup_voice(settings: &Settings, project_root: &Path) -> Option<VoiceRuntime> {
    if !settings.voice_enabled() {
        info!("voice: disabled in settings");
        return None;
    }
    let transcriber = build_transcriber(settings, project_root);
    let (speaker, tts_worker) = build_speaker(settings, project_root);
    log_voice_config(settings, transcriber.is_some(), speaker.is_some());
    let (service, events_rx) = VoiceService::start(
        Box::new(RealCaptureFactory),
        transcriber,
        speaker,
        settings.voice_trigger_mode(),
        settings.voice_wake_phrase(),
    );
    Some(VoiceRuntime {
        events_rx,
        service,
        tts_worker,
    })
}

/// Resolve and load the whisper speech-to-text model, if speech to text is
/// enabled and the model can be found. A resolution failure is already
/// logged by `resolve_whisper_model_path`. A load failure is logged here.
fn build_transcriber(settings: &Settings, project_root: &Path) -> Option<Box<dyn Transcriber>> {
    if !settings.voice_stt_enabled() {
        return None;
    }
    let model_path =
        resolve_whisper_model_path(settings.voice_stt_model_path().as_deref(), project_root)?;
    match WhisperEngine::new(&model_path) {
        Ok(engine) => Some(Box::new(engine)),
        Err(e) => {
            error!(
                "voice: failed to load whisper model at {}: {e}",
                model_path.display()
            );
            None
        }
    }
}

/// Resolve and start the Kokoro text-to-speech worker, if text to speech
/// is enabled and both the model and voice packs can be found. A
/// resolution failure is already logged by `resolve_kokoro_paths`.
fn build_speaker(
    settings: &Settings,
    project_root: &Path,
) -> (Option<Box<dyn Speaker>>, Option<thread::JoinHandle<()>>) {
    if !settings.voice_tts_enabled() {
        return (None, None);
    }
    let Some((model_path, voices_path)) = resolve_kokoro_paths(
        settings.voice_tts_model_path().as_deref(),
        settings.voice_tts_voices_path().as_deref(),
        project_root,
    ) else {
        return (None, None);
    };
    let (handle, worker) = TtsHandle::start(model_path, voices_path);
    handle.set_voice(settings.voice_tts_voice());
    handle.set_speed(settings.voice_tts_speed());
    (Some(Box::new(handle)), Some(worker))
}

/// Log the resolved voice configuration at startup: the enabled flags, the
/// trigger mode, the configured model paths, and the voice id. `stt_ready`
/// and `tts_ready` report whether each half actually came up, not just
/// whether it was requested in settings.
fn log_voice_config(settings: &Settings, stt_ready: bool, tts_ready: bool) {
    info!(
        "voice config: enabled={} stt_enabled={} stt_ready={} tts_enabled={} \
         tts_ready={} trigger_mode={:?} wake_phrase=\"{}\" stt_model={:?} \
         tts_model={:?} tts_voices={:?} voice_id={}",
        settings.voice_enabled(),
        settings.voice_stt_enabled(),
        stt_ready,
        settings.voice_tts_enabled(),
        tts_ready,
        settings.voice_trigger_mode(),
        settings.voice_wake_phrase(),
        settings.voice_stt_model_path(),
        settings.voice_tts_model_path(),
        settings.voice_tts_voices_path(),
        settings.voice_tts_voice(),
    );
}

fn procedure_index_limits(
    settings: &Settings,
) -> deepseek_custom::config::settings::RepositoryIndexLimits {
    settings
        .procedure()
        .map(|procedure| procedure.repository_index.clone())
        .unwrap_or_default()
}

fn procedure_verifier_commands(settings: &Settings) -> Vec<String> {
    settings
        .procedure()
        .map(|procedure| procedure.verifier_commands.clone())
        .unwrap_or_default()
}

/// Forward web application voice commands into the synchronous `VoiceService`, until
/// the application drops its sender. That closes `rx_voice_cmd`. That closed
/// channel is this task's signal that the server has exited. It then shuts
/// the voice service and its text-to-speech worker down cleanly. That
/// runs on a blocking task. The join calls inside `VoiceService::shutdown`
/// must never stall the async runtime.
fn spawn_voice_command_forwarder(
    mut rx_voice_cmd: mpsc::UnboundedReceiver<VoiceCommand>,
    mut service: VoiceService,
    tts_worker: Option<thread::JoinHandle<()>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(cmd) = rx_voice_cmd.recv().await {
            service.send(cmd);
        }
        info!("voice: web application closed, shutting down voice service");
        let _ = tokio::task::spawn_blocking(move || {
            service.shutdown();
            if let Some(worker) = tts_worker {
                let _ = worker.join();
            }
        })
        .await;
        info!("voice: shut down cleanly");
    })
}
