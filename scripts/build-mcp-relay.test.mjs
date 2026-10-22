import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync, existsSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildRelay, SIGNING_INPUTS } from './build-mcp-relay.mjs';
const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
function fixture(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'cartograph-relay-package-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}
test('builds locked native slices, verifies universal output, and stages atomically', (t) => {
  const root = fixture(t); const calls = [];
  const output = buildRelay({ root, platform: 'darwin', env: {}, run(command, args, options) {
    calls.push([command, args]);
    assert.equal(options.cwd, root);
    if (command === 'lipo' && args[0] === '-create') writeFileSync(args.at(-1), 'universal-fixture');
    if (command === 'lipo' && args[0] === '-verify_arch') assert.equal(existsSync(args.at(-1)), true);
  } });
  assert.equal(readFileSync(output, 'utf8'), 'universal-fixture');
  assert.equal(existsSync(`${output}.tmp`), false);
  assert.deepEqual(calls.slice(0, 2).map(([command, args]) => [command, ...args]),
    ['aarch64-apple-darwin', 'x86_64-apple-darwin'].map((triple) => ['cargo', 'build', '--locked', '--release', '-p', 'mcp-relay', '--bin', 'cartograph-mcp', '--target', triple, '--target-dir', path.join(root, 'target')]));
  assert.deepEqual(calls[3].slice(0, 1), ['lipo']);
  assert.deepEqual(calls[3][1].slice(0, 3), ['-verify_arch', 'arm64', 'x86_64']);
});
test('compilation and universal verification failures cannot leave a stale staged relay', (t) => {
  const root = fixture(t);
  const output = buildRelay({ root, platform: 'darwin', env: {}, run(command, args) {
    if (command === 'lipo' && args[0] === '-create') writeFileSync(args.at(-1), 'old');
  } });
  for (const failure of ['cargo', '-verify_arch']) {
    assert.throws(() => buildRelay({ root, platform: 'darwin', env: {}, run(command, args) {
      if (command === 'lipo' && args[0] === '-create') writeFileSync(args.at(-1), 'new');
      if (command === failure || args[0] === failure) throw new Error('fixture failure');
    } }), /fixture failure/u);
    assert.equal(existsSync(output), false);
    assert.equal(existsSync(`${output}.tmp`), false);
  }
});
test('unsupported platforms and every signing input fail before commands run', () => {
  const run = () => assert.fail('must not invoke compiler');
  assert.throws(() => buildRelay({ platform: 'linux', env: {}, run }), /requires macOS/u);
  for (const key of SIGNING_INPUTS) assert.throws(() => buildRelay({ platform: 'darwin', env: { [key]: 'secret' }, run }), /signing inputs/u);
});
test('package-only overlay resolves staged target suffix and preserves dev configuration', () => {
  const overlayPath = path.join(ROOT, 'src-tauri/tauri.mcp-bundle.conf.json');
  const overlay = JSON.parse(readFileSync(overlayPath));
  const base = JSON.parse(readFileSync(path.join(ROOT, 'src-tauri/tauri.conf.json')));
  assert.equal(base.bundle.externalBin, undefined);
  assert.deepEqual(overlay.bundle.externalBin, ['../target/mcp-relay/cartograph-mcp']);
  assert.equal(path.resolve(path.dirname(overlayPath), `${overlay.bundle.externalBin[0]}-universal-apple-darwin`), path.join(ROOT, 'target/mcp-relay/cartograph-mcp-universal-apple-darwin'));
  const manifest = JSON.parse(readFileSync(path.join(ROOT, 'package.json')));
  assert.equal(manifest.scripts['package:macos'], 'npm run package:macos:compile && npm run package:macos:bundle');
  assert.doesNotMatch(manifest.scripts['package:macos:compile'], /--config/u);
  assert.match(manifest.scripts['package:macos:bundle'], /--config src-tauri\/tauri\.mcp-bundle\.conf\.json$/u);
  assert.doesNotMatch(manifest.scripts['package:macos:bundle'], /build-mcp-relay|cargo/u);
  const workflow = readFileSync(path.join(ROOT, '.github/workflows/package.yml'), 'utf8');
  assert.match(workflow, /relay="\$app\/Contents\/MacOS\/cartograph-mcp"[\s\S]*test -x "\$relay"[\s\S]*lipo -verify_arch arm64 x86_64 "\$relay"[\s\S]*codesign --verify --strict --verbose=2 "\$relay"/u);
});
