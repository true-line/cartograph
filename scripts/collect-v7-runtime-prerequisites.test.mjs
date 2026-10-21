// AC-0279: all transports/processes are scripted; no HTTP, Ollama or Cargo calls.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, writeFile, readdir, symlink, rename, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { collect, endpointOrigin, getLocal, LIMITS, normalizeDigest, parseArguments, preparationCommand } from './collect-v7-runtime-prerequisites.mjs';
const digest = 'a'.repeat(64);
async function fixture(t) {
  const root = await mkdtemp(path.join(os.tmpdir(), 'cartograph-673-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  for (const name of ['checkout', 'capture', 'visible', 'external']) await mkdir(path.join(root, name));
  const options = { checkout: path.join(root, 'checkout'), endpoint: 'http://127.0.0.1:11434/', model: 'installed:8b', 'expected-runtime-version': 'operator-expected', 'expected-model-digest': digest, 'target-host': 'mac14x13-m2max-32g-60f1', 'observation-path': 'operator-established-forward', 'confirm-target-path': 'operator-asserted', 'output-dir': path.join(root, 'observation-1'), 'run-dir': path.join(root, 'run-1'), captureRoots: [path.join(root, 'capture')], modelVisibleRoots: [path.join(root, 'visible')], prepareOnly: false };
  return { root, options };
}
function transport(overrides = {}) {
  const calls = [];
  const fn = async (url, bounds) => {
    calls.push({ url, bounds }); const route = new URL(url).pathname;
    return { status: 200, bytes: Buffer.from(JSON.stringify(route === '/api/version' ? { version: 'operator-expected' } : { models: [{ name: 'installed:8b', digest: `sha256:${digest}` }] })), ...overrides[route] };
  };
  return { fn, calls };
}
const json = async (file) => JSON.parse(await readFile(file, 'utf8'));

test('collector observes only two bounded GET identities and writes the exact runtime contract', async (t) => {
  const { options } = await fixture(t); const wire = transport(); let prepared = false;
  const record = await collect(options, { transport: wire.fn, prepare: async () => { prepared = true; throw Error(); }, now: () => '2026-10-21T00:00:00.000Z' });
  assert.equal(prepared, false); assert.deepEqual(wire.calls.map((v) => new URL(v.url).pathname), ['/api/version', '/api/tags']);
  assert.ok(wire.calls.every((v) => v.bounds.timeoutMs === 5000 && v.bounds.maxBytes === LIMITS.response));
  assert.deepEqual(await json(path.join(options['output-dir'], 'runtime-receipt.json')), { schema_version: 1, endpoint: 'http://127.0.0.1:11434/', model: 'installed:8b', model_digest: digest, runtime_version: 'operator-expected', temperature: null, seed: null, context_length: null, verified_read_only: true });
  assert.equal(record.target.provenance, 'operator_asserted_not_attested'); assert.equal(record.frozen_definition_sha256, null); assert.equal(record.delivery_pass, false);
  assert.deepEqual(record.observations.map((v) => v.sha256.length), [64, 64]); assert.equal(record.sampling_status, 'unset_or_unverified_not_observed');
  assert.deepEqual((await readdir(options['output-dir'])).sort(), ['attempt.json', 'observation.json', 'runtime-receipt.json']);
});

test('collector rejects credentialed nonloopback or redirected endpoints before identity is trusted', async (t) => {
  for (const endpoint of ['http://user:secret@127.0.0.1/', 'http://127.0.0.1/?token=secret', 'http://127.0.0.1/#secret', 'http://127.0.0.1/?', 'https://127.0.0.1/', 'http://studio.local/', 'http://192.168.1.2/', 'http://127.0.0.1/api/generate']) {
    assert.throws(() => endpointOrigin(endpoint), /invalid_endpoint/u);
  }
  assert.equal(endpointOrigin('http://[::1]:11434/'), 'http://[::1]:11434');
  assert.throws(() => getLocal('http://127.0.0.1/api/generate'), /invalid_route/u);
  assert.throws(() => getLocal('http://127.0.0.1/api/version', { maxBytes: LIMITS.response + 1 }), /invalid_transport_bounds/u);
  assert.throws(() => getLocal('http://127.0.0.1/api/version', { timeoutMs: 0 }), /invalid_transport_bounds/u);
  assert.throws(() => getLocal('http://user:secret@127.0.0.1/api/version'), /invalid_route/u);
  assert.throws(() => getLocal('http://127.0.0.1/api/tags?token=secret'), /invalid_route/u);
  const { options } = await fixture(t); const wire = transport({ '/api/version': { status: 302 } });
  await assert.rejects(collect(options, { transport: wire.fn }), /artifacts_retained/u);
  assert.equal(wire.calls.length, 1); assert.equal((await json(path.join(options['output-dir'], 'failure.json'))).status, 'failed');
});

test('collector requires explicit Studio alias and path assertion without host attestation', async (t) => {
  const { options } = await fixture(t); let calls = 0;
  for (const patch of [{ 'target-host': 'another-host' }, { 'confirm-target-path': undefined }, { 'observation-path': 'create-tunnel' }]) await assert.rejects(collect({ ...options, ...patch }, { transport: async () => { calls++; } }));
  assert.equal(calls, 0);
  for (const [i, host] of ['mac14x13-m2max-32g-60f1.local', 'mac14x13-m2max-32g-60f1.lan'].entries()) {
    const result = await collect({ ...options, 'target-host': host, 'output-dir': `${options['output-dir']}-${i}` }, { transport: transport().fn });
    assert.equal(result.target.host, 'mac14x13-m2max-32g-60f1'); assert.equal(result.target.provenance, 'operator_asserted_not_attested');
  }
});

test('collector rejects malformed oversized timed out and ambiguous identities preserving failures', async (t) => {
  const { options } = await fixture(t);
  const variants = [
    { '/api/version': { bytes: Buffer.from('{"version":"operator-expected","version":"other"}') } },
    { '/api/version': { bytes: Buffer.from([0xff]) } },
    { '/api/version': { bytes: Buffer.alloc(LIMITS.response + 1) } },
    { '/api/version': { bytes: Buffer.from('{"version":"wrong"}') } },
    { '/api/tags': { bytes: Buffer.from(JSON.stringify({ models: [{ name: options.model, digest }, { name: options.model, digest }] })) } },
    { '/api/tags': { bytes: Buffer.from(JSON.stringify({ models: [{ name: options.model, digest: 'b'.repeat(64) }] })) } },
    { '/api/tags': { bytes: Buffer.from(JSON.stringify({ models: [] })) } },
  ];
  for (const [i, variant] of variants.entries()) {
    const current = { ...options, 'output-dir': `${options['output-dir']}-${i}` };
    await assert.rejects(collect(current, { transport: transport(variant).fn }), /artifacts_retained/u);
    assert.ok((await readdir(current['output-dir'])).includes('failure.json'));
    assert.ok(!(await readdir(current['output-dir'])).includes('runtime-receipt.json'));
  }
  await assert.rejects(collect({ ...options, 'output-dir': `${options['output-dir']}-timeout` }, { timeoutMs: 5, transport: () => new Promise(() => {}) }), /artifacts_retained/u);
  assert.equal(normalizeDigest(`sha256:${'A'.repeat(64)}`), digest);
});

test('collector canonical exclusions reject symlink aliases and never overwrite output', async (t) => {
  const { root, options } = await fixture(t); const wire = transport();
  await symlink(options.captureRoots[0], path.join(root, 'capture-alias'), 'dir');
  for (const output of [path.join(root, 'capture-alias', 'new'), path.join(options['run-dir'], 'new'), path.join(options.checkout, 'new'), options.modelVisibleRoots[0]]) await assert.rejects(collect({ ...options, 'output-dir': output }, { transport: wire.fn }));
  assert.equal(wire.calls.length, 0);
  await assert.rejects(collect({ ...options, captureRoots: [path.parse(root).root] }, { transport: wire.fn }));
  assert.equal(wire.calls.length, 0);
  await collect(options, { transport: wire.fn }); const before = await readFile(path.join(options['output-dir'], 'observation.json'));
  await assert.rejects(collect(options, { transport: wire.fn }));
  assert.deepEqual(await readFile(path.join(options['output-dir'], 'observation.json')), before); assert.equal(wire.calls.length, 2);
});

test('collector prepare-only handoff scrubs execution env and preserves failed preparation', async (t) => {
  const { root, options } = await fixture(t); const rubric = path.join(root, 'external', 'rubric.json'); await writeFile(rubric, '{"withheld":"oracle-secret"}');
  const current = { ...options, prepareOnly: true, rubric };
  const command = preparationCommand(current, '/external/runtime.json', { PATH: '/bin', CARTOGRAPH_V7_STAGE: 'auditor', CARTOGRAPH_V7_OPERATOR_OPT_IN: 'auditor', CARTOGRAPH_V7_SEMANTIC_ATTESTATION: '/secret', CARTOGRAPH_ACCEPTANCE_MODEL: 'other', NODE_OPTIONS: '--require evil' });
  assert.deepEqual(command.args, ['test', '-p', 'app', 'investigation_v7_completion_control_explicit_stage', '--', '--ignored', '--nocapture']);
  assert.equal(command.env.CARTOGRAPH_V7_STAGE, 'prepare'); assert.ok(!Object.hasOwn(command.env, 'CARTOGRAPH_V7_OPERATOR_OPT_IN')); assert.ok(!Object.hasOwn(command.env, 'CARTOGRAPH_V7_SEMANTIC_ATTESTATION')); assert.ok(!Object.hasOwn(command.env, 'CARTOGRAPH_ACCEPTANCE_MODEL')); assert.ok(!Object.hasOwn(command.env, 'NODE_OPTIONS'));
  let invoked = 0;
  await assert.rejects(collect(current, { transport: transport().fn, prepare: async (received) => { invoked++; assert.equal(received.env.CARTOGRAPH_V7_STAGE, 'prepare'); await mkdir(options['run-dir']); await writeFile(path.join(options['run-dir'], 'partial'), 'keep'); return { code: 1 }; } }), /artifacts_retained/u);
  assert.equal(invoked, 1); assert.equal(await readFile(path.join(options['run-dir'], 'partial'), 'utf8'), 'keep');
  const failure = await readFile(path.join(options['output-dir'], 'failure.json'), 'utf8'); assert.ok(!failure.includes('oracle-secret')); assert.equal(JSON.parse(failure).delivery_pass, false);
  await assert.rejects(collect(current, { transport: transport().fn, prepare: async () => { invoked++; } })); assert.equal(invoked, 1);
});

test('collector accepts only the existing authoritative preparation digest and runtime', async (t) => {
  const { root, options } = await fixture(t); const rubric = path.join(root, 'external', 'rubric'); await writeFile(rubric, 'external-only');
  const result = await collect({ ...options, prepareOnly: true, rubric }, { transport: transport().fn, prepare: async (command) => {
    const runtime = await json(command.env.CARTOGRAPH_V7_RUNTIME_RECEIPT);
    const definition = { protocol_sha256: '6f584efbfa7f91a002eea4735294096a87979e4e4ffff618d92d00f09ad911e7', delivery_pass: false, runtime };
    const bytes = Buffer.from(JSON.stringify(definition)); const hash = createHash('sha256').update(bytes).digest('hex');
    await mkdir(options['run-dir']); await writeFile(path.join(options['run-dir'], 'frozen-definition.json'), bytes); await writeFile(path.join(options['run-dir'], 'frozen-definition.sha256'), hash); return { code: 0 };
  } });
  assert.equal(result.status, 'prepared'); assert.match(result.frozen_definition_sha256, /^[a-f0-9]{64}$/u); assert.equal(result.delivery_pass, false);
});

test('collector CLI is closed and never accepts analyst auditor opt-in or implicit preparation', () => {
  assert.throws(() => parseArguments(['--stage', 'analyst'])); assert.throws(() => parseArguments(['--operator-opt-in', 'auditor'])); assert.throws(() => parseArguments(['--prepare-only'])); assert.throws(() => parseArguments(['--endpoint', 'a', '--endpoint', 'b']));
});


test('collector rejects rubric leakage and tampered preparation seals without replay', async (t) => {
  const { root, options } = await fixture(t);
  const leaked = path.join(options.captureRoots[0], 'oracle'); await writeFile(leaked, 'withheld');
  let calls = 0;
  await assert.rejects(collect({ ...options, prepareOnly: true, rubric: leaked }, { transport: async () => { calls++; } }), /rubric_overlaps_visible_root/u);
  assert.equal(calls, 0);
  const rubric = path.join(root, 'external', 'oracle'); await writeFile(rubric, 'withheld');
  await assert.rejects(collect({ ...options, prepareOnly: true, rubric }, { transport: transport().fn, prepare: async () => {
    await mkdir(options['run-dir']); await writeFile(path.join(options['run-dir'], 'frozen-definition.json'), '{}');
    await writeFile(path.join(options['run-dir'], 'frozen-definition.sha256'), 'b'.repeat(64)); return { code: 0 };
  } }), /artifacts_retained/u);
  const failure = await json(path.join(options['output-dir'], 'failure.json'));
  assert.equal(failure.phase, 'read_prepared_definition'); assert.equal(failure.frozen_definition_sha256, null);
  assert.ok((await readdir(options['output-dir'])).includes('runtime-receipt.json'));
});


test('collector pins output directory and ancestor identities across HTTP awaits', async (t) => {
  const { root, options } = await fixture(t);
  for (const ancestor of [false, true]) {
    const parent = path.join(root, ancestor ? 'swapped-parent' : 'ordinary-parent'); await mkdir(parent);
    const output = path.join(parent, 'observation'); const moved = path.join(root, ancestor ? 'retained-parent' : 'retained-output');
    const current = { ...options, 'output-dir': output };
    let calls = 0;
    await assert.rejects(collect(current, { transport: async (...args) => {
      const result = await transport().fn(...args);
      if (++calls === 1) {
        if (ancestor) { await rename(parent, moved); await symlink(options.modelVisibleRoots[0], parent, 'dir'); await mkdir(path.join(options.modelVisibleRoots[0], 'observation')); }
        else { await rename(output, moved); await symlink(options.captureRoots[0], output, 'dir'); }
      }
      return result;
    } }), /artifacts_retained/u);
    assert.equal((await json(path.join(moved, ...(ancestor ? ['observation'] : []), 'attempt.json'))).kind, 'prerequisite_observation_attempt');
    assert.deepEqual(await readdir(options.captureRoots[0]), []);
    const visible = ancestor ? path.join(options.modelVisibleRoots[0], 'observation') : options.modelVisibleRoots[0];
    assert.deepEqual(await readdir(visible), []);
  }
});

test('collector rejects changed run or rubric identity before prepare-only handoff', async (t) => {
  const { root, options } = await fixture(t);
  for (const change of ['run', 'rubric']) {
    const rubric = path.join(root, 'external', `rubric-${change}`); await writeFile(rubric, 'frozen-rubric');
    const current = { ...options, 'output-dir': `${options['output-dir']}-${change}`, 'run-dir': `${options['run-dir']}-${change}`, rubric, prepareOnly: true };
    let prepared = false;
    await assert.rejects(collect(current, { transport: async (...args) => {
      const result = await transport().fn(...args);
      if (new URL(args[0]).pathname === '/api/tags') {
        if (change === 'run') await symlink(options.captureRoots[0], current['run-dir'], 'dir');
        else await writeFile(rubric, 'changed-rubric');
      }
      return result;
    }, prepare: async () => { prepared = true; } }), /artifacts_retained/u);
    assert.equal(prepared, false); assert.deepEqual(await readdir(options.captureRoots[0]), []);
    assert.deepEqual(await readdir(current['output-dir']), ['attempt.json']);
  }
});

test('collector retains closed failure diagnostics without raw exception or response disclosure', async (t) => {
  const { options } = await fixture(t);
  const secret = 'http://user:password@127.0.0.1/?token=withheld-secret';
  const cases = [
    { name: 'version', transport: transport({ '/api/version': { bytes: Buffer.from('{"version":"0.40.7"}') } }).fn, code: 'runtime_identity_mismatch', version: '0.40.7', selected: null },
    { name: 'digest', transport: transport({ '/api/tags': { bytes: Buffer.from(JSON.stringify({ models: [{ name: options.model, digest: `sha256:${'B'.repeat(64)}` }] })) } }).fn, code: 'model_identity_mismatch', version: null, selected: 'b'.repeat(64) },
    { name: 'timeout', transport: () => new Promise(() => {}), code: 'http_timeout', version: null, selected: null },
    { name: 'exception', transport: async () => { throw new Error(secret); }, code: 'http_failure', version: null, selected: null },
    { name: 'unsafe-version', transport: transport({ '/api/version': { bytes: Buffer.from(JSON.stringify({ version: secret })) } }).fn, code: 'runtime_identity_mismatch', version: null, selected: null },
    { name: 'unsafe-version-suffix', transport: transport({ '/api/version': { bytes: Buffer.from('{\"version\":\"0.40.7-withheld-secret\"}') } }).fn, code: 'runtime_identity_mismatch', version: null, selected: null },
    { name: 'redirect', transport: transport({ '/api/version': { status: 302 } }).fn, code: 'http_redirect_refused', version: null, selected: null },
    { name: 'malformed', transport: transport({ '/api/version': { bytes: Buffer.from(`{"secret":"${secret}",`) } }).fn, code: 'invalid_response_payload', version: null, selected: null },
  ];
  for (const item of cases) {
    const current = { ...options, 'output-dir': `${options['output-dir']}-${item.name}` };
    await assert.rejects(collect(current, { transport: item.transport, timeoutMs: 10 }), /artifacts_retained/u);
    const bytes = await readFile(path.join(current['output-dir'], 'failure.json'), 'utf8');
    const failed = JSON.parse(bytes);
    assert.equal(failed.failure_code, item.code); assert.equal(failed.observed_identity.runtime_version, item.version); assert.equal(failed.observed_identity.model_digest, item.selected);
    assert.equal(failed.status, 'failed'); assert.equal(failed.delivery_pass, false); assert.ok(!bytes.includes('withheld-secret')); assert.ok(!bytes.includes('user:password'));
    assert.deepEqual((await readdir(current['output-dir'])).sort(), ['attempt.json', 'failure.json']);
    await assert.rejects(collect(current, { transport: item.transport, timeoutMs: 10 }));
    assert.equal(await readFile(path.join(current['output-dir'], 'failure.json'), 'utf8'), bytes);
  }
});
