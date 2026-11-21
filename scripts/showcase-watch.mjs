#!/usr/bin/env node
// Scheduled showcase watch (#1768, AC-1700). Runs a pinned real-repository
// showcase with a release build and compares what it recovered with the
// thresholds recorded in bench/thresholds/<lane>.json, so a change that lowers
// recovery on real software is seen even though every per-PR gate uses
// fixtures.
//
//   node scripts/showcase-watch.mjs simplelogin --work <dir> --thresholds <json> [--results <dir>] [--report <md>] [--bin <app test binary>]
//   node scripts/showcase-watch.mjs scale       --work <dir> --thresholds <json> [--results <dir>] [--report <md>] [--only <name>]
//   node scripts/showcase-watch.mjs check       --metrics <json> --thresholds <json> [--report <md>]
//
// Each threshold names a metric, its recorded `baseline`, a `direction`
// (`min`: the value may not fall below the baseline; `max`: it may not rise
// above it) and a relative `tolerance` (0 for counts that must hold exactly,
// for example 1.5 for a wall time that may take up to 2.5x the baseline). A
// value past its limit, or a tracked metric the run did not measure, is a
// miss. A value better than its baseline passes and is reported, so the
// thresholds can be raised in a reviewed change. Metrics the file does not
// track are reported only.
//
// Exit status: 0 when every threshold holds, 1 on a miss, 2 when the run
// itself failed.
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { build, checkout, contained, parseTime, realPath } from './bench-ingest.mjs';
import { documentedEndpoints, documentedInputs, documentedOutputs, recoveredEndpoints, recoveredInputs, recoveredOutputs, score, scoreInputs, scoreOutputs } from './score-api-contract.mjs';

const SCRIPT_PATH = fileURLToPath(import.meta.url);
const ROOT = path.resolve(path.dirname(SCRIPT_PATH), '..');
const TEST = 'showcase_tests::showcase_recover_and_export';
// Old run directories are removed so the persistent work directory stays bounded.
const KEEP_RUNS = 2;

/** Validated thresholds file. */
export function parseThresholds(text) {
  const file = JSON.parse(text);
  if (!file.metrics || typeof file.metrics !== 'object') throw new Error('thresholds need a metrics object');
  for (const [name, rule] of Object.entries(file.metrics)) {
    if (typeof rule.baseline !== 'number' || !Number.isFinite(rule.baseline)) throw new Error(`${name}: baseline must be a number`);
    if (!['min', 'max'].includes(rule.direction)) throw new Error(`${name}: direction must be min or max`);
    if (typeof rule.tolerance !== 'number' || rule.tolerance < 0) throw new Error(`${name}: tolerance must be a number >= 0`);
  }
  return file;
}

/** The limit a metric's value may reach. */
export function limitOf({ baseline, direction, tolerance }) {
  const limit = direction === 'min' ? baseline * (1 - tolerance) : baseline * (1 + tolerance);
  return Number(limit.toFixed(6));
}

/**
 * Compare measured metrics with thresholds. Returns one row per tracked
 * metric (in file order) and per untracked measured metric.
 */
export function check(metrics, thresholds) {
  const rows = [];
  for (const [name, rule] of Object.entries(thresholds.metrics)) {
    const value = metrics[name];
    const limit = limitOf(rule);
    if (typeof value !== 'number' || !Number.isFinite(value)) {
      rows.push({ name, value: null, baseline: rule.baseline, limit, direction: rule.direction, status: 'missing' });
      continue;
    }
    const held = rule.direction === 'min' ? value >= limit : value <= limit;
    const better = rule.direction === 'min' ? value > rule.baseline : value < rule.baseline;
    rows.push({
      name, value, baseline: rule.baseline, limit, direction: rule.direction,
      status: held ? (better ? 'better' : 'held') : 'missed',
    });
  }
  for (const [name, value] of Object.entries(metrics)) {
    if (!(name in thresholds.metrics)) rows.push({ name, value, status: 'untracked' });
  }
  return { rows, passed: rows.every((row) => row.status !== 'missed' && row.status !== 'missing') };
}

const shown = (value) => (value === null || value === undefined ? '—' : Number.isInteger(value) ? value.toLocaleString('en-US') : String(Number(value.toFixed(3))));

/** A Markdown report of a check. */
export function report(lane, result, context = {}) {
  const lines = [`## ${lane} showcase against its recorded thresholds`, ''];
  if (context.measuredAt) lines.push(`Measured at Cartograph \`${context.measuredAt.commit}\` (${context.measuredAt.version}) on ${context.measuredAt.platform}.`, '');
  if (context.baselineAt) lines.push(`Thresholds recorded at Cartograph \`${context.baselineAt.commit}\` (${context.baselineAt.version}).`, '');
  const tracked = result.rows.filter((row) => row.status !== 'untracked');
  lines.push('| Metric | Value | Baseline | Limit | Result |', '|---|---:|---:|---:|---|');
  for (const row of tracked) {
    const bound = `${row.direction === 'min' ? '≥' : '≤'} ${shown(row.limit)}`;
    const verdict = { held: 'held', better: 'better than baseline', missed: '**missed**', missing: '**not measured**' }[row.status];
    lines.push(`| \`${row.name}\` | ${shown(row.value)} | ${shown(row.baseline)} | ${bound} | ${verdict} |`);
  }
  lines.push('');
  const untracked = result.rows.filter((row) => row.status === 'untracked');
  if (untracked.length) {
    lines.push('<details><summary>Measured but not tracked</summary>', '', '| Metric | Value |', '|---|---:|');
    for (const row of untracked) lines.push(`| \`${row.name}\` | ${shown(row.value)} |`);
    lines.push('', '</details>', '');
  }
  const missed = tracked.filter((row) => row.status === 'missed' || row.status === 'missing');
  if (missed.length) {
    lines.push(`**${missed.length} threshold(s) missed:** ${missed.map((row) => `\`${row.name}\``).join(', ')}. Find the change that moved them; a deliberate change records new thresholds in the same PR.`);
  } else {
    lines.push('Every threshold held.');
    if (tracked.some((row) => row.status === 'better')) lines.push('Some values are better than their baselines; raise the thresholds in a reviewed change.');
  }
  return `${lines.join('\n')}\n`;
}

// --- SimpleLogin -------------------------------------------------------------

/** The system manifest of the three SimpleLogin repositories (gap 1 in docs/showcases/simplelogin.md). */
export function simpleloginManifest(sources, env) {
  const lines = [];
  for (const source of sources) lines.push('[[repos]]', `url = "${source.name}"`);
  lines.push('', '[env]');
  for (const [key, value] of Object.entries(env)) lines.push(`${JSON.stringify(key)} = ${JSON.stringify(value)}`);
  lines.push('', '[[services]]', 'name = "simplelogin"', 'repo = "app"', 'base_urls = ["https://app.simplelogin.io"]', '');
  return lines.join('\n');
}

/** Gap findings from `gap_register.json`: the total, the class count and the findings per extractor. */
export function gapMetrics(register) {
  const metrics = { gap_findings: 0, gap_classes: register.classes.length };
  for (const entry of register.classes) {
    const count = Array.isArray(entry.members) ? entry.members.length : entry.instances;
    metrics.gap_findings += count;
    const key = `gap_findings.${entry.extractor}`;
    metrics[key] = (metrics[key] ?? 0) + count;
  }
  return metrics;
}

/** Traced flows in `flow_dossiers.md`, by the trace's own verdict. */
export function flowMetrics(dossiers) {
  const verdicts = [...dossiers.matchAll(/^## .+ — (Verified|Partial|Inferred)\b/gmu)].map((match) => match[1]);
  return {
    flows_traced: verdicts.length,
    flows_verified: verdicts.filter((verdict) => verdict === 'Verified').length,
    flows_partial: verdicts.filter((verdict) => verdict === 'Partial').length,
  };
}

/** Metrics of one SimpleLogin showcase output directory. */
export function simpleloginMetrics(output, { answerKey, timing }) {
  const read = (name) => JSON.parse(readFileSync(path.join(output, name), 'utf8'));
  const labels = read('labels.json');
  const nodes = read('endpoints.json');
  const markdown = readFileSync(answerKey, 'utf8');
  const recovered = recoveredEndpoints(nodes);
  const api = score(documentedEndpoints(markdown), recovered, '/api');
  const inputs = scoreInputs(new Map([...documentedInputs(markdown)].filter(([key]) => recovered.found.has(key))), recoveredInputs(nodes));
  const outputs = scoreOutputs(new Map([...documentedOutputs(markdown)].filter(([key]) => recovered.found.has(key))), recoveredOutputs(nodes));
  const fetches = read('fetches.json');
  const metrics = {
    endpoints: labels.Endpoint ?? 0,
    api_documented_matched: api.matched,
    api_recovered_in_scope: api.recovered_in_scope,
    api_inputs_matched: inputs.matched_names ?? null,
    api_statuses_matched: outputs.statuses?.matched ?? null,
    api_keys_matched: outputs.keys?.matched ?? null,
    client_calls_linked: fetches.filter((fetch) => fetch.reason === null || fetch.reason === undefined).length,
    client_calls_gap: fetches.filter((fetch) => fetch.reason !== null && fetch.reason !== undefined).length,
    business_rules: labels.BusinessRule ?? 0,
    data_entities: labels.DataEntity ?? 0,
    test_cases: labels.TestCase ?? 0,
    gap_nodes: labels.Gap ?? 0,
  };
  Object.assign(metrics, flowMetrics(readFileSync(path.join(output, 'verified-only', 'flow_dossiers.md'), 'utf8')));
  Object.assign(metrics, gapMetrics(read(path.join('verified-only', 'gap_register.json'))));
  if (timing) {
    metrics.wall_seconds = timing.wall_seconds;
    metrics.max_rss_bytes = timing.max_rss_bytes;
  }
  for (const [key, value] of Object.entries(metrics)) if (value === null || value === undefined) delete metrics[key];
  return metrics;
}

// --- runs --------------------------------------------------------------------

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: 'utf8', maxBuffer: 1 << 30, ...options });
  if (result.status !== 0) throw new Error(`${command} ${args.join(' ')} failed:\n${result.stderr || result.stdout}`.trimEnd());
  return result;
}

function cartograph() {
  const commit = run('git', ['rev-parse', 'HEAD'], { cwd: ROOT }).stdout.trim();
  const version = `v${JSON.parse(readFileSync(path.join(ROOT, 'package.json'), 'utf8')).version}`;
  return { commit, version, platform: `${process.platform}-${process.arch}` };
}

function prune(runs) {
  if (!existsSync(runs)) return;
  const old = readdirSync(runs).sort().slice(0, -KEEP_RUNS);
  for (const name of old) rmSync(path.join(runs, name), { recursive: true, force: true });
}

function workDir(work) {
  const resolved = path.resolve(work);
  if (contained(realpathSync(ROOT), realPath(resolved))) throw new Error('--work must be outside the Cartograph checkout');
  mkdirSync(resolved, { recursive: true });
  return resolved;
}

function runSimplelogin(options, thresholds) {
  const work = workDir(options.work);
  const lane = thresholds.lane;
  for (const source of lane.sources) checkout(source, work);
  const sources = path.join(work, 'sources');
  writeFileSync(path.join(sources, 'cartograph.system.toml'), simpleloginManifest(lane.sources, lane.env));
  const binary = options.bin ? path.resolve(options.bin) : build();
  const runs = path.join(work, 'runs');
  mkdirSync(runs, { recursive: true });
  prune(runs);
  const output = path.join(runs, new Date().toISOString().replace(/[:.]/g, '-'));
  const time = process.platform === 'darwin' ? ['-l'] : ['-v'];
  const timed = spawnSync('/usr/bin/time', [...time, binary, '--ignored', '--exact', TEST], {
    cwd: path.join(ROOT, 'src-tauri'), encoding: 'utf8', maxBuffer: 1 << 30,
    env: { ...process.env, CARTOGRAPH_SHOWCASE_MANIFEST: path.join(sources, 'cartograph.system.toml'), CARTOGRAPH_SHOWCASE_OUTPUT: output },
  });
  if (timed.status !== 0) throw new Error(`showcase failed\n${timed.stdout.slice(-4000)}\n${timed.stderr.slice(-4000)}`.trimEnd());
  const metrics = simpleloginMetrics(output, {
    answerKey: path.join(sources, 'app', 'docs', 'api.md'),
    timing: parseTime(timed.stderr, process.platform),
  });
  return { metrics, output, files: ['ingest.json', 'labels.json', 'fetches.json', 'knowledge.json', 'verified-only/gap_register.md'] };
}

/** Per-repository metrics of a bench-ingest result. */
export function scaleMetrics(result) {
  const metrics = {};
  for (const repo of result.repos) {
    const labels = repo.counts.labels ?? {};
    Object.assign(metrics, {
      [`${repo.name}.files`]: repo.counts.files,
      [`${repo.name}.nodes`]: repo.counts.nodes,
      [`${repo.name}.edges`]: repo.counts.edges,
      [`${repo.name}.endpoints`]: labels.Endpoint ?? 0,
      [`${repo.name}.business_rules`]: labels.BusinessRule ?? 0,
      [`${repo.name}.gap_nodes`]: labels.Gap ?? 0,
      [`${repo.name}.wall_seconds`]: repo.measurements.wall_seconds,
      [`${repo.name}.max_rss_bytes`]: repo.measurements.max_rss_bytes,
    });
  }
  for (const [key, value] of Object.entries(metrics)) if (value === null || value === undefined) delete metrics[key];
  return metrics;
}

function runScale(options) {
  const work = workDir(options.work);
  prune(path.join(work, 'runs'));
  const args = [path.join(ROOT, 'scripts', 'bench-ingest.mjs'), '--work', work];
  if (options.only) args.push('--only', options.only);
  const out = run(process.execPath, args, { cwd: ROOT, stdio: ['ignore', 'pipe', 'pipe'] });
  const file = out.stdout.trim().split('\n').at(-1);
  const result = JSON.parse(readFileSync(file, 'utf8'));
  return { metrics: scaleMetrics(result), raw: result };
}

function parseArguments(argv) {
  const [mode, ...rest] = argv;
  const options = { mode };
  for (let i = 0; i < rest.length; i += 2) {
    const [flag, value] = [rest[i], rest[i + 1]];
    if (!['--work', '--thresholds', '--results', '--report', '--bin', '--only', '--metrics'].includes(flag) || !value) {
      throw new Error(`unknown argument: ${flag}`);
    }
    options[flag.slice(2)] = value;
  }
  if (!['simplelogin', 'scale', 'check'].includes(mode)) throw new Error('mode must be simplelogin, scale or check');
  if (!options.thresholds) throw new Error('--thresholds is required');
  if (mode !== 'check' && !options.work) throw new Error('--work is required');
  if (mode === 'check' && !options.metrics) throw new Error('--metrics is required');
  return options;
}

function main() {
  const options = parseArguments(process.argv.slice(2));
  const thresholds = parseThresholds(readFileSync(options.thresholds, 'utf8'));
  let measured;
  try {
    if (options.mode === 'check') measured = { metrics: JSON.parse(readFileSync(options.metrics, 'utf8')) };
    else measured = options.mode === 'simplelogin' ? runSimplelogin(options, thresholds) : runScale(options);
  } catch (error) {
    const text = `## ${options.mode} showcase did not run\n\n\`\`\`text\n${String(error.message).slice(-6000)}\n\`\`\`\n`;
    if (options.report) writeFileSync(options.report, text);
    process.stdout.write(text);
    process.exitCode = 2;
    return;
  }
  const measuredAt = options.mode === 'check' ? null : cartograph();
  const result = check(measured.metrics, thresholds);
  const text = report(options.mode, result, { measuredAt, baselineAt: thresholds.measured_at });
  if (options.report) writeFileSync(options.report, text);
  if (options.results) {
    mkdirSync(options.results, { recursive: true });
    writeFileSync(path.join(options.results, 'metrics.json'), `${JSON.stringify({ measured_at: measuredAt, metrics: measured.metrics }, null, 2)}\n`);
    writeFileSync(path.join(options.results, 'report.md'), text);
    if (measured.raw) writeFileSync(path.join(options.results, 'bench.json'), `${JSON.stringify(measured.raw, null, 2)}\n`);
    for (const name of measured.files ?? []) {
      const file = path.join(measured.output, name);
      if (existsSync(file)) writeFileSync(path.join(options.results, path.basename(name)), readFileSync(file));
    }
  }
  process.stdout.write(text);
  process.exitCode = result.passed ? 0 : 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === SCRIPT_PATH) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 2;
  }
}
