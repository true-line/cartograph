#!/usr/bin/env node
// ADR-0085 decision 6, AC-1427: builds the `cartograph-agent` sidecar as one
// Node single executable per target, from agent-runtime's locked dependencies
// and a pinned, checksum-verified Node release. The app ships it as a Tauri
// external binary, so the packaged app needs no install step.
//
// AC-1424: on macOS it also stages the Claude Code executable the Agent SDK
// drives, checked against its pinned sha256 and Anthropic's Developer ID
// signature. It ships as an app resource, so it keeps that signature.
//
// Never run under signing credentials: the app bundle step signs the result.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
export const SIGNING_INPUTS = [
  'APPLE_CERTIFICATE', 'APPLE_CERTIFICATE_PASSWORD', 'APPLE_API_KEY',
  'APPLE_API_KEY_PATH', 'APPLE_API_ISSUER', 'APPLE_SIGNING_IDENTITY',
  'CSC_LINK', 'CSC_KEY_PASSWORD',
  // ADR-0093: the Windows Artifact Signing service principal.
  'AZURE_TENANT_ID', 'AZURE_CLIENT_ID', 'AZURE_CLIENT_SECRET',
];
// Node's documented single-executable fuse.
export const SEA_FUSE = 'NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2';
/** Target triple -> the Node release slices it is built from. */
export const TARGETS = {
  'universal-apple-darwin': ['darwin-arm64', 'darwin-x64'],
  'x86_64-unknown-linux-gnu': ['linux-x64'],
  // #227: the unsigned Windows installer (.github/workflows/windows.yml).
  'x86_64-pc-windows-msvc': ['win-x64'],
  // The ARM64 installer, cross-built on an x64 Windows runner.
  'aarch64-pc-windows-msvc': ['win-arm64'],
};
/**
 * Target triple -> other hosts that may build it. The blob carries no code
 * cache or snapshot, so a pinned host Node of the same release writes a blob
 * every slice reads; that host binary is verified but never shipped.
 */
export const CROSS_HOSTS = { 'aarch64-pc-windows-msvc': ['win-x64'] };

/**
 * Removes a PE file's Authenticode certificate table: truncates the file at
 * the table and clears its data directory entry and the header checksum.
 * Anything that is not a PE image, or carries no certificate, is returned
 * as is; a table that is not the file's tail cannot be removed safely.
 */
export function stripAuthenticode(image) {
  if (image.length < 0x40 || image.readUInt16LE(0) !== 0x5a4d) return image;
  const pe = image.readUInt32LE(0x3c);
  if (pe + 24 > image.length || image.readUInt32LE(pe) !== 0x00004550) return image;
  const optional = pe + 24;
  const magic = image.readUInt16LE(optional);
  if (magic !== 0x10b && magic !== 0x20b) throw new Error('unknown PE optional header');
  // Data directory 4 is the certificate table; its address is a file offset.
  const entry = optional + (magic === 0x20b ? 112 : 96) + 4 * 8;
  const offset = image.readUInt32LE(entry);
  const size = image.readUInt32LE(entry + 4);
  if (size === 0) return image;
  if (offset + size !== image.length) throw new Error('the Authenticode certificate table is not at the end of the image');
  const stripped = Buffer.from(image.subarray(0, offset));
  stripped.writeUInt32LE(0, entry);
  stripped.writeUInt32LE(0, entry + 4);
  stripped.writeUInt32LE(0, optional + 64);
  return stripped;
}

/** The pinned Node release; every archive must match its recorded sha256. */
export function pinnedNode(root = ROOT) {
  const pin = JSON.parse(readFileSync(path.join(root, 'scripts/agent-runtime-node.json'), 'utf8'));
  if (!/^\d+\.\d+\.\d+$/u.test(pin.version)) throw new Error('pinned Node version is not exact');
  if (!/^[0-9a-f]{64}$/u.test(pin.license_sha256 ?? '')) throw new Error('pinned Node LICENSE has no sha256');
  return pin;
}

/** Target triple -> the Claude Code slice its app carries (AC-1424). The
 * universal app carries arm64 only until per-architecture builds exist. */
export const CLAUDE_CODE_SLICES = { 'universal-apple-darwin': 'darwin-arm64' };

/** The pinned Claude Code: SDK version, signing team and sha256 per slice. */
export function pinnedClaudeCode(root = ROOT) {
  const pin = JSON.parse(readFileSync(path.join(root, 'scripts/agent-runtime-claude.json'), 'utf8'));
  if (!/^[A-Z0-9]{10}$/u.test(pin.team_id ?? '')) throw new Error('pinned Claude Code has no signing team');
  if (!/^\d+\.\d+\.\d+$/u.test(pin.sdk_version ?? '')) throw new Error('pinned Claude Code has no exact SDK version');
  return pin;
}

/** The code requirement Claude Code must meet: Apple-issued Developer ID
 * Application certificate of the pinned team. The app checks the same one. */
export function claudeCodeRequirement(team) {
  return 'anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists '
    + 'and certificate leaf[field.1.2.840.113635.100.6.1.13] exists '
    + `and certificate leaf[subject.OU] = "${team}"`;
}

/** `pi-agent-core@<locked>` and `claude-agent-sdk@<locked>`, from the lockfile. */
export function lockedRunners(root = ROOT) {
  const lock = JSON.parse(readFileSync(path.join(root, 'agent-runtime/package-lock.json'), 'utf8'));
  const locked = (name) => {
    const version = lock.packages?.[`node_modules/${name}`]?.version;
    if (typeof version !== 'string' || version === '') throw new Error(`${name.split('/').at(-1)} is not locked`);
    return version;
  };
  const pi = locked('@mariozechner/pi-agent-core');
  const claude = locked('@anthropic-ai/claude-agent-sdk');
  const pinned = pinnedClaudeCode(root).sdk_version;
  if (claude !== pinned) throw new Error(`Claude Code is pinned for claude-agent-sdk ${pinned}, the lockfile has ${claude}`);
  return { pi: `pi-agent-core@${pi}`, claude: `claude-agent-sdk@${claude}` };
}

/** This host's Node release slice: darwin-arm64, linux-x64, win-x64, ... */
export function hostNodeSlice(platform = process.platform, arch = process.arch) {
  return `${platform === 'win32' ? 'win' : platform}-${arch}`;
}

function archiveName(version, slice) {
  const extension = slice.startsWith('linux') ? 'tar.xz' : slice.startsWith('win') ? 'zip' : 'tar.gz';
  return `node-v${version}-${slice}.${extension}`;
}

/** The release's LICENSE text with LF line ends: the Windows archive ships it
 * with CRLF, so one pinned sha256 covers every slice. */
function licenseText(file) {
  return readFileSync(file, 'utf8').replace(/\r\n/gu, '\n');
}

function sha256(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

function defaultBundle({ root, entry, outfile, runners }) {
  const esbuild = createRequire(path.join(root, 'agent-runtime/package.json'))('esbuild');
  esbuild.buildSync({
    entryPoints: [entry], outfile, bundle: true, platform: 'node', target: 'node22', format: 'cjs',
    // ESM dependencies read import.meta.url (the Agent SDK builds a require
    // from it); in the single executable it is the executable's own URL.
    banner: { js: 'const __cartograph_import_meta_url = require("node:url").pathToFileURL(__filename).href;' },
    define: { CARTOGRAPH_RUNNERS: JSON.stringify(runners), 'import.meta.url': '__cartograph_import_meta_url' },
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
  const windows = target.endsWith('windows-msvc');
  // Node names its Windows release slices win-*, not win32-* (#227).
  const host = hostSlice ?? hostNodeSlice();
  const hosts = [...slices, ...(CROSS_HOSTS[target] ?? [])];
  if (!hosts.includes(host)) throw new Error(`target ${target} must be built on one of ${hosts.join(', ')}`);
  const node = pinnedNode(root);
  const runners = lockedRunners(root);
  const claudeCode = pinnedClaudeCode(root);
  const claudeSlice = CLAUDE_CODE_SLICES[target];
  const work = path.join(root, 'target', 'agent-runtime');
  const output = path.join(work, `cartograph-agent-${target}${windows ? '.exe' : ''}`);
  const temporary = `${output}.tmp`;
  // Tool output goes to stderr: stdout carries only the built path, which
  // callers capture with command substitution.
  const options = { cwd: root, env, stdio: ['ignore', 2, 2] };
  mkdirSync(path.join(work, 'cache'), { recursive: true });
  rmSync(output, { force: true });
  rmSync(temporary, { force: true });
  rmSync(path.join(work, 'NODE-LICENSE'), { force: true });
  rmSync(path.join(work, 'claude-code'), { recursive: true, force: true });
  try {
    // npm is a .cmd script on Windows, which only a shell can start.
    run(windows ? 'npm.cmd' : 'npm', ['ci', '--ignore-scripts', '--no-audit', '--no-fund'],
      { ...options, cwd: path.join(root, 'agent-runtime'), ...(windows ? { shell: true } : {}) });
    const script = path.join(work, 'cartograph-agent.cjs');
    bundle({ root, entry: path.join(root, 'agent-runtime/src/sea-entry.ts'), outfile: script, runners });
    if (claudeSlice) {
      // From the locked install; copied byte for byte so Anthropic's
      // signature stays valid, then checked as the app will check it.
      const source = path.join(root, `agent-runtime/node_modules/@anthropic-ai/claude-agent-sdk-${claudeSlice}/claude`);
      if (!existsSync(source) || sha256(source) !== claudeCode.sha256?.[claudeSlice]) {
        throw new Error(`Claude Code for ${claudeSlice} does not match its pinned sha256`);
      }
      const staged = path.join(work, 'claude-code', 'claude');
      mkdirSync(path.dirname(staged), { recursive: true });
      copyFileSync(source, staged);
      chmodSync(staged, 0o755);
      run('codesign', ['--verify', '--strict', '-R', `=${claudeCodeRequirement(claudeCode.team_id)}`, staged], options);
    }
    const binaries = {};
    const licenses = [];
    for (const slice of slices.includes(host) ? slices : [...slices, host]) {
      const archive = path.join(work, 'cache', archiveName(node.version, slice));
      if (!existsSync(archive) || sha256(archive) !== node.sha256[slice]) {
        run('curl', ['-fsSL', '--retry', '3', '-o', archive,
          `https://nodejs.org/dist/v${node.version}/${archiveName(node.version, slice)}`], options);
      }
      if (sha256(archive) !== node.sha256[slice]) throw new Error(`Node archive for ${slice} does not match its pinned sha256`);
      const extracted = path.join(work, slice);
      rmSync(extracted, { recursive: true, force: true });
      mkdirSync(extracted, { recursive: true });
      // Windows tar (bsdtar) reads the zip archive too.
      const member = `node-v${node.version}-${slice}/${windows ? 'node.exe' : 'bin/node'}`;
      const license = `node-v${node.version}-${slice}/LICENSE`;
      run('tar', ['-xf', archive, '-C', extracted, member, license], options);
      // The shipped notices reproduce this text (scripts/generate-notices.mjs
      // --with-agent-runtime reads it from the build output).
      if (createHash('sha256').update(licenseText(path.join(extracted, license))).digest('hex') !== node.license_sha256) {
        throw new Error(`Node ${node.version} ${slice} LICENSE does not match its pinned sha256`);
      }
      if (slices.includes(slice)) licenses.push(path.join(extracted, license));
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
      const file = path.join(work, `cartograph-agent-${slice}${windows ? '.exe' : ''}`);
      copyFileSync(binaries[slice], file);
      if (mac) run('codesign', ['--remove-signature', file], options);
      // node.exe ships Authenticode signed. postject edits the PE after that
      // signature, which signtool then rejects (0x800700C1), so drop it first,
      // as `signtool remove /s` does in Node's single-executable guide (#227).
      if (windows) writeFileSync(file, stripAuthenticode(readFileSync(file)));
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
    writeFileSync(path.join(work, 'NODE-LICENSE'), licenseText(licenses[0]));
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
