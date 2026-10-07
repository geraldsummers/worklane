// Reproducible extension tests/typecheck against the exact locked Pi SDK.
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { existsSync } from 'node:fs';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const sdk = resolve(process.env.WORKLANE_PI_SDK || join(root, '.tmp/pi-validation/node_modules/@earendil-works/pi-coding-agent'));
const require = createRequire(join(sdk, 'package.json'));
const { createJiti } = require('jiti');
// pi-ai is import-only; require.resolve cannot use its conditional exports.
const ai = [join(sdk, 'node_modules/@earendil-works/pi-ai/dist/index.js'),
  join(dirname(sdk), 'pi-ai/dist/index.js')].find(existsSync);
if (!ai) throw new Error('Locked Pi AI dependency is unavailable');
const jiti = createJiti(import.meta.url, {moduleCache: true, fsCache: false, alias: {'@earendil-works/pi-ai': ai}});
if (process.argv.includes('--typecheck')) {
  const temporary = await mkdtemp(join(tmpdir(), 'worklane-pi-types-'));
  try {
    const types = dirname(dirname(require.resolve('@types/node/package.json')));
    const config = {
      compilerOptions: {target: 'ES2022', module: 'ESNext', moduleResolution: 'bundler', strict: true,
        noEmit: true, skipLibCheck: true, allowImportingTsExtensions: true, types: ['node'], typeRoots: [types],
        paths: {'@earendil-works/pi-coding-agent': [join(sdk, 'dist/index.d.ts')],
          '@earendil-works/pi-ai': [join(dirname(ai), 'index.d.ts')]}},
      include: [join(root, 'assets/worklane-runtime/*.ts')]
    };
    const path = join(temporary, 'tsconfig.json');
    await writeFile(path, JSON.stringify(config));
    const tsc = process.env.WORKLANE_TSC || join(dirname(require.resolve('typescript/package.json')), 'bin/tsc');
    const result = spawnSync(process.execPath, [tsc, '-p', path], {stdio: 'inherit'});
    if (result.status !== 0) process.exitCode = result.status || 1;
  } finally { await rm(temporary, {recursive: true, force: true}); }
} else {
  await jiti.import(join(root, 'assets/worklane-runtime/collab.test.ts'));
  await jiti.import(join(root, 'assets/worklane-runtime/p0.test.ts'));
}
