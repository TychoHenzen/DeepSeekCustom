/// Harness-owned API tool surface for one Controlled Development run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlledApiProfile {
    /// Read-only planning against one fixed root.
    Planning,
    /// File editing inside one fixed disposable root.
    Execution,
}
