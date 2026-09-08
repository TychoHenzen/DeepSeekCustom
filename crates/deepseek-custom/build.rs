#[path = "src/web/asset_contract.rs"]
mod asset_contract;

use std::path::PathBuf;

fn main() {
    let assets =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("src/web/assets");

    println!("cargo:rerun-if-changed={}", assets.display());

    if let Err(error) = asset_contract::validate_production_assets(&assets) {
        panic!("{}", asset_contract::build_failure_message(&error));
    }
}
