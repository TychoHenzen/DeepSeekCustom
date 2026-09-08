import { createHash } from 'node:crypto';
import { readdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const workspace = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = resolve(workspace, '../crates/deepseek-custom/src/web/assets');

async function filesBelow(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = await Promise.all(
    entries.map(async (entry) => {
      const path = resolve(directory, entry.name);
      return entry.isDirectory() ? filesBelow(path) : [path];
    }),
  );
  return files.flat();
}

const files = (await filesBelow(output))
  .filter((path) => !path.endsWith('asset-manifest.json'))
  .sort((left, right) => left.localeCompare(right));
const assets = await Promise.all(
  files.map(async (path) => ({
    path: relative(output, path).replaceAll('\\', '/'),
    sha256: createHash('sha256').update(await readFile(path)).digest('hex'),
  })),
);
const manifest = {
  contract_version: 1,
  generator: 'vite',
  build_command: 'npm --prefix web run build',
  assets,
};
await writeFile(
  resolve(output, 'asset-manifest.json'),
  `${JSON.stringify(manifest, null, 2)}\n`,
  'utf8',
);
