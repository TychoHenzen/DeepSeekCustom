import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const RUST_INTEGRATION_TEST_ROOT = "crates/deepseek-custom-tests/tests/it";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
export const DEFAULT_WORKSPACE_ROOT = path.resolve(scriptDirectory, "..");

function portableRelativePath(testFile) {
  if (typeof testFile !== "string" || testFile.length === 0) {
    throw new Error("expected one repository-relative Rust test file path");
  }

  const portable = testFile.replaceAll("\\", "/");
  if (path.posix.isAbsolute(portable) || path.win32.isAbsolute(testFile)) {
    throw new Error(`Rust test file path must be repository-relative: ${testFile}`);
  }

  return path.posix.normalize(portable);
}

export function deriveIntegrationModule(testFile) {
  const normalized = portableRelativePath(testFile);
  if (path.posix.dirname(normalized) !== RUST_INTEGRATION_TEST_ROOT) {
    throw new Error(`Rust test file must belong to ${RUST_INTEGRATION_TEST_ROOT}: ${testFile}`);
  }
  if (path.posix.extname(normalized) !== ".rs") {
    throw new Error(`Rust test file must use the .rs extension: ${testFile}`);
  }

  const moduleName = path.posix.basename(normalized, ".rs");
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(moduleName)) {
    throw new Error(`Rust test filename does not form a module filter: ${testFile}`);
  }
  return moduleName;
}

export function cargoInvocation(testFile, workspaceRoot = DEFAULT_WORKSPACE_ROOT) {
  const moduleName = deriveIntegrationModule(testFile);
  return {
    command: "cargo",
    args: ["test", "-p", "deepseek-custom-tests", "--test", "it", moduleName],
    cwd: workspaceRoot,
  };
}

export function runRustTestFile(
  testFile,
  { workspaceRoot = DEFAULT_WORKSPACE_ROOT, spawn = spawnSync } = {},
) {
  const invocation = cargoInvocation(testFile, workspaceRoot);
  const child = spawn(invocation.command, invocation.args, {
    cwd: invocation.cwd,
    shell: false,
    stdio: "inherit",
  });

  if (child.error) throw child.error;
  if (typeof child.status !== "number") {
    throw new Error(`cargo test did not return an exit code${child.signal ? ` (signal: ${child.signal})` : ""}`);
  }
  return child.status;
}

export function runCli(args, options) {
  if (args.length !== 1) {
    throw new Error("usage: node scripts/run-rust-test-file.mjs <repo-relative-test-file.rs>");
  }
  return runRustTestFile(args[0], options);
}

const invokedDirectly =
  process.argv[1] !== undefined && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (invokedDirectly) {
  try {
    process.exitCode = runCli(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 2;
  }
}
