use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use tokio::sync::mpsc;

use crate::agent::agent_types::AgentConfig;
use crate::agent::events::RoutedEvent;
use crate::agent::prompt::build_system_prompt;
use crate::api::client::ApiClient;
use crate::backend::registry::SubagentRegistry;
use crate::backend::{Backend, ToolPolicy};
use crate::effort::Effort;
use crate::tools::ToolRegistry;
use crate::tools::{
    edit::EditTool, glob::GlobTool, grep::GrepTool, read::ReadTool, read_image::ReadImageTool,
    write::WriteTool,
};

use super::build_api::{GatedToolCtx, finish_agent};
use super::factory::{BackendFactory, ControlledApiProfile};
use super::resolved::ResolvedBackend;

/// Build an API backend for one Controlled Development run.
pub(crate) fn build_controlled_api_backend(
    resolved: ResolvedBackend,
    factory: &Arc<BackendFactory>,
    tx_events: mpsc::UnboundedSender<RoutedEvent>,
    profile: ControlledApiProfile,
    root: PathBuf,
) -> Result<Backend, String> {
    let ResolvedBackend::Api {
        provider,
        api_key,
        base_url,
        model,
        ..
    } = resolved
    else {
        return Err("controlled API profile requires an API backend".to_string());
    };

    let client = ApiClient::new(provider, api_key, base_url);
    let tools = ToolRegistry::new();
    tools.register(Arc::new(ReadTool::rooted(root.clone())?));
    tools.register(Arc::new(ReadImageTool::rooted(root.clone())?));
    tools.register(Arc::new(GlobTool::rooted(root.clone())?));
    tools.register(Arc::new(GrepTool::rooted(root.clone())?));
    if profile == ControlledApiProfile::Execution {
        tools.register(Arc::new(WriteTool::rooted(root.clone())?));
        tools.register(Arc::new(EditTool::rooted(root)?));
    }

    let system_prompt = build_system_prompt(None, None, &tools.to_api_definitions());
    let gated = GatedToolCtx {
        tx_events,
        subagent_registry: Arc::new(SubagentRegistry::new()),
        effort_flag: Arc::new(AtomicU8::new(Effort::None.to_u8())),
    };
    let config = AgentConfig {
        model,
        max_tokens: factory.settings.max_tokens(),
        accept_exact_text_tool_calls: profile == ControlledApiProfile::Execution,
        ..Default::default()
    };
    Ok(finish_agent(
        client,
        tools,
        system_prompt,
        config,
        factory,
        gated,
        ToolPolicy::All,
    ))
}
