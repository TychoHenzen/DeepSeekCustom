//! Provider preflight and one-shot dispatch for Stage 1 localization.

use std::path::Path;

use async_trait::async_trait;
use thiserror::Error;

use crate::api::client::ApiClient;
use crate::api::provider::Provider;
use crate::api::types::{ChatRequest, Content, Message, Role};
use crate::backend::resolved::{ResolvedBackend, resolve_named_backend};
use crate::config::settings::Settings;
use crate::effort::Effort;

use super::{
    LocalizationEnvelope, LocalizationPromptInput, RepositoryIndexEntry, build_localization_prompt,
    localization_response_format,
};

/// One schema-constrained localization request.
///
/// The runner depends on this narrow boundary so tests can provide a
/// scripted localizer without starting a network request or chat session.
#[async_trait]
pub trait LocalizationDispatch: Send + Sync {
    /// The configured backend name retained in each attempt report.
    fn backend_name(&self) -> &str;

    /// The configured model retained in each attempt report.
    fn model(&self) -> &str;

    /// Dispatch one already reconstructed prompt against the current index.
    async fn dispatch_prompt(
        &self,
        prompt: String,
        repository_index: &[RepositoryIndexEntry],
    ) -> Result<LocalizationEnvelope, LocalizationDispatchError>;
}

/// A localization backend that passed the structured-output preflight.
///
/// Construction is the dispatch boundary. Only an Ollama API entry can
/// produce this value, so unsupported API and CLI entries fail before any
/// HTTP request or child process can start.
pub struct LocalizationDispatcher {
    backend_name: String,
    model: String,
    effort: Effort,
    max_tokens: u32,
    client: ApiClient,
}

impl LocalizationDispatcher {
    /// Resolve and validate the backend selected in `settings.procedure`.
    pub fn from_settings(
        settings: &Settings,
        project_root: &Path,
    ) -> Result<Self, LocalizationDispatchError> {
        let backend_name = settings
            .procedure()
            .and_then(|procedure| procedure.localization_backend.as_deref())
            .ok_or(LocalizationDispatchError::BackendNotSelected)?;
        let resolved = resolve_named_backend(settings, project_root, backend_name, None).map_err(
            |reason| LocalizationDispatchError::BackendResolution {
                backend: backend_name.to_string(),
                reason,
            },
        )?;

        match resolved {
            ResolvedBackend::Api {
                name,
                provider: Provider::Ollama,
                api_key,
                base_url,
                model,
            } => Ok(Self {
                backend_name: name,
                model,
                effort: settings.effort(),
                max_tokens: settings.max_tokens(),
                client: ApiClient::new(Provider::Ollama, api_key, base_url),
            }),
            ResolvedBackend::Api {
                name,
                provider: Provider::DeepSeek,
                ..
            } => Err(LocalizationDispatchError::UnsupportedBackend {
                backend: name,
                description: "kind api provider deepseek".to_string(),
            }),
            ResolvedBackend::ClaudeCli { name, .. } => {
                Err(LocalizationDispatchError::UnsupportedBackend {
                    backend: name,
                    description: "kind claude_cli".to_string(),
                })
            }
            ResolvedBackend::CodexCli { name, .. } => {
                Err(LocalizationDispatchError::UnsupportedBackend {
                    backend: name,
                    description: "kind codex_cli".to_string(),
                })
            }
            #[cfg(feature = "test-support")]
            ResolvedBackend::Stub { name, .. } => {
                Err(LocalizationDispatchError::UnsupportedBackend {
                    backend: name,
                    description: "kind stub".to_string(),
                })
            }
        }
    }

    /// The configured backend name retained for run reports.
    pub fn backend_name(&self) -> &str {
        &self.backend_name
    }

    /// The configured model retained for the request and run reports.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Send one tool-free, non-streaming localization request and decode
    /// only the assistant's final content into the typed envelope.
    pub async fn localize(
        &self,
        input: LocalizationPromptInput<'_>,
    ) -> Result<LocalizationEnvelope, LocalizationDispatchError> {
        let repository_index = input.repository_index;
        let prompt = build_localization_prompt(input).map_err(|error| {
            LocalizationDispatchError::PromptSerialization {
                reason: error.to_string(),
            }
        })?;
        self.dispatch_prompt(prompt, repository_index).await
    }
}

#[async_trait]
impl LocalizationDispatch for LocalizationDispatcher {
    fn backend_name(&self) -> &str {
        &self.backend_name
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn dispatch_prompt(
        &self,
        prompt: String,
        repository_index: &[RepositoryIndexEntry],
    ) -> Result<LocalizationEnvelope, LocalizationDispatchError> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![Message {
                role: Role::User,
                content: Some(Content::text(prompt)),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.0),
            max_tokens: Some(self.max_tokens),
            thinking: None,
            thinking_mode: None,
            reasoning_effort: None,
            response_format: Some(localization_response_format(repository_index)),
            effort: Some(self.effort),
        };
        let response = self.client.chat(&request).await.map_err(|error| {
            LocalizationDispatchError::Request {
                backend: self.backend_name.clone(),
                reason: error.to_string(),
            }
        })?;
        let final_content = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_ref())
            .and_then(Content::as_text)
            .ok_or(LocalizationDispatchError::MissingFinalContent)?;

        serde_json::from_str(final_content).map_err(|error| {
            LocalizationDispatchError::InvalidEnvelope {
                reason: error.to_string(),
            }
        })
    }
}

/// A preflight or one-shot localization failure.
#[derive(Debug, Error)]
pub enum LocalizationDispatchError {
    #[error("no procedure localization backend is selected")]
    BackendNotSelected,
    #[error("failed to resolve localization backend \"{backend}\": {reason}")]
    BackendResolution { backend: String, reason: String },
    #[error(
        "localization backend \"{backend}\" is unsupported: {description} cannot enforce the localization JSON Schema"
    )]
    UnsupportedBackend {
        backend: String,
        description: String,
    },
    #[error("failed to serialize the localization prompt: {reason}")]
    PromptSerialization { reason: String },
    #[error("localization request through backend \"{backend}\" failed: {reason}")]
    Request { backend: String, reason: String },
    #[error("localization response has no plain final content")]
    MissingFinalContent,
    #[error("localization response final content is not a valid LocalizationEnvelope: {reason}")]
    InvalidEnvelope { reason: String },
}
