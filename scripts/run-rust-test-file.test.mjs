import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import {
  cargoInvocation,
  deriveIntegrationModule,
  runCli,
  runRustTestFile,
} from "./run-rust-test-file.mjs";

const PROCEDURE_RUNNER = "crates/deepseek-custom-tests/tests/it/procedure_runner.rs";

test("OpenSpec config maps Rust files to the portable Node runner", async () => {
  const config = JSON.parse(
    await readFile(new URL("../openspec/test-runners.json", import.meta.url), "utf8"),
  );
  assert.equal(config.rust, "node scripts/run-rust-test-file.mjs");
});

test("OpenSpec discovery stays inside the maintained external Rust integration tests", async () => {
  const config = JSON.parse(
    await readFile(new URL("../openspec/test-globs.json", import.meta.url), "utf8"),
  );
  assert.deepEqual(config, {
    "deepseek-custom": ["crates/deepseek-custom-tests/tests/it/*.rs"],
  });
});

test("Rust test paths must stay inside the external integration-test directory", () => {
  assert.equal(deriveIntegrationModule(PROCEDURE_RUNNER), "procedure_runner");
  assert.equal(
    deriveIntegrationModule("crates\\deepseek-custom-tests\\tests\\it\\procedure_input.rs"),
    "procedure_input",
  );

  for (const invalidPath of [
    "crates/deepseek-custom/src/procedure/runner.rs",
    "crates/deepseek-custom-tests/tests/it/../../src/lib.rs",
    "crates/deepseek-custom-tests/tests/it/nested/procedure_runner.rs",
    "crates/deepseek-custom-tests/tests/it/procedure_runner.txt",
    "crates/deepseek-custom-tests/tests/it/procedure-runner.rs",
    path.resolve(PROCEDURE_RUNNER),
  ]) {
    assert.throws(() => deriveIntegrationModule(invalidPath));
  }
});

test("Cargo invocation derives the integration module from the Rust filename", () => {
  assert.deepEqual(cargoInvocation(PROCEDURE_RUNNER, "/workspace"), {
    command: "cargo",
    args: ["test", "-p", "deepseek-custom-tests", "--test", "it", "procedure_runner"],
    cwd: "/workspace",
  });
});

test("Cargo receives separate arguments when the workspace and full test path contain spaces", async (t) => {
  const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "rust test runner "));
  t.after(() => rm(temporaryRoot, { recursive: true, force: true }));
  const workspaceRoot = path.join(temporaryRoot, "workspace with spaces");
  await mkdir(workspaceRoot);
  assert.match(path.join(workspaceRoot, PROCEDURE_RUNNER), / /);

  let captured;
  const status = runRustTestFile(PROCEDURE_RUNNER, {
    workspaceRoot,
    spawn(command, args, options) {
      captured = { command, args, options };
      return { status: 0 };
    },
  });

  assert.equal(status, 0);
  assert.deepEqual(captured, {
    command: "cargo",
    args: ["test", "-p", "deepseek-custom-tests", "--test", "it", "procedure_runner"],
    options: { cwd: workspaceRoot, shell: false, stdio: "inherit" },
  });
});

test("Cargo child exit codes propagate through the CLI seam", () => {
  const status = runCli([PROCEDURE_RUNNER], {
    spawn() {
      return { status: 37 };
    },
  });
  assert.equal(status, 37);
});

test("The CLI rejects missing or extra test-file arguments before spawning", () => {
  let spawnCalls = 0;
  const options = {
    spawn() {
      spawnCalls += 1;
      return { status: 0 };
    },
  };

  assert.throws(() => runCli([], options), /usage:/);
  assert.throws(() => runCli([PROCEDURE_RUNNER, PROCEDURE_RUNNER], options), /usage:/);
  assert.equal(spawnCalls, 0);
});
