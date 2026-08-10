use crate::effort::Effort;

/// Default token budget for the context pruning hysteresis oscillator.
/// History grows freely until it passes this high-water mark, then gets
/// pruned hard down to a third of it (see `context_low_water`).
pub const DEFAULT_CONTEXT_BUDGET: usize = 100_000;

/// Default target Flesch-Kincaid grade for the plain-language gate,
/// matching `Settings::style_target_grade`.
pub const DEFAULT_TARGET_GRADE: u8 = 8;

/// Round a configured target grade onto the whole number the shared flag
/// carries. A grade below zero or past 30 is clamped rather than wrapped,
/// so a stray value in settings.json cannot turn into a nonsense target.
pub fn grade_to_u8(grade: f32) -> u8 {
    grade.round().clamp(0.0, 30.0) as u8
}

/// Configuration for the agent loop.
pub struct AgentConfig {
    pub max_turns: u32,
    pub model: String,
    pub effort: Effort,
    /// Cap on the tokens one API reply may produce, reasoning included.
    pub max_tokens: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 100,
            model: "deepseek-v4-flash".into(),
            effort: Effort::None,
            max_tokens: 8192,
        }
    }
}
