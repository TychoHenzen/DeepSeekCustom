use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing::{debug, info, warn};

use crate::error::{HarnessError, Result};

/// Top-level settings (deserialized from settings.json).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<PermissionsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HooksConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingSettingsConfig>,
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_key: None,
            permissions: None,
            hooks: None,
            thinking: None,
            voice: None,
            autopilot: None,
            context_budget: None,
            show_raw_output: None,
            backends: None,
            default_backend: None,
            subagent_max_depth: None,
        }
    }
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
            "config loaded: backend={}, api_key={}, permissions={}, hooks={}, thinking={}, voice={}",
            backend,
            if self.api_key.is_some() {
                "***REDACTED***"
            } else {
                "not set"
            },
            self.permissions.is_some(),
            self.hooks.is_some(),
            self.thinking.is_some(),
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

    /// Whether thinking mode is enabled, defaulting to false.
    pub fn thinking_enabled(&self) -> bool {
        self.thinking.as_ref().map(|t| t.enabled).unwrap_or(false)
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

    /// The thinking block, created with defaults if it is not there yet.
    /// See [`Settings::voice_mut`].
    pub fn thinking_mut(&mut self) -> &mut ThinkingSettingsConfig {
        self.thinking
            .get_or_insert_with(ThinkingSettingsConfig::default)
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
        if other.thinking.is_some() {
            self.thinking = other.thinking;
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
pub struct ThinkingSettingsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
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
pub enum TriggerMode {
    PushToTalk,
    WakeWord,
}

impl Default for TriggerMode {
    fn default() -> Self {
        TriggerMode::PushToTalk
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_nonexistent_file_returns_defaults() {
        let result = Settings::load(Path::new("/nonexistent/path/xyz"));
        assert!(result.is_ok());
        let s = result.unwrap();
        assert!(s.api_key.is_none());
    }

    #[test]
    fn merge_overwrites_some_fields() {
        let mut base = Settings::default();
        let other = Settings {
            api_key: Some("sk-abc".into()),
            ..Default::default()
        };
        base.merge(other);
        assert_eq!(base.api_key.unwrap(), "sk-abc");
    }

    #[test]
    fn voice_defaults_when_key_absent() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(!s.voice_enabled());
        assert!(!s.voice_stt_enabled());
        assert!(!s.voice_tts_enabled());
        assert_eq!(s.voice_trigger_mode(), TriggerMode::PushToTalk);
        assert_eq!(s.voice_wake_phrase(), "hey deepseek");
        assert_eq!(s.voice_tts_speed(), 1.0);
        assert!(s.voice_stt_model_path().is_none());
        assert!(s.voice_tts_model_path().is_none());
        assert!(s.voice_tts_voices_path().is_none());
        assert_eq!(s.voice_tts_voice(), "af_heart");
    }

    #[test]
    fn voice_full_block_deserializes() {
        let json = r#"{
            "voice": {
                "enabled": true,
                "stt_enabled": true,
                "tts_enabled": true,
                "stt_model_path": "C:/models/ggml-base.bin",
                "tts_model_path": "C:/models/model.onnx",
                "tts_voices_path": "C:/voices",
                "trigger_mode": "wake_word",
                "wake_phrase": "hey computer",
                "tts_voice": "am_michael",
                "tts_speed": 1.3
            }
        }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(s.voice_enabled());
        assert!(s.voice_stt_enabled());
        assert!(s.voice_tts_enabled());
        assert_eq!(s.voice_stt_model_path().unwrap(), "C:/models/ggml-base.bin");
        assert_eq!(s.voice_tts_model_path().unwrap(), "C:/models/model.onnx");
        assert_eq!(s.voice_tts_voices_path().unwrap(), "C:/voices");
        assert_eq!(s.voice_trigger_mode(), TriggerMode::WakeWord);
        assert_eq!(s.voice_wake_phrase(), "hey computer");
        assert_eq!(s.voice_tts_voice(), "am_michael");
        assert_eq!(s.voice_tts_speed(), 1.3);
    }

    #[test]
    fn voice_tts_speed_clamps_out_of_range_values() {
        let json = r#"{"voice": {"tts_speed": 9.0}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.voice_tts_speed(), 2.0);

        let json = r#"{"voice": {"tts_speed": 0.01}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.voice_tts_speed(), 0.5);
    }

    #[test]
    fn trigger_mode_parses_push_to_talk() {
        let json = r#"{"voice": {"trigger_mode": "push_to_talk"}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.voice_trigger_mode(), TriggerMode::PushToTalk);
    }

    #[test]
    fn trigger_mode_parses_wake_word() {
        let json = r#"{"voice": {"trigger_mode": "wake_word"}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.voice_trigger_mode(), TriggerMode::WakeWord);
    }

    /// Create a uniquely named directory under the system temp dir.
    fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dsc-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_then_load_round_trips_values() {
        let dir = unique_temp_dir("settings-roundtrip");

        let original = Settings {
            api_key: Some("sk-round-trip".into()),
            permissions: Some(PermissionsConfig {
                allow: Some(vec!["Bash".into(), "Read".into()]),
                deny: None,
            }),
            hooks: None,
            thinking: Some(ThinkingSettingsConfig {
                enabled: true,
                effort: Some("high".into()),
            }),
            voice: Some(VoiceConfig {
                enabled: true,
                stt_enabled: true,
                tts_enabled: true,
                stt_model_path: Some("C:/models/ggml-base.bin".into()),
                tts_model_path: Some("C:/models/model.onnx".into()),
                tts_voices_path: Some("C:/voices".into()),
                trigger_mode: TriggerMode::WakeWord,
                wake_phrase: Some("hey computer".into()),
                tts_voice: Some("am_michael".into()),
                tts_speed: Some(1.3),
            }),
            autopilot: Some(AutopilotConfig {
                iterations: Some(10),
                policy_path: Some("custom-policy.md".into()),
                answerer_model: Some("deepseek-v4-pro".into()),
                task: Some("do the thing".into()),
            }),
            context_budget: Some(150_000),
            show_raw_output: Some(true),
            backends: None,
            default_backend: None,
            subagent_max_depth: Some(3),
        };

        original.save(&dir).unwrap();
        assert!(dir.join("settings.json").exists());

        let loaded = Settings::load(&dir).unwrap();

        assert_eq!(loaded.api_key.as_deref(), Some("sk-round-trip"));
        let perms = loaded.permissions.as_ref().unwrap();
        assert_eq!(
            perms.allow.as_ref().unwrap(),
            &vec!["Bash".to_string(), "Read".to_string()]
        );
        assert!(perms.deny.is_none());
        let thinking = loaded.thinking.as_ref().unwrap();
        assert!(thinking.enabled);
        assert_eq!(thinking.effort.as_deref(), Some("high"));
        assert!(loaded.voice_enabled());
        assert!(loaded.voice_stt_enabled());
        assert!(loaded.voice_tts_enabled());
        assert_eq!(
            loaded.voice_stt_model_path().unwrap(),
            "C:/models/ggml-base.bin"
        );
        assert_eq!(
            loaded.voice_tts_model_path().unwrap(),
            "C:/models/model.onnx"
        );
        assert_eq!(loaded.voice_tts_voices_path().unwrap(), "C:/voices");
        assert_eq!(loaded.voice_trigger_mode(), TriggerMode::WakeWord);
        assert_eq!(loaded.voice_wake_phrase(), "hey computer");
        assert_eq!(loaded.voice_tts_voice(), "am_michael");
        assert_eq!(loaded.voice_tts_speed(), 1.3);
        assert_eq!(loaded.autopilot_iterations(), 10);
        assert_eq!(loaded.autopilot_policy_path().unwrap(), "custom-policy.md");
        assert_eq!(loaded.autopilot_answerer_model(), "deepseek-v4-pro");
        assert_eq!(loaded.autopilot_task().unwrap(), "do the thing");
        assert_eq!(loaded.subagent_max_depth(), 3);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn autopilot_defaults_when_absent() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.autopilot_iterations(), 5);
        assert!(s.autopilot_policy_path().is_none());
        assert_eq!(s.autopilot_answerer_model(), "deepseek-v4-flash");
        assert!(s.autopilot_task().is_none());
    }

    #[test]
    fn autopilot_reads_set_values() {
        let json = r#"{
            "autopilot": {
                "iterations": 3,
                "policy_path": "policies/autopilot.md",
                "answerer_model": "deepseek-v4-pro",
                "task": "fix the build"
            }
        }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.autopilot_iterations(), 3);
        assert_eq!(
            s.autopilot_policy_path().unwrap(),
            "policies/autopilot.md"
        );
        assert_eq!(s.autopilot_answerer_model(), "deepseek-v4-pro");
        assert_eq!(s.autopilot_task().unwrap(), "fix the build");
    }

    #[test]
    fn autopilot_mut_creates_block_with_defaults() {
        let mut s = Settings::default();
        assert!(s.autopilot.is_none());
        let block = s.autopilot_mut();
        assert!(block.iterations.is_none());
        block.iterations = Some(7);
        assert_eq!(s.autopilot_iterations(), 7);
    }

    #[test]
    fn autopilot_absent_settings_serialize_without_key() {
        let s = Settings::default();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("autopilot"));
    }

    #[test]
    fn save_omits_none_fields() {
        let dir = unique_temp_dir("settings-omit");

        let s = Settings {
            api_key: None,
            permissions: None,
            hooks: None,
            thinking: None,
            voice: None,
            autopilot: None,
            context_budget: None,
            show_raw_output: None,
            backends: None,
            default_backend: None,
            subagent_max_depth: None,
        };
        s.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();

        assert!(!text.contains("api_key"));
        assert!(!text.contains("context_budget"));
        assert!(!text.contains("show_raw_output"));
        assert!(!text.contains("subagent_max_depth"));
        assert!(!text.contains("null"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn context_budget_defaults_when_absent() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.context_budget(), 100_000);
    }

    #[test]
    fn context_budget_passes_through_in_range_value() {
        let s: Settings = serde_json::from_str(r#"{"context_budget": 64000}"#).unwrap();
        assert_eq!(s.context_budget(), 64_000);
    }

    #[test]
    fn context_budget_clamps_low_value() {
        let s: Settings = serde_json::from_str(r#"{"context_budget": 1000}"#).unwrap();
        assert_eq!(s.context_budget(), 32_000);
    }

    #[test]
    fn context_budget_clamps_high_value() {
        let s: Settings = serde_json::from_str(r#"{"context_budget": 999999}"#).unwrap();
        assert_eq!(s.context_budget(), 200_000);
    }

    #[test]
    fn show_raw_output_defaults_to_false() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(!s.show_raw_output());
    }

    #[test]
    fn show_raw_output_reads_set_value() {
        let s: Settings = serde_json::from_str(r#"{"show_raw_output": true}"#).unwrap();
        assert!(s.show_raw_output());
    }

    #[test]
    fn thinking_enabled_defaults_to_false() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(!s.thinking_enabled());
    }

    #[test]
    fn thinking_enabled_reads_set_value() {
        let s: Settings = serde_json::from_str(r#"{"thinking": {"enabled": true}}"#).unwrap();
        assert!(s.thinking_enabled());
    }

    #[test]
    fn new_panel_fields_round_trip_through_a_file() {
        let dir = unique_temp_dir("settings-panel-fields");

        let original = Settings {
            context_budget: Some(150_000),
            show_raw_output: Some(true),
            thinking: Some(ThinkingSettingsConfig {
                enabled: true,
                effort: None,
            }),
            ..Default::default()
        };
        original.save(&dir).unwrap();

        let loaded = Settings::load(&dir).unwrap();
        assert_eq!(loaded.context_budget(), 150_000);
        assert!(loaded.show_raw_output());
        assert!(loaded.thinking_enabled());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trigger_mode_unknown_value_falls_back() {
        let json = r#"{"voice": {"trigger_mode": "bogus_mode"}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.voice_trigger_mode(), TriggerMode::PushToTalk);
    }

    #[test]
    fn api_backend_minimal_fields_deserialize() {
        let json = r#"{"kind": "api", "provider": "deepseek", "model": "deepseek-v4-pro"}"#;
        let b: BackendConfig = serde_json::from_str(json).unwrap();
        match b {
            BackendConfig::Api {
                provider,
                model,
                base_url,
                api_key,
                models,
            } => {
                assert_eq!(provider, ApiProvider::DeepSeek);
                assert_eq!(model, "deepseek-v4-pro");
                assert!(base_url.is_none());
                assert!(api_key.is_none());
                assert!(models.is_none());
            }
            BackendConfig::ClaudeCli { .. } => panic!("expected Api variant"),
        }
    }

    #[test]
    fn api_backend_full_fields_round_trip() {
        let original = BackendConfig::Api {
            provider: ApiProvider::Ollama,
            model: "qwen2.5-coder:7b-instruct-q4_K_M".into(),
            base_url: Some("http://localhost:11434/v1".into()),
            api_key: Some("sk-local".into()),
            models: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let loaded: BackendConfig = serde_json::from_str(&json).unwrap();
        match loaded {
            BackendConfig::Api {
                provider,
                model,
                base_url,
                api_key,
                models,
            } => {
                assert_eq!(provider, ApiProvider::Ollama);
                assert_eq!(model, "qwen2.5-coder:7b-instruct-q4_K_M");
                assert_eq!(base_url.as_deref(), Some("http://localhost:11434/v1"));
                assert_eq!(api_key.as_deref(), Some("sk-local"));
                assert!(models.is_none());
            }
            BackendConfig::ClaudeCli { .. } => panic!("expected Api variant"),
        }
    }

    #[test]
    fn claude_cli_backend_round_trips() {
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        env.insert("BAZ".to_string(), "qux".to_string());
        let original = BackendConfig::ClaudeCli {
            model: "opus".into(),
            permission_mode: Some("bypassPermissions".into()),
            env: Some(env),
            models: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let loaded: BackendConfig = serde_json::from_str(&json).unwrap();
        match loaded {
            BackendConfig::ClaudeCli {
                model,
                permission_mode,
                env,
                models,
            } => {
                assert_eq!(model, "opus");
                assert_eq!(permission_mode.as_deref(), Some("bypassPermissions"));
                let env = env.unwrap();
                assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
                assert_eq!(env.get("BAZ").map(String::as_str), Some("qux"));
                assert!(models.is_none());
            }
            BackendConfig::Api { .. } => panic!("expected ClaudeCli variant"),
        }
    }

    #[test]
    fn api_backend_omits_absent_optional_fields() {
        let backend = BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".into(),
            base_url: None,
            api_key: None,
            models: None,
        };
        let json = serde_json::to_string(&backend).unwrap();
        assert!(!json.contains("base_url"));
        assert!(!json.contains("api_key"));
        assert!(!json.contains("models"));
    }

    #[test]
    fn models_override_round_trips_on_api_backend() {
        let original = BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".into(),
            base_url: None,
            api_key: None,
            models: Some(vec!["deepseek-v4-pro".into(), "deepseek-v4-flash".into()]),
        };
        let json = serde_json::to_string(&original).unwrap();
        let loaded: BackendConfig = serde_json::from_str(&json).unwrap();
        match loaded {
            BackendConfig::Api { models, .. } => {
                assert_eq!(
                    models,
                    Some(vec!["deepseek-v4-pro".to_string(), "deepseek-v4-flash".to_string()])
                );
            }
            BackendConfig::ClaudeCli { .. } => panic!("expected Api variant"),
        }
    }

    #[test]
    fn models_override_round_trips_on_claude_cli_backend() {
        let original = BackendConfig::ClaudeCli {
            model: "opus".into(),
            permission_mode: None,
            env: None,
            models: Some(vec!["opus".into(), "sonnet".into()]),
        };
        let json = serde_json::to_string(&original).unwrap();
        let loaded: BackendConfig = serde_json::from_str(&json).unwrap();
        match loaded {
            BackendConfig::ClaudeCli { models, .. } => {
                assert_eq!(
                    models,
                    Some(vec!["opus".to_string(), "sonnet".to_string()])
                );
            }
            BackendConfig::Api { .. } => panic!("expected ClaudeCli variant"),
        }
    }

    #[test]
    fn models_field_absent_from_json_when_none() {
        let api = BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".into(),
            base_url: None,
            api_key: None,
            models: None,
        };
        assert!(!serde_json::to_string(&api).unwrap().contains("models"));

        let claude = BackendConfig::ClaudeCli {
            model: "opus".into(),
            permission_mode: None,
            env: None,
            models: None,
        };
        assert!(!serde_json::to_string(&claude).unwrap().contains("models"));
    }

    #[test]
    fn resolve_backend_finds_by_name_and_misses_unknown() {
        let mut backends = HashMap::new();
        backends.insert(
            "deepseek".to_string(),
            BackendConfig::Api {
                provider: ApiProvider::DeepSeek,
                model: "deepseek-v4-pro".into(),
                base_url: None,
                api_key: None,
                models: None,
            },
        );
        let s = Settings {
            backends: Some(backends),
            default_backend: Some("deepseek".into()),
            ..Default::default()
        };

        match s.resolve_backend("deepseek") {
            Some(BackendConfig::Api { provider, .. }) => {
                assert_eq!(*provider, ApiProvider::DeepSeek);
            }
            _ => panic!("expected to resolve deepseek backend"),
        }
        assert!(s.resolve_backend("nonexistent").is_none());
        assert_eq!(s.default_backend(), Some("deepseek"));
    }

    #[test]
    fn subagent_max_depth_defaults_to_two() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.subagent_max_depth(), 2);
    }

    #[test]
    fn subagent_max_depth_round_trips_through_serialize_and_deserialize() {
        let s = Settings {
            subagent_max_depth: Some(4),
            ..Default::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"subagent_max_depth\":4"));

        let loaded: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.subagent_max_depth(), 4);
    }

    #[test]
    fn subagent_max_depth_absent_settings_serialize_without_key() {
        let s = Settings::default();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("subagent_max_depth"));
    }

    #[test]
    fn subagent_max_depth_takes_part_in_merge() {
        let mut base = Settings::default();
        assert_eq!(base.subagent_max_depth(), 2);

        let other = Settings {
            subagent_max_depth: Some(5),
            ..Default::default()
        };
        base.merge(other);
        assert_eq!(base.subagent_max_depth(), 5);
    }

    /// The repo `settings.json` is also the live settings file: the GUI
    /// rewrites it whenever a control changes. So this test checks the
    /// shape it must keep, not the choices a user is free to make. Asserting
    /// an exact `default_backend` here would fail the suite for anyone who
    /// touched the backend picker.
    #[test]
    fn repo_settings_json_parses_with_three_backends() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let contents = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        let s: Settings = serde_json::from_str(&contents).unwrap();

        let backends = s.backends().unwrap();
        assert_eq!(backends.len(), 3);

        let selected = s.default_backend().expect("default_backend must be set");
        assert!(
            backends.contains_key(selected),
            "default_backend {selected} names no configured entry"
        );

        match backends.get("deepseek").unwrap() {
            BackendConfig::Api {
                provider, model, ..
            } => {
                assert_eq!(*provider, ApiProvider::DeepSeek);
                assert!(!model.is_empty());
            }
            _ => panic!("expected deepseek to be an Api backend"),
        }

        match backends.get("ollama").unwrap() {
            BackendConfig::Api {
                provider, model, ..
            } => {
                assert_eq!(*provider, ApiProvider::Ollama);
                assert!(!model.is_empty());
            }
            _ => panic!("expected ollama to be an Api backend"),
        }

        match backends.get("claude").unwrap() {
            BackendConfig::ClaudeCli { model, .. } => {
                assert!(!model.is_empty());
            }
            _ => panic!("expected claude to be a ClaudeCli backend"),
        }
    }

    #[test]
    fn provider_and_kind_wire_tags_round_trip() {
        let deepseek_json = serde_json::to_string(&ApiProvider::DeepSeek).unwrap();
        assert_eq!(deepseek_json, "\"deepseek\"");
        let ollama_json = serde_json::to_string(&ApiProvider::Ollama).unwrap();
        assert_eq!(ollama_json, "\"ollama\"");
        assert_eq!(
            serde_json::from_str::<ApiProvider>("\"deepseek\"").unwrap(),
            ApiProvider::DeepSeek
        );
        assert_eq!(
            serde_json::from_str::<ApiProvider>("\"ollama\"").unwrap(),
            ApiProvider::Ollama
        );

        let api_backend = BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-v4-pro".into(),
            base_url: None,
            api_key: None,
            models: None,
        };
        let api_json = serde_json::to_string(&api_backend).unwrap();
        assert!(api_json.contains("\"kind\":\"api\""));

        let claude_backend = BackendConfig::ClaudeCli {
            model: "opus".into(),
            permission_mode: None,
            env: None,
            models: None,
        };
        let claude_json = serde_json::to_string(&claude_backend).unwrap();
        assert!(claude_json.contains("\"kind\":\"claude_cli\""));
    }
}
