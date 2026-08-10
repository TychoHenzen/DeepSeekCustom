/// Which backend an `ApiClient` talks to. Both providers accept the same
/// OpenAI-compatible request shape at `{base_url}/chat/completions`, so one
/// client type serves both. `prepare_request` adapts the request per
/// provider before it goes out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Provider {
    DeepSeek,
    Ollama,
}
