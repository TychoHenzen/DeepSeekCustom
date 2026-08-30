// covers: deepseek-custom/web-frontend-automation :: Browser prerequisites and failures are explicit :: Focused browser suite runs
#[test]
fn production_frontend_command_builds_assets_and_runs_the_isolated_suite() {
    let package = include_str!("../../../../web/package.json");
    let runner = include_str!("../../../../web/scripts/run-browser-tests.mjs");

    assert!(package.contains(r#""browser:test": "node scripts/run-browser-tests.mjs""#));
    assert!(runner.contains("await build({ configFile:"));
    assert!(runner.contains("'deepseek-custom-tests'"));
    assert!(runner.contains("'web_browser'"));
    assert!(runner.contains("'--test-threads=1'"));
    assert!(!runner.contains("shell: true"));
}
