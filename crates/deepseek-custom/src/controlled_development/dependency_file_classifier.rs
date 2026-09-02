use super::DependencyFileKind;

/// Classify only the dependency files owned by this repository's Cargo and web builds.
pub fn classify_dependency_file(path: &str) -> Option<DependencyFileKind> {
    if path == "Cargo.lock" {
        return Some(DependencyFileKind::CargoLock);
    }
    if path == "web/package.json" {
        return Some(DependencyFileKind::WebPackageManifest);
    }
    if path == "web/package-lock.json" {
        return Some(DependencyFileKind::WebPackageLock);
    }
    if path == "Cargo.toml" || path.ends_with("/Cargo.toml") {
        return Some(DependencyFileKind::CargoManifest);
    }
    None
}
