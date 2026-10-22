#!/usr/bin/env node
// Score a recovered HTTP endpoint inventory against a Markdown API reference
// that lists endpoints as headings such as "#### GET /api/aliases/:alias_id"
// (#721). Deterministic: no network, no model. Path parameters are compared by
// position only, because their names differ between docs and frameworks.
import { readFileSync } from 'node:fs';

const METHODS = new Set(['GET', 'POST', 'PUT', 'PATCH', 'DELETE']);

/** Canonical "METHOD /path" with every parameter segment reduced to {}. */
export function canonicalEndpoint(method, path) {
  const segments = path
    .trim()
    .replace(/\/+$/u, '')
    .split('/')
    .map((segment) => (/^(:.+|<.+>|\{.+\})$/u.test(segment) ? '{}' : segment));
  return `${method.toUpperCase()} ${segments.join('/') || '/'}`;
}

export function documentedEndpoints(markdown) {
  const found = new Set();
  for (const line of markdown.split('\n')) {
    const match = line.match(/^#+\s+([A-Z]+)\s+(\/\S*)/u);
    if (match && METHODS.has(match[1])) found.add(canonicalEndpoint(match[1], match[2]));
  }
  return found;
}

/** Endpoint nodes as written by the showcase harness: `{id, props}`, where
 *  props carry `method` and `path`. Nodes without both are reported, not guessed. */
export function recoveredEndpoints(nodes) {
  const found = new Map();
  const unusable = [];
  for (const node of nodes) {
    const { method, path } = node.props ?? {};
    if (typeof method !== 'string' || typeof path !== 'string') {
      unusable.push(node.id);
      continue;
    }
    const key = canonicalEndpoint(method, path);
    found.set(key, [...(found.get(key) ?? []), node.id]);
  }
  return { found, unusable };
}

// Recall is over every documented endpoint; precision only over recoveries
// under `prefix`, so its numerator counts scoped matches alone.
export function score(documented, recovered, prefix = '') {
  const inScope = (key) => key.split(' ')[1].startsWith(prefix);
  const scoped = [...recovered.found.keys()].filter(inScope);
  const matched = [...documented].filter((key) => recovered.found.has(key)).sort();
  const scopedMatched = matched.filter(inScope);
  const missed = [...documented].filter((key) => !recovered.found.has(key)).sort();
  const undocumented = scoped.filter((key) => !documented.has(key)).sort();
  const ratio = (a, b) => (b === 0 ? null : Number((a / b).toFixed(4)));
  return {
    documented: documented.size,
    recovered_in_scope: scoped.length,
    matched: matched.length,
    recall: ratio(matched.length, documented.size),
    documented_precision: ratio(scopedMatched.length, scoped.length),
    missed,
    undocumented,
    unusable_nodes: recovered.unusable.length,
  };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [answerKey, endpointsJson, prefix = ''] = process.argv.slice(2);
  if (!answerKey || !endpointsJson) {
    console.error('usage: score-api-contract.mjs <api.md> <endpoints.json> [path-prefix]');
    process.exit(2);
  }
  const documented = documentedEndpoints(readFileSync(answerKey, 'utf8'));
  const recovered = recoveredEndpoints(JSON.parse(readFileSync(endpointsJson, 'utf8')));
  console.log(JSON.stringify(score(documented, recovered, prefix), null, 2));
}
