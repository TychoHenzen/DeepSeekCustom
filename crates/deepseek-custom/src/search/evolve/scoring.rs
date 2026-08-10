//! Scoring and feature extraction: run a shell command with the
//! candidate's text on stdin and read a number (or numbers) back.

use std::path::Path;

use crate::tools::shell_stdin::run_command_with_stdin;

/// Run a scoring command with the candidate's text on stdin, and read one
/// number back off stdout.
pub async fn run_score_cmd(
    cmd_str: &str,
    candidate_text: &str,
    work_dir: &Path,
) -> Result<f64, String> {
    let output = run_command_with_stdin(cmd_str, candidate_text, work_dir).await?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    stdout
        .parse::<f64>()
        .map_err(|e| format!("fitness_cmd did not print a number: '{stdout}': {e}"))
}

/// Run a feature command the same way, reading comma-separated numbers back.
pub async fn run_feature_cmd(
    cmd_str: &str,
    candidate_text: &str,
    work_dir: &Path,
) -> Result<Vec<f64>, String> {
    let output = run_command_with_stdin(cmd_str, candidate_text, work_dir).await?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err("feature_cmd printed nothing".to_string());
    }
    stdout
        .split(',')
        .map(|s| {
            s.trim()
                .parse::<f64>()
                .map_err(|e| format!("feature_cmd did not print numbers: '{stdout}': {e}"))
        })
        .collect()
}
