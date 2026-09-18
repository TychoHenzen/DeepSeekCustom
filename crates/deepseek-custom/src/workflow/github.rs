use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{
    EligibleWorkItem, FeedbackError, QueueError, TerminalObservation, WorkflowEvidence,
    WorkflowFeedback, WorkflowFeedbackEvent, WorkflowFeedbackPort, WorkflowIdentity, WorkflowQueue,
    WorkflowRegistry, WorkflowRunId, WorkflowScheduler, WorkflowStepExecutor, WorkflowStepOutcome,
    WorkflowStore, WorkflowTick, WorkflowWorker,
};

#[derive(Debug, Clone)]
pub struct GitHubWorkflowConfig {
    pub owner: String,
    pub project_number: u64,
    pub repository: String,
    pub project_root: PathBuf,
    pub working_dir: PathBuf,
}

impl GitHubWorkflowConfig {
    pub fn from_env(project_root: &Path) -> Option<Self> {
        let owner = std::env::var("DEEPSEEK_WORKFLOW_OWNER").ok()?;
        let project_number = std::env::var("DEEPSEEK_WORKFLOW_PROJECT")
            .ok()?
            .parse()
            .ok()?;
        let repository = std::env::var("DEEPSEEK_WORKFLOW_REPOSITORY").ok()?;
        Some(Self {
            owner,
            project_number,
            repository,
            project_root: project_root.to_path_buf(),
            working_dir: project_root.to_path_buf(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct GitHubCliQueue {
    config: GitHubWorkflowConfig,
    project_item_ids: Arc<Mutex<HashMap<String, String>>>,
}

impl GitHubCliQueue {
    pub fn new(config: GitHubWorkflowConfig) -> Self {
        Self {
            config,
            project_item_ids: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn gh_json(&self, args: &[String]) -> Result<serde_json::Value, QueueError> {
        let output = Command::new("gh")
            .args(args)
            .output()
            .map_err(|error| QueueError::Adapter(format!("could not start gh: {error}")))?;
        if !output.status.success() {
            return Err(QueueError::Adapter(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| QueueError::Adapter(format!("gh returned invalid JSON: {error}")))
    }

    fn repository_parts(&self) -> Result<(&str, &str), QueueError> {
        self.config
            .repository
            .split_once('/')
            .ok_or_else(|| QueueError::Adapter("repository must be owner/name".to_string()))
    }

    fn mark_in_progress(&self, item_id: &str) -> Result<(), QueueError> {
        let project_id = self.project_node_id()?;
        let fields = self.gh_json(&[
            "project".to_string(),
            "field-list".to_string(),
            self.config.project_number.to_string(),
            "--owner".to_string(),
            self.config.owner.clone(),
            "--format".to_string(),
            "json".to_string(),
        ])?;
        let Some(status) = fields
            .get("fields")
            .and_then(serde_json::Value::as_array)
            .and_then(|fields| {
                fields.iter().find(|field| {
                    field.get("name").and_then(serde_json::Value::as_str) == Some("Status")
                })
            })
        else {
            return Err(QueueError::Adapter(
                "GitHub Project has no Status field".to_string(),
            ));
        };
        let Some(option_id) = status
            .get("options")
            .and_then(serde_json::Value::as_array)
            .and_then(|options| {
                options.iter().find_map(|option| {
                    (option.get("name").and_then(serde_json::Value::as_str) == Some("In Progress"))
                        .then(|| option.get("id").and_then(serde_json::Value::as_str))
                        .flatten()
                })
            })
        else {
            return Err(QueueError::Adapter(
                "GitHub Project has no In Progress option".to_string(),
            ));
        };
        let output = Command::new("gh")
            .args([
                "project",
                "item-edit",
                "--id",
                item_id,
                "--project-id",
                &project_id,
                "--field-id",
                status
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
                "--single-select-option-id",
                option_id,
            ])
            .output()
            .map_err(|error| QueueError::Adapter(format!("could not start gh: {error}")))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(QueueError::Adapter(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ))
        }
    }

    fn project_node_id(&self) -> Result<String, QueueError> {
        let payload = self.gh_json(&[
            "project".to_string(),
            "list".to_string(),
            "--owner".to_string(),
            self.config.owner.clone(),
            "--format".to_string(),
            "json".to_string(),
            "--limit".to_string(),
            "100".to_string(),
        ])?;
        payload
            .get("projects")
            .and_then(serde_json::Value::as_array)
            .and_then(|projects| {
                projects.iter().find_map(|project| {
                    (project.get("number").and_then(serde_json::Value::as_u64)
                        == Some(self.config.project_number))
                    .then(|| project.get("id").and_then(serde_json::Value::as_str))
                    .flatten()
                    .map(str::to_string)
                })
            })
            .ok_or_else(|| QueueError::Adapter("GitHub Project node id was not found".to_string()))
    }

    fn project_item_status(&self, item_id: &str) -> Result<Option<String>, QueueError> {
        let payload = self.gh_json(&[
            "project".to_string(),
            "item-list".to_string(),
            self.config.project_number.to_string(),
            "--owner".to_string(),
            self.config.owner.clone(),
            "--format".to_string(),
            "json".to_string(),
            "--limit".to_string(),
            "100".to_string(),
        ])?;
        Ok(payload
            .get("items")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| {
                items.iter().find_map(|item| {
                    (item.get("id").and_then(serde_json::Value::as_str) == Some(item_id))
                        .then(|| item.get("status").and_then(serde_json::Value::as_str))
                        .flatten()
                        .map(str::to_string)
                })
            }))
    }
}

impl WorkflowQueue for GitHubCliQueue {
    fn accepts(&self, identity: &WorkflowIdentity) -> bool {
        identity.repository.provider == crate::recovery::RepositoryProvider::GitHub
            && identity
                .repository
                .namespace
                .eq_ignore_ascii_case(&self.config.owner)
            && format!(
                "{}/{}",
                identity.repository.namespace, identity.repository.name
            )
            .eq_ignore_ascii_case(&self.config.repository)
            && identity.project.key == self.config.project_number.to_string()
    }

    fn eligible(&self) -> Result<Vec<EligibleWorkItem>, QueueError> {
        let args = vec![
            "project".to_string(),
            "item-list".to_string(),
            self.config.project_number.to_string(),
            "--owner".to_string(),
            self.config.owner.clone(),
            "--format".to_string(),
            "json".to_string(),
            "--limit".to_string(),
            "100".to_string(),
        ];
        let payload = self.gh_json(&args)?;
        let items = payload
            .get("items")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| QueueError::Adapter("gh project item-list omitted items".to_string()))?;
        let mut eligible = Vec::new();
        for item in items {
            let status = item.get("status").and_then(serde_json::Value::as_str);
            let content = item.get("content").unwrap_or(item);
            let repository = content
                .get("repository")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    content
                        .get("nameWithOwner")
                        .and_then(serde_json::Value::as_str)
                });
            if status != Some("Todo") || repository != Some(self.config.repository.as_str()) {
                continue;
            }
            if content.get("type").and_then(serde_json::Value::as_str) == Some("PullRequest") {
                continue;
            }
            let Some(number) = content.get("number").and_then(serde_json::Value::as_u64) else {
                continue;
            };
            let title = content
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Untitled workflow item")
                .to_string();
            let identity = WorkflowIdentity::github(
                self.config.owner.clone(),
                self.config.repository.split('/').nth(1).unwrap_or_default(),
                self.config.project_number.to_string(),
                number,
                self.config.project_root.to_string_lossy(),
                self.config.working_dir.to_string_lossy(),
                format!("codex/{number}-workflow"),
                None,
            );
            if let Some(item_id) = item.get("id").and_then(serde_json::Value::as_str) {
                self.project_item_ids
                    .lock()
                    .expect("workflow queue item map is not poisoned")
                    .insert(identity.key(), item_id.to_string());
            }
            eligible.push(EligibleWorkItem {
                identity,
                title,
                priority: 0,
                created_at: number,
            });
        }
        Ok(eligible)
    }

    fn claim(&mut self, item: &EligibleWorkItem, _run_id: WorkflowRunId) -> Result<(), QueueError> {
        let (owner, repository) = self.repository_parts()?;
        let output = Command::new("gh")
            .args([
                "issue",
                "edit",
                &item.identity.item.number.to_string(),
                "--repo",
                &format!("{owner}/{repository}"),
                "--add-assignee",
                "@me",
            ])
            .output()
            .map_err(|error| QueueError::Adapter(format!("could not start gh: {error}")))?;
        if !output.status.success() {
            return Err(QueueError::Adapter(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let item_id = self
            .project_item_ids
            .lock()
            .expect("workflow queue item map is not poisoned")
            .get(&item.identity.key())
            .cloned()
            .ok_or_else(|| {
                QueueError::Adapter("GitHub Project item id was not returned".to_string())
            })?;
        if self.project_item_status(&item_id)?.as_deref() != Some("Todo") {
            return Err(QueueError::AlreadyClaimed);
        }
        self.mark_in_progress(&item_id)?;
        if self.project_item_status(&item_id)?.as_deref() == Some("In Progress") {
            Ok(())
        } else {
            Err(QueueError::Adapter(
                "GitHub Project claim was not observed as In Progress".to_string(),
            ))
        }
    }

    fn terminal_observation(
        &self,
        identity: &WorkflowIdentity,
    ) -> Result<TerminalObservation, QueueError> {
        let (owner, repository) = self.repository_parts()?;
        let repo = format!("{owner}/{repository}");
        let pull_request_args = vec![
            "pr".to_string(),
            "list".to_string(),
            "--repo".to_string(),
            repo.clone(),
            "--head".to_string(),
            identity.branch.clone(),
            "--state".to_string(),
            "merged".to_string(),
            "--json".to_string(),
            "number".to_string(),
        ];
        let merged = self.gh_json(&pull_request_args)?;
        if merged
            .get("[]")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| !items.is_empty())
            || merged.as_array().is_some_and(|items| !items.is_empty())
        {
            return Ok(TerminalObservation::Merged);
        }
        let issue_args = vec![
            "issue".to_string(),
            "view".to_string(),
            identity.item.number.to_string(),
            "--repo".to_string(),
            repo,
            "--json".to_string(),
            "state".to_string(),
        ];
        let payload = self.gh_json(&issue_args)?;
        match payload.get("state").and_then(serde_json::Value::as_str) {
            Some("CLOSED") => Ok(TerminalObservation::Closed),
            Some("OPEN") => Ok(TerminalObservation::Open),
            _ => Ok(TerminalObservation::Unknown),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GitHubCliFeedback {
    config: GitHubWorkflowConfig,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ConfiguredStepExecutor;

impl WorkflowStepExecutor for ConfiguredStepExecutor {
    fn execute(&mut self, run: &super::WorkflowRunRecord) -> Result<WorkflowStepOutcome, String> {
        let key = format!(
            "DEEPSEEK_WORKFLOW_STEP_{}",
            run.current_step.to_string().to_ascii_uppercase()
        );
        let Some(command) = std::env::var_os(&key) else {
            return Ok(WorkflowStepOutcome::NeedsDecision {
                question: format!(
                    "No command is configured for workflow step {}.",
                    run.current_step
                ),
                evidence: vec![WorkflowEvidence::new("worker", format!("missing {key}"))],
                requested_action: format!(
                    "Set {key} to the reviewed step command, then answer this question."
                ),
            });
        };
        let command = command.to_string_lossy();
        let mut child = if cfg!(windows) {
            let mut process = Command::new("cmd");
            process
                .args(["/c", &command])
                .current_dir(&run.identity.working_dir)
                .env("DEEPSEEK_WORKFLOW_RUN_ID", run.id.as_str())
                .env("DEEPSEEK_WORKFLOW_SESSION_ID", &run.session_id)
                .env("DEEPSEEK_WORKFLOW_CONTEXT_ID", &run.context_id)
                .env("DEEPSEEK_WORKFLOW_BRANCH", &run.identity.branch)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        } else {
            let mut process = Command::new("sh");
            process
                .args(["-c", &command])
                .current_dir(&run.identity.working_dir)
                .env("DEEPSEEK_WORKFLOW_RUN_ID", run.id.as_str())
                .env("DEEPSEEK_WORKFLOW_SESSION_ID", &run.session_id)
                .env("DEEPSEEK_WORKFLOW_CONTEXT_ID", &run.context_id)
                .env("DEEPSEEK_WORKFLOW_BRANCH", &run.identity.branch)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        }
        .map_err(|error| format!("could not start {key}: {error}"))?;
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            if child
                .try_wait()
                .map_err(|error| format!("could not observe {key}: {error}"))?
                .is_some()
            {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(WorkflowStepOutcome::Blocked {
                    question: format!("Workflow step {key} exceeded the five-minute bound."),
                    evidence: vec![WorkflowEvidence::new("executor", "process timed out")],
                    requested_action:
                        "Inspect the step command and resume only after deciding how to recover."
                            .to_string(),
                });
            }
            thread::sleep(Duration::from_millis(100));
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("could not collect {key} output: {error}"))?;
        let detail = format!(
            "stdout: {}\nstderr: {}",
            bounded_output(&output.stdout),
            bounded_output(&output.stderr)
        );
        if output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("WORKFLOW_STEP_COMPLETED")
        {
            Ok(WorkflowStepOutcome::Completed {
                evidence: vec![WorkflowEvidence::new(key, detail)],
            })
        } else if output.status.success() {
            Ok(WorkflowStepOutcome::NeedsDecision {
                question: format!("Workflow step {key} did not emit WORKFLOW_STEP_COMPLETED."),
                evidence: vec![WorkflowEvidence::new(key, detail)],
                requested_action:
                    "Review the step artifact and provide a structured completion result."
                        .to_string(),
            })
        } else {
            Ok(WorkflowStepOutcome::Retryable {
                reason: format!("{key} exited with {}", output.status),
                evidence: vec![WorkflowEvidence::new(key, detail)],
            })
        }
    }
}

fn bounded_output(bytes: &[u8]) -> String {
    let value = String::from_utf8_lossy(bytes);
    let mut output: String = value.chars().take(4_000).collect();
    if value.chars().count() > 4_000 {
        output.push('\u{2026}');
    }
    output
}

impl GitHubCliFeedback {
    pub fn new(config: GitHubWorkflowConfig) -> Self {
        Self { config }
    }

    fn repository(&self) -> Result<String, FeedbackError> {
        self.config
            .repository
            .split_once('/')
            .map(|(owner, repository)| format!("{owner}/{repository}"))
            .ok_or_else(|| FeedbackError::Adapter("repository must be owner/name".to_string()))
    }
}

impl WorkflowFeedbackPort for GitHubCliFeedback {
    fn publish_question(
        &mut self,
        identity: &WorkflowIdentity,
        feedback: &WorkflowFeedback,
    ) -> Result<String, FeedbackError> {
        let repository = self.repository()?;
        let existing = Command::new("gh")
            .args([
                "issue",
                "view",
                &identity.item.number.to_string(),
                "--repo",
                &repository,
                "--comments",
                "--json",
                "comments",
            ])
            .output()
            .map_err(|error| FeedbackError::Adapter(format!("could not start gh: {error}")))?;
        if existing.status.success() {
            let payload: CommentPayload =
                serde_json::from_slice(&existing.stdout).map_err(|error| {
                    FeedbackError::Adapter(format!("gh returned invalid JSON: {error}"))
                })?;
            if payload
                .comments
                .iter()
                .any(|comment| comment.body.contains(&format!("feedback={}", feedback.id)))
            {
                return Ok(format!("github:feedback/{}", feedback.id));
            }
        }
        let body = format!(
            "<!-- deepseek-workflow feedback={} -->\n## Workflow question\n{}\n\n### Evidence\n{}\n\n### Requested action\n{}\n\nReply with `<!-- deepseek-workflow answer={} -->` followed by the answer.",
            feedback.id,
            feedback.question,
            feedback
                .evidence
                .iter()
                .map(|evidence| format!("- {}: {}", evidence.source, evidence.detail))
                .collect::<Vec<_>>()
                .join("\n"),
            feedback.requested_action,
            feedback.id,
        );
        let output = Command::new("gh")
            .args([
                "issue",
                "comment",
                &identity.item.number.to_string(),
                "--repo",
                &repository,
                "--body",
                &body,
            ])
            .output()
            .map_err(|error| FeedbackError::Adapter(format!("could not start gh: {error}")))?;
        if !output.status.success() {
            return Err(FeedbackError::Adapter(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn poll(
        &mut self,
        run_id: WorkflowRunId,
        identity: &WorkflowIdentity,
    ) -> Result<Vec<WorkflowFeedbackEvent>, FeedbackError> {
        let repository = self.repository()?;
        let issue_numbers = vec![identity.item.number];
        let mut events = Vec::new();
        for issue_number in issue_numbers {
            let output = Command::new("gh")
                .args([
                    "issue",
                    "view",
                    &issue_number.to_string(),
                    "--repo",
                    &repository,
                    "--comments",
                    "--json",
                    "comments",
                ])
                .output()
                .map_err(|error| FeedbackError::Adapter(format!("could not start gh: {error}")))?;
            if !output.status.success() {
                return Err(FeedbackError::Adapter(
                    String::from_utf8_lossy(&output.stderr).trim().to_string(),
                ));
            }
            let payload: CommentPayload =
                serde_json::from_slice(&output.stdout).map_err(|error| {
                    FeedbackError::Adapter(format!("gh returned invalid JSON: {error}"))
                })?;
            for comment in payload.comments {
                if comment.body.contains("## Workflow question") {
                    continue;
                }
                let Some(feedback_id) = comment
                    .body
                    .split("<!-- deepseek-workflow answer=")
                    .nth(1)
                    .and_then(|value| value.split(" -->").next())
                else {
                    continue;
                };
                events.push(WorkflowFeedbackEvent {
                    run_id,
                    feedback_id: feedback_id.to_string(),
                    answer: comment.body,
                    evidence: Vec::new(),
                });
            }
        }
        Ok(events)
    }
}

#[derive(Debug, Deserialize)]
struct CommentPayload {
    #[serde(default)]
    comments: Vec<CommentRecord>,
}

#[derive(Debug, Deserialize)]
struct CommentRecord {
    body: String,
}

pub fn spawn_github_worker(
    config: GitHubWorkflowConfig,
    store: WorkflowStore,
    worker_name: impl Into<String>,
) -> thread::JoinHandle<()> {
    let worker_name = worker_name.into();
    thread::spawn(move || {
        let registry = match WorkflowRegistry::new(store, 4) {
            Ok(registry) => registry,
            Err(error) => {
                tracing::warn!(error = %error, "workflow worker could not open registry");
                return;
            }
        };
        let queue = GitHubCliQueue::new(config.clone());
        let feedback = GitHubCliFeedback::new(config);
        let mut worker = WorkflowWorker::new(
            WorkflowScheduler::new(queue, feedback, registry, worker_name),
            ConfiguredStepExecutor,
        );
        loop {
            match worker.run_once() {
                Ok(WorkflowTick::QueueEmpty | WorkflowTick::CapacityReached) => {
                    tracing::info!("workflow worker stopped: queue is empty or at capacity");
                    break;
                }
                Ok(WorkflowTick::Waiting { state, .. })
                    if state == super::WorkflowRunState::Interrupted
                        || state == super::WorkflowRunState::Failed =>
                {
                    tracing::info!(state = %state, "workflow worker stopped at a durable gate");
                    break;
                }
                Ok(_) => thread::sleep(Duration::from_secs(5)),
                Err(error) => {
                    tracing::warn!(error = %error, "workflow worker stopped after an adapter error");
                    break;
                }
            }
        }
    })
}
