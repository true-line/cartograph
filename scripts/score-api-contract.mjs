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

// One documented input: an optional "(Optional…)" marker, then a bare or
// backticked name ending the bullet, before ":" or before "in"/"passed"/"used"
// prose. Headers and URL path parameters are not request inputs.
const INPUT = /^\s*-\s+(?:\([^)]*\)\s*)?`?([A-Za-z_][A-Za-z0-9_]*)`?(?:\s*$|\s*:(.*)|\s+(?:in|passed|used)\b(.*))/u;

/** Documented input names per canonical endpoint, from each heading's
 *  "Input:" bullets (nested ones included) up to "Output:" or a heading. */
export function documentedInputs(markdown) {
  const inputs = new Map();
  let key = null;
  let pathParams = new Set();
  let inInput = false;
  for (const line of markdown.split('\n')) {
    const heading = line.match(/^#+\s+([A-Z]+)\s+(\/\S*)/u);
    if (/^#/u.test(line)) {
      key = heading && METHODS.has(heading[1]) ? canonicalEndpoint(heading[1], heading[2]) : null;
      // Path parameters are part of the route path, not request inputs.
      pathParams = new Set(
        [...(heading?.[2] ?? '').matchAll(/[:{<](?:\w+:)?([A-Za-z_]\w*)/gu)].map((m) => m[1]),
      );
      inInput = false;
      continue;
    }
    if (/^Input:/u.test(line)) {
      inInput = key !== null;
      continue;
    }
    if (/^Output:/u.test(line)) inInput = false;
    if (!inInput) continue;
    const match = line.match(INPUT);
    if (!match || /\b(header|url)\b/iu.test(match[2] ?? match[3] ?? '') || pathParams.has(match[1])) continue;
    if (!inputs.has(key)) inputs.set(key, new Set());
    inputs.get(key).add(match[1]);
  }
  return inputs;
}

/** Recovered input names per canonical endpoint (query, body and form
 *  together), and the endpoints whose recovery is marked partial. */
export function recoveredInputs(nodes) {
  const inputs = new Map();
  const partial = new Set();
  for (const node of nodes) {
    const { method, path, request_inputs: recovered } = node.props ?? {};
    if (typeof method !== 'string' || typeof path !== 'string' || !recovered) continue;
    const key = canonicalEndpoint(method, path);
    const names = inputs.get(key) ?? new Set();
    for (const where of ['query', 'body', 'form']) for (const name of recovered[where] ?? []) names.add(name);
    inputs.set(key, names);
    if ((recovered.partial ?? []).length > 0) partial.add(key);
  }
  return { inputs, partial };
}

// Over endpoints matched by method and path that document at least one input:
// name recall over documented names, precision over recovered names.
export function scoreInputs(documented, recovered) {
  let documentedNames = 0;
  let recoveredNames = 0;
  let matchedNames = 0;
  const endpoints = [];
  for (const [key, names] of [...documented].sort(([a], [b]) => a.localeCompare(b))) {
    const found = recovered.inputs.get(key) ?? new Set();
    const missed = [...names].filter((name) => !found.has(name)).sort();
    const extra = [...found].filter((name) => !names.has(name)).sort();
    documentedNames += names.size;
    recoveredNames += found.size;
    matchedNames += names.size - missed.length;
    if (missed.length || extra.length)
      endpoints.push({ endpoint: key, missed, undocumented: extra, partial: recovered.partial.has(key) });
  }
  const ratio = (a, b) => (b === 0 ? null : Number((a / b).toFixed(4)));
  return {
    endpoints_with_documented_inputs: documented.size,
    documented_names: documentedNames,
    matched_names: matchedNames,
    recall: ratio(matchedNames, documentedNames),
    precision: ratio(matchedNames, recoveredNames),
    differences: endpoints,
  };
}

// Documented statuses: "- 200: …" bullets, "Output: 200 means …", and
// "return(s) 403" / "403 if|when|means" prose.
const STATUS = [
  /^\s*-\s+`?([1-5]\d\d)\b/u,
  /^Output:\s*`?([1-5]\d\d)\b/u,
  /\breturns?(?:ed)?\s+`?([1-5]\d\d)\b/giu,
  /\b([1-5]\d\d)`?\s+(?:if|when|means|is returned)\b/giu,
];

/** Documented top-level response keys and statuses per canonical endpoint,
 *  from each heading's "Output:" section up to the next heading. Only
 *  unindented bullets name top-level keys; nested ones describe their fields. */
export function documentedOutputs(markdown) {
  const outputs = new Map();
  let key = null;
  let inOutput = false;
  for (const line of markdown.split('\n')) {
    const heading = line.match(/^#+\s+([A-Z]+)\s+(\/\S*)/u);
    if (/^#/u.test(line)) {
      key = heading && METHODS.has(heading[1]) ? canonicalEndpoint(heading[1], heading[2]) : null;
      inOutput = false;
      continue;
    }
    if (/^Output:/u.test(line)) inOutput = key !== null;
    if (!inOutput) continue;
    const entry = outputs.get(key) ?? { keys: new Set(), statuses: new Set() };
    outputs.set(key, entry);
    for (const pattern of STATUS) {
      for (const match of pattern.global ? line.matchAll(pattern) : [line.match(pattern)].filter(Boolean))
        entry.statuses.add(Number(match[1]));
    }
    const bullet = /^-\s/u.test(line) && line.match(INPUT);
    if (bullet) entry.keys.add(bullet[1]);
  }
  return outputs;
}

/** Recovered response keys and statuses per canonical endpoint, and the
 *  endpoints whose recovery is marked partial. */
export function recoveredOutputs(nodes) {
  const outputs = new Map();
  const partial = new Set();
  for (const node of nodes) {
    const { method, path, responses } = node.props ?? {};
    if (typeof method !== 'string' || typeof path !== 'string' || !responses) continue;
    const key = canonicalEndpoint(method, path);
    const entry = outputs.get(key) ?? { keys: new Set(), statuses: new Set() };
    for (const name of responses.keys ?? []) entry.keys.add(name);
    for (const status of responses.statuses ?? []) entry.statuses.add(status);
    outputs.set(key, entry);
    if ((responses.partial ?? []).length > 0) partial.add(key);
  }
  return { outputs, partial };
}

// Keys: recall over documented top-level keys, precision over recovered keys,
// both on endpoints that document keys. Statuses: recall only, because docs
// rarely list every error status a handler returns.
export function scoreOutputs(documented, recovered) {
  const ratio = (a, b) => (b === 0 ? null : Number((a / b).toFixed(4)));
  const totals = { keyEndpoints: 0, keys: 0, recoveredKeys: 0, matchedKeys: 0, statusEndpoints: 0, statuses: 0, matchedStatuses: 0 };
  const differences = [];
  for (const [key, doc] of [...documented].sort(([a], [b]) => a.localeCompare(b))) {
    const found = recovered.outputs.get(key) ?? { keys: new Set(), statuses: new Set() };
    const difference = { endpoint: key, partial: recovered.partial.has(key) };
    if (doc.keys.size > 0) {
      totals.keyEndpoints += 1;
      totals.keys += doc.keys.size;
      totals.recoveredKeys += found.keys.size;
      const missed = [...doc.keys].filter((name) => !found.keys.has(name)).sort();
      totals.matchedKeys += doc.keys.size - missed.length;
      const extra = [...found.keys].filter((name) => !doc.keys.has(name)).sort();
      if (missed.length) difference.missed_keys = missed;
      if (extra.length) difference.undocumented_keys = extra;
    }
    if (doc.statuses.size > 0) {
      totals.statusEndpoints += 1;
      totals.statuses += doc.statuses.size;
      const missed = [...doc.statuses].filter((status) => !found.statuses.has(status)).sort();
      totals.matchedStatuses += doc.statuses.size - missed.length;
      if (missed.length) difference.missed_statuses = missed;
    }
    if (Object.keys(difference).length > 2) differences.push(difference);
  }
  return {
    keys: {
      endpoints_with_documented_keys: totals.keyEndpoints,
      documented: totals.keys,
      matched: totals.matchedKeys,
      recall: ratio(totals.matchedKeys, totals.keys),
      precision: ratio(totals.matchedKeys, totals.recoveredKeys),
    },
    statuses: {
      endpoints_with_documented_statuses: totals.statusEndpoints,
      documented: totals.statuses,
      matched: totals.matchedStatuses,
      recall: ratio(totals.matchedStatuses, totals.statuses),
    },
    differences,
  };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [answerKey, endpointsJson, prefix = ''] = process.argv.slice(2);
  if (!answerKey || !endpointsJson) {
    console.error('usage: score-api-contract.mjs <api.md> <endpoints.json> [path-prefix]');
    process.exit(2);
  }
  const markdown = readFileSync(answerKey, 'utf8');
  const nodes = JSON.parse(readFileSync(endpointsJson, 'utf8'));
  const documented = documentedEndpoints(markdown);
  const recovered = recoveredEndpoints(nodes);
  const result = score(documented, recovered, prefix);
  // Inputs are scored only on endpoints recovered by method and path.
  const inputs = new Map(
    [...documentedInputs(markdown)].filter(([key]) => recovered.found.has(key)),
  );
  result.inputs = scoreInputs(inputs, recoveredInputs(nodes));
  const outputs = new Map(
    [...documentedOutputs(markdown)].filter(([key]) => recovered.found.has(key)),
  );
  result.outputs = scoreOutputs(outputs, recoveredOutputs(nodes));
  console.log(JSON.stringify(result, null, 2));
}
