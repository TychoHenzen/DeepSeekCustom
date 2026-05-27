use std::path::Path;

use serde::Deserialize;
use tracing::{info, debug};

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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: Some("deepseek-v4-flash".into()),
            api_key: None,
            permissions: None,
            hooks: None,
            thinking: None,
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
            "config loaded: model={}, api_key={}, permissions={}, hooks={}, thinking={}",
            self.model(),
            if self.api_key.is_some() { "***REDACTED***" } else { "not set" },
            self.permissions.is_some(),
            self.hooks.is_some(),
            self.thinking.is_some(),
        );
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
        if path.exists() {
            Some(path)
        } else {
            None
        }
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
}
