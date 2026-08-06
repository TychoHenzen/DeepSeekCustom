use std::path::PathBuf;

use tracing::{debug, warn};

const DECISIONS_DIR: &str = ".autopilot";
const DECISIONS_FILE: &str = "decisions.log";
/// Unconditionally `pub`: the moved test crate builds its temp-directory
/// fixtures against this exact filename, and it has no side effects that
/// would make a narrower seam meaningful.
pub const DEFAULT_POLICY_FILE: &str = "autopilot-policy.md";

/// Owns the policy file and decision log used by the automatic question
/// answerer. The policy file is read fresh on every call, never cached,
/// since the user may edit it mid-run.
pub struct PolicyStore {
    project_root: PathBuf,
    policy_path: Option<String>,
}

impl PolicyStore {
    /// Create a new store rooted at `project_root`. `policy_path` overrides
    /// the default `<project_root>/autopilot-policy.md` location. A relative
    /// path resolves against `project_root`. An absolute path is used as is.
    pub fn new(project_root: PathBuf, policy_path: Option<String>) -> Self {
        Self {
            project_root,
            policy_path,
        }
    }

    /// Resolve the policy file path, honoring an override.
    pub fn resolved_policy_path(&self) -> PathBuf {
        match &self.policy_path {
            Some(path) => {
                let p = PathBuf::from(path);
                if p.is_absolute() {
                    p
                } else {
                    self.project_root.join(p)
                }
            }
            None => self.project_root.join(DEFAULT_POLICY_FILE),
        }
    }

    fn decisions_log_path(&self) -> PathBuf {
        self.project_root.join(DECISIONS_DIR).join(DECISIONS_FILE)
    }

    /// Read the policy file fresh from disk. A missing file yields an empty
    /// policy and logs at debug. An unreadable file (present but unreadable)
    /// also yields an empty policy but logs at warn.
    pub fn load_policy(&self) -> String {
        let path = self.resolved_policy_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!("autopilot: no policy file at {}", path.display());
                String::new()
            }
            Err(e) => {
                warn!(
                    "autopilot: could not read policy file at {}: {e}",
                    path.display()
                );
                String::new()
            }
        }
    }

    /// Return the most recent `limit` decision-log entries, newest last. A
    /// missing log yields an empty list.
    pub fn recent_decisions(&self, limit: usize) -> Vec<String> {
        let path = self.decisions_log_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => return Vec::new(),
        };

        let lines: Vec<String> = content
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.to_string())
            .collect();

        let start = lines.len().saturating_sub(limit);
        lines[start..].to_vec()
    }

    /// Append one resolved question/answer pair to the decision log. Creates
    /// the `.autopilot` directory on first append. A write failure logs at
    /// warn and is otherwise ignored: losing a log line must never take a
    /// run down.
    pub fn append_decision(&self, question: &str, answer: &str) {
        let path = self.decisions_log_path();
        let dir = self.project_root.join(DECISIONS_DIR);

        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(
                "autopilot: could not create decision log directory {}: {e}",
                dir.display()
            );
            return;
        }

        let line = format!(
            "question={} answer={}\n",
            single_line(question),
            single_line(answer)
        );

        use std::io::Write as _;
        let result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(line.as_bytes()));

        if let Err(e) = result {
            warn!(
                "autopilot: could not append decision to {}: {e}",
                path.display()
            );
        }
    }
}

/// Replace newlines with spaces so a value stays on one line in the log.
fn single_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// Format the policy text plus recent decisions into one prompt section for
/// pasting into an LLM prompt. Returns an empty string when there is neither.
pub fn format_policy_prompt_section(policy: &str, recent_decisions: &[String]) -> String {
    let policy_trimmed = policy.trim();
    if policy_trimmed.is_empty() && recent_decisions.is_empty() {
        return String::new();
    }

    let mut parts: Vec<String> = Vec::new();

    if !policy_trimmed.is_empty() {
        parts.push(format!("## Autopilot Policy\n\n{policy_trimmed}"));
    }

    if !recent_decisions.is_empty() {
        let joined = recent_decisions.join("\n");
        parts.push(format!("## Recent Decisions\n\n{joined}"));
    }

    parts.join("\n\n")
}
