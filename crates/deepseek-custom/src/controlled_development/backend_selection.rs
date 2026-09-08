/// Backend identity captured from the owning session for one packet.
///
/// Controlled runs use this value to build fresh backend instances. They do
/// not replace or mutate the normal chat backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlledBackendSelection {
    pub backend: String,
    pub model: Option<String>,
}

impl ControlledBackendSelection {
    pub fn new(backend: impl Into<String>, model: Option<String>) -> Self {
        Self {
            backend: backend.into(),
            model,
        }
    }
}
