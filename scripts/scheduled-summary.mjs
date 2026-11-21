#!/usr/bin/env node
// Summarizes the parts of a scheduled lane (#1770): each part's outcome and,
// for a failed part, the lines of its log that say what failed, so the
// tracking issue names the failing tests without opening the run.
//
//   node scripts/scheduled-summary.mjs --title <title> \
//     --step "<label>=<outcome>=<log file or empty>" [...] \
//     --report <file> [--status-output <GITHUB_OUTPUT>]
//
// The status written is 0 when every part succeeded and 1 otherwise; a part
// that was skipped because an earlier part failed counts as not passed.
import { appendFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT_PATH = fileURLToPath(import.meta.url);
const KEEP = 60;

/** `label=outcome=log` as a part. */
export function parseStep(text) {
  const [label, outcome, log = ''] = text.split('=');
  if (!label || outcome === undefined) throw new Error(`--step must be <label>=<outcome>=<log>, got ${text}`);
  return { label, outcome: outcome.trim() || 'skipped', log };
}

/**
 * The lines of a failed log worth quoting: libtest's failed test names and
 * panics, then the log's tail. At most KEEP lines.
 */
export function failureLines(text) {
  const lines = text.split('\n');
  const named = lines.filter((line) => /^test .+ \.\.\. FAILED$|panicked at|^error(\[E\d+\])?:|^---- .+ stdout ----$|shard .*(fail|expected)/u.test(line));
  const unique = [...new Set(named)].slice(0, KEEP / 2);
  const tail = lines.filter((line) => line.trim() !== '').slice(-(KEEP - unique.length));
  return unique.length ? [...unique, '…', ...tail.slice(-(KEEP / 2))] : tail;
}

export function summarize(title, steps, readLog = (file) => (file && existsSync(file) ? readFileSync(file, 'utf8') : '')) {
  const passed = steps.every((step) => step.outcome === 'success');
  const lines = [`## ${title}`, '', '| Part | Result |', '|---|---|'];
  for (const step of steps) lines.push(`| ${step.label} | ${step.outcome === 'success' ? 'passed' : step.outcome} |`);
  lines.push('');
  for (const step of steps.filter((part) => part.outcome === 'failure')) {
    const log = readLog(step.log);
    if (!log) continue;
    lines.push(`### ${step.label}`, '', '```text', ...failureLines(log), '```', '');
  }
  lines.push(passed ? 'Every part passed.' : 'At least one part did not pass.');
  return { passed, text: `${lines.join('\n')}\n` };
}

function main() {
  const argv = process.argv.slice(2);
  const options = { steps: [] };
  for (let i = 0; i < argv.length; i += 2) {
    const [flag, value] = [argv[i], argv[i + 1]];
    if (value === undefined) throw new Error(`${flag} needs a value`);
    if (flag === '--title') options.title = value;
    else if (flag === '--step') options.steps.push(parseStep(value));
    else if (flag === '--report') options.report = value;
    else if (flag === '--status-output') options.statusOutput = value;
    else throw new Error(`unknown argument: ${flag}`);
  }
  if (!options.title || !options.report || options.steps.length === 0) throw new Error('--title, --report and at least one --step are required');
  const { passed, text } = summarize(options.title, options.steps);
  writeFileSync(options.report, text);
  if (options.statusOutput) appendFileSync(options.statusOutput, `status=${passed ? 0 : 1}\n`);
  process.stdout.write(text);
}

if (process.argv[1] && path.resolve(process.argv[1]) === SCRIPT_PATH) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 2;
  }
}
