import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  canonicalEndpoint,
  documentedEndpoints,
  documentedInputs,
  documentedOutputs,
  recoveredEndpoints,
  recoveredInputs,
  recoveredOutputs,
  score,
  scoreInputs,
  scoreOutputs,
} from './score-api-contract.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));

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

test('shared fixture produces the exact canonical endpoint set expected by Rust spec::contract', () => {
  const fixturePath = join(HERE, '..', 'crates', 'spec', 'tests', 'fixtures', 'api_contract_parity.md');
  const markdown = readFileSync(fixturePath, 'utf8');
  assert.deepEqual([...documentedEndpoints(markdown)].sort(), [
    'DELETE /',
    'GET /api/aliases/{}',
    'GET /api/edge/:/<>',
    'PATCH /api/profile/{}',
    'POST /api/aliases/{}/items/{}',
    'PUT /api/v2/settings',
  ]);
});

test('documented inputs skip headers, path parameters and prose bullets', () => {
  const markdown = [
    '#### POST /api/v3/alias/custom/new',
    'Input:',
    '- `Authentication` header that contains the api key',
    '- (Optional but recommended) `hostname` passed in query string',
    '- Request Message Body in json (`Content-Type` is `application/json`)',
    '    - alias_prefix: string.',
    '    - (Optional) note: alias note',
    '- `alias_id` in url.',
    '- Or the correct cookie is set',
    '- 401 if user is not authenticated',
    '- email',
    'Output:',
    '- name: not an input',
    '#### GET /api/x',
    'Input:',
    '- `page_id` in query.',
    '#### DELETE /api/mailboxes/:mailbox_id',
    'Input:',
    '- mailbox_id: in url',
    '- transfer_aliases_to: optional',
    '- Authorization: header bearer token',
    '- `X-Sudo`: Header for sudo mode',
  ].join('\n');
  const inputs = documentedInputs(markdown);
  assert.deepEqual([...inputs.get('POST /api/v3/alias/custom/new')].sort(), [
    'alias_prefix',
    'email',
    'hostname',
    'note',
  ]);
  assert.deepEqual([...inputs.get('GET /api/x')], ['page_id']);
  assert.deepEqual([...inputs.get('DELETE /api/mailboxes/{}')], ['transfer_aliases_to']);
});

test('input score compares names across locations and flags partial recoveries', () => {
  const documented = new Map([
    ['POST /api/auth/login', new Set(['email', 'password', 'device'])],
    ['GET /api/x', new Set(['page_id'])],
  ]);
  const recovered = recoveredInputs([
    {
      id: 'a',
      props: {
        method: 'POST',
        path: '/api/auth/login',
        request_inputs: { body: ['email', 'password'], query: ['otp'], partial: ['body_read_dynamically'] },
      },
    },
    { id: 'b', props: { method: 'GET', path: '/api/x', request_inputs: { query: ['page_id'] } } },
  ]);
  const result = scoreInputs(documented, recovered);
  assert.equal(result.documented_names, 4);
  assert.equal(result.matched_names, 3);
  assert.equal(result.recall, 0.75);
  assert.equal(result.precision, 0.75);
  assert.deepEqual(result.differences, [
    { endpoint: 'POST /api/auth/login', missed: ['device'], undocumented: ['otp'], partial: true },
  ]);
});

test('documented outputs read top-level key bullets and mentioned statuses', () => {
  const markdown = [
    '#### POST /api/auth/login',
    'Input:',
    '- email',
    'Output:',
    '- name: user name',
    '- `api_key`: the key',
    '    - nested: a field of api_key',
    'Return 403 if user has enabled FIDO.',
    '#### POST /api/auth/activate',
    'Output:',
    '- 200: account is activated',
    '- 400: wrong email or code',
    'Max 100 aliases.',
  ].join('\n');
  const outputs = documentedOutputs(markdown);
  const login = outputs.get('POST /api/auth/login');
  assert.deepEqual([...login.keys], ['name', 'api_key']);
  assert.deepEqual([...login.statuses], [403]);
  const activate = outputs.get('POST /api/auth/activate');
  assert.deepEqual([...activate.keys], []);
  assert.deepEqual([...activate.statuses], [200, 400]);
});

test('output score: key recall and precision, documented-status recall', () => {
  const documented = new Map([
    ['POST /api/auth/login', { keys: new Set(['name', 'api_key']), statuses: new Set([403]) }],
    ['POST /api/auth/activate', { keys: new Set(), statuses: new Set([200, 410]) }],
  ]);
  const recovered = recoveredOutputs([
    { props: { method: 'POST', path: '/api/auth/login', responses: { keys: ['api_key', 'error'], statuses: [200, 400] } } },
    { props: { method: 'POST', path: '/api/auth/activate', responses: { statuses: [200], partial: ['response_built_elsewhere'] } } },
  ]);
  const result = scoreOutputs(documented, recovered);
  assert.deepEqual(result.keys, {
    endpoints_with_documented_keys: 1,
    documented: 2,
    matched: 1,
    recall: 0.5,
    precision: 0.5,
  });
  assert.deepEqual(result.statuses, {
    endpoints_with_documented_statuses: 2,
    documented: 3,
    matched: 1,
    recall: 0.3333,
  });
  assert.deepEqual(result.differences, [
    { endpoint: 'POST /api/auth/activate', partial: true, missed_statuses: [410] },
    {
      endpoint: 'POST /api/auth/login',
      partial: false,
      missed_keys: ['name'],
      undocumented_keys: ['error'],
      missed_statuses: [403],
    },
  ]);
});
