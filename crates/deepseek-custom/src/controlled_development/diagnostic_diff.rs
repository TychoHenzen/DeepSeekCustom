use std::path::Path;
use std::process::Command;

use crate::mcp::spawn::resolve_command;

const NO_CHANGES: &str = "No isolated workspace changes.\n";

/// Render a display-only diff between the immutable baseline and execution root.
///
/// Authorization continues to use the workspace inventories. This output is
/// retained only so a blocked packet remains inspectable.
pub fn render_diagnostic_diff(baseline_root: &Path, execution_root: &Path) -> String {
    let resolved = resolve_command("git");
    let output = Command::new(&resolved.program)
        .args(&resolved.prefix_args)
        .args([
            "-c",
            "core.autocrlf=false",
            "diff",
            "--no-index",
            "--binary",
            "--no-ext-diff",
            "--no-renames",
            "--src-prefix=baseline/",
            "--dst-prefix=execution/",
            "--",
        ])
        .arg(baseline_root)
        .arg(execution_root)
        .current_dir(std::env::temp_dir())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output();

    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return format!("Diagnostic diff unavailable: could not start git: {error}\n");
        }
    };
    let mut rendered = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        rendered.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    match output.status.code() {
        Some(0) if rendered.is_empty() => NO_CHANGES.to_string(),
        Some(0 | 1) => rendered,
        status => format!("Diagnostic diff failed with exit code {status:?}.\n{rendered}"),
    }
}
