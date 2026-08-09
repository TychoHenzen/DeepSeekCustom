use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing::{debug, info, warn};

use crate::effort::Effort;
use crate::error::{HarnessError, Result};

/// Top-level settings (deserialized from settings.json).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<PermissionsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HooksConfig>,
    /// The reasoning-effort level, shared across every backend. Replaces
    /// the old `thinking` block (`enabled: bool`, one bit for what is now
    /// a five-level control). A `settings.json` that still carries a
    /// `thinking` block is not an error: `serde` drops the unknown field
    /// silently, and this field just defaults to `None`, i.e.
    /// `Effort::None`. The old value is not migrated, on purpose: see
    /// `Settings::effort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<VoiceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autopilot: Option<AutopilotConfig>,
    /// Context pruning high-water mark, in tokens. Clamped 32000-200000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_budget: Option<usize>,
    /// Whether the GUI shows raw output instead of rendered markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_raw_output: Option<bool>,
    /// Named backend configurations, keyed by an arbitrary id chosen in
    /// `settings.json` (e.g. "deepseek", "ollama", "claude").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backends: Option<HashMap<String, BackendConfig>>,
    /// Which entry in `backends` is active by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_backend: Option<String>,
    /// Depth limit for subagent dispatch through the `Task` tool.
    /// Defaults to 2. See `Settings::subagent_max_depth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_max_depth: Option<u32>,
    /// Cap on turns within one `SendMessage`-kept-open subagent session,
    /// counting the turn that opened it. Defaults to 20. See
    /// `Settings::session_turn_cap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_turn_cap: Option<u32>,
    /// Cap on total `SendMessage` calls during one parent turn, across
    /// every session that parent has open. Defaults to 10. See
    /// `Settings::send_message_call_cap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_message_call_cap: Option<u32>,
    /// Where the `Bash`, `Read`, and `Write` tools act, as distinct from
    /// `project_root`. `None` means the tools act at `project_root`. See
    /// the phase 4 section of `docs/plans/2026-08-04-long-term-roadmap.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// MCP servers for the `Api` backend. See `Settings::mcp_enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpSettings>,
}

impl Settings {
    /// Load settings from standard locations, merging with defaults.
    ///
    /// Search order (later overrides earlier):
    /// 1. `<project_root>/settings.json`
    /// 2. `~/.claude/settings.json`
    /// 3. `~/.deepseek/settings.json`
    ///
    /// Missing files are not an error, defaults apply.
    pub fn load(project_root: &Path) -> Result<Self> {
        let mut settings = Self::default();

        // 1. Project settings
        let project_settings = project_root.join("settings.json");
        if project_settings.exists() {
            debug!("loading project settings: {}", project_settings.display());
            if let Some(project) = Self::load_file(&project_settings) {
                settings.merge(project);
            }
        }

        // 2. Global ~/.claude/settings.json
        if let Some(claude_settings) = Self::home_settings(".claude") {
            debug!("loading ~/.claude/settings.json");
            if let Some(global) = Self::load_file(&claude_settings) {
                settings.merge(global);
            }
        }

        // 3. Global ~/.deepseek/settings.json
        if let Some(ds_settings) = Self::home_settings(".deepseek") {
            debug!("loading ~/.deepseek/settings.json");
            if let Some(ds) = Self::load_file(&ds_settings) {
                settings.merge(ds);
            }
        }

        Ok(settings)
    }

    /// Write these settings to `<project_root>/settings.json` as pretty JSON.
    pub fn save(&self, project_root: &Path) -> Result<()> {
        let path = project_root.join("settings.json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| HarnessError::Config(format!("failed to serialize settings: {e}")))?;
        std::fs::write(&path, json)?;
        debug!("saved settings: {}", path.display());
        Ok(())
    }

    /// Log the loaded config, redacting the API key.
    pub fn log_redacted(&self) {
        let backend = self.default_backend().unwrap_or("deepseek");
        info!(
            "config loaded: backend={}, api_key={}, permissions={}, hooks={}, effort={:?}, voice={}",
            backend,
            if self.api_key.is_some() {
                "***REDACTED***"
            } else {
                "not set"
            },
            self.permissions.is_some(),
            self.hooks.is_some(),
            self.effort(),
            self.voice_enabled(),
        );
    }

    /// Context pruning high-water mark in tokens, clamped to 32000-200000.
    ///
    /// Defaults to 100000, matching `DEFAULT_CONTEXT_BUDGET` in the agent loop.
    /// The bounds match the range of the GUI slider.
    pub fn context_budget(&self) -> usize {
        self.context_budget
            .unwrap_or(100_000)
            .clamp(32_000, 200_000)
    }

    /// Whether the GUI shows raw output instead of rendered markdown.
    pub fn show_raw_output(&self) -> bool {
        self.show_raw_output.unwrap_or(false)
    }

    /// The configured working directory override, if any. `None` means the
    /// tools act at `project_root`. A caller that seeds the shared working
    /// directory at startup decides for itself what an invalid or missing
    /// path here should fall back to; this accessor only reports what was
    /// saved.
    pub fn working_dir(&self) -> Option<String> {
        self.working_dir.clone()
    }

    /// The current reasoning-effort level, defaulting to `Effort::None`.
    pub fn effort(&self) -> Effort {
        self.effort.unwrap_or(Effort::None)
    }

    /// Whether the voice subsystem is enabled at all.
    pub fn voice_enabled(&self) -> bool {
        self.voice.as_ref().map(|v| v.enabled).unwrap_or(false)
    }

    /// Whether speech-to-text is enabled (implies `voice_enabled()`).
    pub fn voice_stt_enabled(&self) -> bool {
        self.voice_enabled() && self.voice.as_ref().map(|v| v.stt_enabled).unwrap_or(false)
    }

    /// Whether text-to-speech is enabled (implies `voice_enabled()`).
    pub fn voice_tts_enabled(&self) -> bool {
        self.voice_enabled() && self.voice.as_ref().map(|v| v.tts_enabled).unwrap_or(false)
    }

    /// Path to the whisper GGML speech-to-text model file, if configured.
    pub fn voice_stt_model_path(&self) -> Option<String> {
        self.voice.as_ref().and_then(|v| v.stt_model_path.clone())
    }

    /// Path to the Kokoro ONNX text-to-speech model file, if configured.
    pub fn voice_tts_model_path(&self) -> Option<String> {
        self.voice.as_ref().and_then(|v| v.tts_model_path.clone())
    }

    /// Path to the Kokoro voice packs directory, if configured.
    pub fn voice_tts_voices_path(&self) -> Option<String> {
        self.voice.as_ref().and_then(|v| v.tts_voices_path.clone())
    }

    /// Effective trigger mode, defaulting to push-to-talk.
    pub fn voice_trigger_mode(&self) -> TriggerMode {
        self.voice
            .as_ref()
            .map(|v| v.trigger_mode)
            .unwrap_or(TriggerMode::PushToTalk)
    }

    /// Effective wake phrase, defaulting to "hey deepseek".
    pub fn voice_wake_phrase(&self) -> String {
        self.voice
            .as_ref()
            .and_then(|v| v.wake_phrase.clone())
            .unwrap_or_else(|| "hey deepseek".to_string())
    }

    /// Effective Kokoro voice id, defaulting to `af_heart`.
    pub fn voice_tts_voice(&self) -> String {
        self.voice
            .as_ref()
            .and_then(|v| v.tts_voice.clone())
            .unwrap_or_else(|| "af_heart".to_string())
    }

    /// Effective Kokoro speaking speed, defaulting to 1.0, clamped 0.5-2.0.
    pub fn voice_tts_speed(&self) -> f32 {
        self.voice
            .as_ref()
            .and_then(|v| v.tts_speed)
            .unwrap_or(1.0)
            .clamp(0.5, 2.0)
    }

    /// The voice block, created with defaults if it is not there yet.
    ///
    /// A panel control changing a voice field must not silently drop the
    /// write just because `settings.json` had no voice block.
    pub fn voice_mut(&mut self) -> &mut VoiceConfig {
        self.voice.get_or_insert_with(VoiceConfig::default)
    }

    /// Number of times autopilot repeats the task, defaulting to 5.
    pub fn autopilot_iterations(&self) -> u32 {
        self.autopilot
            .as_ref()
            .and_then(|a| a.iterations)
            .unwrap_or(5)
    }

    /// Path to the autopilot policy file, if configured. Callers fall back
    /// to `<project_root>/autopilot-policy.md` when this is `None`.
    pub fn autopilot_policy_path(&self) -> Option<String> {
        self.autopilot.as_ref().and_then(|a| a.policy_path.clone())
    }

    /// Model used by the answerer that responds to AskUserQuestion calls
    /// during an autopilot run, defaulting to `deepseek-v4-flash`.
    pub fn autopilot_answerer_model(&self) -> String {
        self.autopilot_answerer_model_override()
            .unwrap_or_else(|| "deepseek-v4-flash".to_string())
    }

    /// The answerer model exactly as configured, with no default applied.
    /// A caller that must tell "the user picked deepseek-v4-flash" apart
    /// from "the user picked nothing" needs this, since the default only
    /// fits a DeepSeek backend.
    pub fn autopilot_answerer_model_override(&self) -> Option<String> {
        self.autopilot
            .as_ref()
            .and_then(|a| a.answerer_model.clone())
    }

    /// The last-used autopilot task text, so the GUI can restore it.
    pub fn autopilot_task(&self) -> Option<String> {
        self.autopilot.as_ref().and_then(|a| a.task.clone())
    }

    /// The autopilot block, created with defaults if it is not there yet.
    /// See [`Settings::voice_mut`].
    pub fn autopilot_mut(&mut self) -> &mut AutopilotConfig {
        self.autopilot.get_or_insert_with(AutopilotConfig::default)
    }

    /// The configured backends map, if any.
    pub fn backends(&self) -> Option<&HashMap<String, BackendConfig>> {
        self.backends.as_ref()
    }

    /// The name of the default backend, if set.
    pub fn default_backend(&self) -> Option<&str> {
        self.default_backend.as_deref()
    }

    /// Look up one backend by name.
    pub fn resolve_backend(&self, name: &str) -> Option<&BackendConfig> {
        self.backends.as_ref().and_then(|b| b.get(name))
    }

    /// Depth limit for subagent dispatch through the `Task` tool.
    /// Defaults to 2. Depth 0 is the main session. The default lets it
    /// dispatch a depth-1 subagent. That subagent may dispatch one more
    /// at depth 2. Depth 2 may not dispatch further.
    pub fn subagent_max_depth(&self) -> u32 {
        self.subagent_max_depth.unwrap_or(2)
    }

    /// Cap on turns within one kept-open subagent session, counting the
    /// turn that opened it. Defaults to 20. Exceeding it comes back as a
    /// `SendMessage` tool error, not a hard failure. See "Risk to name
    /// plainly" in phase 3 of the long-term roadmap.
    pub fn session_turn_cap(&self) -> u32 {
        self.session_turn_cap.unwrap_or(20)
    }

    /// Cap on total `SendMessage` calls during one parent turn, across
    /// every session that parent has open. Defaults to 10. Resets when the
    /// parent's turn ends, the same moment its subagent registry closes
    /// every session it has open.
    pub fn send_message_call_cap(&self) -> u32 {
        self.send_message_call_cap.unwrap_or(10)
    }

    /// Whether to start the MCP servers Claude Code's own config files
    /// name. On by default: without them the `Api` backend has no way to
    /// reach a tool the user has already installed and expects to work.
    pub fn mcp_enabled(&self) -> bool {
        self.mcp.as_ref().and_then(|m| m.enabled).unwrap_or(true)
    }

    /// Servers named here are never started, even when they appear in a
    /// config file. This is the escape hatch for one server that is slow,
    /// broken, or simply not wanted on this machine.
    pub fn mcp_disabled_servers(&self) -> Vec<String> {
        self.mcp
            .as_ref()
            .and_then(|m| m.disabled_servers.clone())
            .unwrap_or_default()
    }

    // ── private helpers ──

    fn load_file(path: &Path) -> Option<Settings> {
        let contents = std::fs::read_to_string(path).ok()?;
        serde_json::from_str::<Settings>(&contents).ok()
    }

    fn home_settings(dir: &str) -> Option<std::path::PathBuf> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        let path = Path::new(&home).join(dir).join("settings.json");
        if path.exists() { Some(path) } else { None }
    }

    /// Test seam for `merge`. `load` calls the real method unconditionally
    /// on every project/global/deepseek settings file it finds, so `merge`
    /// keeps its own private visibility; driving it through `load` itself
    /// would mean writing into the real `~/.claude/settings.json`, which a
    /// test must never touch. This thin wrapper is the only way a test
    /// outside the module can exercise the merge behavior directly.
    #[cfg(feature = "test-support")]
    pub fn merge_for_test(&mut self, other: Settings) {
        self.merge(other)
    }

    /// Merge another Settings into self (other overwrites self for Some fields).
    fn merge(&mut self, other: Settings) {
        if other.api_key.is_some() {
            self.api_key = other.api_key;
        }
        if other.permissions.is_some() {
            self.permissions = other.permissions;
        }
        if other.hooks.is_some() {
            self.hooks = other.hooks;
        }
        if other.effort.is_some() {
            self.effort = other.effort;
        }
        if other.voice.is_some() {
            self.voice = other.voice;
        }
        if other.autopilot.is_some() {
            self.autopilot = other.autopilot;
        }
        if other.context_budget.is_some() {
            self.context_budget = other.context_budget;
        }
        if other.show_raw_output.is_some() {
            self.show_raw_output = other.show_raw_output;
        }
        if other.backends.is_some() {
            self.backends = other.backends;
        }
        if other.default_backend.is_some() {
            self.default_backend = other.default_backend;
        }
        if other.subagent_max_depth.is_some() {
            self.subagent_max_depth = other.subagent_max_depth;
        }
        if other.session_turn_cap.is_some() {
            self.session_turn_cap = other.session_turn_cap;
        }
        if other.send_message_call_cap.is_some() {
            self.send_message_call_cap = other.send_message_call_cap;
        }
        if other.working_dir.is_some() {
            self.working_dir = other.working_dir;
        }
        if other.mcp.is_some() {
            self.mcp = other.mcp;
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PermissionsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HooksConfig {
    #[serde(
        rename = "PreToolUse",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub pre_tool_use: Option<Vec<HookDef>>,
    #[serde(
        rename = "PostToolUse",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub post_tool_use: Option<Vec<HookDef>>,
    #[serde(
        rename = "SessionStart",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub session_start: Option<Vec<HookDef>>,
    #[serde(
        rename = "SessionEnd",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub session_end: Option<Vec<HookDef>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HookDef {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct McpSettings {
    /// Whether to start MCP servers at all. Defaults to true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Server names to skip, by the key they appear under in the
    /// `mcpServers` block of whichever file defines them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_servers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AutopilotConfig {
    /// Number of times to repeat the task. Defaults to 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<u32>,
    /// Path to the autopilot policy file. Falls back to
    /// `<project_root>/autopilot-policy.md` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_path: Option<String>,
    /// Model used to answer AskUserQuestion calls during a run.
    /// Defaults to `deepseek-v4-flash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answerer_model: Option<String>,
    /// Last-used task text, so the GUI can restore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

/// How voice input is triggered.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TriggerMode {
    #[default]
    PushToTalk,
    WakeWord,
}

fn deserialize_trigger_mode<'de, D>(deserializer: D) -> std::result::Result<TriggerMode, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    match raw.as_str() {
        "push_to_talk" => Ok(TriggerMode::PushToTalk),
        "wake_word" => Ok(TriggerMode::WakeWord),
        other => {
            warn!(
                "unknown voice.trigger_mode '{}', falling back to push_to_talk",
                other
            );
            Ok(TriggerMode::PushToTalk)
        }
    }
}

fn serialize_trigger_mode<S>(
    mode: &TriggerMode,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let raw = match mode {
        TriggerMode::PushToTalk => "push_to_talk",
        TriggerMode::WakeWord => "wake_word",
    };
    serializer.serialize_str(raw)
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct VoiceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub stt_enabled: bool,
    #[serde(default)]
    pub tts_enabled: bool,
    /// Path to the whisper GGML speech-to-text model file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stt_model_path: Option<String>,
    /// Path to the Kokoro ONNX text-to-speech model file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_model_path: Option<String>,
    /// Path to the directory holding Kokoro `<voice_id>.bin` voice packs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_voices_path: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_trigger_mode",
        serialize_with = "serialize_trigger_mode"
    )]
    pub trigger_mode: TriggerMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_phrase: Option<String>,
    /// A Kokoro voice id, e.g. `af_heart`, `am_michael`, `bf_emma`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_voice: Option<String>,
    /// Speaking speed, clamped 0.5-2.0. Defaults to 1.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_speed: Option<f32>,
}

/// A named provider for an `api`-kind backend.
///
/// This is a config-side enum only. `src/api/client.rs` has its own runtime
/// `Provider` enum. Keep them separate, `src/config/` must not depend on
/// `src/api/`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ApiProvider {
    DeepSeek,
    Ollama,
}

/// One backend entry from the `backends` map in `settings.json`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackendConfig {
    Api {
        provider: ApiProvider,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<String>,
        /// Explicit model list override. A user listing models by hand
        /// always wins over live discovery. Absent means "discover".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models: Option<Vec<String>>,
    },
    ClaudeCli {
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<HashMap<String, String>>,
        /// Explicit model list override. See the `Api` variant's field of
        /// the same name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models: Option<Vec<String>>,
    },
}

impl BackendConfig {
    /// The model configured for this backend, whichever variant it is.
    pub fn model(&self) -> &str {
        match self {
            BackendConfig::Api { model, .. } => model,
            BackendConfig::ClaudeCli { model, .. } => model,
        }
    }
}
