//! Resolves a named backend entry from `settings.json` into a
//! `ResolvedBackend`, either to build a `Backend` or to reach the raw
//! fields for a one-shot `claude -p` subagent.

use std::collections::HashMap;
use std::path::Path;
#[cfg(feature = "test-support")]
use std::sync::Arc;
#[cfg(feature = "test-support")]
use std::sync::atomic::AtomicUsize;

use crate::api::key::resolve_api_key;
use crate::api::provider::Provider;
#[cfg(feature = "test-support")]
use crate::backend::stub::StubTurn;
use crate::config::settings::{ApiProvider, BackendConfig, Settings};

/// The pieces needed to build either kind of backend, resolved from a
/// named entry in `settings.json`. `pub`, not `pub(crate)`: a subagent
/// dispatch (`src/backend/subagent.rs`) resolves a `ClaudeCli` entry
/// through `BackendFactory::resolve` to reach `run_once` directly, and the
/// factory's own tests, now in `deepseek-custom-tests/tests/backend_factory.rs`,
/// match on this type from outside the crate, which `pub(crate)` cannot
/// reach. The `Stub` variant keeps its own narrower gate below.
#[derive(Debug)]
pub enum ResolvedBackend {
    /// Enough to build an `ApiClient` and drive an in-process `AgentLoop`.
    Api {
        name: String,
        provider: Provider,
        api_key: String,
        base_url: Option<String>,
        model: String,
    },
    /// Enough to spawn a `claude -p` child through `ClaudeCliDriver`.
    ClaudeCli {
        name: String,
        model: String,
        permission_mode: Option<String>,
        env: Option<HashMap<String, String>>,
    },
    /// Enough to build a `StubBackend`. Never produced from `settings.json`:
    /// only `BackendFactory::with_stub` puts an entry in the map `resolve`
    /// checks first. Gated the same way as the stub itself, see
    /// `src/backend/stub.rs`.
    #[cfg(feature = "test-support")]
    Stub {
        name: String,
        script: Vec<StubTurn>,
        model: String,
        /// Shared across every `StubBackend` built from this named entry.
        /// Each call to `StubBackend::new` calls `fetch_add(1)` on this
        /// to consume the next script entry, so different dispatches of
        /// the same stub return different answers.
        cursor: Arc<AtomicUsize>,
    },
}

/// Map the config-side `ApiProvider` to the runtime `Provider` used by
/// `ApiClient`. This lives here, not in `src/config/`. That module must
/// not depend on `src/api/`.
fn map_provider(provider: &ApiProvider) -> Provider {
    match provider {
        ApiProvider::DeepSeek => Provider::DeepSeek,
        ApiProvider::Ollama => Provider::Ollama,
    }
}

/// Resolve one named backend entry, applying `model_override` when given.
/// Returns an error message naming the requested entry and listing the
/// entries that exist when `name` does not match a configured backend.
/// This is the one resolution path both `BackendFactory::build` and the
/// `resolve_active_backend` test helper below go through.
pub fn resolve_named_backend(
    settings: &Settings,
    project_root: &Path,
    name: &str,
    model_override: Option<&str>,
) -> Result<ResolvedBackend, String> {
    let backend = settings.resolve_backend(name).ok_or_else(|| {
        let known = settings
            .backends()
            .map(|b| b.keys().cloned().collect::<Vec<_>>().join(", "))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "none configured".to_string());
        format!("backend \"{name}\" is not a known backend (known: {known})")
    })?;

    match backend {
        BackendConfig::Api {
            provider,
            model,
            base_url,
            api_key,
            models: _,
        } => {
            let runtime_provider = map_provider(provider);
            let key = match api_key {
                Some(k) => k.clone(),
                None => {
                    resolve_api_key(runtime_provider, project_root).map_err(|e| e.to_string())?
                }
            };
            Ok(ResolvedBackend::Api {
                name: name.to_string(),
                provider: runtime_provider,
                api_key: key,
                base_url: base_url.clone(),
                model: model_override
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| model.clone()),
            })
        }
        BackendConfig::ClaudeCli {
            model,
            permission_mode,
            env,
            models: _,
        } => Ok(ResolvedBackend::ClaudeCli {
            name: name.to_string(),
            model: model_override
                .map(|m| m.to_string())
                .unwrap_or_else(|| model.clone()),
            permission_mode: permission_mode.clone(),
            env: env.clone(),
        }),
    }
}

/// Resolve the backend named by `settings.default_backend()`, or
/// `"deepseek"` when that field is absent. Kept as a free function, with
/// the same signature it always had, so the test suite (now
/// `deepseek-custom-tests/tests/backend_factory.rs`) can call it directly
/// without going through `BackendFactory`. Production code goes through
/// `BackendFactory::build` instead, which is why this is test-only. `pub`,
/// not merely a plain-test gate: that test suite lives in a separate crate
/// now, so both the gate and the visibility must reach across the crate
/// boundary.
#[cfg(feature = "test-support")]
pub fn resolve_active_backend(
    settings: &Settings,
    project_root: &Path,
) -> Result<ResolvedBackend, String> {
    let name = settings.default_backend().unwrap_or("deepseek").to_string();
    resolve_named_backend(settings, project_root, &name, None)
}

/// The model the policy answerer runs on for this backend.
///
/// An explicit `autopilot.answerer_model` always wins. Without one, the
/// default `deepseek-v4-flash` only fits a DeepSeek backend. Any other
/// provider would be asked for a model it does not have, so the answerer
/// falls back to the model the backend itself runs.
pub fn answerer_model(settings: &Settings, provider: Provider, backend_model: &str) -> String {
    match settings.autopilot_answerer_model_override() {
        Some(explicit) => explicit,
        None if provider == Provider::DeepSeek => settings.autopilot_answerer_model(),
        None => backend_model.to_string(),
    }
}
