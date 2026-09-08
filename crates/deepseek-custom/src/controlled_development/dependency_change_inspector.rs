use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::Value;

use super::{
    ControlledChangeGateError, DependencyExceptionMatch, DependencyFileKind,
    classify_dependency_file,
};

pub(crate) fn authorize_dependency_file_changes(
    changed_paths: &[String],
    baseline_root: &Path,
    execution_root: &Path,
    complexity_exceptions: &[String],
) -> Result<Vec<DependencyExceptionMatch>, ControlledChangeGateError> {
    changed_paths
        .iter()
        .filter_map(|path| classify_dependency_file(path).map(|kind| (path, kind)))
        .map(|(path, kind)| {
            authorize_dependency_file(
                path,
                kind,
                baseline_root,
                execution_root,
                complexity_exceptions,
            )
        })
        .collect()
}

fn authorize_dependency_file(
    path: &str,
    kind: DependencyFileKind,
    baseline_root: &Path,
    execution_root: &Path,
    complexity_exceptions: &[String],
) -> Result<DependencyExceptionMatch, ControlledChangeGateError> {
    let baseline = read_optional(&baseline_root.join(path), path)?;
    let execution = read_optional(&execution_root.join(path), path)?;
    let changed_dependencies =
        changed_dependency_names(kind, baseline.as_deref(), execution.as_deref()).map_err(
            |reason| ControlledChangeGateError::DependencyInspectionFailed {
                path: path.to_string(),
                reason,
            },
        )?;

    for dependency_name in &changed_dependencies {
        if let Some(complexity_exception) = complexity_exceptions
            .iter()
            .find(|exception| explicitly_names(exception, dependency_name))
        {
            return Ok(DependencyExceptionMatch {
                path: path.to_string(),
                dependency_name: dependency_name.clone(),
                complexity_exception: complexity_exception.clone(),
            });
        }
    }

    let system_name = kind.dependency_system_exception();
    if let Some(complexity_exception) = complexity_exceptions
        .iter()
        .find(|exception| explicitly_names(exception, system_name))
    {
        return Ok(DependencyExceptionMatch {
            path: path.to_string(),
            dependency_name: system_name.to_string(),
            complexity_exception: complexity_exception.clone(),
        });
    }

    Err(ControlledChangeGateError::MissingDependencyException {
        path: path.to_string(),
        changed_dependencies: if changed_dependencies.is_empty() {
            vec![system_name.to_string()]
        } else {
            changed_dependencies
        },
    })
}

fn read_optional(
    path: &Path,
    display_path: &str,
) -> Result<Option<Vec<u8>>, ControlledChangeGateError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ControlledChangeGateError::DependencyInspectionFailed {
            path: display_path.to_string(),
            reason: error.to_string(),
        }),
    }
}

fn changed_dependency_names(
    kind: DependencyFileKind,
    baseline: Option<&[u8]>,
    execution: Option<&[u8]>,
) -> Result<Vec<String>, String> {
    let baseline = dependency_entries(kind, baseline)?;
    let execution = dependency_entries(kind, execution)?;
    Ok(baseline
        .keys()
        .chain(execution.keys())
        .filter(|name| baseline.get(*name) != execution.get(*name))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn dependency_entries(
    kind: DependencyFileKind,
    contents: Option<&[u8]>,
) -> Result<BTreeMap<String, String>, String> {
    let Some(contents) = contents else {
        return Ok(BTreeMap::new());
    };
    let text = std::str::from_utf8(contents).map_err(|error| error.to_string())?;
    match kind {
        DependencyFileKind::CargoManifest => Ok(cargo_manifest_entries(text)),
        DependencyFileKind::CargoLock => Ok(cargo_lock_entries(text)),
        DependencyFileKind::WebPackageManifest => package_manifest_entries(text),
        DependencyFileKind::WebPackageLock => package_lock_entries(text),
    }
}

fn cargo_manifest_entries(text: &str) -> BTreeMap<String, String> {
    let mut entries = BTreeMap::new();
    let mut dependency_section = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section = &trimmed[1..trimmed.len() - 1];
            dependency_section = matches!(
                section,
                "dependencies"
                    | "dev-dependencies"
                    | "build-dependencies"
                    | "workspace.dependencies"
            ) || section.ends_with(".dependencies")
                || section.ends_with(".dev-dependencies")
                || section.ends_with(".build-dependencies");
            continue;
        }
        if dependency_section
            && !trimmed.starts_with('#')
            && let Some((name, value)) = trimmed.split_once('=')
        {
            let name = name.trim().trim_matches(['\'', '"']);
            if !name.is_empty() {
                entries.insert(name.to_string(), value.trim().to_string());
            }
        }
    }
    entries
}

fn cargo_lock_entries(text: &str) -> BTreeMap<String, String> {
    let mut entries = BTreeMap::new();
    for block in text.split("[[package]]").skip(1) {
        let name = block.lines().find_map(|line| {
            line.trim()
                .strip_prefix("name = ")
                .map(|value| value.trim_matches('"').to_string())
        });
        if let Some(name) = name {
            entries
                .entry(name)
                .and_modify(|value: &mut String| value.push_str(block))
                .or_insert_with(|| block.to_string());
        }
    }
    entries
}

fn package_manifest_entries(text: &str) -> Result<BTreeMap<String, String>, String> {
    let value = serde_json::from_str::<Value>(text).map_err(|error| error.to_string())?;
    let mut entries = BTreeMap::new();
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        if let Some(values) = value.get(section).and_then(Value::as_object) {
            for (name, value) in values {
                entries.insert(name.clone(), value.to_string());
            }
        }
    }
    Ok(entries)
}

fn package_lock_entries(text: &str) -> Result<BTreeMap<String, String>, String> {
    let value = serde_json::from_str::<Value>(text).map_err(|error| error.to_string())?;
    let mut entries = BTreeMap::new();
    if let Some(packages) = value.get("packages").and_then(Value::as_object) {
        for (path, value) in packages {
            let Some(name) = path
                .rsplit("node_modules/")
                .next()
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            entries.insert(name.to_string(), value.to_string());
        }
    }
    Ok(entries)
}

fn explicitly_names(complexity_exception: &str, required_name: &str) -> bool {
    let exception = complexity_exception.to_lowercase();
    let required = required_name.to_lowercase();
    exception.match_indices(&required).any(|(start, _)| {
        let before = exception[..start].chars().next_back();
        let after = exception[start + required.len()..].chars().next();
        before.is_none_or(|character| !is_dependency_name_character(character))
            && after.is_none_or(|character| !is_dependency_name_character(character))
    })
}

fn is_dependency_name_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '@' | '/' | '_' | '-' | '.')
}
