use std::path::Path;

use serde::{Deserialize, Deserializer};
use tracing::{debug, info, warn};

use crate::error::Result;

/// Top-level settings (deserialized from settings.json).
#[derive(Deserialize, Debug, Clone)]
pub struct Settings {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub permissions: Option<PermissionsConfig>,
    #[serde(default)]
    pub hooks: Option<HooksConfig>,
    #[serde(default)]
    pub thinking: Option<ThinkingSettingsConfig>,
    #[serde(default)]
    pub voice: Option<VoiceConfig>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: Some("deepseek-v4-flash".into()),
            api_key: None,
            permissions: None,
            hooks: None,
            thinking: None,
            voice: None,
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
    /// Missing files are not an error — defaults apply.
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

    /// Resolve the active model name.
    ///
    /// Priority: `DEEPSEEK_MODEL` env var → `model` field → `"deepseek-v4-flash"`.
    pub fn model(&self) -> String {
        if let Ok(env_model) = std::env::var("DEEPSEEK_MODEL") {
            if !env_model.is_empty() {
                return env_model;
            }
        }
        self.model
            .clone()
            .unwrap_or_else(|| "deepseek-v4-flash".to_string())
    }

    /// Log the loaded config, redacting the API key.
    pub fn log_redacted(&self) {
        info!(
            "config loaded: model={}, api_key={}, permissions={}, hooks={}, thinking={}, voice={}",
            self.model(),
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
        if other.model.is_some() {
            self.model = other.model;
        }
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
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct PermissionsConfig {
    #[serde(default)]
    pub allow: Option<Vec<String>>,
    #[serde(default)]
    pub deny: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct HooksConfig {
    #[serde(rename = "PreToolUse", default)]
    pub pre_tool_use: Option<Vec<HookDef>>,
    #[serde(rename = "PostToolUse", default)]
    pub post_tool_use: Option<Vec<HookDef>>,
    #[serde(rename = "SessionStart", default)]
    pub session_start: Option<Vec<HookDef>>,
    #[serde(rename = "SessionEnd", default)]
    pub session_end: Option<Vec<HookDef>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct HookDef {
    pub command: String,
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ThinkingSettingsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub effort: Option<String>,
}

/// How voice input is triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Deserialize, Debug, Clone)]
pub struct VoiceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub stt_enabled: bool,
    #[serde(default)]
    pub tts_enabled: bool,
    /// Path to the whisper GGML speech-to-text model file.
    #[serde(default)]
    pub stt_model_path: Option<String>,
    /// Path to the Kokoro ONNX text-to-speech model file.
    #[serde(default)]
    pub tts_model_path: Option<String>,
    /// Path to the directory holding Kokoro `<voice_id>.bin` voice packs.
    #[serde(default)]
    pub tts_voices_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_trigger_mode")]
    pub trigger_mode: TriggerMode,
    #[serde(default)]
    pub wake_phrase: Option<String>,
    /// A Kokoro voice id, e.g. `af_heart`, `am_michael`, `bf_emma`.
    #[serde(default)]
    pub tts_voice: Option<String>,
    /// Speaking speed, clamped 0.5-2.0. Defaults to 1.0.
    #[serde(default)]
    pub tts_speed: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_have_model() {
        let s = Settings::default();
        assert_eq!(s.model(), "deepseek-v4-flash");
    }

    #[test]
    fn model_resolution_uses_field() {
        let s = Settings {
            model: Some("deepseek-v4-pro".into()),
            ..Default::default()
        };
        assert_eq!(s.model(), "deepseek-v4-pro");
    }

    #[test]
    fn model_resolution_falls_back_to_default() {
        let s = Settings {
            model: None,
            ..Default::default()
        };
        assert_eq!(s.model(), "deepseek-v4-flash");
    }

    #[test]
    fn load_nonexistent_file_returns_defaults() {
        let result = Settings::load(Path::new("/nonexistent/path/xyz"));
        assert!(result.is_ok());
        let s = result.unwrap();
        assert_eq!(s.model(), "deepseek-v4-flash");
        assert!(s.api_key.is_none());
    }

    #[test]
    fn merge_overwrites_some_fields() {
        let mut base = Settings::default();
        let other = Settings {
            model: Some("v4-pro".into()),
            api_key: Some("sk-abc".into()),
            ..Default::default()
        };
        base.merge(other);
        assert_eq!(base.model(), "v4-pro");
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

    #[test]
    fn trigger_mode_unknown_value_falls_back() {
        let json = r#"{"voice": {"trigger_mode": "bogus_mode"}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.voice_trigger_mode(), TriggerMode::PushToTalk);
    }
}
