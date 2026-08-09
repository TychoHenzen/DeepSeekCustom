use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;

use eframe::egui;
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use deepseek_custom::agent::agent_loop::{AgentCommand, RoutedEvent, StreamEvent};
use deepseek_custom::agent::repeat::RepeatCommand;
use deepseek_custom::backend::SharedFlags;
use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::config::settings::Settings;
use deepseek_custom::gui::DeepSeekGui;
use deepseek_custom::gui::agent_handles::AgentHandles;
use deepseek_custom::mcp::McpManager;
use deepseek_custom::voice::service::{
    RealCaptureFactory, Speaker, Transcriber, VoiceCommand, VoiceEvent, VoiceService,
};
use deepseek_custom::voice::stt::WhisperEngine;
use deepseek_custom::voice::tts::TtsHandle;
use deepseek_custom::voice::{resolve_kokoro_paths, resolve_whisper_model_path};

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
    // The handles the GUI keeps for the life of the process. Created here
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

    let (tx_events, rx_events) = mpsc::unbounded_channel::<RoutedEvent>();
    let (tx_input, mut rx_input) = mpsc::unbounded_channel::<AgentCommand>();
    let (tx_repeat, mut rx_repeat) = mpsc::unbounded_channel::<RepeatCommand>();

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
    info!("seeded from settings: effort={effort:?} context_budget={context_budget}");

    // ── Spawn agent task ────────────────────────────────────

    // Moved into the agent task so a `SwitchBackend` command can build a
    // replacement backend without going back to the GUI thread. The event
    // sender is cloned rather than moved for the same reason: every
    // backend this process ever builds streams onto the one channel the
    // GUI reads.
    let switch_factory = Arc::clone(&factory);
    let switch_tx_events = tx_events;
    let repeat_project_root = project_root.clone();

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
            }
        }
        info!("agent task shutting down");
    });

    // ── Run GUI (blocking, main thread) ─────────────────────

    info!("starting GUI");
    let mut gui = DeepSeekGui::new(
        rx_events,
        tx_input,
        AgentHandles {
            interrupt: Arc::clone(&flags.interrupt),
            effort: Arc::clone(&flags.effort),
            voice_mode: Arc::clone(&flags.voice_mode),
            context_budget: Arc::clone(&flags.context_budget),
            model: Arc::clone(&flags.model),
            working_dir: working_dir_flag,
            cascade_total: Arc::clone(&flags.cascade_total),
            cascade_escalated: Arc::clone(&flags.cascade_escalated),
        },
        settings.clone(),
        project_root.clone(),
    )
    .with_repeat(tx_repeat, Arc::clone(&flags.repeat_interrupt));

    let voice_forwarder = if let Some(v) = voice {
        let (tx_voice_cmd, rx_voice_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
        gui = gui.with_voice(v.events_rx, tx_voice_cmd);
        Some(spawn_voice_command_forwarder(
            rx_voice_cmd,
            v.service,
            v.tts_worker,
        ))
    } else {
        None
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 768.0])
            .with_title("DeepSeekCustom"),
        ..Default::default()
    };

    eframe::run_native(
        "DeepSeekCustom",
        native_options,
        Box::new(|cc| {
            // Installs the loader that turns `egui::Image::from_bytes` into
            // an actual texture, for the transcript's `Image` block (see
            // `render_image_block` in `src/gui/mod.rs`). Without this call
            // that widget silently shows nothing: the bytes reach the
            // context, but no loader is registered to decode them.
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(gui))
        }),
    )
    .expect("GUI failed");

    // The GUI (and its voice command sender) has just been dropped, so the
    // forwarder's loop has already ended or is about to. Awaiting it here
    // blocks until the voice thread has actually shut down.
    if let Some(forwarder) = voice_forwarder {
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

/// Tell the GUI that a turn failed and is over.
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

/// Voice subsystem pieces `main` wires into the GUI. `tts_worker` is
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

/// Forward GUI voice commands into the synchronous `VoiceService`, until
/// the GUI drops its sender. That closes `rx_voice_cmd`. That closed
/// channel is this task's signal that the GUI has exited. It then shuts
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
        info!("voice: GUI closed, shutting down voice service");
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
