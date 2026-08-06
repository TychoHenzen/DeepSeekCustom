//! Live model discovery for the GUI's model picker. Answers one question:
//! which models can a backend run.
//!
//! An explicit `models` override on the entry always wins, since a user
//! listing models by hand should never be second-guessed. Otherwise
//! discovery runs by kind and provider. Ollama queries its local server.
//! `claude_cli` queries the Anthropic `/v1/models` endpoint using the
//! OAuth token from Claude Code's credentials file, and prepends the
//! short aliases (`opus`, `sonnet`, `haiku`, `fable`). DeepSeek queries
//! its own `/models` endpoint using the resolved API key. If discovery
//! yields nothing, the result falls back to the entry's declared model,
//! so the picker is never empty and always contains the current
//! selection.
//!
//! Discovery never blocks the GUI and never panics. A connection failure,
//! a timeout, a non-success status, missing credentials, or malformed
//! JSON all yield a fallback (aliases for `claude_cli`, empty for Ollama),
//! which the final fallback then covers.

use std::path::PathBuf;
use std::time::Duration;

use tracing::{debug, warn};

use crate::config::settings::{ApiProvider, BackendConfig};

const OLLAMA_QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_OLLAMA_HOST: &str = "http://localhost:11434";

const ANTHROPIC_MODELS_URL: &str = "https://api.anthropic.com/v1/models?limit=100";
const ANTHROPIC_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const ANTHROPIC_API_VERSION: &str = "2023-06-01";

const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
const DEEPSEEK_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

const CLAUDE_CLI_ALIASES: &[&str] = &["opus", "sonnet", "haiku", "fable"];

/// Models a backend can run, for the GUI picker.
pub async fn list_models(entry: &BackendConfig) -> Vec<String> {
    if let Some(models) = entry_models_override(entry) {
        return models;
    }

    let discovered = match entry {
        BackendConfig::Api {
            provider: ApiProvider::Ollama,
            base_url,
            ..
        } => query_ollama_models(base_url.as_deref()).await,
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            base_url,
            api_key,
            ..
        } => query_deepseek_models(base_url.as_deref(), api_key.as_deref()).await,
        BackendConfig::ClaudeCli { .. } => query_anthropic_models().await,
    };

    apply_fallback(discovered, entry.model())
}

/// The entry's explicit `models` override, if it carries one.
pub fn entry_models_override(entry: &BackendConfig) -> Option<Vec<String>> {
    match entry {
        BackendConfig::Api { models, .. } => models.clone(),
        BackendConfig::ClaudeCli { models, .. } => models.clone(),
    }
}

/// Fall back to a single-element list holding `declared_model` when
/// `discovered` came back empty, so the picker always contains the
/// current selection.
pub fn apply_fallback(discovered: Vec<String>, declared_model: &str) -> Vec<String> {
    if discovered.is_empty() {
        vec![declared_model.to_string()]
    } else {
        discovered
    }
}

/// Query the DeepSeek `/models` endpoint for available models.
/// Falls back to an empty list on any failure, so `apply_fallback`
/// covers it with the declared model.
async fn query_deepseek_models(
    base_url: Option<&str>,
    config_api_key: Option<&str>,
) -> Vec<String> {
    let key = match resolve_deepseek_key(config_api_key) {
        Some(k) => k,
        None => return Vec::new(),
    };

    let host = base_url
        .map(|u| u.trim_end_matches('/'))
        .unwrap_or(DEFAULT_DEEPSEEK_BASE_URL);
    let url = format!("{host}/models");

    let client = match reqwest::Client::builder()
        .timeout(DEEPSEEK_QUERY_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("deepseek model discovery: failed to build client: {e}");
            return Vec::new();
        }
    };

    let response = match client
        .get(&url)
        .header("Authorization", format!("Bearer {key}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            debug!("deepseek model discovery: request to {url} failed: {e}");
            return Vec::new();
        }
    };

    if !response.status().is_success() {
        debug!(
            "deepseek model discovery: {url} returned {}",
            response.status()
        );
        return Vec::new();
    }

    let body = match response.text().await {
        Ok(b) => b,
        Err(e) => {
            debug!("deepseek model discovery: failed to read body: {e}");
            return Vec::new();
        }
    };

    parse_models_response(&body)
}

/// Try the config's `api_key`, then `DEEPSEEK_API_KEY`, then
/// `ANTHROPIC_AUTH_TOKEN`. Returns `None` when no key is found.
fn resolve_deepseek_key(config_key: Option<&str>) -> Option<String> {
    if let Some(k) = config_key {
        if !k.is_empty() {
            return Some(k.to_string());
        }
    }
    for var in ["DEEPSEEK_API_KEY", "ANTHROPIC_AUTH_TOKEN"] {
        if let Ok(k) = std::env::var(var) {
            if !k.is_empty() {
                return Some(k);
            }
        }
    }
    None
}

/// The short aliases the `claude` CLI accepts (`--model opus`, etc.).
pub fn claude_cli_aliases() -> Vec<String> {
    CLAUDE_CLI_ALIASES.iter().map(|s| s.to_string()).collect()
}

/// Query the Anthropic API for available models using the OAuth token
/// from Claude Code's credentials file. Returns aliases followed by
/// discovered full model IDs. Falls back to aliases alone on any
/// failure: missing credentials, network error, bad response.
async fn query_anthropic_models() -> Vec<String> {
    let token = match read_oauth_token() {
        Some(t) => t,
        None => return claude_cli_aliases(),
    };

    let client = match reqwest::Client::builder()
        .timeout(ANTHROPIC_QUERY_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("anthropic model discovery: failed to build client: {e}");
            return claude_cli_aliases();
        }
    };

    let response = match client
        .get(ANTHROPIC_MODELS_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-version", ANTHROPIC_API_VERSION)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            debug!("anthropic model discovery: request failed: {e}");
            return claude_cli_aliases();
        }
    };

    if !response.status().is_success() {
        debug!(
            "anthropic model discovery: returned {}",
            response.status()
        );
        return claude_cli_aliases();
    }

    let body = match response.text().await {
        Ok(b) => b,
        Err(e) => {
            debug!("anthropic model discovery: failed to read body: {e}");
            return claude_cli_aliases();
        }
    };

    let discovered = parse_models_response(&body);
    if discovered.is_empty() {
        return claude_cli_aliases();
    }

    let mut result = claude_cli_aliases();
    result.extend(discovered);
    result
}

/// Read the OAuth access token from Claude Code's credentials file.
fn read_oauth_token() -> Option<String> {
    let creds_path = claude_credentials_path()?;
    let content = std::fs::read_to_string(&creds_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("claudeAiOauth")?
        .get("accessToken")?
        .as_str()
        .map(|s| s.to_string())
}

fn claude_credentials_path() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    Some(PathBuf::from(home).join(".claude").join(".credentials.json"))
}

/// Parse an OpenAI-compatible `/models` response body into model
/// IDs. Both Anthropic and DeepSeek use this shape. Malformed JSON,
/// a missing `data` key, or an empty array each yield an empty list.
pub fn parse_models_response(body: &str) -> Vec<String> {
    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let Some(data) = json.get("data").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    data.iter()
        .filter_map(|m| m.get("id").and_then(|n| n.as_str()).map(str::to_string))
        .collect()
}

/// Query the local Ollama server's `/api/tags` endpoint for installed
/// model names. Any failure returns an empty list rather than an error:
/// the GUI must open with Ollama not running.
async fn query_ollama_models(base_url: Option<&str>) -> Vec<String> {
    let url = ollama_tags_url(base_url);

    let client = match reqwest::Client::builder()
        .timeout(OLLAMA_QUERY_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("ollama model discovery: failed to build client: {e}");
            return Vec::new();
        }
    };

    let response = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            debug!("ollama model discovery: request to {url} failed: {e}");
            return Vec::new();
        }
    };

    if !response.status().is_success() {
        debug!(
            "ollama model discovery: {url} returned {}",
            response.status()
        );
        return Vec::new();
    }

    let body = match response.text().await {
        Ok(b) => b,
        Err(e) => {
            debug!("ollama model discovery: failed to read response body: {e}");
            return Vec::new();
        }
    };

    parse_ollama_tags(&body)
}

/// Parse an `/api/tags` response body into model names, in order.
/// Malformed JSON, an empty `models` array, or a body missing the
/// `models` key each yield an empty list.
pub fn parse_ollama_tags(body: &str) -> Vec<String> {
    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let Some(models) = json.get("models").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    models
        .iter()
        .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect()
}

/// Derive the `/api/tags` URL from a backend entry's `base_url`,
/// stripping a trailing `/v1` so a non-default Ollama host still works.
/// Falls back to `http://localhost:11434` when the entry carries no
/// `base_url`.
pub fn ollama_tags_url(base_url: Option<&str>) -> String {
    let host = base_url
        .map(|u| u.trim_end_matches('/').trim_end_matches("/v1"))
        .unwrap_or(DEFAULT_OLLAMA_HOST);
    format!("{host}/api/tags")
}

