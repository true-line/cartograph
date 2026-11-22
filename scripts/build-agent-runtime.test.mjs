// AC-1427 (ADR-0085 decision 6): the packaged agent runtime is built from the
// committed lockfile and a pinned, checksum-verified Node release.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildAgentRuntime, claudeCodeRequirement, hostNodeSlice, lockedRunners, pinnedClaudeCode, pinnedNode, SEA_FUSE, SIGNING_INPUTS } from './build-agent-runtime.mjs';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const archive = (slice) => `node archive ${slice}`;
const digest = (text) => createHash('sha256').update(text).digest('hex');

function fixture(t, { pi = '0.73.1', sdk = '0.3.330', claude = 'claude code', license = 'node license' } = {}) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'cartograph-agent-package-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(path.join(root, 'scripts'));
  mkdirSync(path.join(root, 'agent-runtime'));
  const slices = ['darwin-arm64', 'darwin-x64', 'linux-x64', 'win-x64'];
  writeFileSync(path.join(root, 'scripts/agent-runtime-node.json'), JSON.stringify({
    version: '22.23.5', sha256: Object.fromEntries(slices.map((slice) => [slice, digest(archive(slice))])),
    license_sha256: digest(license),
  }));
  writeFileSync(path.join(root, 'agent-runtime/package-lock.json'), JSON.stringify({
    packages: {
      ...(pi ? { 'node_modules/@mariozechner/pi-agent-core': { version: pi } } : {}),
      'node_modules/@anthropic-ai/claude-agent-sdk': { version: sdk },
    },
  }));
  writeFileSync(path.join(root, 'scripts/agent-runtime-claude.json'), JSON.stringify({
    sdk_version: '0.3.330', team_id: 'Q6L2SF6YDW', sha256: { 'darwin-arm64': digest('claude code') },
  }));
  const native = path.join(root, 'agent-runtime/node_modules/@anthropic-ai/claude-agent-sdk-darwin-arm64');
  mkdirSync(native, { recursive: true });
  writeFileSync(path.join(native, 'claude'), claude);
  return root;
}

/** A command stand-in that writes what each real tool would. */
function tools(root, calls, { corrupt, license = 'node license' } = {}) {
  return (command, args, options) => {
    calls.push([path.basename(command), ...args]);
    if (command === 'curl') {
      const slice = /node-v[\d.]+-(.+?)\.(?:tar|zip)/u.exec(args.at(-1))[1];
      writeFileSync(args[args.indexOf('-o') + 1], slice === corrupt ? 'tampered' : archive(slice));
    }
    if (command === 'tar') {
      const into = args[args.indexOf('-C') + 1];
      const [binary, notice] = args.slice(-2).map((member) => path.join(into, member));
      mkdirSync(path.dirname(binary), { recursive: true });
      writeFileSync(binary, 'node');
      writeFileSync(notice, license);
    }
    if (args[0] === '--experimental-sea-config') {
      const config = JSON.parse(readFileSync(args[1], 'utf8'));
      assert.equal(config.useCodeCache, false, 'one blob must serve every architecture');
      assert.equal(config.useSnapshot, false);
      writeFileSync(config.output, 'blob');
    }
    if (command === 'lipo' && args[0] === '-create') writeFileSync(args.at(-1), 'universal');
    if (command === 'npm') assert.equal(options.cwd, path.join(root, 'agent-runtime'));
  };
}

test('mac: locked install, verified Node slices, one blob, injected and merged into a universal binary', (t) => {
  const root = fixture(t);
  const calls = [];
  let bundled;
  const output = buildAgentRuntime({ root, target: 'universal-apple-darwin', env: {}, hostSlice: 'darwin-arm64',
    run: tools(root, calls), bundle: (options) => { bundled = options; } });
  assert.equal(output, path.join(root, 'target/agent-runtime/cartograph-agent-universal-apple-darwin'));
  assert.equal(readFileSync(output, 'utf8'), 'universal');
  assert.equal(existsSync(`${output}.tmp`), false);
  assert.deepEqual(bundled.runners, { pi: 'pi-agent-core@0.73.1', claude: 'claude-agent-sdk@0.3.330' });
  // AC-1424: the pinned Claude Code is staged byte for byte and its Anthropic
  // signature checked as the app checks it.
  const staged = path.join(root, 'target/agent-runtime/claude-code/claude');
  assert.equal(readFileSync(staged, 'utf8'), 'claude code');
  assert.deepEqual(calls.find(([command, first]) => command === 'codesign' && first === '--verify'),
    ['codesign', '--verify', '--strict', '-R', `=${claudeCodeRequirement('Q6L2SF6YDW')}`, staged]);
  assert.deepEqual(calls[0], ['npm', 'ci', '--ignore-scripts', '--no-audit', '--no-fund']);
  const curls = calls.filter(([command]) => command === 'curl').map((call) => call.at(-1));
  assert.deepEqual(curls, ['darwin-arm64.tar.gz', 'darwin-x64.tar.gz'].map((tail) => `https://nodejs.org/dist/v22.23.5/node-v22.23.5-${tail}`));
  const blob = calls.find(([, first]) => first === '--experimental-sea-config');
  assert.equal(blob[0], 'node', 'the pinned host Node writes the blob');
  const injected = calls.filter((call) => call.includes('NODE_SEA_BLOB'));
  assert.equal(injected.length, 2);
  for (const call of injected) assert.deepEqual(call.slice(-4), ['--sentinel-fuse', SEA_FUSE, '--macho-segment-name', 'NODE_SEA']);
  assert.equal(calls.filter(([command, first]) => command === 'codesign' && first === '--remove-signature').length, 2);
  assert.deepEqual(calls.filter(([command, , second]) => command === 'lipo' && second === '-verify_arch').map((call) => call.at(-1)), ['arm64', 'x86_64']);
});

test('linux: one slice, no Mach-O steps', (t) => {
  const root = fixture(t);
  const calls = [];
  const output = buildAgentRuntime({ root, target: 'x86_64-unknown-linux-gnu', env: {}, hostSlice: 'linux-x64',
    run: tools(root, calls), bundle: () => {} });
  assert.equal(readFileSync(output, 'utf8'), 'node');
  assert.equal(readFileSync(path.join(root, 'target/agent-runtime/NODE-LICENSE'), 'utf8'), 'node license', 'the verified LICENSE feeds the shipped notices');
  assert.equal(calls.some(([command]) => command === 'codesign' || command === 'lipo'), false);
  assert.equal(existsSync(path.join(root, 'target/agent-runtime/claude-code')), false, 'only the macOS app carries Claude Code');
  assert.equal(calls.find((call) => call.includes('NODE_SEA_BLOB')).includes('--macho-segment-name'), false);
});

test('windows: the zip archive, node.exe, a CRLF LICENSE and an .exe sidecar (#227)', (t) => {
  const root = fixture(t, { license: 'node\nlicense\n' });
  const calls = [];
  const output = buildAgentRuntime({ root, target: 'x86_64-pc-windows-msvc', env: {}, hostSlice: 'win-x64',
    run: tools(root, calls, { license: 'node\r\nlicense\r\n' }), bundle: () => {} });
  assert.equal(path.basename(output), 'cartograph-agent-x86_64-pc-windows-msvc.exe');
  assert.equal(readFileSync(output, 'utf8'), 'node');
  assert.ok(calls.some((call) => call[0] === 'curl' && call.at(-1).endsWith('/node-v22.23.5-win-x64.zip')));
  assert.ok(calls.some((call) => call[0] === 'tar' && call.includes('node-v22.23.5-win-x64/node.exe')));
  assert.ok(calls.some((call) => call[0] === 'npm.cmd'), 'npm is a .cmd script on Windows');
  assert.equal(calls.some(([command]) => command === 'codesign' || command === 'lipo'), false);
  assert.equal(calls.find((call) => call.includes('NODE_SEA_BLOB')).includes('--macho-segment-name'), false);
  assert.equal(readFileSync(path.join(root, 'target/agent-runtime/NODE-LICENSE'), 'utf8'), 'node\nlicense\n',
    'one pinned sha256 covers the CRLF copy, and the shipped notices carry LF line ends');
});

test('the host slice follows Node release names, so Windows is win-x64 (#227)', () => {
  assert.equal(hostNodeSlice('win32', 'x64'), 'win-x64');
  assert.equal(hostNodeSlice('darwin', 'arm64'), 'darwin-arm64');
  assert.equal(hostNodeSlice('linux', 'x64'), 'linux-x64');
});

test('a cached archive that still matches its pin is not downloaded again', (t) => {
  const root = fixture(t);
  const build = (calls) => buildAgentRuntime({ root, target: 'x86_64-unknown-linux-gnu', env: {}, hostSlice: 'linux-x64',
    run: tools(root, calls), bundle: () => {} });
  build([]);
  const calls = [];
  build(calls);
  assert.equal(calls.some(([command]) => command === 'curl'), false);
});

test('a Node archive that does not match its pin fails the build and leaves nothing staged', (t) => {
  const root = fixture(t);
  const target = 'universal-apple-darwin';
  assert.throws(() => buildAgentRuntime({ root, target, env: {}, hostSlice: 'darwin-arm64',
    run: tools(root, [], { corrupt: 'darwin-x64' }), bundle: () => {} }), /darwin-x64 does not match its pinned sha256/u);
  const output = path.join(root, 'target/agent-runtime/cartograph-agent-universal-apple-darwin');
  assert.equal(existsSync(output), false);
  assert.equal(existsSync(`${output}.tmp`), false);
});

test('a Claude Code off its pin, or a pin for another SDK version, fails the build', (t) => {
  const mac = (root) => () => buildAgentRuntime({ root, target: 'universal-apple-darwin', env: {}, hostSlice: 'darwin-arm64',
    run: tools(root, []), bundle: () => {} });
  const tampered = fixture(t, { claude: 'tampered' });
  assert.throws(mac(tampered), /Claude Code for darwin-arm64 does not match its pinned sha256/u);
  assert.equal(existsSync(path.join(tampered, 'target/agent-runtime/claude-code/claude')), false);
  assert.throws(mac(fixture(t, { sdk: '0.3.331' })), /pinned for claude-agent-sdk 0\.3\.330, the lockfile has 0\.3\.331/u);
});

test('a Node release whose LICENSE differs from its pin fails the build', (t) => {
  const root = fixture(t);
  assert.throws(() => buildAgentRuntime({ root, target: 'x86_64-unknown-linux-gnu', env: {}, hostSlice: 'linux-x64',
    run: tools(root, [], { license: 'changed' }), bundle: () => {} }), /LICENSE does not match its pinned sha256/u);
});

test('signing inputs, unknown targets, the wrong host and an unlocked Pi all fail before any command runs', (t) => {
  const run = () => assert.fail('must not run a command');
  const bundle = () => assert.fail('must not bundle');
  const root = fixture(t);
  for (const key of SIGNING_INPUTS) {
    assert.throws(() => buildAgentRuntime({ root, target: 'universal-apple-darwin', env: { [key]: 'secret' }, hostSlice: 'darwin-arm64', run, bundle }), /signing inputs/u);
  }
  assert.throws(() => buildAgentRuntime({ root, target: 'aarch64-pc-windows-msvc', env: {}, run, bundle }), /unsupported target/u);
  assert.throws(() => buildAgentRuntime({ root, target: 'universal-apple-darwin', env: {}, hostSlice: 'linux-x64', run, bundle }), /must be built on/u);
  assert.throws(() => buildAgentRuntime({ root: fixture(t, { pi: null }), target: 'x86_64-unknown-linux-gnu', env: {}, hostSlice: 'linux-x64', run, bundle }), /pi-agent-core is not locked/u);
});

test('the repository pins exact Node and Pi versions and packages the sidecar on macOS', () => {
  const node = pinnedNode(ROOT);
  for (const slice of ['darwin-arm64', 'darwin-x64', 'linux-x64', 'win-x64']) assert.match(node.sha256[slice], /^[0-9a-f]{64}$/u);
  assert.match(node.license_sha256, /^[0-9a-f]{64}$/u);
  assert.match(lockedRunners(ROOT).pi, /^pi-agent-core@\d+\.\d+\.\d+$/u);
  assert.match(lockedRunners(ROOT).claude, /^claude-agent-sdk@\d+\.\d+\.\d+$/u);
  const claude = pinnedClaudeCode(ROOT);
  assert.equal(claude.team_id, 'Q6L2SF6YDW', 'Anthropic PBC');
  assert.match(claude.sha256['darwin-arm64'], /^[0-9a-f]{64}$/u);
  const manifest = JSON.parse(readFileSync(path.join(ROOT, 'agent-runtime/package.json'), 'utf8'));
  for (const [name, version] of Object.entries({ ...manifest.dependencies, ...manifest.devDependencies })) {
    assert.match(version, /^\d+\.\d+\.\d+(-[\w.]+)?$/u, `${name} is pinned exactly`);
  }
  const overlay = JSON.parse(readFileSync(path.join(ROOT, 'src-tauri/tauri.mcp-bundle.conf.json'), 'utf8'));
  assert.ok(overlay.bundle.externalBin.includes('../target/agent-runtime/cartograph-agent'));
  const scripts = JSON.parse(readFileSync(path.join(ROOT, 'package.json'), 'utf8')).scripts;
  assert.match(scripts['package:macos:compile'], /node scripts\/build-agent-runtime\.mjs universal-apple-darwin/u);
  // CI captures the printed path with $(...), so tools must not write to stdout.
  assert.match(readFileSync(path.join(ROOT, 'scripts/build-agent-runtime.mjs'), 'utf8'), /stdio: \['ignore', 2, 2\]/u);
  const workflow = readFileSync(path.join(ROOT, '.github/workflows/package.yml'), 'utf8');
  assert.match(workflow, /node scripts\/build-agent-runtime\.mjs universal-apple-darwin\n[\s\S]*node scripts\/generate-notices\.mjs --with-agent-runtime/u, 'the sidecar is built before the shipped notices are generated');
  assert.match(workflow, /agent="\$app\/Contents\/MacOS\/cartograph-agent"[\s\S]*lipo "\$agent" -verify_arch arm64[\s\S]*lipo "\$agent" -verify_arch x86_64[\s\S]*codesign --verify --strict --verbose=2 "\$agent"[\s\S]*"type":"ready"/u);
});
