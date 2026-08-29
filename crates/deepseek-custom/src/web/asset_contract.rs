use std::fmt;
use std::fs;
use std::path::{Component, Path};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const ASSET_BUILD_COMMAND: &str = "npm --prefix web run build";
pub const ASSET_MANIFEST: &str = "asset-manifest.json";
const ASSET_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetManifest {
    contract_version: u32,
    generator: AssetGenerator,
    build_command: String,
    assets: Vec<AssetEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum AssetGenerator {
    Vite,
    PlaceholderUntilViteWorkspaceExists,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetEntry {
    path: String,
    sha256: String,
}

#[derive(Debug)]
pub enum AssetContractError {
    MissingManifest,
    ReadManifest(std::io::Error),
    InvalidManifest(serde_json::Error),
    UnsupportedVersion(u32),
    WrongBuildCommand(String),
    EmptyManifest,
    UnsafePath(String),
    MissingAsset(String),
    ReadAsset {
        path: String,
        source: std::io::Error,
    },
    StaleAsset(String),
}

impl fmt::Display for AssetContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingManifest => write!(formatter, "missing {ASSET_MANIFEST}"),
            Self::ReadManifest(error) => write!(formatter, "cannot read {ASSET_MANIFEST}: {error}"),
            Self::InvalidManifest(error) => {
                write!(formatter, "invalid {ASSET_MANIFEST}: {error}")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported asset contract version {version}")
            }
            Self::WrongBuildCommand(command) => write!(
                formatter,
                "manifest build command is `{command}`, expected `{ASSET_BUILD_COMMAND}`"
            ),
            Self::EmptyManifest => write!(formatter, "asset manifest contains no files"),
            Self::UnsafePath(path) => {
                write!(formatter, "asset path is not relative and safe: {path}")
            }
            Self::MissingAsset(path) => write!(formatter, "manifest asset is missing: {path}"),
            Self::ReadAsset { path, source } => {
                write!(formatter, "cannot read manifest asset {path}: {source}")
            }
            Self::StaleAsset(path) => write!(formatter, "manifest asset is stale: {path}"),
        }
    }
}

impl std::error::Error for AssetContractError {}

pub fn build_failure_message(error: &AssetContractError) -> String {
    format!(
        "production web assets are missing or stale: {error}. Run `{ASSET_BUILD_COMMAND}` and rebuild."
    )
}

pub fn validate_production_assets(directory: &Path) -> Result<(), AssetContractError> {
    let manifest_path = directory.join(ASSET_MANIFEST);
    let bytes = match fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AssetContractError::MissingManifest);
        }
        Err(error) => return Err(AssetContractError::ReadManifest(error)),
    };
    let manifest: AssetManifest =
        serde_json::from_slice(&bytes).map_err(AssetContractError::InvalidManifest)?;

    if manifest.contract_version != ASSET_CONTRACT_VERSION {
        return Err(AssetContractError::UnsupportedVersion(
            manifest.contract_version,
        ));
    }
    let _generator = manifest.generator;
    if manifest.build_command != ASSET_BUILD_COMMAND {
        return Err(AssetContractError::WrongBuildCommand(
            manifest.build_command,
        ));
    }
    if manifest.assets.is_empty() {
        return Err(AssetContractError::EmptyManifest);
    }

    for asset in manifest.assets {
        let relative = Path::new(&asset.path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(AssetContractError::UnsafePath(asset.path));
        }

        let asset_path = directory.join(relative);
        let asset_bytes = match fs::read(&asset_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(AssetContractError::MissingAsset(asset.path));
            }
            Err(source) => {
                return Err(AssetContractError::ReadAsset {
                    path: asset.path,
                    source,
                });
            }
        };
        let actual = format!("{:x}", Sha256::digest(asset_bytes));
        if !actual.eq_ignore_ascii_case(&asset.sha256) {
            return Err(AssetContractError::StaleAsset(asset.path));
        }
    }

    Ok(())
}
