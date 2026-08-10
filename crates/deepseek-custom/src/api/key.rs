use std::path::{Path, PathBuf};

use tracing::debug;

use crate::api::provider::Provider;
use crate::error::{HarnessError, Result};

/// Resolve the API key for `provider` from environment and config files.
///
/// `Provider::Ollama` returns a placeholder immediately. Ollama requires an
/// `Authorization` header to be present but ignores its value entirely. No
/// environment variable or config file is read for it.
///
/// `Provider::DeepSeek` priority: `DEEPSEEK_API_KEY` env var ->
/// `ANTHROPIC_AUTH_TOKEN` env var -> project `settings.json` `api_key`
/// field -> `~/.claude/settings.json` -> `~/.claude/backends.json`.
pub fn resolve_api_key(provider: Provider, project_root: &Path) -> Result<String> {
    if provider == Provider::Ollama {
        debug!("api_key: using placeholder value for Ollama");
        return Ok("ollama".to_string());
    }

    if let Some(key) = key_from_env("DEEPSEEK_API_KEY") {
        debug!("api_key resolved from DEEPSEEK_API_KEY env var");
        return Ok(key);
    }

    if let Some(key) = key_from_env("ANTHROPIC_AUTH_TOKEN") {
        debug!("api_key resolved from ANTHROPIC_AUTH_TOKEN env var");
        return Ok(key);
    }

    let settings_path = project_root.join("settings.json");
    if let Some(key) = key_from_json_file(&settings_path) {
        debug!("api_key resolved from project settings.json");
        return Ok(key);
    }

    if let Some(home) = home_dir() {
        let global_settings = home.join(".claude").join("settings.json");
        if let Some(key) = key_from_json_file(&global_settings) {
            debug!("api_key resolved from ~/.claude/settings.json");
            return Ok(key);
        }
    }

    if let Some(key) = key_from_backends_json() {
        debug!("api_key resolved from ~/.claude/backends.json");
        return Ok(key);
    }

    Err(HarnessError::Config(
        "DEEPSEEK_API_KEY not set. Get one at \
         https://platform.deepseek.com/api_keys"
            .to_string(),
    ))
}

/// Read a non-empty environment variable as a potential API key.
fn key_from_env(name: &str) -> Option<String> {
    let key = std::env::var(name).ok()?;
    if key.is_empty() { None } else { Some(key) }
}

/// The current user's home directory, if one can be determined.
fn home_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    Some(PathBuf::from(home))
}

/// Read the `api_key` field from a JSON file at `path`.
fn key_from_json_file(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let key = json.get("api_key")?.as_str()?;
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}

/// Read `~/.claude/backends.json` and pull the DeepSeek key out of it.
pub fn key_from_backends_json() -> Option<String> {
    let home = home_dir()?;
    let path = home.join(".claude").join("backends.json");
    let contents = std::fs::read_to_string(path).ok()?;
    parse_backends_json(&contents)
}

/// Pick an `apiKey` out of the backends config.
///
/// The backend named by `default` wins. Otherwise the first backend that
/// carries a key wins, so a config with a single DeepSeek entry still works.
pub fn parse_backends_json(contents: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(contents).ok()?;
    let backends = json.get("backends")?.as_object()?;

    let default_key = json
        .get("default")
        .and_then(|v| v.as_str())
        .and_then(|name| backends.get(name))
        .and_then(backend_api_key);
    if default_key.is_some() {
        return default_key;
    }

    backends.values().find_map(backend_api_key)
}

/// Read the non-empty `apiKey` field of one backend entry.
fn backend_api_key(backend: &serde_json::Value) -> Option<String> {
    let key = backend.get("apiKey")?.as_str()?;
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}
