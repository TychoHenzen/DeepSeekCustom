//! Provider-neutral repository and pull-request identity resolution.
//!
//! Recovery must never infer a repository or pull request from a vague label.
//! This module accepts explicit URLs, checkout remotes, and an explicitly
//! verified project reference, then records one normalized identity. Conflicts
//! are errors rather than a reason to choose one source silently.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A repository provider supported by the recovery identity boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryProvider {
    GitHub,
    AzureDevOps,
}

impl fmt::Display for RepositoryProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitHub => f.write_str("github"),
            Self::AzureDevOps => f.write_str("azure_devops"),
        }
    }
}

/// The normalized repository identity used by a recovery run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepositoryIdentity {
    pub provider: RepositoryProvider,
    /// GitHub owner or Azure DevOps organization.
    pub namespace: String,
    pub name: String,
    /// Azure DevOps project when the repository reference carries it.
    pub project: Option<String>,
}

impl RepositoryIdentity {
    fn new(provider: RepositoryProvider, namespace: &str, name: &str) -> Self {
        Self {
            provider,
            namespace: normalize_segment(namespace),
            name: normalize_segment(name),
            project: None,
        }
    }

    fn new_azure(namespace: &str, project: &str, name: &str) -> Self {
        Self {
            provider: RepositoryProvider::AzureDevOps,
            namespace: normalize_segment(namespace),
            name: normalize_segment(name),
            project: Some(normalize_segment(project)),
        }
    }
}

/// A project reference must come from a verified provider fact. It is not
/// parsed from a model's prose, because a repository can belong to several
/// projects at once.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectReference {
    pub provider: RepositoryProvider,
    pub key: String,
}

impl ProjectReference {
    pub fn new(provider: RepositoryProvider, key: impl Into<String>) -> Self {
        Self {
            provider,
            key: normalize_project_key(&key.into()),
        }
    }

    pub fn github(key: impl Into<String>) -> Self {
        Self::new(RepositoryProvider::GitHub, key)
    }

    pub fn azure_devops(key: impl Into<String>) -> Self {
        Self::new(RepositoryProvider::AzureDevOps, key)
    }
}

/// The normalized project identity used by a recovery run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectIdentity {
    pub provider: RepositoryProvider,
    pub key: String,
}

/// The kind of work item a reference identifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemKind {
    Issue,
    PullRequest,
}

/// The item number within the resolved repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkItemIdentity {
    pub kind: WorkItemKind,
    pub number: u64,
}

/// One immutable identity for a recovery run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResolvedWorkIdentity {
    pub provider: RepositoryProvider,
    pub repository: RepositoryIdentity,
    pub project: ProjectIdentity,
    pub item: WorkItemIdentity,
}

impl ResolvedWorkIdentity {
    pub fn summary(&self) -> String {
        format!(
            "{}:{}/{} project={} {}#{}",
            self.provider,
            self.repository.namespace,
            self.repository.name,
            self.project.key,
            match self.item.kind {
                WorkItemKind::Issue => "issue",
                WorkItemKind::PullRequest => "pull_request",
            },
            self.item.number,
        )
    }
}

/// Inputs accepted by [`resolve_identity`]. At least one repository source,
/// one item reference, and one verified project reference are required.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityInput {
    pub repository_reference: Option<String>,
    pub checkout_remote: Option<String>,
    pub project: Option<ProjectReference>,
    pub item_reference: Option<String>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum IdentityError {
    #[error("missing verified identity fact: {fact}")]
    MissingFact { fact: &'static str },
    #[error("unsupported repository reference: {0}")]
    UnsupportedReference(String),
    #[error("ambiguous pull-request reference: {0}")]
    AmbiguousItemReference(String),
    #[error("provider mismatch between {reference_source} ({found}) and {expected}")]
    ProviderMismatch {
        reference_source: String,
        expected: RepositoryProvider,
        found: RepositoryProvider,
    },
    #[error("repository mismatch between {reference_source} and the selected repository")]
    RepositoryMismatch { reference_source: String },
    #[error("project reference does not match the resolved provider")]
    ProjectProviderMismatch,
    #[error("item reference does not match the resolved repository")]
    ItemRepositoryMismatch,
    #[error("project reference does not match the resolved repository project")]
    ProjectMismatch,
    #[error("item number is invalid: {0}")]
    InvalidItemNumber(String),
}

#[derive(Debug, Clone)]
struct ParsedItem {
    provider: RepositoryProvider,
    repository: RepositoryIdentity,
    item: WorkItemIdentity,
}

/// Resolve equivalent explicit and checkout references into one identity.
pub fn resolve_identity(input: &IdentityInput) -> Result<ResolvedWorkIdentity, IdentityError> {
    let explicit_repository = input
        .repository_reference
        .as_deref()
        .map(parse_repository_reference)
        .transpose()?;
    let checkout_repository = input
        .checkout_remote
        .as_deref()
        .map(parse_repository_reference)
        .transpose()?;

    if let (Some(explicit), Some(checkout)) = (&explicit_repository, &checkout_repository) {
        ensure_same_repository(explicit, checkout, "checkout remote")?;
    }

    let parsed_item = input
        .item_reference
        .as_deref()
        .ok_or(IdentityError::MissingFact {
            fact: "issue or pull-request reference",
        })
        .and_then(|reference| parse_item_reference(reference, explicit_repository.as_ref()))?;

    let repository = explicit_repository
        .or(checkout_repository)
        .or_else(|| Some(parsed_item.repository.clone()))
        .ok_or(IdentityError::MissingFact {
            fact: "repository reference or checkout remote",
        })?;

    if repository != parsed_item.repository {
        return Err(IdentityError::ItemRepositoryMismatch);
    }
    if repository.provider != parsed_item.provider {
        return Err(IdentityError::ProviderMismatch {
            reference_source: "work item reference".to_string(),
            expected: repository.provider,
            found: parsed_item.provider,
        });
    }

    let project = input.project.as_ref().ok_or(IdentityError::MissingFact {
        fact: "verified project identity",
    })?;
    if project.provider != repository.provider {
        return Err(IdentityError::ProjectProviderMismatch);
    }
    if let Some(repository_project) = &repository.project {
        let project_key = format!("{}/{}", repository.namespace, repository_project);
        if project.key != project_key && project.key != *repository_project {
            return Err(IdentityError::ProjectMismatch);
        }
    }

    let project_key = repository
        .project
        .as_ref()
        .map(|repository_project| format!("{}/{}", repository.namespace, repository_project))
        .unwrap_or_else(|| project.key.clone());

    Ok(ResolvedWorkIdentity {
        provider: repository.provider,
        repository,
        project: ProjectIdentity {
            provider: project.provider,
            key: project_key,
        },
        item: parsed_item.item,
    })
}

fn ensure_same_repository(
    first: &RepositoryIdentity,
    second: &RepositoryIdentity,
    source: &str,
) -> Result<(), IdentityError> {
    if first.provider != second.provider {
        return Err(IdentityError::ProviderMismatch {
            reference_source: source.to_string(),
            expected: first.provider,
            found: second.provider,
        });
    }
    if first != second {
        return Err(IdentityError::RepositoryMismatch {
            reference_source: source.to_string(),
        });
    }
    Ok(())
}

fn parse_repository_reference(reference: &str) -> Result<RepositoryIdentity, IdentityError> {
    let (host, path) = split_reference(reference)?;
    let segments = path_segments(&path);
    match provider_for_host(&host) {
        Some(RepositoryProvider::GitHub) => {
            if segments.len() != 2 {
                return Err(IdentityError::UnsupportedReference(reference.to_string()));
            }
            Ok(RepositoryIdentity::new(
                RepositoryProvider::GitHub,
                &segments[0],
                &segments[1],
            ))
        }
        Some(RepositoryProvider::AzureDevOps) => parse_azure_repository(&host, &segments)
            .ok_or_else(|| IdentityError::UnsupportedReference(reference.to_string())),
        None => Err(IdentityError::UnsupportedReference(reference.to_string())),
    }
}

fn parse_item_reference(
    reference: &str,
    explicit_repository: Option<&RepositoryIdentity>,
) -> Result<ParsedItem, IdentityError> {
    let trimmed = reference.trim();
    if !looks_like_url_or_remote(trimmed) {
        return Err(IdentityError::AmbiguousItemReference(trimmed.to_string()));
    }

    let (host, path) = split_reference(trimmed)?;
    let segments = path_segments(&path);
    let provider = provider_for_host(&host)
        .ok_or_else(|| IdentityError::UnsupportedReference(trimmed.to_string()))?;
    let (repository, kind, number) = match provider {
        RepositoryProvider::GitHub => parse_github_item(&segments, trimmed)?,
        RepositoryProvider::AzureDevOps => parse_azure_item(&host, &segments, trimmed)?,
    };
    let item = WorkItemIdentity { kind, number };

    if let Some(explicit) = explicit_repository {
        ensure_same_repository(explicit, &repository, "item reference")?;
    }

    Ok(ParsedItem {
        provider,
        repository,
        item,
    })
}

fn parse_github_item(
    segments: &[String],
    reference: &str,
) -> Result<(RepositoryIdentity, WorkItemKind, u64), IdentityError> {
    if segments.len() != 4 {
        return Err(IdentityError::UnsupportedReference(reference.to_string()));
    }
    let kind = match segments[2].as_str() {
        "issues" => WorkItemKind::Issue,
        "pull" => WorkItemKind::PullRequest,
        _ => return Err(IdentityError::UnsupportedReference(reference.to_string())),
    };
    let number = parse_item_number(&segments[3])?;
    Ok((
        RepositoryIdentity::new(RepositoryProvider::GitHub, &segments[0], &segments[1]),
        kind,
        number,
    ))
}

fn parse_azure_item(
    host: &str,
    segments: &[String],
    reference: &str,
) -> Result<(RepositoryIdentity, WorkItemKind, u64), IdentityError> {
    let pull_request_index = segments
        .iter()
        .position(|segment| segment == "pullrequest")
        .ok_or_else(|| IdentityError::UnsupportedReference(reference.to_string()))?;
    if pull_request_index < 2 || pull_request_index + 1 >= segments.len() {
        return Err(IdentityError::UnsupportedReference(reference.to_string()));
    }
    let repository = parse_azure_repository(host, segments)
        .ok_or_else(|| IdentityError::UnsupportedReference(reference.to_string()))?;
    let number = parse_item_number(&segments[pull_request_index + 1])?;
    Ok((repository, WorkItemKind::PullRequest, number))
}

fn parse_azure_repository(host: &str, segments: &[String]) -> Option<RepositoryIdentity> {
    if host == "ssh.dev.azure.com" && segments.first().map(String::as_str) == Some("v3") {
        let namespace = segments.get(1)?;
        let _project = segments.get(2)?;
        let name = segments.get(3)?;
        if segments.len() != 4 {
            return None;
        }
        return Some(RepositoryIdentity::new_azure(namespace, _project, name));
    }

    let git_index = segments.iter().position(|segment| segment == "_git")?;
    let name = segments.get(git_index + 1)?;
    if git_index == 0 {
        return None;
    }
    let namespace = if host == "dev.azure.com" {
        segments.first()?
    } else {
        host.strip_suffix(".visualstudio.com")?
    };
    let _project = segments.get(git_index - 1)?;
    Some(RepositoryIdentity::new_azure(namespace, _project, name))
}

fn parse_item_number(value: &str) -> Result<u64, IdentityError> {
    value
        .parse::<u64>()
        .map_err(|_| IdentityError::InvalidItemNumber(value.to_string()))
}

fn split_reference(reference: &str) -> Result<(String, String), IdentityError> {
    let trimmed = reference.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(IdentityError::UnsupportedReference(reference.to_string()));
    }

    if let Some(rest) = trimmed.strip_prefix("git@") {
        let (host, path) = rest
            .split_once(':')
            .ok_or_else(|| IdentityError::UnsupportedReference(reference.to_string()))?;
        return Ok((host.to_ascii_lowercase(), path.to_string()));
    }

    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("ssh://"))
        .ok_or_else(|| IdentityError::UnsupportedReference(reference.to_string()))?;
    let (authority, path) = without_scheme
        .split_once('/')
        .ok_or_else(|| IdentityError::UnsupportedReference(reference.to_string()))?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = authority
        .split(':')
        .next()
        .unwrap_or(authority)
        .to_ascii_lowercase();
    Ok((
        host,
        path.split(['?', '#']).next().unwrap_or(path).to_string(),
    ))
}

fn path_segments(path: &str) -> Vec<String> {
    path.trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.trim_end_matches(".git").to_string())
        .collect()
}

fn provider_for_host(host: &str) -> Option<RepositoryProvider> {
    if host == "github.com" || host == "www.github.com" {
        Some(RepositoryProvider::GitHub)
    } else if host == "dev.azure.com"
        || host == "ssh.dev.azure.com"
        || host.ends_with(".visualstudio.com")
    {
        Some(RepositoryProvider::AzureDevOps)
    } else {
        None
    }
}

fn looks_like_url_or_remote(reference: &str) -> bool {
    reference.starts_with("https://")
        || reference.starts_with("http://")
        || reference.starts_with("ssh://")
        || reference.starts_with("git@")
}

fn normalize_segment(value: &str) -> String {
    value.trim().trim_end_matches(".git").to_ascii_lowercase()
}

fn normalize_project_key(value: &str) -> String {
    value.trim().trim_matches('/').to_ascii_lowercase()
}
