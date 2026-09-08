use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Resolution policy used by the built-in file tools.
///
/// Normal sessions retain their mutable, unrestricted working directory.
/// Controlled runs instead hold one canonical root for their whole lifetime.
#[derive(Clone)]
pub(crate) enum FileToolRoot {
    WorkingDirectory(Arc<Mutex<PathBuf>>),
    Fixed(PathBuf),
}

impl FileToolRoot {
    pub(crate) fn working_directory(working_dir: Arc<Mutex<PathBuf>>) -> Self {
        Self::WorkingDirectory(working_dir)
    }

    pub(crate) fn fixed(root: PathBuf) -> Result<Self, String> {
        let canonical = std::fs::canonicalize(&root).map_err(|error| {
            format!(
                "controlled tool root {} is unavailable: {error}",
                root.display()
            )
        })?;
        if !canonical.is_dir() {
            return Err(format!(
                "controlled tool root {} is not a directory",
                canonical.display()
            ));
        }
        Ok(Self::Fixed(canonical))
    }

    pub(crate) fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        match self {
            Self::WorkingDirectory(working_dir) => Ok(resolve_against(working_dir, path)),
            Self::Fixed(root) => resolve_within(root, path),
        }
    }

    pub(crate) fn root(&self) -> Result<PathBuf, String> {
        match self {
            Self::WorkingDirectory(working_dir) => working_dir
                .lock()
                .map(|root| root.clone())
                .map_err(|_| "working directory lock is poisoned".to_string()),
            Self::Fixed(root) => Ok(root.clone()),
        }
    }

    pub(crate) fn permits_existing(&self, path: &Path) -> bool {
        match self {
            Self::WorkingDirectory(_) => true,
            Self::Fixed(root) => {
                std::fs::canonicalize(path).is_ok_and(|canonical| canonical.starts_with(root))
            }
        }
    }

    pub(crate) fn validate_glob_pattern(&self, pattern: &str) -> Result<(), String> {
        if matches!(self, Self::WorkingDirectory(_)) {
            return Ok(());
        }
        let path = Path::new(pattern);
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(
                "controlled glob patterns must be relative and must not contain traversal"
                    .to_string(),
            );
        }
        Ok(())
    }
}

pub(crate) fn resolve_against(working_dir: &Mutex<PathBuf>, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let working_dir = working_dir
        .lock()
        .expect("working_dir mutex poisoned")
        .clone();
    working_dir.join(path)
}

fn resolve_within(root: &Path, input: &str) -> Result<PathBuf, String> {
    let relative = Path::new(input);
    if input.trim().is_empty() || relative.is_absolute() {
        return Err("controlled file paths must be nonempty and relative".to_string());
    }
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("controlled file paths must not contain roots or traversal".to_string());
    }

    let candidate = root.join(relative);
    let mut existing = candidate.as_path();
    while !existing.exists() {
        existing = existing.parent().ok_or_else(|| {
            format!(
                "controlled path {} has no existing parent",
                candidate.display()
            )
        })?;
    }
    let canonical_existing = std::fs::canonicalize(existing).map_err(|error| {
        format!(
            "controlled path {} cannot be resolved: {error}",
            candidate.display()
        )
    })?;
    if !canonical_existing.starts_with(root) {
        return Err(format!(
            "controlled path {} resolves outside {}",
            candidate.display(),
            root.display()
        ));
    }
    Ok(candidate)
}
