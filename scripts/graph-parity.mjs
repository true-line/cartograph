#!/usr/bin/env node
// Compares the pinned-repository ingest records of two or more platforms
// (#227, AC-1711): the same Cartograph commit ingesting the same pinned
// checkout must yield the same whole-graph content hash on each of them.
//
// Each record is the directory the ignored determinism test writes when
// CARTOGRAPH_DETERMINISM_RECORD is set: graph-hash.json (pinned commit,
// platform, graph hash, recorded hash, node and edge counts) and facts.tsv
// (one `identity key<TAB>sha256 of the fact` line per fact, in key order).
//
//   node scripts/graph-parity.mjs --record <label>=<directory> [...] \
//     --report <file> [--status-output <GITHUB_OUTPUT>]
//
// The status written is 0 when every record agrees with the first and 1
// otherwise; a missing or unreadable record counts as not agreeing, so a
// platform that produced nothing cannot pass.
import { appendFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT_PATH = fileURLToPath(import.meta.url);
const COMPARED = ['pinned_commit', 'graph_hash', 'recorded_hash', 'nodes', 'edges'];

/** `label=directory` as a record source. */
export function parseRecord(text) {
  const at = text.indexOf('=');
  if (at <= 0 || at === text.length - 1) throw new Error(`--record must be <label>=<directory>, got ${text}`);
  return { label: text.slice(0, at), directory: text.slice(at + 1) };
}

/** facts.tsv as [key, digest] pairs, in file order. */
export function parseFacts(text) {
  return text.split('\n').filter((line) => line !== '').map((line) => {
    const at = line.lastIndexOf('\t');
    return at < 0 ? [line, ''] : [line.slice(0, at), line.slice(at + 1)];
  });
}

// Rust sorts the keys as byte strings; compare the same way, not by UTF-16.
const order = (a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b));

/**
 * The first fact, in key order, on which two fact lists disagree: present in
 * only one, or present in both with a different digest. Null when identical.
 */
export function firstDifferingFact(first, second, [firstLabel, secondLabel] = ['the first', 'the second']) {
  let [i, j] = [0, 0];
  while (i < first.length || j < second.length) {
    if (j >= second.length) return `${first[i][0]} only on ${firstLabel}`;
    if (i >= first.length) return `${second[j][0]} only on ${secondLabel}`;
    const by = order(first[i][0], second[j][0]);
    if (by < 0) return `${first[i][0]} only on ${firstLabel}`;
    if (by > 0) return `${second[j][0]} only on ${secondLabel}`;
    if (first[i][1] !== second[j][1]) return `${first[i][0]} differs between ${firstLabel} and ${secondLabel}`;
    i += 1;
    j += 1;
  }
  return null;
}

/** One record read from its directory, or the reason it could not be. */
export function readRecord({ label, directory }, read = (file) => (existsSync(file) ? readFileSync(file, 'utf8') : null)) {
  const summary = read(path.join(directory, 'graph-hash.json'));
  if (summary === null) return { label, error: 'no graph-hash.json: the platform recorded nothing' };
  let fields;
  try {
    fields = JSON.parse(summary);
  } catch {
    return { label, error: 'graph-hash.json is not JSON' };
  }
  const missing = COMPARED.filter((field) => fields[field] === undefined || fields[field] === '');
  if (missing.length) return { label, error: `graph-hash.json lacks ${missing.join(', ')}` };
  const facts = read(path.join(directory, 'facts.tsv'));
  return { label, fields, facts: facts === null ? null : parseFacts(facts) };
}

/** The comparison as a report and whether every record agrees with the first. */
export function compare(records) {
  if (records.length < 2) throw new Error('at least two records are needed to compare');
  const lines = ['## Whole-graph hash across platforms', '', '| Record | Platform | Pinned commit | Nodes | Edges | Graph hash |', '|---|---|---|---:|---:|---|'];
  for (const record of records) {
    lines.push(record.error
      ? `| ${record.label} | ${record.error} | | | | |`
      : `| ${record.label} | ${record.fields.platform ?? ''} | \`${record.fields.pinned_commit}\` | ${record.fields.nodes} | ${record.fields.edges} | \`${record.fields.graph_hash}\` |`);
  }
  lines.push('');
  const problems = records.filter((record) => record.error).map((record) => `${record.label}: ${record.error}.`);
  const [base, ...others] = records;
  if (!base.error) {
    for (const other of others.filter((record) => !record.error)) {
      const fields = COMPARED.filter((field) => base.fields[field] !== other.fields[field]);
      if (fields.length === 0) continue;
      const labels = [base.label, other.label];
      if (fields.includes('pinned_commit')) {
        problems.push(`${labels.join(' and ')} ingested different commits, so their hashes cannot be compared.`);
        continue;
      }
      const fact = base.facts && other.facts ? firstDifferingFact(base.facts, other.facts, labels) : null;
      problems.push(`${labels.join(' and ')} differ in ${fields.join(', ')}. ${fact ? `First differing fact: ${fact}.` : 'The fact lists are equal or were not recorded.'}`);
    }
  }
  const passed = problems.length === 0;
  lines.push(...(passed ? [`Every platform yields graph hash \`${base.fields.graph_hash}\`.`] : problems));
  return { passed, text: `${lines.join('\n')}\n` };
}

function main() {
  const argv = process.argv.slice(2);
  const options = { records: [] };
  for (let i = 0; i < argv.length; i += 2) {
    const [flag, value] = [argv[i], argv[i + 1]];
    if (value === undefined) throw new Error(`${flag} needs a value`);
    if (flag === '--record') options.records.push(parseRecord(value));
    else if (flag === '--report') options.report = value;
    else if (flag === '--status-output') options.statusOutput = value;
    else throw new Error(`unknown argument: ${flag}`);
  }
  if (!options.report || options.records.length < 2) throw new Error('--report and at least two --record are required');
  const { passed, text } = compare(options.records.map((record) => readRecord(record)));
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
