#!/usr/bin/env node
// Prints the Linux CI image reference for this checkout. The tag hashes every
// input of ci/linux.Dockerfile, so a change to the Dockerfile or the pinned
// toolchain names a new image, and an unchanged checkout reuses the published
// one (ci.yml builds the image only when its tag is missing).
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const CI_IMAGE_REPOSITORY = 'ghcr.io/true-line/cartograph-ci';
export const CI_IMAGE_INPUTS = ['ci/linux.Dockerfile', 'rust-toolchain.toml'];

export function ciImageTag(root) {
  const hash = createHash('sha256');
  for (const input of CI_IMAGE_INPUTS) {
    hash.update(`${input}\0`);
    hash.update(readFileSync(path.join(root, input)));
    hash.update('\0');
  }
  return hash.digest('hex').slice(0, 16);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  process.stdout.write(`${CI_IMAGE_REPOSITORY}:${ciImageTag(root)}\n`);
}
