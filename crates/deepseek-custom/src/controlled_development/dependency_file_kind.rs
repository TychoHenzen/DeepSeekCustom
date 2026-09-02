/// Fixed dependency-file classes understood by this repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyFileKind {
    CargoManifest,
    CargoLock,
    WebPackageManifest,
    WebPackageLock,
}

impl DependencyFileKind {
    pub const fn dependency_system_exception(self) -> &'static str {
        match self {
            Self::CargoManifest | Self::CargoLock => "Cargo dependency system",
            Self::WebPackageManifest | Self::WebPackageLock => "npm dependency system",
        }
    }
}
