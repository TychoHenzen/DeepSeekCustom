//! Stage 0 OpenSpec validation and typed change input.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::ProcedureTask;
use crate::mcp::spawn::{ResolvedCommand, resolve_command};

const OPENSPEC_COMMAND: &str = "openspec";

/// Successful output from strict OpenSpec validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSpecValidation {
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Exact process evidence from a failed strict OpenSpec validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSpecCommandFailure {
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl std::fmt::Display for OpenSpecCommandFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "OpenSpec validation failed with exit code {:?}\nstdout:\n{}\nstderr:\n{}",
            self.exit_code, self.stdout, self.stderr
        )
    }
}

/// Failure before Stage 1 localization can begin.
#[derive(Debug, Error)]
pub enum OpenSpecInputError {
    #[error("failed to run `{command}` in {working_dir}: {source}")]
    CommandSpawn {
        command: String,
        working_dir: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    ValidationFailed(Box<OpenSpecCommandFailure>),
    #[error("OpenSpec input error: {0}")]
    Input(String),
}

/// One active change and the unchecked tasks available for selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSpecChange {
    pub id: String,
    pub tasks: Vec<ProcedureTask>,
}

/// The compact parts of the proposal that define the change's scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalScope {
    pub why: String,
    pub what_changes: String,
}

/// One scenario selected from an OpenSpec requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioSlice {
    pub name: String,
    pub text: String,
}

/// One requirement and only the scenarios retained for the selected slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementSlice {
    pub name: String,
    pub text: String,
    pub scenarios: Vec<ScenarioSlice>,
}

/// A typed capability delta used when a task has no `covers` binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDeltaSlice {
    pub capability: String,
    pub purpose: String,
    pub requirements: Vec<RequirementSlice>,
}

/// The smallest spec selection that can define the selected task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "binding", rename_all = "snake_case")]
pub enum ContractSelection {
    Bound {
        capability: String,
        requirement: RequirementSlice,
    },
    Unbound {
        capability_delta: CapabilityDeltaSlice,
    },
}

/// Stage 1 contract context for one unchecked task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedContractSlice {
    pub change_id: String,
    pub task: ProcedureTask,
    pub proposal_scope: ProposalScope,
    pub selection: ContractSelection,
}

/// A selected contract whose change already passed strict validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedContractInput {
    pub validation: OpenSpecValidation,
    pub contract: SelectedContractSlice,
}

/// Reads procedure input from one OpenSpec project.
#[derive(Clone)]
pub struct OpenSpecInput {
    project_root: PathBuf,
    command: String,
}

impl OpenSpecInput {
    /// Use the `openspec` command resolved from the process `PATH` and
    /// `PATHEXT` at validation time.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self::with_command(project_root, OPENSPEC_COMMAND)
    }

    /// Override the command name or path. This keeps tests and embedded
    /// installations independent from the machine-wide OpenSpec install.
    pub fn with_command(project_root: impl Into<PathBuf>, command: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
            command: command.into(),
        }
    }

    /// Run `openspec validate <change> --strict --no-interactive` and retain
    /// the process output without trimming or rewriting it.
    pub fn validate_change(
        &self,
        change_id: &str,
    ) -> Result<OpenSpecValidation, OpenSpecInputError> {
        let resolved = resolve_openspec_command(&self.command);
        let mut args = resolved.prefix_args;
        args.extend([
            "validate".to_string(),
            change_id.to_string(),
            "--strict".to_string(),
            "--no-interactive".to_string(),
        ]);
        let command = command_display(&resolved.program, &args);
        let output = Command::new(&resolved.program)
            .args(&args)
            .current_dir(&self.project_root)
            .output()
            .map_err(|source| OpenSpecInputError::CommandSpawn {
                command: command.join(" "),
                working_dir: self.project_root.clone(),
                source,
            })?;
        let validation = OpenSpecValidation {
            command,
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        };
        if output.status.success() {
            return Ok(validation);
        }

        let failure = OpenSpecCommandFailure {
            command: validation.command,
            exit_code: validation.exit_code,
            stdout: validation.stdout,
            stderr: validation.stderr,
        };
        Err(OpenSpecInputError::ValidationFailed(Box::new(failure)))
    }

    /// List change directories with a `tasks.md`, excluding the archive.
    /// Each returned task is unchecked and therefore selectable.
    pub fn active_changes(&self) -> Result<Vec<OpenSpecChange>, OpenSpecInputError> {
        let changes_dir = self.project_root.join("openspec").join("changes");
        let entries = std::fs::read_dir(&changes_dir).map_err(|error| {
            OpenSpecInputError::Input(format!(
                "could not read active changes at {}: {error}",
                changes_dir.display()
            ))
        })?;
        let mut paths = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| OpenSpecInputError::Input(error.to_string()))?
                .path();
            if path.is_dir()
                && path.file_name().is_some_and(|name| name != "archive")
                && path.join("tasks.md").is_file()
            {
                paths.push(path);
            }
        }
        paths.sort();

        paths
            .into_iter()
            .map(|path| self.load_change_path(&path))
            .collect()
    }

    /// Validate a change before reading its selected task and contract.
    pub fn validate_and_select_task(
        &self,
        change_id: &str,
        task_id: &str,
    ) -> Result<ValidatedContractInput, OpenSpecInputError> {
        let validation = self.validate_change(change_id)?;
        let change_dir = self.change_dir(change_id)?;
        let parsed_tasks = parse_tasks(&read_artifact(&change_dir.join("tasks.md"))?)?;
        let parsed = parsed_tasks
            .iter()
            .find(|task| task.task.id == task_id)
            .ok_or_else(|| {
                OpenSpecInputError::Input(format!(
                    "OpenSpec change `{change_id}` has no task `{task_id}`"
                ))
            })?;
        if parsed.completed {
            return Err(OpenSpecInputError::Input(format!(
                "OpenSpec task `{task_id}` in change `{change_id}` is completed and cannot be selected"
            )));
        }

        let proposal = read_artifact(&change_dir.join("proposal.md"))?;
        // ASSUMPTION: `Why` and `What Changes` are the compact proposal
        // scope. Impact and capability inventories are not stage context.
        let proposal_scope = ProposalScope {
            why: section_body(&proposal, "## Why").ok_or_else(|| {
                OpenSpecInputError::Input(format!(
                    "OpenSpec proposal for `{change_id}` has no `## Why` section"
                ))
            })?,
            what_changes: section_body(&proposal, "## What Changes").ok_or_else(|| {
                OpenSpecInputError::Input(format!(
                    "OpenSpec proposal for `{change_id}` has no `## What Changes` section"
                ))
            })?,
        };
        let specs = load_capability_deltas(&change_dir)?;
        let selection = match parsed.task.covers.as_deref() {
            Some(binding) => select_bound_contract(binding, &specs)?,
            None => {
                // ASSUMPTION: without a binding or another task-to-capability
                // marker, only a single capability delta is deterministic.
                let [capability_delta] = specs.as_slice() else {
                    return Err(OpenSpecInputError::Input(format!(
                        "unbound task `{task_id}` in change `{change_id}` needs exactly one capability delta, found {}",
                        specs.len()
                    )));
                };
                ContractSelection::Unbound {
                    capability_delta: capability_delta.clone(),
                }
            }
        };

        Ok(ValidatedContractInput {
            validation,
            contract: SelectedContractSlice {
                change_id: change_id.to_string(),
                task: parsed.task.clone(),
                proposal_scope,
                selection,
            },
        })
    }

    fn load_change_path(&self, change_dir: &Path) -> Result<OpenSpecChange, OpenSpecInputError> {
        let id = change_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                OpenSpecInputError::Input(format!(
                    "active change path has no UTF-8 name: {}",
                    change_dir.display()
                ))
            })?
            .to_string();
        let parsed = parse_tasks(&read_artifact(&change_dir.join("tasks.md"))?)?;
        Ok(OpenSpecChange {
            id,
            tasks: parsed
                .into_iter()
                .filter(|task| !task.completed)
                .map(|task| task.task)
                .collect(),
        })
    }

    fn change_dir(&self, change_id: &str) -> Result<PathBuf, OpenSpecInputError> {
        let id_path = Path::new(change_id);
        if id_path.components().count() != 1 || change_id == "archive" {
            return Err(OpenSpecInputError::Input(format!(
                "invalid active OpenSpec change id `{change_id}`"
            )));
        }
        let path = self
            .project_root
            .join("openspec")
            .join("changes")
            .join(change_id);
        if !path.is_dir() {
            return Err(OpenSpecInputError::Input(format!(
                "OpenSpec change `{change_id}` does not exist at {}",
                path.display()
            )));
        }
        Ok(path)
    }
}

fn resolve_openspec_command(command: &str) -> ResolvedCommand {
    let resolved = resolve_command(command);
    if !cfg!(windows) || command != OPENSPEC_COMMAND || resolved.prefix_args.len() != 2 {
        return resolved;
    }

    let batch = Path::new(&resolved.prefix_args[1]);
    let Some(directory) = batch.parent() else {
        return resolved;
    };
    let script = directory.join("node_modules/@fission-ai/openspec/bin/openspec.js");
    let node = resolve_command("node");
    if !script.is_file() || !Path::new(&node.program).is_file() {
        return resolved;
    }

    ResolvedCommand {
        program: node.program,
        prefix_args: vec![script.display().to_string()],
    }
}

#[derive(Debug, Clone)]
struct ParsedTask {
    task: ProcedureTask,
    completed: bool,
}

fn parse_tasks(markdown: &str) -> Result<Vec<ParsedTask>, OpenSpecInputError> {
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut tasks = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let (completed, remainder) = if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
            (false, rest)
        } else if let Some(rest) = trimmed
            .strip_prefix("- [x] ")
            .or_else(|| trimmed.strip_prefix("- [X] "))
        {
            (true, rest)
        } else {
            continue;
        };
        let (id, text) = remainder.split_once(' ').ok_or_else(|| {
            OpenSpecInputError::Input(format!("task line has no text: `{trimmed}`"))
        })?;
        let covers = lines
            .get(index + 1)
            .and_then(|next| parse_covers_comment(next));
        tasks.push(ParsedTask {
            task: ProcedureTask {
                id: id.to_string(),
                text: text.trim().to_string(),
                covers,
            },
            completed,
        });
    }
    Ok(tasks)
}

fn parse_covers_comment(line: &str) -> Option<String> {
    line.trim()
        .strip_prefix("<!-- covers:")
        .and_then(|value| value.strip_suffix("-->"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn read_artifact(path: &Path) -> Result<String, OpenSpecInputError> {
    std::fs::read_to_string(path).map_err(|error| {
        OpenSpecInputError::Input(format!(
            "could not read OpenSpec artifact {}: {error}",
            path.display()
        ))
    })
}

fn section_body(markdown: &str, heading: &str) -> Option<String> {
    let mut lines = markdown.lines();
    lines.find(|line| line.trim() == heading)?;
    let body = lines
        .take_while(|line| !line.trim_start().starts_with("## "))
        .collect::<Vec<_>>()
        .join("\n");
    Some(body.trim().to_string())
}

fn load_capability_deltas(
    change_dir: &Path,
) -> Result<Vec<CapabilityDeltaSlice>, OpenSpecInputError> {
    let specs_dir = change_dir.join("specs");
    if !specs_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    collect_spec_files(&specs_dir, &mut files)?;
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let parent = path.parent().ok_or_else(|| {
                OpenSpecInputError::Input(format!("spec path has no parent: {}", path.display()))
            })?;
            let capability = parent
                .strip_prefix(&specs_dir)
                .map_err(|_| {
                    OpenSpecInputError::Input(format!(
                        "spec path is outside its change: {}",
                        path.display()
                    ))
                })?
                .iter()
                .map(|part| part.to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            parse_capability_delta(&capability, &read_artifact(&path)?)
        })
        .collect()
}

fn collect_spec_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), OpenSpecInputError> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        OpenSpecInputError::Input(format!(
            "could not read OpenSpec specs at {}: {error}",
            dir.display()
        ))
    })?;
    for entry in entries {
        let path = entry
            .map_err(|error| OpenSpecInputError::Input(error.to_string()))?
            .path();
        if path.is_dir() {
            collect_spec_files(&path, files)?;
        } else if path.file_name().is_some_and(|name| name == "spec.md") {
            files.push(path);
        }
    }
    Ok(())
}

fn parse_capability_delta(
    capability: &str,
    markdown: &str,
) -> Result<CapabilityDeltaSlice, OpenSpecInputError> {
    let purpose = section_body(markdown, "## Purpose").unwrap_or_default();
    let mut requirements = Vec::new();
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        let Some(name) = lines[index]
            .trim()
            .strip_prefix("### Requirement:")
            .map(str::trim)
        else {
            index += 1;
            continue;
        };
        let start = index + 1;
        index = start;
        while index < lines.len() && !lines[index].trim().starts_with("### Requirement:") {
            index += 1;
        }
        requirements.push(parse_requirement(name, &lines[start..index]));
    }
    if requirements.is_empty() {
        return Err(OpenSpecInputError::Input(format!(
            "capability delta `{capability}` has no requirements"
        )));
    }
    Ok(CapabilityDeltaSlice {
        capability: capability.to_string(),
        purpose,
        requirements,
    })
}

fn parse_requirement(name: &str, lines: &[&str]) -> RequirementSlice {
    let first_scenario = lines
        .iter()
        .position(|line| line.trim().starts_with("#### Scenario:"))
        .unwrap_or(lines.len());
    let text = lines[..first_scenario].join("\n").trim().to_string();
    let mut scenarios = Vec::new();
    let mut index = first_scenario;
    while index < lines.len() {
        let Some(scenario_name) = lines[index]
            .trim()
            .strip_prefix("#### Scenario:")
            .map(str::trim)
        else {
            index += 1;
            continue;
        };
        let start = index + 1;
        index = start;
        while index < lines.len() && !lines[index].trim().starts_with("#### Scenario:") {
            index += 1;
        }
        scenarios.push(ScenarioSlice {
            name: scenario_name.to_string(),
            text: lines[start..index].join("\n").trim().to_string(),
        });
    }
    RequirementSlice {
        name: name.to_string(),
        text,
        scenarios,
    }
}

fn select_bound_contract(
    binding: &str,
    specs: &[CapabilityDeltaSlice],
) -> Result<ContractSelection, OpenSpecInputError> {
    let parts = binding.split("::").map(str::trim).collect::<Vec<_>>();
    let [capability, requirement_name, scenario_name] = parts.as_slice() else {
        return Err(OpenSpecInputError::Input(format!(
            "invalid covers binding `{binding}`; expected `capability :: requirement :: scenario`"
        )));
    };
    let delta = specs
        .iter()
        .find(|delta| delta.capability == *capability)
        .ok_or_else(|| {
            OpenSpecInputError::Input(format!(
                "covers binding `{binding}` names missing capability `{capability}`"
            ))
        })?;
    let requirement = delta
        .requirements
        .iter()
        .find(|requirement| requirement.name == *requirement_name)
        .ok_or_else(|| {
            OpenSpecInputError::Input(format!(
                "covers binding `{binding}` names missing requirement `{requirement_name}`"
            ))
        })?;
    let scenario = requirement
        .scenarios
        .iter()
        .find(|scenario| scenario.name == *scenario_name)
        .ok_or_else(|| {
            OpenSpecInputError::Input(format!(
                "covers binding `{binding}` names missing scenario `{scenario_name}`"
            ))
        })?;
    Ok(ContractSelection::Bound {
        capability: capability.to_string(),
        requirement: RequirementSlice {
            name: requirement.name.clone(),
            text: requirement.text.clone(),
            scenarios: vec![scenario.clone()],
        },
    })
}

fn command_display(program: &str, args: &[String]) -> Vec<String> {
    std::iter::once(program.to_string())
        .chain(args.iter().cloned())
        .collect()
}

impl OpenSpecCommandFailure {
    /// Return the exact argument vector that was passed to the operating
    /// system, including any `cmd /c` prefix selected on Windows.
    pub fn command(&self) -> &[String] {
        &self.command
    }
}

impl OpenSpecInput {
    /// Return the project root used as the validation process directory.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}
