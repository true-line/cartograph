import { test } from 'node:test';
import assert from 'node:assert/strict';
import { canonicalEndpoint, documentedEndpoints, recoveredEndpoints, score } from './score-api-contract.mjs';

test('parameter segments compare by position across doc and framework syntax', () => {
  assert.equal(canonicalEndpoint('get', '/api/aliases/:alias_id/'), 'GET /api/aliases/{}');
  assert.equal(canonicalEndpoint('GET', '/api/aliases/<int:alias_id>'), 'GET /api/aliases/{}');
  assert.equal(canonicalEndpoint('GET', '/api/aliases/{id}'), 'GET /api/aliases/{}');
});

test('scores recall, documented precision, misses and undocumented routes', () => {
  const documented = documentedEndpoints(
    '# API\n#### POST /api/auth/login\ntext\n#### GET /api/aliases/:alias_id\n### Not an endpoint\n#### FETCH /x\n',
  );
  assert.deepEqual([...documented].sort(), ['GET /api/aliases/{}', 'POST /api/auth/login']);
  const recovered = recoveredEndpoints([
    { id: 'a', props: { method: 'POST', path: '/api/auth/login' } },
    { id: 'b', props: { method: 'GET', path: '/api/internal/<x>' } },
    { id: 'c', props: { method: 'GET', path: '/dashboard' } },
    { id: 'd', props: {} },
  ]);
  const result = score(documented, recovered, '/api');
  assert.equal(result.matched, 1);
  assert.equal(result.recall, 0.5);
  assert.equal(result.recovered_in_scope, 2);
  assert.equal(result.documented_precision, 0.5);
  assert.deepEqual(result.missed, ['GET /api/aliases/{}']);
  assert.deepEqual(result.undocumented, ['GET /api/internal/{}']);
  assert.equal(result.unusable_nodes, 1);
});

test('precision counts only matches inside the requested scope', () => {
  // #741 review: a documented, recovered /health must not lift /api precision.
  const documented = documentedEndpoints('#### GET /api/x\n#### GET /health\n');
  const recovered = recoveredEndpoints([
    { id: 'a', props: { method: 'GET', path: '/api/x' } },
    { id: 'b', props: { method: 'GET', path: '/health' } },
  ]);
  const result = score(documented, recovered, '/api');
  assert.equal(result.matched, 2);
  assert.equal(result.recall, 1);
  assert.equal(result.recovered_in_scope, 1);
  assert.equal(result.documented_precision, 1);
});
