// AC-0279: operator-only observation; preparation never selects an execution role.
import assert from 'node:assert/strict';
import http from 'node:http';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { constants } from 'node:fs';
import { lstat, open, realpath, mkdir } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { strictJson } from './verify-v7-semantic-review.mjs';

export const LIMITS = Object.freeze({ response: 128 * 1024, artifact: 1024 * 1024, timeout: 5000, models: 128, prepareTimeout: 300_000 });
const PROTOCOL_SHA256 = '6f584efbfa7f91a002eea4735294096a87979e4e4ffff618d92d00f09ad911e7';
const HOST = 'mac14x13-m2max-32g-60f1';
const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');
const fail = (code) => { throw new Error(code); };
const check = (value, code) => { if (!value) fail(code); };
const text = (value) => typeof value === 'string' && value.length > 0 && Buffer.byteLength(value) <= 256 && !/[\x00-\x1f\x7f]/u.test(value);
export function normalizeDigest(value) {
  check(typeof value === 'string' && /^(?:sha256:)?[a-fA-F0-9]{64}$/u.test(value), 'invalid_model_digest');
  return value.replace(/^sha256:/u, '').toLowerCase();
}
export function endpointOrigin(value) {
  check(typeof value === 'string' && value.length <= 256 && !/[\x00-\x20\x7f\\]/u.test(value), 'invalid_endpoint');
  let u; try { u = new URL(value); } catch { fail('invalid_endpoint'); }
  check(u.protocol === 'http:' && ['127.0.0.1', '[::1]', 'localhost'].includes(u.hostname) &&
    !u.username && !u.password && !u.search && !u.hash && u.pathname === '/' && !value.includes('?') && !value.includes('#') && !value.includes('@'), 'invalid_endpoint');
  return u.origin;
}
const within = (child, root) => {
  const relative = path.relative(root, child);
  return relative === '' || (relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
};
const disjoint = (a, b) => !within(a, b) && !within(b, a);
export async function canonicalFuture(value) {
  check(typeof value === 'string' && path.isAbsolute(value) && value.length <= 4096, 'invalid_path');
  let ancestor = path.resolve(value); const tail = [];
  for (;;) {
    try { return path.join(await realpath(ancestor), ...tail); }
    catch (error) {
      if (error.code !== 'ENOENT' || ancestor === path.dirname(ancestor)) fail('invalid_path');
      tail.unshift(path.basename(ancestor)); ancestor = path.dirname(ancestor);
    }
  }
}
async function readBounded(file, maximum = LIMITS.artifact) {
  const info = await lstat(file); check(info.isFile() && !info.isSymbolicLink() && info.nlink === 1 && info.size <= maximum, 'invalid_artifact');
  const handle = await open(file, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const stat = await handle.stat(); check(stat.isFile() && stat.nlink === 1 && stat.size <= maximum, 'invalid_artifact');
    const buffer = Buffer.alloc(maximum + 1); let count = 0;
    while (count <= maximum) { const result = await handle.read(buffer, count, buffer.length - count); if (!result.bytesRead) break; count += result.bytesRead; }
    check(count <= maximum, 'artifact_too_large'); return buffer.subarray(0, count);
  } finally { await handle.close(); }
}
async function writeExclusive(file, value) {
  const bytes = Buffer.from(`${JSON.stringify(value, null, 2)}\n`); check(bytes.length <= LIMITS.artifact, 'artifact_too_large');
  const handle = await open(file, 'wx', 0o600);
  try { await handle.writeFile(bytes); await handle.sync(); } finally { await handle.close(); }
  const parent = await open(path.dirname(file), 'r'); try { await parent.sync(); } finally { await parent.close(); }
}
export function getLocal(url, { timeoutMs = LIMITS.timeout, maxBytes = LIMITS.response } = {}) {
  // Node http is direct: no environment proxy, redirect following or inference route.
  check(typeof url === 'string' && url.length <= 512 && Number.isInteger(timeoutMs) && timeoutMs > 0 && timeoutMs <= LIMITS.timeout && Number.isInteger(maxBytes) && maxBytes > 0 && maxBytes <= LIMITS.response, 'invalid_transport_bounds');
  const parsed = new URL(url); check(['/api/version', '/api/tags'].includes(parsed.pathname) && !parsed.search && !parsed.hash && !parsed.username && !parsed.password && !url.includes('@') && !url.includes('?') && !url.includes('#'), 'invalid_route');
  endpointOrigin(parsed.origin);
  return new Promise((resolve, reject) => {
    const request = http.get(parsed, { signal: AbortSignal.timeout(timeoutMs), headers: { accept: 'application/json', 'accept-encoding': 'identity' }, lookup: (_host, options, callback) => options.all ? callback(null, [{ address: '127.0.0.1', family: 4 }]) : callback(null, '127.0.0.1', 4) }, (response) => {
      const chunks = []; let size = 0;
      response.on('data', (chunk) => { size += chunk.length; if (size > maxBytes) { response.destroy(); reject(new Error('response_too_large')); } else chunks.push(chunk); });
      response.on('end', () => resolve({ status: response.statusCode, bytes: Buffer.concat(chunks) }));
      response.on('error', () => reject(new Error('http_failure')));
    });
    request.on('error', () => reject(new Error('http_failure')));
  });
}
async function observedGet(origin, route, transport, timeoutMs) {
  let timer;
  try {
    const result = await Promise.race([transport(`${origin}${route}`, { timeoutMs, maxBytes: LIMITS.response }), new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('http_timeout')), timeoutMs); })]);
    check(result.status === 200 && Buffer.isBuffer(result.bytes) && result.bytes.length <= LIMITS.response, 'invalid_http_response');
    const raw = new TextDecoder('utf-8', { fatal: true }).decode(result.bytes);
    return { value: strictJson(raw, LIMITS.response), hash: sha(result.bytes), byte_len: result.bytes.length };
  } catch { fail('runtime_observation_failed'); } finally { clearTimeout(timer); }
}
const scalarOptions = ['checkout', 'endpoint', 'model', 'expected-runtime-version', 'expected-model-digest', 'target-host', 'observation-path', 'confirm-target-path', 'output-dir', 'run-dir', 'rubric'];
export function parseArguments(argv) {
  const options = { captureRoots: [], modelVisibleRoots: [], prepareOnly: false };
  check(argv.length <= 80, 'too_many_arguments');
  for (let i = 0; i < argv.length; i += 1) {
    const name = argv[i];
    if (name === '--prepare-only') { check(!options.prepareOnly, 'duplicate_option'); options.prepareOnly = true; continue; }
    check(name.startsWith('--') && i + 1 < argv.length, 'invalid_arguments');
    const value = argv[++i]; check(typeof value === 'string' && value.length <= 4096 && !value.startsWith('--'), 'invalid_arguments');
    if (name === '--capture-root') options.captureRoots.push(value);
    else if (name === '--model-visible-root') options.modelVisibleRoots.push(value);
    else { const key = name.slice(2); check(scalarOptions.includes(key) && !Object.hasOwn(options, key), 'unknown_or_duplicate_option'); options[key] = value; }
  }
  for (const name of scalarOptions.filter((key) => key !== 'rubric')) check(options[name], 'missing_option');
  check(options.captureRoots.length > 0 && options.captureRoots.length <= 8 && options.modelVisibleRoots.length > 0 && options.modelVisibleRoots.length <= 8, 'missing_exclusion_roots');
  check(!options.prepareOnly || options.rubric, 'missing_rubric');
  check(options.prepareOnly || !options.rubric, 'unexpected_rubric');
  return options;
}
export function preparationCommand(options, runtimePath, environment = process.env) {
  const env = Object.fromEntries(Object.entries(environment).filter(([key]) => !key.startsWith('CARTOGRAPH_') && !['NODE_OPTIONS', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_TARGET_RUNNER'].includes(key) && !/^CARGO_TARGET_.*_RUNNER$/u.test(key)));
  Object.assign(env, { CARTOGRAPH_V7_STAGE: 'prepare', CARTOGRAPH_V7_RUN_DIR: options['run-dir'], CARTOGRAPH_V7_RUN_ID: path.basename(options['output-dir']), CARTOGRAPH_V7_RUNTIME_RECEIPT: runtimePath, CARTOGRAPH_V7_RUBRIC: options.rubric });
  return { executable: 'cargo', args: ['test', '-p', 'app', 'investigation_v7_completion_control_explicit_stage', '--', '--ignored', '--nocapture'], cwd: options.checkout, env };
}
export function runPreparation(command) {
  return new Promise((resolve) => {
    const child = spawn(command.executable, command.args, { cwd: command.cwd, env: command.env, shell: false, stdio: ['ignore', 'pipe', 'pipe'], detached: process.platform !== 'win32' });
    let bytes = 0; let timedOut = false;
    const stop = () => { try { if (process.platform !== 'win32') process.kill(-child.pid, 'SIGKILL'); else child.kill('SIGKILL'); } catch { /* Already exited. */ } };
    const timer = setTimeout(() => { timedOut = true; stop(); }, LIMITS.prepareTimeout);
    const count = (chunk) => { bytes += chunk.length; if (bytes > LIMITS.artifact) stop(); };
    child.stdout.on('data', count); child.stderr.on('data', count);
    child.on('error', () => { clearTimeout(timer); resolve({ code: -1 }); });
    child.on('close', (code) => { clearTimeout(timer); resolve({ code: !timedOut && bytes <= LIMITS.artifact ? code : -1 }); });
  });
}
export async function collect(options, { transport = getLocal, prepare = runPreparation, now = () => new Date().toISOString(), timeoutMs = LIMITS.timeout } = {}) {
  // Validate identities and exclusions before any network or output creation.
  const origin = endpointOrigin(options.endpoint);
  check([HOST, `${HOST}.local`, `${HOST}.lan`].includes(options['target-host']) && ['on-studio', 'operator-established-forward'].includes(options['observation-path']) && options['confirm-target-path'] === 'operator-asserted', 'missing_studio_assertion');
  check(text(options.model) && text(options['expected-runtime-version']), 'invalid_expected_identity');
  const digest = normalizeDigest(options['expected-model-digest']);
  check(Number.isInteger(timeoutMs) && timeoutMs > 0 && timeoutMs <= LIMITS.timeout, 'invalid_timeout');
  const opts = { ...options };
  for (const key of ['checkout', 'output-dir', 'run-dir']) opts[key] = await canonicalFuture(options[key]);
  const roots = await Promise.all([...options.captureRoots, ...options.modelVisibleRoots].map(canonicalFuture));
  check(roots.length >= 2 && roots.every((root) => disjoint(opts['output-dir'], root)), 'output_overlaps_visible_root');
  check(disjoint(opts['output-dir'], opts['run-dir']) && disjoint(opts['output-dir'], opts.checkout) && disjoint(opts['run-dir'], opts.checkout), 'overlapping_artifact_roots');
  check(/^[A-Za-z0-9._-]{1,100}$/u.test(path.basename(opts['output-dir'])) && !['.', '..'].includes(path.basename(opts['output-dir'])), 'invalid_run_id');
  let rubricHash;
  if (opts.prepareOnly) {
    opts.rubric = await realpath(opts.rubric);
    check([opts['output-dir'], opts['run-dir'], opts.checkout, ...roots].every((root) => !within(opts.rubric, root)), 'rubric_overlaps_visible_root');
    rubricHash = sha(await readBounded(opts.rubric));
  }
  await mkdir(opts['output-dir'], { mode: 0o700 });
  const outputParent = await open(path.dirname(opts['output-dir']), 'r');
  try { await outputParent.sync(); } finally { await outputParent.close(); }
  const outputDirectory = await open(opts['output-dir'], constants.O_RDONLY | (constants.O_DIRECTORY ?? 0) | (constants.O_NOFOLLOW ?? 0));
  const pinnedOutput = await outputDirectory.stat();
  const runParentPath = path.dirname(opts['run-dir']);
  const pinnedRunParent = opts.prepareOnly ? await lstat(runParentPath) : null;
  const verifyPaths = async () => {
    const current = await lstat(opts['output-dir']);
    const held = await outputDirectory.stat();
    check(current.isDirectory() && !current.isSymbolicLink() && current.dev === pinnedOutput.dev && current.ino === pinnedOutput.ino && held.dev === pinnedOutput.dev && held.ino === pinnedOutput.ino && await realpath(opts['output-dir']) === opts['output-dir'], 'output_directory_changed');
    const currentRoots = await Promise.all([...options.captureRoots, ...options.modelVisibleRoots].map(canonicalFuture));
    check(currentRoots.every((root, index) => root === roots[index] && disjoint(opts['output-dir'], root)), 'exclusion_root_changed');
    check(await canonicalFuture(options['run-dir']) === opts['run-dir'] && await canonicalFuture(options.checkout) === opts.checkout, 'preparation_path_changed');
    if (pinnedRunParent) {
      const parent = await lstat(runParentPath);
      check(parent.isDirectory() && !parent.isSymbolicLink() && parent.dev === pinnedRunParent.dev && parent.ino === pinnedRunParent.ino, 'run_parent_changed');
    }
    if (opts.prepareOnly) check(await realpath(options.rubric) === opts.rubric && sha(await readBounded(opts.rubric)) === rubricHash, 'rubric_changed');
  };
  const writeOutput = async (name, value) => {
    // Node has no portable openat API. Pin and recheck the directory around
    // pathname writes; this detects boundary changes, not a privileged racing writer.
    await verifyPaths();
    await writeExclusive(path.join(opts['output-dir'], name), value);
    await verifyPaths();
  };
  const record = { schema_version: 1, kind: 'v7_runtime_prerequisites', target: { host: HOST, asserted_host: opts['target-host'], observation_path: opts['observation-path'], provenance: 'operator_asserted_not_attested' }, endpoint: origin, expected_identity: { model: opts.model, model_digest: digest, runtime_version: opts['expected-runtime-version'] }, phase: 'version', started_at: now(), completed_at: null, status: 'observing', observations: [], sampling_status: 'unset_or_unverified_not_observed', frozen_definition_sha256: null, delivery_pass: false };
  try {
    await writeOutput('attempt.json', { schema_version: 1, kind: 'prerequisite_observation_attempt', delivery_pass: false });
    const version = await observedGet(origin, '/api/version', transport, timeoutMs);
    await verifyPaths();
    record.observations.push({ route: '/api/version', sha256: version.hash, byte_len: version.byte_len });
    check(text(version.value?.version) && version.value.version === opts['expected-runtime-version'], 'runtime_identity_mismatch');
    record.phase = 'tags';
    const tags = await observedGet(origin, '/api/tags', transport, timeoutMs);
    await verifyPaths();
    record.observations.push({ route: '/api/tags', sha256: tags.hash, byte_len: tags.byte_len });
    check(Array.isArray(tags.value?.models) && tags.value.models.length <= LIMITS.models, 'invalid_model_inventory');
    const selected = tags.value.models.filter((item) => item && item.name === opts.model);
    check(selected.length === 1 && normalizeDigest(selected[0].digest) === digest, 'model_identity_mismatch');
    const runtime = { schema_version: 1, endpoint: `${origin}/`, model: opts.model, model_digest: digest, runtime_version: version.value.version, temperature: null, seed: null, context_length: null, verified_read_only: true };
    record.phase = 'write_runtime_receipt';
    const runtimePath = path.join(opts['output-dir'], 'runtime-receipt.json');
    await writeOutput('runtime-receipt.json', runtime);
    record.runtime_receipt_sha256 = sha(await readBounded(runtimePath));
    record.status = 'observed';
    if (opts.prepareOnly) {
      record.status = 'prepare_failed'; record.phase = 'prepare';
      await verifyPaths();
      const outcome = await prepare(preparationCommand(opts, runtimePath));
      await verifyPaths();
      check(outcome?.code === 0, 'prepare_failed');
      record.phase = 'read_prepared_definition';
      const definitionBytes = await readBounded(path.join(opts['run-dir'], 'frozen-definition.json'));
      const definition = strictJson(new TextDecoder('utf-8', { fatal: true }).decode(definitionBytes), LIMITS.artifact);
      const definitionHash = (await readBounded(path.join(opts['run-dir'], 'frozen-definition.sha256'), 64)).toString('ascii');
      check(definitionHash === sha(definitionBytes) && definition.protocol_sha256 === PROTOCOL_SHA256 && definition.delivery_pass === false, 'invalid_prepared_definition');
      assert.deepEqual(definition.runtime, runtime);
      record.frozen_definition_sha256 = definitionHash; record.status = 'prepared';
    }
    record.completed_at = now(); record.phase = 'complete';
    await writeOutput('observation.json', record);
    await verifyPaths();
    return record;
  } catch {
    record.status = 'failed'; record.completed_at = now();
    try { await writeOutput('failure.json', record); } catch { /* Preserve original directory and any partial files; never retry. */ }
    fail('prerequisite_collection_failed_artifacts_retained');
  } finally { await outputDirectory.close(); }
}
export async function main(argv, dependencies) { return collect(parseArguments(argv), dependencies); }
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main(process.argv.slice(2)).then((record) => {
  console.log(JSON.stringify({ status: record.status, frozen_definition_sha256: record.frozen_definition_sha256, delivery_pass: false }));
}).catch(() => { console.error('Prerequisite collection refused or failed; preserve existing artifacts. No inference stage was selected.'); process.exitCode = 1; });
