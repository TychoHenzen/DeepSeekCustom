//! Live model discovery for the GUI's model picker. Answers one question:
//! which models can a backend run.
//!
//! An explicit `models` override on the entry always wins, since a user
//! listing models by hand should never be second-guessed. Otherwise
//! discovery runs by kind and provider. Ollama queries its local server.
//! DeepSeek and `claude_cli` return a known, static list. If discovery
//! yields nothing, the result falls back to the entry's declared model, so
//! the picker is never empty and always contains the current selection.
//!
//! Discovery never blocks the GUI and never panics. A connection failure,
//! a timeout, a non-success status, or malformed JSON from Ollama all
//! yield an empty list, which the fallback then covers.

use std::time::Duration;

use tracing::{debug, warn};

use crate::config::settings::{ApiProvider, BackendConfig};

const OLLAMA_QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_OLLAMA_HOST: &str = "http://localhost:11434";

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
            ..
        } => known_deepseek_models(),
        BackendConfig::ClaudeCli { .. } => known_claude_cli_aliases(),
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

/// The known DeepSeek model ids.
fn known_deepseek_models() -> Vec<String> {
    vec![
        "deepseek-v4-flash".to_string(),
        "deepseek-v4-pro".to_string(),
    ]
}

/// The known `claude_cli` model aliases.
fn known_claude_cli_aliases() -> Vec<String> {
    vec![
        "opus".to_string(),
        "sonnet".to_string(),
        "haiku".to_string(),
        "fable".to_string(),
    ]
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

