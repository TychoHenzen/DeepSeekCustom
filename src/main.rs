use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;

use eframe::egui;
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use DeepSeekCustom::agent::agent_loop::{AgentConfig, AgentLoop, StreamEvent};
use DeepSeekCustom::agent::prompt::SystemPromptBuilder;
use DeepSeekCustom::api::client::{DeepSeekClient, resolve_api_key};
use DeepSeekCustom::config::settings::Settings;
use DeepSeekCustom::gui::DeepSeekGui;
use DeepSeekCustom::memory::MemoryStore;
use DeepSeekCustom::skills::{SkillLoader, format_skills_for_prompt};
use DeepSeekCustom::tools::ToolRegistry;
use DeepSeekCustom::tools::{bash::BashTool, read::ReadTool, reset::ResetTool, write::WriteTool};
use DeepSeekCustom::voice::service::{
    RealCaptureFactory, Speaker, Transcriber, VoiceCommand, VoiceEvent, VoiceService,
};
use DeepSeekCustom::voice::stt::WhisperEngine;
use DeepSeekCustom::voice::tts::TtsHandle;
use DeepSeekCustom::voice::{resolve_kokoro_paths, resolve_whisper_model_path};

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

    // ── API key ─────────────────────────────────────────────

    let api_key = match resolve_api_key(&project_root) {
        Ok(key) => key,
        Err(e) => {
            error!("{e}");
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    // ── Client ──────────────────────────────────────────────

    let model = settings.model();
    let client = DeepSeekClient::new(api_key, None, Some(model.clone()));

    // ── Memory & Skills ─────────────────────────────────────

    let memory = MemoryStore::load(&project_root);
    let project_skills = SkillLoader::load_all(&project_root).unwrap_or_default();
    let global_skills = SkillLoader::load_global().unwrap_or_default();
    let global_count = global_skills.len();
    let skills = SkillLoader::merge(project_skills, global_skills);
    let project_count = skills.len().saturating_sub(global_count);
    info!(
        "loaded {} skills ({} project, {} global)",
        skills.len(),
        project_count,
        global_count,
    );

    // ── Tool registry ───────────────────────────────────────

    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(BashTool::new(project_root.clone())));
    tools.register(Arc::new(ReadTool::new(project_root.clone())));
    tools.register(Arc::new(WriteTool::new(project_root.clone())));
    tools.register(Arc::new(ResetTool));
    info!("registered {} tools", tools.list().len());

    // ── Voice ───────────────────────────────────────────────

    let voice = setup_voice(&settings, &project_root);

    // ── System prompt ───────────────────────────────────────

    let memory_fragment = memory.to_system_prompt_fragment();
    let skills_fragment = format_skills_for_prompt(&skills);
    let tool_defs = tools.to_api_definitions();
    let system_prompt = SystemPromptBuilder::new().build(
        if memory_fragment.is_empty() {
            None
        } else {
            Some(memory_fragment.as_str())
        },
        if skills_fragment.is_empty() {
            None
        } else {
            Some(skills_fragment.as_str())
        },
        &tool_defs,
    );
    info!(
        "system prompt built: {} chars, {} tools defined",
        system_prompt.len(),
        tool_defs.len(),
    );

    // ── Agent loop ──────────────────────────────────────────

    let config = AgentConfig {
        model,
        ..Default::default()
    };
    let mut agent = AgentLoop::new(client, tools, system_prompt, config);

    // ── Channels ────────────────────────────────────────────

    let (tx_events, rx_events) = mpsc::unbounded_channel::<StreamEvent>();
    let (tx_input, mut rx_input) = mpsc::unbounded_channel::<String>();

    agent.set_event_sender(tx_events);

    // Share interrupt flag between GUI and agent
    let interrupt_flag = agent.interrupt_flag();
    let thinking_flag = agent.thinking_flag();
    // Seeded by `DeepSeekGui::new`, which sets the checkbox and this flag
    // together from the same settings value.
    let voice_mode_flag = agent.voice_mode_flag();
    let context_budget_flag = agent.context_budget_flag();
    let model_flag = agent.model_flag();

    // ── Seed agent flags from settings ──────────────────────

    let thinking_enabled = settings.thinking_enabled();
    let context_budget = settings.context_budget();
    thinking_flag.store(thinking_enabled, Ordering::SeqCst);
    context_budget_flag.store(context_budget, Ordering::SeqCst);
    info!("seeded from settings: thinking={thinking_enabled} context_budget={context_budget}");

    // ── Spawn agent task ────────────────────────────────────

    tokio::spawn(async move {
        info!("agent task started");
        while let Some(input) = rx_input.recv().await {
            debug_agent_input(&input);
            match agent.run(&input).await {
                Ok(responses) => {
                    info!("agent turn complete: {} response segments", responses.len());
                }
                Err(e) => {
                    error!("agent error: {e}");
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
        interrupt_flag,
        thinking_flag,
        voice_mode_flag,
        context_budget_flag,
        model_flag,
        settings.clone(),
        project_root.clone(),
    );

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
        Box::new(|_cc| Ok(Box::new(gui))),
    )
    .expect("GUI failed");

    // The GUI (and its voice command sender) has just been dropped, so the
    // forwarder's loop has already ended or is about to. Awaiting it here
    // blocks until the voice thread has actually shut down.
    if let Some(forwarder) = voice_forwarder {
        let _ = forwarder.await;
    }

    info!("DeepSeekCustom harness shutting down");
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
