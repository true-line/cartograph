#!/usr/bin/env node
// Package-only sidecar staging. Never run under signing credentials: Cargo can
// execute build scripts and embed its environment. No app launch or activation.
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, renameSync, rmSync } from 'node:fs';
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
// #227: the Windows MSVC slices the installers ship; x64 is the default.
export const WINDOWS_TARGETS = ['x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc'];
export function buildRelay({ root = ROOT, platform = process.platform, env = process.env,
  target: triple = WINDOWS_TARGETS[0],
  run = (command, args, options) => execFileSync(command, args, options) } = {}) {
  if (platform !== 'darwin' && platform !== 'win32') throw new Error('relay packaging requires macOS or Windows');
  if (platform === 'win32' && !WINDOWS_TARGETS.includes(triple)) throw new Error(`unsupported Windows target ${triple}`);
  if (SIGNING_INPUTS.some((name) => Boolean(env[name]))) {
    throw new Error('signing inputs must be absent while compiling the relay');
  }
  // Match the existing package pipeline's fixed target directory. This also
  // keeps the overlay independent of caller CARGO_TARGET_DIR overrides.
  const target = path.join(root, 'target');
  // #227: Windows packages one MSVC slice, named as Tauri's externalBin expects.
  const windows = platform === 'win32';
  const output = path.join(target, 'mcp-relay', windows
    ? `cartograph-mcp-${triple}.exe` : 'cartograph-mcp-universal-apple-darwin');
  const temporary = `${output}.tmp`;
  mkdirSync(path.dirname(output), { recursive: true });
  rmSync(output, { force: true });
  rmSync(temporary, { force: true });
  const options = { cwd: root, env, stdio: 'inherit' };
  try {
    if (windows) {
      run('cargo', ['build', '--locked', '--release', '-p', 'mcp-relay', '--bin',
        'cartograph-mcp', '--target', triple, '--target-dir', target], options);
      copyFileSync(path.join(target, triple, 'release', 'cartograph-mcp.exe'), temporary);
      renameSync(temporary, output);
      return output;
    }
    const slices = ['aarch64-apple-darwin', 'x86_64-apple-darwin'].map((triple) => {
      run('cargo', ['build', '--locked', '--release', '-p', 'mcp-relay', '--bin',
        'cartograph-mcp', '--target', triple, '--target-dir', target], options);
      return path.join(target, triple, 'release', 'cartograph-mcp');
    });
    run('lipo', ['-create', ...slices, '-output', temporary], options);
    for (const arch of ['arm64', 'x86_64']) {
      run('lipo', [temporary, '-verify_arch', arch], options);
    }
    renameSync(temporary, output);
    return output;
  } finally {
    rmSync(temporary, { force: true });
  }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { buildRelay(process.argv[2] ? { target: process.argv[2] } : {}); } catch { console.error('relay build failed'); process.exitCode = 1; }
}
