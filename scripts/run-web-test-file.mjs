import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(scriptDirectory, "..");
const webRoot = path.join(workspaceRoot, "web");

function webRelativeTestPath(testFile) {
  if (typeof testFile !== "string" || testFile.length === 0) {
    throw new Error("expected one repository-relative web test file path");
  }

  const normalized = path.posix.normalize(testFile.replaceAll("\\", "/"));
  if (!normalized.startsWith("web/src/") || !normalized.endsWith(".test.ts")) {
    throw new Error(`web test file must match web/src/**/*.test.ts: ${testFile}`);
  }
  return normalized.slice("web/".length);
}

function run(testFile) {
  const vitestEntrypoint = path.join(webRoot, "node_modules", "vitest", "vitest.mjs");
  const child = spawnSync(process.execPath, [vitestEntrypoint, "run", webRelativeTestPath(testFile)], {
    cwd: webRoot,
    shell: false,
    stdio: "inherit",
  });

  if (child.error) throw child.error;
  if (typeof child.status !== "number") {
    throw new Error(`web test did not return an exit code${child.signal ? ` (signal: ${child.signal})` : ""}`);
  }
  return child.status;
}

if (process.argv.length !== 3) {
  console.error("usage: node scripts/run-web-test-file.mjs <repo-relative-test-file.test.ts>");
  process.exitCode = 2;
} else {
  try {
    process.exitCode = run(process.argv[2]);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 2;
  }
}
