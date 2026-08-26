//! Schema-constrained local patch drafting through an Ollama API backend.

use async_trait::async_trait;
use thiserror::Error;

use crate::api::client::ApiClient;
use crate::api::provider::Provider;
use crate::api::types::{ChatRequest, Content, Message};
use crate::backend::resolved::ResolvedBackend;
use crate::effort::Effort;

use super::{
    PatchCandidate, PatchEnvelopeError, decode_patch_envelope, patch_envelope_response_format,
};

/// Narrow dispatch boundary for one local patch draft.
#[async_trait]
pub trait LocalPatchDraftDispatch: Send + Sync {
    fn backend_name(&self) -> &str;
    fn model(&self) -> &str;
    async fn draft(&self, prompt: String) -> Result<PatchCandidate, LocalPatchDraftError>;
}

/// An Ollama backend prepared for a schema-constrained patch request.
pub struct LocalPatchDraftDispatcher {
    backend_name: String,
    model: String,
    effort: Effort,
    max_tokens: u32,
    client: ApiClient,
}

impl LocalPatchDraftDispatcher {
    /// Build from the same resolved backend value used by the backend factory.
    pub fn from_resolved_backend(
        backend: ResolvedBackend,
        effort: Effort,
        max_tokens: u32,
    ) -> Result<Self, LocalPatchDraftError> {
        match backend {
            ResolvedBackend::Api {
                name,
                provider: Provider::Ollama,
                api_key,
                base_url,
                model,
            } => Ok(Self {
                backend_name: name,
                model,
                effort,
                max_tokens,
                client: ApiClient::new(Provider::Ollama, api_key, base_url),
            }),
            ResolvedBackend::Api {
                name,
                provider: Provider::DeepSeek,
                ..
            } => Err(LocalPatchDraftError::UnsupportedBackend {
                backend: name,
                description: "kind api provider deepseek".to_string(),
            }),
            ResolvedBackend::ClaudeCli { name, .. } => {
                Err(LocalPatchDraftError::UnsupportedBackend {
                    backend: name,
                    description: "kind claude_cli".to_string(),
                })
            }
            ResolvedBackend::CodexCli { name, .. } => {
                Err(LocalPatchDraftError::UnsupportedBackend {
                    backend: name,
                    description: "kind codex_cli".to_string(),
                })
            }
            #[cfg(feature = "test-support")]
            ResolvedBackend::Stub { name, .. } => Err(LocalPatchDraftError::UnsupportedBackend {
                backend: name,
                description: "kind stub".to_string(),
            }),
        }
    }

    pub fn backend_name(&self) -> &str {
        &self.backend_name
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

#[async_trait]
impl LocalPatchDraftDispatch for LocalPatchDraftDispatcher {
    fn backend_name(&self) -> &str {
        &self.backend_name
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn draft(&self, prompt: String) -> Result<PatchCandidate, LocalPatchDraftError> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![Message::user(prompt)],
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.0),
            max_tokens: Some(self.max_tokens),
            thinking: None,
            thinking_mode: None,
            reasoning_effort: None,
            response_format: Some(patch_envelope_response_format()),
            effort: Some(self.effort),
        };
        let response =
            self.client
                .chat(&request)
                .await
                .map_err(|error| LocalPatchDraftError::Request {
                    backend: self.backend_name.clone(),
                    reason: error.to_string(),
                })?;
        let final_content = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_ref())
            .and_then(Content::as_text)
            .ok_or(LocalPatchDraftError::MissingFinalContent)?;

        decode_patch_envelope(final_content)
            .map_err(|source| LocalPatchDraftError::InvalidEnvelope { source })
    }
}

/// Construction, request, or shared decoding failure for a local draft.
#[derive(Debug, Error)]
pub enum LocalPatchDraftError {
    #[error(
        "local patch backend \"{backend}\" is unsupported: {description} cannot enforce the patch-envelope JSON Schema"
    )]
    UnsupportedBackend {
        backend: String,
        description: String,
    },
    #[error("local patch request through backend \"{backend}\" failed: {reason}")]
    Request { backend: String, reason: String },
    #[error("local patch response has no plain final content")]
    MissingFinalContent,
    #[error("local patch response failed shared decoding: {source}")]
    InvalidEnvelope { source: PatchEnvelopeError },
}
