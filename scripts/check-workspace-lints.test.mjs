import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { RATCHET_MARKER, check, manifestsWithoutLints, memberManifests } from './check-workspace-lints.mjs';

const LINTS = '[lints]\nworkspace = true\n';

function workspace(crates) {
  const root = mkdtempSync(path.join(tmpdir(), 'workspace-lints-'));
  writeFileSync(path.join(root, 'Cargo.toml'), '[workspace]\nmembers = ["crates/*", "shell"]\n\n[workspace.lints.rust]\nmissing_docs = "warn"\n');
  for (const [dir, { manifest, files = {} }] of Object.entries(crates)) {
    mkdirSync(path.join(root, dir, 'src'), { recursive: true });
    writeFileSync(path.join(root, dir, 'Cargo.toml'), manifest);
    for (const [name, body] of Object.entries(files)) writeFileSync(path.join(root, dir, 'src', name), body);
  }
  return root;
}

const allow = `#![allow(missing_docs, reason = "${RATCHET_MARKER}; document them")]\n`;
// The shape rustfmt gives a long allow attribute.
const wrappedAllow = `#![allow(\n    missing_docs,\n    reason = "${RATCHET_MARKER}: document and delete"\n)]\n`;

test('lists members from globs and explicit paths (#1764)', () => {
  const root = workspace({
    'crates/a': { manifest: `[package]\nname = "a"\n\n${LINTS}`, files: { 'lib.rs': '' } },
    shell: { manifest: `[package]\nname = "shell"\n\n${LINTS}` },
  });
  assert.deepEqual(memberManifests(root), ['crates/a/Cargo.toml', 'shell/Cargo.toml']);
  assert.deepEqual(check(root, 0), []);
});

test('fails a member without [lints] workspace = true (#1764)', () => {
  const root = workspace({
    'crates/a': { manifest: '[package]\nname = "a"\n', files: { 'lib.rs': '' } },
    'crates/b': { manifest: '[package]\nname = "b"\n\n[lints]\nworkspace = false\n' },
    shell: { manifest: `[package]\nname = "shell"\n\n${LINTS}` },
  });
  assert.deepEqual(manifestsWithoutLints(root), ['crates/a/Cargo.toml', 'crates/b/Cargo.toml']);
  assert.equal(check(root, 0).length, 2);
});

test('the missing_docs allow count can only shrink (#1764)', () => {
  const root = workspace({
    'crates/a': { manifest: `[package]\nname = "a"\n\n${LINTS}`, files: { 'x.rs': allow, 'y.rs': wrappedAllow } },
    shell: { manifest: `[package]\nname = "shell"\n\n${LINTS}` },
  });
  assert.deepEqual(check(root, 2), []);
  assert.match(check(root, 1).join('\n'), /exceed the ratchet of 1/u);
  assert.match(check(root, 3).join('\n'), /lower MAX_MISSING_DOCS_ALLOWS .* to 2/u);
});

test('a tree without Rust sources fails instead of passing (#1764)', () => {
  const root = workspace({
    'crates/a': { manifest: `[package]\nname = "a"\n\n${LINTS}` },
    shell: { manifest: `[package]\nname = "shell"\n\n${LINTS}` },
  });
  assert.throws(() => check(root, 0), /no Rust sources found/u);
});
