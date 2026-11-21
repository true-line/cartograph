#!/usr/bin/env node
// Changelog versions against release tags (#1762).
//
// Every `## X.Y.Z` heading in CHANGELOG.md is a version that was cut. The
// version-cut workflow tags each cut once its exact commit's CI succeeds, so a
// version with no `vX.Y.Z` tag is a release that cannot be reproduced or
// pointed to. This check reports every such version except the ones recorded
// as never released in docs/release/untagged-versions.json. A version cut
// within the handoff window is still waiting for its tag and is reported as
// pending, not as a gap. A recorded version that has since been tagged fails
// too, so the record only shrinks.
//
//   node scripts/release-tags.mjs [--window-hours 3] [--report <file>]
//
// Exit status: 0 when every version is tagged, pending or recorded; 1 when a
// version has no tag; 2 on a usage or input error.
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT_PATH = fileURLToPath(import.meta.url);
const ROOT = path.resolve(path.dirname(SCRIPT_PATH), '..');
export const RECORD = 'docs/release/untagged-versions.json';

/** Versions with a `## X.Y.Z` heading, in changelog order. */
export function changelogVersions(markdown) {
  const versions = [...markdown.matchAll(/^## (\d+\.\d+\.\d+)\s*$/gmu)].map((match) => match[1]);
  if (new Set(versions).size !== versions.length) throw new Error('CHANGELOG.md lists a version twice');
  return versions;
}

/** The recorded never-released versions, validated. */
export function parseRecord(text) {
  const record = JSON.parse(text);
  if (!Array.isArray(record.versions)) throw new Error(`${RECORD}: versions must be a list`);
  const seen = new Set();
  for (const entry of record.versions) {
    if (!/^\d+\.\d+\.\d+$/u.test(entry.version ?? '')) throw new Error(`${RECORD}: bad version ${entry.version}`);
    if (!/^[0-9a-f]{40}$/u.test(entry.cut_commit ?? '')) throw new Error(`${RECORD}: ${entry.version} needs a full cut_commit`);
    if (!Object.hasOwn(record.causes ?? {}, entry.cause ?? '')) throw new Error(`${RECORD}: ${entry.version} needs a cause listed under causes`);
    if (seen.has(entry.version)) throw new Error(`${RECORD}: ${entry.version} is recorded twice`);
    seen.add(entry.version);
  }
  return new Map(record.versions.map((entry) => [entry.version, entry]));
}

/**
 * Classify each changelog version. `cutAt(version)` returns the cut commit's
 * time in milliseconds, or null when no main commit carries that version.
 */
export function classify({ versions, tags, recorded, cutAt, now, windowHours }) {
  const result = { tagged: [], pending: [], recorded: [], gaps: [], stale: [] };
  for (const version of versions) {
    const tagged = tags.has(`v${version}`);
    if (recorded.has(version)) {
      (tagged ? result.stale : result.recorded).push(version);
      continue;
    }
    if (tagged) {
      result.tagged.push(version);
      continue;
    }
    const at = cutAt(version);
    if (at !== null && now - at < windowHours * 3_600_000) result.pending.push(version);
    else result.gaps.push(version);
  }
  for (const version of recorded.keys()) {
    if (!versions.includes(version)) result.stale.push(version);
  }
  return result;
}

/** A Markdown report of a classification. */
export function report(result, { windowHours }) {
  const lines = ['## Changelog versions without a release tag', ''];
  lines.push(`- Tagged: ${result.tagged.length}`);
  lines.push(`- Recorded as never released (\`${RECORD}\`): ${result.recorded.length}`);
  lines.push(`- Cut within the last ${windowHours} h, tag pending: ${result.pending.length ? result.pending.map((v) => `v${v}`).join(', ') : 'none'}`);
  lines.push('');
  if (result.gaps.length) {
    lines.push(`**${result.gaps.length} version(s) have no tag after the handoff window:** ${result.gaps.map((v) => `v${v}`).join(', ')}.`);
    lines.push('');
    lines.push('Find why the version-cut tag job did not tag each one (its run on the cut commit\'s CI completion), then either recover it through the version-cut workflow or record it in the file above with its cause. Existing tags are never moved.');
    lines.push('');
  }
  if (result.stale.length) {
    lines.push(`**Stale record entries:** ${result.stale.map((v) => `v${v}`).join(', ')} — tagged since, or no longer in the changelog. Remove them from the record.`);
    lines.push('');
  }
  if (!result.gaps.length && !result.stale.length) lines.push('Every changelog version is tagged, pending or recorded.');
  return `${lines.join('\n')}\n`;
}

function git(args) {
  return execFileSync('git', args, { cwd: ROOT, encoding: 'utf8', maxBuffer: 1 << 28 });
}

/** Commit time of the first first-parent main commit whose package.json has each version. */
function cutTimes(ref) {
  const times = new Map();
  const log = git(['log', '--first-parent', '--reverse', '--format=%H %ct', ref, '--', 'package.json']);
  for (const line of log.split('\n').filter(Boolean)) {
    const [sha, seconds] = line.split(' ');
    let version;
    try {
      version = JSON.parse(git(['show', `${sha}:package.json`])).version;
    } catch {
      continue;
    }
    if (version && !times.has(version)) times.set(version, Number(seconds) * 1000);
  }
  return times;
}

function parseArguments(argv) {
  const options = { windowHours: 3, ref: 'HEAD' };
  for (let i = 0; i < argv.length; i += 1) {
    const [flag, value] = [argv[i], argv[i + 1]];
    if (flag === '--window-hours' && /^\d+(\.\d+)?$/u.test(value ?? '')) options.windowHours = Number(value);
    else if (flag === '--report' && value) options.report = value;
    else if (flag === '--ref' && value) options.ref = value;
    else throw new Error(`unknown argument: ${flag}`);
    i += 1;
  }
  return options;
}

function main() {
  const options = parseArguments(process.argv.slice(2));
  const versions = changelogVersions(readFileSync(path.join(ROOT, 'CHANGELOG.md'), 'utf8'));
  const recorded = parseRecord(readFileSync(path.join(ROOT, RECORD), 'utf8'));
  const tags = new Set(git(['tag', '--list', 'v*']).split('\n').filter(Boolean));
  if (tags.size === 0) throw new Error('no v* tags in this checkout; fetch tags first');
  const times = cutTimes(options.ref);
  const result = classify({
    versions, tags, recorded, now: Date.now(), windowHours: options.windowHours,
    cutAt: (version) => times.get(version) ?? null,
  });
  const text = report(result, options);
  if (options.report) writeFileSync(options.report, text);
  process.stdout.write(text);
  process.exitCode = result.gaps.length || result.stale.length ? 1 : 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === SCRIPT_PATH) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 2;
  }
}
