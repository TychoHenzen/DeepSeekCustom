import { spawnSync } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { build } from 'vite';

const webRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const workspaceRoot = process.env.DEEPSEEK_BROWSER_CARGO_ROOT
  ? resolve(process.env.DEEPSEEK_BROWSER_CARGO_ROOT)
  : resolve(webRoot, '..');

process.chdir(webRoot);
await build({ configFile: resolve(webRoot, 'vite.config.ts') });
await import(`${pathToFileURL(resolve(webRoot, 'scripts/write-asset-manifest.mjs')).href}?browser-test`);

const cargo = process.platform === 'win32' ? 'cargo.exe' : 'cargo';
const result = spawnSync(
  cargo,
  [
    'test',
    '-p',
    'deepseek-custom-tests',
    '--test',
    'it',
    'web_browser',
    '--',
    '--test-threads=1',
  ],
  {
    cwd: workspaceRoot,
    env: process.env,
    stdio: 'inherit',
    windowsHide: true,
  },
);

if (result.error) {
  throw result.error;
}
process.exitCode = result.status ?? 1;
