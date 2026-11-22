#!/usr/bin/env node
// ADR-0085 decision 6, AC-1427: builds the `cartograph-agent` sidecar as one
// Node single executable per target, from agent-runtime's locked dependencies
// and a pinned, checksum-verified Node release. The app ships it as a Tauri
// external binary, so the packaged app needs no install step.
//
// Never run under signing credentials: the app bundle step signs the result.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
export const SIGNING_INPUTS = [
  'APPLE_CERTIFICATE', 'APPLE_CERTIFICATE_PASSWORD', 'APPLE_API_KEY',
  'APPLE_API_KEY_PATH', 'APPLE_API_ISSUER', 'APPLE_SIGNING_IDENTITY',
  'CSC_LINK', 'CSC_KEY_PASSWORD',
];
// Node's documented single-executable fuse.
export const SEA_FUSE = 'NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2';
/** Target triple -> the Node release slices it is built from. */
export const TARGETS = {
  'universal-apple-darwin': ['darwin-arm64', 'darwin-x64'],
  'x86_64-unknown-linux-gnu': ['linux-x64'],
};

/** The pinned Node release; every archive must match its recorded sha256. */
export function pinnedNode(root = ROOT) {
  const pin = JSON.parse(readFileSync(path.join(root, 'scripts/agent-runtime-node.json'), 'utf8'));
  if (!/^\d+\.\d+\.\d+$/u.test(pin.version)) throw new Error('pinned Node version is not exact');
  if (!/^[0-9a-f]{64}$/u.test(pin.license_sha256 ?? '')) throw new Error('pinned Node LICENSE has no sha256');
  return pin;
}

/** `pi-agent-core@<locked version>`, from agent-runtime's lockfile. */
export function lockedRunners(root = ROOT) {
  const lock = JSON.parse(readFileSync(path.join(root, 'agent-runtime/package-lock.json'), 'utf8'));
  const version = lock.packages?.['node_modules/@mariozechner/pi-agent-core']?.version;
  if (typeof version !== 'string' || version === '') throw new Error('pi-agent-core is not locked');
  // The Agent SDK lane is not in the packaged build yet (AC-1424): an empty
  // version makes the host refuse a Claude run before it starts.
  return { pi: `pi-agent-core@${version}`, claude: '' };
}

function archiveName(version, slice) {
  return `node-v${version}-${slice}.${slice.startsWith('linux') ? 'tar.xz' : 'tar.gz'}`;
}

function sha256(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

function defaultBundle({ root, entry, outfile, runners }) {
  const esbuild = createRequire(path.join(root, 'agent-runtime/package.json'))('esbuild');
  esbuild.buildSync({
    entryPoints: [entry], outfile, bundle: true, platform: 'node', target: 'node22', format: 'cjs',
    // Only reached on the Claude lane, which this build does not carry.
    external: ['@anthropic-ai/claude-agent-sdk', '@modelcontextprotocol/sdk'],
    define: { CARTOGRAPH_RUNNERS: JSON.stringify(runners), 'import.meta.url': 'undefined' },
    legalComments: 'eof', logLevel: 'warning',
  });
}

export function buildAgentRuntime({ root = ROOT, target, env = process.env, hostSlice,
  run = (command, args, options) => execFileSync(command, args, options), bundle = defaultBundle } = {}) {
  const slices = TARGETS[target];
  if (!slices) throw new Error(`unsupported target ${target}`);
  if (SIGNING_INPUTS.some((name) => Boolean(env[name]))) {
    throw new Error('signing inputs must be absent while building the agent runtime');
  }
  const mac = target.endsWith('apple-darwin');
  const host = hostSlice ?? `${process.platform}-${process.arch}`;
  if (!slices.includes(host)) throw new Error(`target ${target} must be built on one of ${slices.join(', ')}`);
  const node = pinnedNode(root);
  const runners = lockedRunners(root);
  const work = path.join(root, 'target', 'agent-runtime');
  const output = path.join(work, `cartograph-agent-${target}`);
  const temporary = `${output}.tmp`;
  // Tool output goes to stderr: stdout carries only the built path, which
  // callers capture with command substitution.
  const options = { cwd: root, env, stdio: ['ignore', 2, 2] };
  mkdirSync(path.join(work, 'cache'), { recursive: true });
  rmSync(output, { force: true });
  rmSync(temporary, { force: true });
  rmSync(path.join(work, 'NODE-LICENSE'), { force: true });
  try {
    run('npm', ['ci', '--ignore-scripts', '--no-audit', '--no-fund'], { ...options, cwd: path.join(root, 'agent-runtime') });
    const script = path.join(work, 'cartograph-agent.cjs');
    bundle({ root, entry: path.join(root, 'agent-runtime/src/sea-entry.ts'), outfile: script, runners });
    const binaries = {};
    const licenses = [];
    for (const slice of slices) {
      const archive = path.join(work, 'cache', archiveName(node.version, slice));
      if (!existsSync(archive) || sha256(archive) !== node.sha256[slice]) {
        run('curl', ['-fsSL', '--retry', '3', '-o', archive,
          `https://nodejs.org/dist/v${node.version}/${archiveName(node.version, slice)}`], options);
      }
      if (sha256(archive) !== node.sha256[slice]) throw new Error(`Node archive for ${slice} does not match its pinned sha256`);
      const extracted = path.join(work, slice);
      rmSync(extracted, { recursive: true, force: true });
      mkdirSync(extracted, { recursive: true });
      const member = `node-v${node.version}-${slice}/bin/node`;
      const license = `node-v${node.version}-${slice}/LICENSE`;
      run('tar', ['-xf', archive, '-C', extracted, member, license], options);
      // The shipped notices reproduce this text (scripts/generate-notices.mjs
      // --with-agent-runtime reads it from the build output).
      if (sha256(path.join(extracted, license)) !== node.license_sha256) {
        throw new Error(`Node ${node.version} ${slice} LICENSE does not match its pinned sha256`);
      }
      licenses.push(path.join(extracted, license));
      binaries[slice] = path.join(extracted, member);
    }
    // The blob carries no code cache or snapshot, so one blob serves every
    // slice; the pinned host binary writes it in its own format.
    const blob = path.join(work, 'sea-prep.blob');
    const config = path.join(work, 'sea-config.json');
    writeFileSync(config, JSON.stringify({
      main: script, output: blob, disableExperimentalSEAWarning: true,
      useSnapshot: false, useCodeCache: false, execArgvExtension: 'none',
    }));
    run(binaries[host], ['--experimental-sea-config', config], options);
    const postject = path.join(root, 'agent-runtime/node_modules/postject/dist/cli.js');
    const built = slices.map((slice) => {
      const file = path.join(work, `cartograph-agent-${slice}`);
      copyFileSync(binaries[slice], file);
      if (mac) run('codesign', ['--remove-signature', file], options);
      run(process.execPath, [postject, file, 'NODE_SEA_BLOB', blob, '--sentinel-fuse', SEA_FUSE,
        ...(mac ? ['--macho-segment-name', 'NODE_SEA'] : [])], options);
      return file;
    });
    if (mac) {
      run('lipo', ['-create', ...built, '-output', temporary], options);
      for (const arch of ['arm64', 'x86_64']) run('lipo', [temporary, '-verify_arch', arch], options);
    } else {
      copyFileSync(built[0], temporary);
    }
    copyFileSync(licenses[0], path.join(work, 'NODE-LICENSE'));
    renameSync(temporary, output);
    return output;
  } finally {
    rmSync(temporary, { force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    console.log(buildAgentRuntime({ target: process.argv[2] }));
  } catch (error) {
    console.error(`agent runtime build failed: ${error.message}`);
    process.exitCode = 1;
  }
}
