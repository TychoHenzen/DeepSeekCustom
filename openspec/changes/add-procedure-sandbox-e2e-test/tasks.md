## 1. Sandbox and OpenSpec fixture

- [x] 1.1 Add an external integration-test fixture that creates a unique temporary project with a minimal Rust target, one unrelated file, and the required OpenSpec directory structure, proposal, spec delta, and unchecked task.
<!-- status: completed -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance fixture uses a valid isolated proposal :: Minimal proposal passes the preflight gate -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance test is runnable without live model services :: The test runs offline -->
- [x] 1.2 Add fixture helpers that invoke strict OpenSpec validation through the production input path and can remove or corrupt one required artifact for a preflight-failure case.
<!-- status: completed -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance fixture uses a valid isolated proposal :: Invalid proposal stops before execution -->

## 2. Deterministic dispatch and evidence seams

- [x] 2.1 Add recording localization, patch, frontier, and verifier seams using the existing external test interfaces, with a shared ordered event log and call counters.
<!-- status: completed -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: A passing fixture proves the complete procedure lifecycle :: Stage order and dispatch boundaries are recorded -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance test is runnable without live model services :: The test runs offline -->
- [x] 2.2 Define the valid indexed target, mechanical patch envelope, passing verifier command, and expected local route for the one-task fixture.
<!-- status: completed -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: A passing fixture proves the complete procedure lifecycle :: One simple task is promoted end to end -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: The fixture proves workspace and evidence isolation :: Promotion is limited to the localized target -->
- [x] 2.3 Define localization response sequences for repeated invalid symbols and for invalid-then-valid retry recovery, preserving the exact structural diagnostic.
<!-- status: completed -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: Localization failures remain diagnosable and bounded :: Invalid symbol produces the known immediate failure -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: Localization failures remain diagnosable and bounded :: One invalid response is repaired by the bounded retry -->

## 3. Passing whole-change acceptance path

- [x] 3.1 Execute the existing whole-change procedure composition against the sandbox proposal and assert strict validation, localization approval, agreement sampling, local routing, patch generation, isolated verification, promotion, and completion of the single task.
<!-- status: completed -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: A passing fixture proves the complete procedure lifecycle :: One simple task is promoted end to end -->
- [x] 3.2 Assert the ordered event log, localization and patch dispatch counts, absence of frontier dispatch, verifier gate execution, and successful terminal evidence.
<!-- status: completed -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: A passing fixture proves the complete procedure lifecycle :: Stage order and dispatch boundaries are recorded -->
- [ ] 3.3 Reload the persisted procedure report and assert the selected target, route metadata, verifier evidence, promotion result, terminal disposition, and bounded metrics.
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: A passing fixture proves the complete procedure lifecycle :: One simple task is promoted end to end -->

## 4. Failure and isolation acceptance paths

- [ ] 4.1 Run the repeated-invalid-symbol fixture and assert exactly the configured localization attempts, the error text `symbol is not present under the indexed path`, failed terminal disposition, and zero patch, verifier, frontier, or promotion calls.
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: Localization failures remain diagnosable and bounded :: Invalid symbol produces the known immediate failure -->
- [ ] 4.2 Run the invalid-then-valid localization fixture, assert both attempts and the repaired target, approve the report, and confirm that downstream execution begins only after approval.
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: Localization failures remain diagnosable and bounded :: One invalid response is repaired by the bounded retry -->
- [ ] 4.3 Run invalid-proposal and interrupted cases, assert exact preflight or interruption evidence, and verify that no source promotion occurs.
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: The acceptance fixture uses a valid isolated proposal :: Invalid proposal stops before execution -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: The fixture proves workspace and evidence isolation :: Failure does not mutate source files -->
- [ ] 4.4 Compare sandbox target and unrelated-file bytes before and after every case, and compare the real checkout including the existing `settings.json` before and after the test group.
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: The fixture proves workspace and evidence isolation :: Promotion is limited to the localized target -->
<!-- covers: deepseek-custom/procedure-sandbox-e2e-test :: The fixture proves workspace and evidence isolation :: Failure does not mutate source files -->

## 5. Verification and maintenance

- [ ] 5.1 Register the integration module in the single `tests/it` target, keep all fixture code in `crates/deepseek-custom-tests`, and document the focused serial command for the acceptance cases.
- [ ] 5.2 Run `cargo test -p deepseek-custom-tests --test it procedure_sandbox_e2e -- --test-threads=1` and repair any test or fixture failures without weakening the production gates.
- [ ] 5.3 Run `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace` after the focused acceptance test passes.
