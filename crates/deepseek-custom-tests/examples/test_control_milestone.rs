//! Runs real discovery and one exact current test through production executors.
//!
//! Run from the repository root:
//! `cargo run -p deepseek-custom-tests --example test_control_milestone`

use std::path::{Path, PathBuf};
use std::time::Duration;

use deepseek_custom::application::test_control::{
    CargoTestDiscoveryExecutor, CargoTestExecutor, SystemTestClock, TestDiscoveryState,
    TestOutcome, TestRunCoordinator, TestRunRequest,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("test crate must be two levels below the workspace root")
        .to_path_buf();
    let clock = SystemTestClock;
    let mut discovery = TestDiscoveryState::default();
    discovery
        .refresh(&project_root, &CargoTestDiscoveryExecutor, &clock)
        .map_err(|error| format!("real discovery failed: {error:?}"))?;
    let catalogue = discovery
        .catalogue
        .as_ref()
        .expect("successful discovery must retain a catalogue");
    let exact_name = "application_test_control::passing_result_serializes_only_the_selected_scope_and_observed_outcome";
    let identity = catalogue
        .modules
        .iter()
        .flat_map(|module| &module.tests)
        .find(|test| test.name == exact_name)
        .cloned()
        .ok_or("selected exact current test was not discovered")?;
    let discovered_count: usize = catalogue
        .modules
        .iter()
        .map(|module| module.tests.len())
        .sum();
    let catalogue_revision = catalogue.discovered_at_ms;
    let mut runs = TestRunCoordinator::default();
    runs.start(
        &discovery,
        &TestRunRequest {
            identity,
            catalogue_revision,
        },
        &project_root,
        &CargoTestExecutor,
        &clock,
    )
    .map_err(|error| format!("real exact test failed to start: {error:?}"))?;
    for _ in 0..1_200 {
        runs.poll(&clock)
            .map_err(|error| format!("real exact test polling failed: {error:?}"))?;
        if runs.active.is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let result = runs
        .latest_result
        .as_ref()
        .ok_or("exact test did not reach a terminal outcome in 30 seconds")?;
    println!("test-control milestone");
    println!("discovered exact tests: {discovered_count}");
    println!("selected exact test: {exact_name}");
    println!("command: {}", result.command.join(" "));
    println!("outcome: {:?}", result.outcome);
    println!(
        "counts: {} passed; {} failed; {} ignored; {} filtered out",
        result.counts.passed, result.counts.failed, result.counts.ignored, result.counts.filtered
    );
    if result.outcome != TestOutcome::Passed
        || result.counts.passed != 1
        || result.counts.failed != 0
    {
        return Err(format!("exact current test did not pass: {}", result.output).into());
    }
    Ok(())
}
