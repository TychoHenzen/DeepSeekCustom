//! Builds an `Api` backend: an `AgentLoop` wired up with the tool
//! registry, memory, skills, and system prompt. The `claude_cli` path in
//! `BackendFactory::build` skips all of this.

use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use tokio::sync::mpsc;
use tracing::info;

use crate::agent::agent_loop::AgentLoop;
use crate::agent::agent_types::AgentConfig;
use crate::agent::events::RoutedEvent;
use crate::agent::prompt::SystemPromptBuilder;
use crate::api::client::ApiClient;
use crate::api::provider::Provider;
use crate::api::types::ToolDef;
use crate::autopilot::answerer::{PolicyAnswerer, QuestionAnswerer};
use crate::autopilot::policy::PolicyStore;
use crate::backend::Backend;
use crate::backend::codex_cli::CodexCliDriver;
use crate::backend::registry::SubagentRegistry;
#[cfg(feature = "test-support")]
use crate::backend::stub::StubBackend;
use crate::effort::Effort;
use crate::memory::MemoryStore;
use crate::skills::{SkillLoader, format_skills_for_prompt};
use crate::tools::ToolRegistry;
use crate::tools::{
    ask::AskUserQuestionTool, bash::BashTool, cd::CdTool, close_session::CloseSessionTool,
    edit::EditTool, glob::GlobTool, grep::GrepTool, read::ReadTool, read_image::ReadImageTool,
    reset::ResetTool, send_message::SendMessageTool, skill::SkillTool, task::TaskTool,
    write::WriteTool,
};

use super::factory::{BackendFactory, may_dispatch};
use super::resolved::{self, ResolvedBackend};

/// The agent's `answerer` built from the resolved backend.
fn build_answerer(
    provider: Provider,
    api_key: &str,
    base_url: &Option<String>,
    factory: &Arc<BackendFactory>,
    model: &str,
) -> Arc<dyn QuestionAnswerer> {
    let settings = &factory.settings;
    let project_root = &factory.project_root;
    let answerer_client = ApiClient::new(provider, api_key.to_string(), base_url.clone());
    let policy_store =
        PolicyStore::new(project_root.to_path_buf(), settings.autopilot_policy_path());
    Arc::new(PolicyAnswerer::new(
        answerer_client,
        policy_store,
        resolved::answerer_model(settings, provider, model),
    ))
}

/// Construction-time handles shared by the depth-gated tools and the
/// `AgentLoop` itself: the event sender, the subagent registry, and the
/// effort flag.
struct GatedToolCtx {
    tx_events: mpsc::UnboundedSender<RoutedEvent>,
    subagent_registry: Arc<SubagentRegistry>,
    effort_flag: Arc<AtomicU8>,
}

/// Register every built-in tool onto `tools`, plus MCP tools known so far.
fn register_tools(
    tools: &ToolRegistry,
    factory: &Arc<BackendFactory>,
    depth: u32,
    gated: &GatedToolCtx,
    skills: &Arc<Vec<crate::skills::Skill>>,
    answerer: &Arc<dyn QuestionAnswerer>,
) {
    let wd = factory.working_dir();
    tools.register(Arc::new(BashTool::new(wd.clone())));
    tools.register(Arc::new(ReadTool::new(wd.clone())));
    tools.register(Arc::new(ReadImageTool::new(wd.clone())));
    tools.register(Arc::new(WriteTool::new(wd.clone())));
    tools.register(Arc::new(EditTool::new(wd.clone())));
    tools.register(Arc::new(GlobTool::new(wd.clone())));
    tools.register(Arc::new(GrepTool::new(wd.clone())));
    tools.register(Arc::new(SkillTool::new(Arc::clone(skills))));
    tools.register(Arc::new(CdTool::new(factory.working_dir())));
    tools.register(Arc::new(ResetTool));
    tools.register(Arc::new(AskUserQuestionTool::new(Arc::clone(answerer))));
    let settings = &factory.settings;
    if may_dispatch(depth, settings.subagent_max_depth()) {
        tools.register(Arc::new(TaskTool::new(
            factory.clone(),
            depth + 1,
            gated.tx_events.clone(),
            gated.subagent_registry.clone(),
            gated.effort_flag.clone(),
        )));
        tools.register(Arc::new(SendMessageTool::new(
            gated.subagent_registry.clone(),
            settings.session_turn_cap(),
            settings.send_message_call_cap(),
        )));
        tools.register(Arc::new(CloseSessionTool::new(
            gated.subagent_registry.clone(),
        )));
    }
    factory.attach_mcp(tools);
}

/// Build the system prompt from memory and skills fragments, plus the tool
/// definitions gathered so far.
fn build_system_prompt(
    memory_fragment: &str,
    skills_fragment: &str,
    tool_defs: &[ToolDef],
) -> String {
    let memory_opt = if memory_fragment.is_empty() {
        None
    } else {
        Some(memory_fragment)
    };
    let skills_opt = if skills_fragment.is_empty() {
        None
    } else {
        Some(skills_fragment)
    };
    SystemPromptBuilder::new().build(memory_opt, skills_opt, tool_defs)
}

/// Wire up the `AgentLoop` with all its handles and wrap it in `Backend::Api`.
fn finish_agent(
    client: ApiClient,
    tools: ToolRegistry,
    system_prompt: String,
    config: AgentConfig,
    factory: &Arc<BackendFactory>,
    gated: GatedToolCtx,
) -> Backend {
    let settings = &factory.settings;
    let mut agent = AgentLoop::new(
        client,
        tools,
        system_prompt,
        config,
        factory.interrupt_flag(),
    );
    agent.set_event_sender(gated.tx_events);
    agent.set_working_dir(factory.working_dir());
    agent.set_subagent_registry(gated.subagent_registry);
    agent.set_effort_flag(gated.effort_flag);
    agent.set_style_config(
        settings.style_plain_language_enabled(),
        settings.style_target_grade(),
        settings.style_grade_tolerance(),
        settings.style_max_revise_attempts(),
        settings.style_critic_backend(),
    );
    Backend::Api(Box::new(agent))
}

/// Build the `Api` backend: an `AgentLoop` wired up with the tool
/// registry, memory, skills, and system prompt.
fn build_api_backend(
    provider: Provider,
    api_key: String,
    base_url: Option<String>,
    model: String,
    factory: &Arc<BackendFactory>,
    tx_events: mpsc::UnboundedSender<RoutedEvent>,
    depth: u32,
) -> Backend {
    let settings = &factory.settings;
    let project_root = &factory.project_root;
    let client = ApiClient::new(provider, api_key.clone(), base_url.clone());

    let memory = MemoryStore::load(project_root);
    let skills = Arc::new(SkillLoader::load(project_root));
    info!("loaded {} skills", skills.len());

    let answerer = build_answerer(provider, &api_key, &base_url, factory, &model);

    let subagent_registry = Arc::new(SubagentRegistry::new());

    let effort_flag: Arc<AtomicU8> = match factory.session_flags_for(depth) {
        Some(flags) => Arc::clone(&flags.effort),
        None => Arc::new(AtomicU8::new(Effort::None.to_u8())),
    };

    let tools = ToolRegistry::new();
    let gated = GatedToolCtx {
        tx_events,
        subagent_registry,
        effort_flag,
    };
    register_tools(&tools, factory, depth, &gated, &skills, &answerer);
    info!("registered {} tools", tools.list().len());

    let memory_fragment = memory.to_system_prompt_fragment();
    let skills_fragment = format_skills_for_prompt(&skills);
    let tool_defs = tools.to_api_definitions();
    let system_prompt = build_system_prompt(&memory_fragment, &skills_fragment, &tool_defs);
    info!(
        "system prompt built: {} chars, {} tools defined",
        system_prompt.len(),
        tool_defs.len(),
    );

    let config = AgentConfig {
        model,
        max_tokens: settings.max_tokens(),
        ..Default::default()
    };
    finish_agent(client, tools, system_prompt, config, factory, gated)
}

/// Build a `Backend` from an already-resolved config.
fn build_from_resolved(
    resolved: ResolvedBackend,
    factory: &Arc<BackendFactory>,
    tx_events: mpsc::UnboundedSender<RoutedEvent>,
    depth: u32,
) -> Backend {
    match resolved {
        ResolvedBackend::Api {
            name,
            provider,
            api_key,
            base_url,
            model,
        } => {
            info!(
                "resolved backend: name={} kind=api \
                 provider={:?} model={} base_url={}",
                name,
                provider,
                model,
                base_url.as_deref().unwrap_or("(provider default)"),
            );
            build_api_backend(
                provider, api_key, base_url, model, factory, tx_events, depth,
            )
        }
        ResolvedBackend::ClaudeCli {
            name,
            model,
            permission_mode,
            env,
        } => {
            info!(
                "resolved backend: name={} kind=claude_cli model={} \
                 permission_mode={}",
                name,
                model,
                permission_mode
                    .as_deref()
                    .unwrap_or("(default: bypassPermissions)"),
            );
            Backend::new_claude_cli(
                model,
                permission_mode,
                env,
                factory.working_dir(),
                tx_events,
            )
        }
        ResolvedBackend::CodexCli {
            name,
            model,
            sandbox,
            env,
        } => {
            info!(
                "resolved backend: name={} kind=codex_cli model={} sandbox={}",
                name,
                model,
                sandbox.as_deref().unwrap_or("(default)"),
            );
            Backend::CodexCli(Box::new(CodexCliDriver::new(
                model,
                sandbox,
                env,
                factory.working_dir(),
                tx_events,
            )))
        }
        #[cfg(feature = "test-support")]
        ResolvedBackend::Stub {
            name,
            script,
            model,
            cursor,
        } => {
            info!(
                "resolved backend: name={} kind=stub turns={}",
                name,
                script.len(),
            );
            let mut stub = StubBackend::new(script, model, factory.interrupt_flag(), cursor);
            stub.set_event_sender(tx_events);
            Backend::Stub(Box::new(stub))
        }
    }
}

/// Build a backend by name from the factory's `backends` map.
pub(crate) fn build_backend(
    factory: &Arc<BackendFactory>,
    name: &str,
    model_override: Option<&str>,
    tx_events: mpsc::UnboundedSender<RoutedEvent>,
    depth: u32,
) -> Result<Backend, String> {
    let resolved = factory.resolve(name, model_override)?;
    let mut backend = build_from_resolved(resolved, factory, tx_events, depth);
    if let Some(flags) = factory.session_flags_for(depth) {
        backend.adopt_flags(flags);
    }
    Ok(backend)
}
