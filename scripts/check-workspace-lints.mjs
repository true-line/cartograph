#!/usr/bin/env node
// Workspace lint inheritance check (#1764).
//
// 1. Every workspace member manifest must contain `[lints]` with
//    `workspace = true`, so `[workspace.lints]` (unsafe_code = deny,
//    missing_docs = warn) applies to it.
// 2. The `missing_docs` allows added when those crates first inherited the
//    lints are a ratchet: their count may only shrink. Documenting a module
//    and deleting its allow must also lower MAX_MISSING_DOCS_ALLOWS here.
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const MAX_MISSING_DOCS_ALLOWS = 36;
export const RATCHET_MARKER = 'undocumented before workspace lints (#1764)';

function tomlSection(text, header) {
  const lines = text.split(/\r?\n/u);
  const start = lines.findIndex((line) => line.trim() === header);
  if (start === -1) return null;
  const body = [];
  for (const line of lines.slice(start + 1)) {
    if (/^\s*\[/u.test(line)) break;
    body.push(line);
  }
  return body;
}

/** Workspace member manifest paths, relative to `root`, from `members` globs. */
export function memberManifests(root) {
  const workspace = tomlSection(readFileSync(path.join(root, 'Cargo.toml'), 'utf8'), '[workspace]') ?? [];
  const match = workspace.join('\n').match(/members\s*=\s*\[([^\]]*)\]/u);
  if (!match) throw new Error('Cargo.toml: [workspace] members not found');
  const patterns = [...match[1].matchAll(/"([^"]+)"/gu)].map((m) => m[1]);
  const manifests = [];
  for (const pattern of patterns) {
    if (pattern.endsWith('/*')) {
      const dir = pattern.slice(0, -2);
      for (const entry of readdirSync(path.join(root, dir), { withFileTypes: true })) {
        const manifest = path.join(dir, entry.name, 'Cargo.toml');
        if (entry.isDirectory() && existsSync(path.join(root, manifest))) manifests.push(manifest);
      }
    } else if (pattern.includes('*')) {
      throw new Error(`Cargo.toml: unsupported members pattern ${pattern}`);
    } else {
      manifests.push(path.join(pattern, 'Cargo.toml'));
    }
  }
  return manifests.sort();
}

/** Manifests that do not opt into the workspace lints. */
export function manifestsWithoutLints(root) {
  return memberManifests(root).filter((manifest) => {
    const lints = tomlSection(readFileSync(path.join(root, manifest), 'utf8'), '[lints]');
    return !lints?.some((line) => /^\s*workspace\s*=\s*true\s*(#.*)?$/u.test(line));
  });
}

function rustFiles(dir, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'target' || entry.name === 'node_modules' || entry.name.startsWith('.')) continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) rustFiles(full, out);
    else if (entry.name.endsWith('.rs')) out.push(full);
  }
  return out;
}

// rustfmt splits a long attribute over several lines, so match across whitespace.
const MISSING_DOCS_ALLOW = /allow\(\s*missing_docs\b/u;

/** Files under the workspace members carrying a ratcheted missing_docs allow. */
export function ratchetedAllows(root) {
  const files = [];
  let scanned = 0;
  for (const manifest of memberManifests(root)) {
    for (const file of rustFiles(path.join(root, path.dirname(manifest)))) {
      scanned += 1;
      const text = readFileSync(file, 'utf8');
      if (MISSING_DOCS_ALLOW.test(text) && text.includes(RATCHET_MARKER)) {
        files.push(path.relative(root, file));
      }
    }
  }
  // Never pass on a tree without sources (e.g. a sparse checkout).
  if (scanned === 0) throw new Error('no Rust sources found under the workspace members');
  return files.sort();
}

export function check(root, max = MAX_MISSING_DOCS_ALLOWS) {
  const errors = manifestsWithoutLints(root).map(
    (manifest) => `${manifest}: add "[lints]\\nworkspace = true" to inherit the workspace lints`,
  );
  const allows = ratchetedAllows(root).length;
  if (allows > max) {
    errors.push(`${allows} missing_docs allows citing #1764 exceed the ratchet of ${max}; document the items instead`);
  } else if (allows < max) {
    errors.push(`only ${allows} missing_docs allows cite #1764; lower MAX_MISSING_DOCS_ALLOWS in scripts/check-workspace-lints.mjs to ${allows}`);
  }
  return errors;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  let errors;
  try {
    errors = check(root);
  } catch (error) {
    errors = [error.message];
  }
  for (const error of errors) console.error(error);
  if (errors.length > 0) process.exit(1);
  console.log('workspace lints: every member inherits [workspace.lints]');
}
