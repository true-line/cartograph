#!/usr/bin/env node
// Opens, updates or closes the one tracking issue of a scheduled check (#1768).
//
// A scheduled lane that misses its thresholds must be seen, and a lane that
// fails every night must not open a new issue every night. Each lane owns one
// issue, found by its exact title among the open issues the Actions bot
// opened: a miss comments on it (or opens it), and a later pass comments and
// closes it. It runs with the workflow's own GITHUB_TOKEN (`issues: write`),
// so the issue is written by github-actions[bot].
//
//   node scripts/scheduled-issue.mjs open  --title <title> --body-file <file>
//   node scripts/scheduled-issue.mjs close --title <title> --body-file <file>
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT_PATH = fileURLToPath(import.meta.url);
export const BOT = 'github-actions[bot]';
const MAX_BODY = 60_000;

/** The open issue the bot opened with exactly this title, from listed issues. */
export function findIssue(issues, title) {
  return issues.find((issue) => !issue.pull_request && issue.state === 'open'
    && issue.title === title && issue.user?.login === BOT) ?? null;
}

/** A body GitHub accepts, with the run that wrote it. */
export function withRun(body, runUrl) {
  const trimmed = body.length > MAX_BODY ? `${body.slice(0, MAX_BODY)}\n\n…(truncated; see the run)\n` : body;
  return runUrl ? `${trimmed.trimEnd()}\n\nRun: ${runUrl}\n` : trimmed;
}

/**
 * What to do for `mode` given the existing issue (or null): `create`,
 * `comment`, `close` (comment then close) or `none`.
 */
export function plan(mode, existing) {
  if (mode === 'open') return existing ? 'comment' : 'create';
  if (mode === 'close') return existing ? 'close' : 'none';
  throw new Error(`mode must be open or close, got ${mode}`);
}

async function api(method, route, body) {
  const base = process.env.GITHUB_API_URL || 'https://api.github.com';
  const response = await fetch(`${base}/repos/${process.env.GITHUB_REPOSITORY}${route}`, {
    method,
    headers: {
      accept: 'application/vnd.github+json',
      authorization: `Bearer ${process.env.GITHUB_TOKEN}`,
      'x-github-api-version': '2022-11-28',
      ...(body ? { 'content-type': 'application/json' } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!response.ok) throw new Error(`${method} ${route}: ${response.status} ${await response.text()}`);
  return response.json();
}

async function openIssues() {
  const issues = [];
  for (let page = 1; page <= 10; page += 1) {
    const batch = await api('GET', `/issues?state=open&per_page=100&page=${page}`);
    issues.push(...batch);
    if (batch.length < 100) break;
  }
  return issues;
}

function parseArguments(argv) {
  const [mode, ...rest] = argv;
  const options = { mode };
  for (let i = 0; i < rest.length; i += 2) {
    const [flag, value] = [rest[i], rest[i + 1]];
    if (!value) throw new Error(`${flag} needs a value`);
    if (flag === '--title') options.title = value;
    else if (flag === '--body-file') options.bodyFile = value;
    else throw new Error(`unknown argument: ${flag}`);
  }
  if (!options.title || !options.bodyFile) throw new Error('usage: scheduled-issue.mjs <open|close> --title <title> --body-file <file>');
  return options;
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  for (const name of ['GITHUB_TOKEN', 'GITHUB_REPOSITORY']) {
    if (!process.env[name]) throw new Error(`${name} is not set`);
  }
  const runUrl = process.env.GITHUB_RUN_ID
    ? `${process.env.GITHUB_SERVER_URL || 'https://github.com'}/${process.env.GITHUB_REPOSITORY}/actions/runs/${process.env.GITHUB_RUN_ID}`
    : '';
  const body = withRun(readFileSync(options.bodyFile, 'utf8'), runUrl);
  const existing = findIssue(await openIssues(), options.title);
  const action = plan(options.mode, existing);
  if (action === 'create') {
    const issue = await api('POST', '/issues', { title: options.title, body });
    console.log(`opened #${issue.number}`);
  } else if (action === 'comment' || action === 'close') {
    await api('POST', `/issues/${existing.number}/comments`, { body });
    if (action === 'close') await api('PATCH', `/issues/${existing.number}`, { state: 'closed', state_reason: 'completed' });
    console.log(`${action === 'close' ? 'closed' : 'commented on'} #${existing.number}`);
  } else {
    console.log('no open issue to close');
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === SCRIPT_PATH) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 2;
  });
}
