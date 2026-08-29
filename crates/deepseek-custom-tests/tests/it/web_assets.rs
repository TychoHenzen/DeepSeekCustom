use std::fs;
use std::path::{Path, PathBuf};

use deepseek_custom::web::asset_contract::{
    ASSET_BUILD_COMMAND, AssetContractError, build_failure_message, validate_production_assets,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("deepseek-custom-assets-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_valid_contract(directory: &Path, content: &str) {
    fs::write(directory.join("index.html"), content).unwrap();
    let digest = format!("{:x}", Sha256::digest(content.as_bytes()));
    fs::write(
        directory.join("asset-manifest.json"),
        format!(
            r#"{{
  "contract_version": 1,
  "generator": "vite",
  "build_command": "{ASSET_BUILD_COMMAND}",
  "assets": [{{"path": "index.html", "sha256": "{digest}"}}]
}}"#
        ),
    )
    .unwrap();
}

#[test]
fn production_asset_contract_accepts_a_matching_vite_manifest() {
    let directory = TestDirectory::new();
    write_valid_contract(directory.path(), "built application");

    validate_production_assets(directory.path()).unwrap();
}

#[test]
fn production_asset_contract_explains_how_to_create_missing_assets() {
    let directory = TestDirectory::new();

    let error = validate_production_assets(directory.path()).unwrap_err();

    assert!(matches!(error, AssetContractError::MissingManifest));
    assert_eq!(
        build_failure_message(&error),
        "production web assets are missing or stale: missing asset-manifest.json. Run `npm --prefix web run build` and rebuild."
    );
}

#[test]
fn production_asset_contract_rejects_content_that_is_stale_against_its_stamp() {
    let directory = TestDirectory::new();
    write_valid_contract(directory.path(), "original application");
    fs::write(directory.path().join("index.html"), "changed application").unwrap();

    let error = validate_production_assets(directory.path()).unwrap_err();

    assert!(matches!(error, AssetContractError::StaleAsset(ref path) if path == "index.html"));
    assert!(build_failure_message(&error).contains("Run `npm --prefix web run build`"));
}
