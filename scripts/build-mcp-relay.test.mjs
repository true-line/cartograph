import test from 'node:test';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync, existsSync, rmSync, mkdirSync } from 'node:fs';
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
    if (command === 'lipo' && args[1] === '-verify_arch') assert.equal(existsSync(args[0]), true);
  } });
  assert.equal(readFileSync(output, 'utf8'), 'universal-fixture');
  assert.equal(existsSync(`${output}.tmp`), false);
  assert.deepEqual(calls.slice(0, 2).map(([command, args]) => [command, ...args]),
    ['aarch64-apple-darwin', 'x86_64-apple-darwin'].map((triple) => ['cargo', 'build', '--locked', '--release', '-p', 'mcp-relay', '--bin', 'cartograph-mcp', '--target', triple, '--target-dir', path.join(root, 'target')]));
  assert.deepEqual(calls[3].slice(0, 1), ['lipo']);
  assert.deepEqual(calls.slice(3).map(([, args]) => args.slice(1)), [['-verify_arch', 'arm64'], ['-verify_arch', 'x86_64']]);
});
test('compilation and universal verification failures cannot leave a stale staged relay', (t) => {
  const root = fixture(t);
  const output = buildRelay({ root, platform: 'darwin', env: {}, run(command, args) {
    if (command === 'lipo' && args[0] === '-create') writeFileSync(args.at(-1), 'old');
  } });
  for (const failure of ['cargo', '-verify_arch']) {
    assert.throws(() => buildRelay({ root, platform: 'darwin', env: {}, run(command, args) {
      if (command === 'lipo' && args[0] === '-create') writeFileSync(args.at(-1), 'new');
      if (command === failure || args.includes(failure)) throw new Error('fixture failure');
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
  assert.deepEqual(overlay.bundle.externalBin, ['../target/mcp-relay/cartograph-mcp', '../target/agent-runtime/cartograph-agent']);
  assert.equal(path.resolve(path.dirname(overlayPath), `${overlay.bundle.externalBin[0]}-universal-apple-darwin`), path.join(ROOT, 'target/mcp-relay/cartograph-mcp-universal-apple-darwin'));
  const manifest = JSON.parse(readFileSync(path.join(ROOT, 'package.json')));
  assert.equal(manifest.scripts['package:macos'], 'npm run package:macos:compile && npm run package:macos:bundle');
  assert.doesNotMatch(manifest.scripts['package:macos:compile'], /--config/u);
  assert.match(manifest.scripts['package:macos:bundle'], /--config src-tauri\/tauri\.mcp-bundle\.conf\.json$/u);
  assert.doesNotMatch(manifest.scripts['package:macos:bundle'], /build-mcp-relay|cargo/u);
  const workflow = readFileSync(path.join(ROOT, '.github/workflows/package.yml'), 'utf8');
  assert.match(workflow, /relay="\$app\/Contents\/MacOS\/cartograph-mcp"[\s\S]*test -x "\$relay"[\s\S]*lipo "\$relay" -verify_arch arm64[\s\S]*lipo "\$relay" -verify_arch x86_64[\s\S]*codesign --verify --strict --verbose=2 "\$relay"/u);
});

// AC-0289: verify each required architecture with the actual platform lipo.
test('native lipo accepts both compiled slices and verifies the staged universal file', { skip: process.platform !== 'darwin' }, (t) => {
  const root = fixture(t);
  const output = buildRelay({ root, platform: 'darwin', env: {}, run(command, args) {
    if (command === 'cargo') {
      const triple = args[args.indexOf('--target') + 1];
      const binary = path.join(root, 'target', triple, 'release', 'cartograph-mcp');
      mkdirSync(path.dirname(binary), { recursive: true });
      execFileSync('/usr/bin/xcrun', ['clang', '-arch', triple.startsWith('aarch64') ? 'arm64' : 'x86_64', '-x', 'c', '-c', '-', '-o', binary], { input: 'int cartograph_relay_fixture;\n' });
    } else {
      execFileSync('/usr/bin/lipo', args);
    }
  } });
  const archs = execFileSync('/usr/bin/lipo', [output, '-archs'], { encoding: 'utf8' }).trim().split(/\s+/u).sort();
  assert.deepEqual(archs, ['arm64', 'x86_64']);
});
