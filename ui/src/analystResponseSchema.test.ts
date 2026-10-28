// AC-0445: actual compiled assets; schema shape is not host/source authority.
import Ajv from 'ajv';
import { expect, it } from 'vitest';
import choice from '../../crates/agents/src/investigation/data/analyst-v13-choice.json';
import finish from '../../crates/agents/src/investigation/data/analyst-v13-finish-only.json';

const validators = [choice, finish].map((schema) => new Ajv().compile(schema));
const finding = { claim_kind: 'inferred_interpretation', title: 'Title', statement: 'Statement', citation_ids: ['evidence-1'], limitations: ['Limited.'] };
const supported = { schema_version: 1, outcome: 'supported', action: { type: 'finish', findings: [finding], knowledge_completeness: 'partial', limitations: ['Limited.'] } };
const insufficient = { schema_version: 1, outcome: 'insufficient_evidence', action: { type: 'finish', findings: [], knowledge_completeness: 'insufficient_evidence', limitations: ['Missing.'] } };
const query = { type: 'query_context', context_id: 'context-v2:' + 'a'.repeat(64), query: { scope: { type: 'all' }, kind: null, labels: [] as string[], max_facts: 12, max_bytes: 16384, cursor: null } };
it('retains every read family and couples the two wrapper outcomes', () => {
  for (const value of [query, { type: 'read_coverage_inventory', context_id: query.context_id, repo: 'repo', inventory_hash: 'a'.repeat(64), max_bytes: 1024 },
    { type: 'replay_query', context_id: query.context_id, query_index: 0, query_id: 'a'.repeat(64) },
    { type: 'read_evidence', fact: { kind: 'node', id: 'node:test' }, role: 'provenance', index: 0 },
    { type: 'read_evidence', fact: { kind: 'edge', source: 'n1', label: 'calls', destination: 'n2' }, role: 'definition_initializer', index: 0 }]) {
    expect(validators.map((validate) => validate(value))).toEqual([true, false]);
  }
  for (const value of [supported, insufficient]) expect(validators.map((validate) => validate(value))).toEqual([true, true]);
  for (const value of [supported.action, { ...supported, outcome: 'insufficient_evidence' }, { ...insufficient, outcome: 'supported' }, { ...supported, narrative: null }])
    expect(validators.map((validate) => validate(value))).toEqual([false, false]);
});
it('pins existing unsigned and cardinality shape boundaries', () => {
  for (const [key, maximum] of [['max_facts', 32], ['max_bytes', 32768]] as const) {
    for (const n of [0, 1, maximum, maximum + 1]) expect(validators[0]({ ...query, query: { ...query.query, [key]: n } })).toBe(n >= 1 && n <= maximum);
  }
  for (const hops of [0, 1, 3, 4]) expect(validators[0]({ ...query, query: { ...query.query, scope: { type: 'neighborhood', anchor: 'n', hops } } })).toBe(hops >= 1 && hops <= 3);
  for (const offset of [-1, 0, 50000, 50001]) expect(validators[0]({ ...query, query: { ...query.query, cursor: { snapshot_id: 's', selection_id: 's', offset } } })).toBe(offset >= 0 && offset <= 50000);
  for (const index of [-1, 0, 1023, 1024]) expect(validators[0]({ type: 'read_evidence', fact: { kind: 'node', id: 'n' }, role: 'provenance', index })).toBe(index >= 0 && index <= 1023);
  for (const n of [16, 17]) expect(validators[0]({ ...query, query: { ...query.query, labels: Array(n).fill('label') } })).toBe(n <= 16);
  for (const n of [12, 13]) expect(validators[1]({ ...supported, action: { ...supported.action, findings: Array(n).fill(finding) } })).toBe(n <= 12);
  for (const n of [8, 9]) expect(validators[1]({ ...supported, action: { ...supported.action, findings: [{ ...finding, citation_ids: Array.from({ length: n }, (_, i) => `evidence-${i}`) }] } })).toBe(n <= 8);
});
