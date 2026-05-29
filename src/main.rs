use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use DeepSeekCustom::agent::agent_loop::{AgentConfig, AgentLoop, StreamEvent};
use DeepSeekCustom::agent::prompt::SystemPromptBuilder;
use DeepSeekCustom::api::client::{resolve_api_key, DeepSeekClient};
use DeepSeekCustom::config::settings::Settings;
use DeepSeekCustom::memory::MemoryStore;
use DeepSeekCustom::skills::{format_skills_for_prompt, SkillLoader};
use DeepSeekCustom::tools::ToolRegistry;
use DeepSeekCustom::tools::{bash::BashTool, read::ReadTool, reset::ResetTool, write::WriteTool};
use DeepSeekCustom::gui::DeepSeekGui;

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
    let project_root = find_project_root();

    // ── Logging: stderr + file ────────────────────────────────

    let log_path = project_root.join("deepseek_custom.log");
    let log_file = File::create(&log_path).expect("create log file");

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

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
    let gui = DeepSeekGui::new(rx_events, tx_input, interrupt_flag);

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

    info!("DeepSeekCustom harness shutting down");
}

fn debug_agent_input(input: &str) {
    let preview: String = input.chars().take(80).collect();
    let suffix = if input.chars().count() > 80 {
        "..."
    } else {
        ""
    };
    info!("agent received input ({} chars): {preview}{suffix}", input.len());
}
