//! Turning a configured command name into something Windows will actually
//! start.
//!
//! `Command::new("npx")` fails on Windows. `npx` is `npx.cmd`, a batch
//! file, and Rust's spawn resolves `.exe` alone: it walks `PATH` but not
//! `PATHEXT`, and a batch file is not a program the kernel can execute in
//! the first place. `cmd.exe` has to run it.
//!
//! This was not hypothetical. The `dod-guard` entry in `~/.mcp.json` names
//! the bare command `npx`, and it was the one server of seven that failed
//! to start, with "program not found". The two that did start through
//! `npx` and `uvx` only worked because one config already wrapped itself in
//! `cmd /c` by hand and the other resolves to a real `uvx.exe`.

use std::path::{Path, PathBuf};

/// A command ready to spawn: the program to run, and any arguments that
/// must come before the configured ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCommand {
    pub program: String,
    pub prefix_args: Vec<String>,
}

impl ResolvedCommand {
    fn direct(program: &str) -> ResolvedCommand {
        ResolvedCommand {
            program: program.to_string(),
            prefix_args: Vec::new(),
        }
    }

    /// Run through `cmd.exe`, which is the only way to start a batch file.
    fn through_cmd(path: &Path) -> ResolvedCommand {
        ResolvedCommand {
            program: "cmd".to_string(),
            prefix_args: vec!["/c".to_string(), path.display().to_string()],
        }
    }
}

/// Resolve a configured command name for the current platform.
///
/// Off Windows this is the identity. The shell already resolves a
/// command on its own, and no batch file exists there to work around.
pub fn resolve_command(command: &str) -> ResolvedCommand {
    if !cfg!(windows) {
        return ResolvedCommand::direct(command);
    }
    resolve_windows(command, &path_entries(), &pathext_entries())
}

/// The Windows resolution, against explicit search paths and extensions so
/// a test does not depend on what the running machine has installed.
pub fn resolve_windows(command: &str, paths: &[PathBuf], extensions: &[String]) -> ResolvedCommand {
    // A path, or a name that already carries an extension, is taken as
    // given. The caller said exactly what to run.
    let named = Path::new(command);
    if named.extension().is_some() || command.contains('/') || command.contains('\\') {
        return classify(named, command);
    }

    for dir in paths {
        for ext in extensions {
            let candidate = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return classify(&candidate, command);
            }
        }
    }
    // Nothing matched. Hand the bare name back and let the spawn fail with
    // the operating system's own message rather than inventing one.
    ResolvedCommand::direct(command)
}

/// Decide whether a resolved file can be executed directly or needs
/// `cmd.exe` to run it.
fn classify(path: &Path, original: &str) -> ResolvedCommand {
    let is_batch = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if is_batch {
        return ResolvedCommand::through_cmd(path);
    }
    // A resolved non-batch file is run by its full path. A bare name that
    // was never resolved keeps its original spelling.
    if path.is_file() {
        return ResolvedCommand::direct(&path.display().to_string());
    }
    ResolvedCommand::direct(original)
}

/// The directories on `PATH`.
fn path_entries() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

/// The extensions on `PATHEXT`, each with its leading dot, falling back to
/// the usual Windows set when the variable is missing.
fn pathext_entries() -> Vec<String> {
    let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    raw.split(';')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| {
            if e.starts_with('.') {
                e.to_string()
            } else {
                format!(".{e}")
            }
        })
        .collect()
}
