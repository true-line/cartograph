#!/usr/bin/env node
// Independent scripted stdio smoke, not installed-client/SDK or LLM acceptance.
import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
export const TOOLS = ['cartograph_capabilities', 'cartograph_discover', 'cartograph_artifacts', 'cartograph_read_artifact', 'cartograph_evidence'];
export function optionsFrom(args) {
  const names = ['relay', 'record', 'max-frame-bytes', 'timeout-ms', 'max-output-bytes', 'max-stderr-bytes'];
  const options = {};
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i]?.replace(/^--/u, '');
    if (args[i] !== `--${key}` || !names.includes(key) || key in options || args[i + 1] === undefined) throw new Error('invalid smoke arguments');
    options[key] = args[i + 1];
  }
  for (const key of names) {
    if (['relay', 'record'].includes(key)) {
      if (!path.isAbsolute(options[key] ?? '')) throw new Error('absolute smoke paths required');
    } else {
      if (!/^[1-9][0-9]*$/u.test(options[key] ?? '') || !Number.isSafeInteger(Number(options[key]))) throw new Error('explicit positive smoke limits required');
      options[key] = Number(options[key]);
    }
  }
  if (options['timeout-ms'] > 2147483647) throw new Error('smoke deadline unsupported');
  return options;
}
export async function smoke(options) {
  const child = spawn(options.relay, ['--record', options.record, '--max-frame-bytes', String(options['max-frame-bytes']), '--timeout-ms', String(options['timeout-ms'])], { shell: false, stdio: ['pipe', 'pipe', 'pipe'] });
  const frames = []; const pending = new Map();
  let buffered = Buffer.alloc(0); let stdoutBytes = 0; let stderrBytes = 0; let failure;
  let rejectFailure;
  const failed = new Promise((_, reject) => { rejectFailure = reject; });
  // Attach a handler immediately; the race below still observes the rejection.
  failed.catch(() => {});
  const fail = () => {
    if (failure) return;
    failure = new Error('scripted stdio smoke failed');
    rejectFailure(failure); child.kill('SIGKILL');
  };
  child.on('error', fail); child.stdin.on('error', fail);
  const closed = new Promise((resolve) => child.on('close', (code, signal) => resolve({ code, signal })));
  child.stderr.on('data', (chunk) => { stderrBytes += chunk.length; if (stderrBytes > options['max-stderr-bytes']) fail(); });
  child.stdout.on('data', (chunk) => {
    stdoutBytes += chunk.length;
    if (stdoutBytes > options['max-output-bytes']) return fail();
    try {
      // Total accumulation is bounded before allocation; each complete frame is
      // checked separately, including data following a newline in one chunk.
      let start = 0;
      while (start < chunk.length) {
        const lf = chunk.indexOf(10, start); const end = lf < 0 ? chunk.length : lf;
        const part = chunk.subarray(start, end);
        if (buffered.length + part.length > options['max-frame-bytes']) throw new Error();
        buffered = Buffer.concat([buffered, part]);
        if (lf < 0) break;
        const value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(buffered));
        const waiter = pending.get(value.id);
        if (value.jsonrpc !== '2.0' || !waiter || (('result' in value) === ('error' in value))) throw new Error();
        pending.delete(value.id);
        frames.push({ method: waiter.method, response_bytes: buffered.length, result_bytes: Buffer.byteLength(JSON.stringify(value.result ?? value.error)),
          tool_input_schema_bytes: value.result?.tools ? value.result.tools.reduce((sum, tool) => sum + Buffer.byteLength(JSON.stringify(tool.inputSchema)), 0) : null,
          structured_bytes: value.result?.structuredContent === undefined ? 0 : Buffer.byteLength(JSON.stringify(value.result.structuredContent)), tokens: null });
        waiter.resolve(value); buffered = Buffer.alloc(0); start = lf + 1;
      }
    } catch { fail(); }
  });
  let nextId = 1;
  function send(method, params, notification = false) {
    const id = nextId++;
    const value = { jsonrpc: '2.0', ...(notification ? {} : { id }), method, ...(params === undefined ? {} : { params }) };
    const line = JSON.stringify(value);
    if (Buffer.byteLength(line) > options['max-frame-bytes']) throw new Error('smoke request exceeds cap');
    const response = notification ? Promise.resolve() : new Promise((resolve) => pending.set(id, { resolve, method }));
    child.stdin.write(`${line}\n`);
    return Promise.race([response, failed, closed.then(() => { throw new Error('relay closed before response'); })]);
  }
  function structured(response) {
    if (response.error || response.result?.isError || typeof response.result?.structuredContent !== 'object') throw new Error('invalid tool result');
    return response.result.structuredContent;
  }
  function refused(response) {
    if (!response.error && response.result?.isError !== true) throw new Error('invalid request was not refused');
  }
  const timer = setTimeout(fail, options['timeout-ms']);
  try {
    return await Promise.race([failed, (async () => {
      const init = await send('initialize', { protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'cartograph-scripted-smoke', version: '1' } });
      if (init.result?.protocolVersion !== '2025-11-25') throw new Error('protocol mismatch');
      await send('notifications/initialized', undefined, true);
      const listed = await send('tools/list');
      if (JSON.stringify(listed.result?.tools?.map((t) => t.name).sort()) !== JSON.stringify([...TOOLS].sort())) throw new Error('unexpected tool list');
      if (!listed.result.tools.every((t) => t.inputSchema?.type === 'object' && t.annotations?.readOnlyHint === true)) throw new Error('invalid tool schemas');
      const call = (name, args) => send('tools/call', { name, arguments: args });
      const capabilities = structured(await call('cartograph_capabilities', {}));
      if (typeof capabilities.session_id !== 'string' || typeof capabilities.basis_id !== 'string' || capabilities.h3_curated_available !== false) throw new Error('invalid capabilities');
      const identity = { session_id: capabilities.session_id, basis_id: capabilities.basis_id };
      const discover = (request, context_id = null) => call('cartograph_discover', { ...identity, request: { request, context_id } });
      const selection = { query: { by: 'text', text: 'rule' }, labels: ['BusinessRule'], max_candidates: 1, max_bytes: options['max-frame-bytes'], cursor: null };
      const first = structured(await discover(selection)).context;
      const candidate = first?.response?.candidates?.[0];
      if (!candidate || first.response.candidates.length !== 1 || first.response.total_candidates < 2 || !first.response.next_cursor || typeof first.context_id !== 'string' || first.coverage?.exhaustive !== false || candidate.reference?.kind !== 'node' || candidate.confidence_tier === 'gap' || !candidate.source?.path) throw new Error('invalid bounded discovery');
      const second = structured(await discover({ ...selection, cursor: first.response.next_cursor }, first.context_id)).context;
      if (second.context_id !== first.context_id || second.response.snapshot_id !== first.response.snapshot_id || second.response.candidates.length !== 1 || second.response.candidates[0].reference.id === candidate.reference.id) throw new Error('invalid discovery continuation');
      refused(await discover({ ...selection, cursor: { ...first.response.next_cursor, snapshot_id: 'substituted-snapshot' } }, first.context_id));
      refused(await discover({ ...selection, cursor: first.response.next_cursor }, 'stale-context'));
      const catalog = structured(await call('cartograph_artifacts', identity));
      if (!catalog.artifacts?.some((a) => a.id === 'rule-evidence')) throw new Error('missing official artifact');
      const artifactArgs = { ...identity, artifact_id: 'rule-evidence', offset: 0, max_content_bytes: 64, max_bytes: options['max-frame-bytes'] };
      const page = structured(await call('cartograph_read_artifact', artifactArgs)).artifact;
      if (page?.artifact_id !== 'rule-evidence' || page.basis_id !== identity.basis_id || typeof page.content_id !== 'string' || typeof page.content !== 'string' || Buffer.byteLength(page.content) > 64 || !page.partial || !Number.isSafeInteger(page.next_offset) || page.next_offset !== Buffer.byteLength(page.content) || page.structured_assertions_included !== false) throw new Error('invalid artifact page');
      const continued = structured(await call('cartograph_read_artifact', { ...artifactArgs, offset: page.next_offset, expected_content_id: page.content_id })).artifact;
      if (continued.content_id !== page.content_id || continued.basis_id !== identity.basis_id || continued.offset !== page.next_offset || Buffer.byteLength(continued.content) > 64) throw new Error('invalid artifact continuation');
      refused(await call('cartograph_read_artifact', { ...artifactArgs, offset: page.next_offset, expected_content_id: 'substituted-content' }));
      const evidenceArgs = { ...identity, kind: 'retained_primary_source', max_bytes: options['max-frame-bytes'] };
      const description = structured(await call('cartograph_evidence', { ...evidenceArgs, mode: 'describe', fact: candidate.reference, ordinal: 0 })).description;
      if (description?.basis_id !== identity.basis_id || description.scope !== 'primary_source_only' || description.input_closure !== 'input_closure_not_established' || description.target?.fact?.id !== candidate.reference.id || description.target.source.path !== candidate.source.path || description.target.source.repo !== candidate.source.repo) throw new Error('invalid evidence provenance');
      const readArgs = { ...evidenceArgs, mode: 'read', target: description.target };
      const span = structured(await call('cartograph_evidence', readArgs)).span;
      if (span?.basis_id !== identity.basis_id || JSON.stringify(span.target) !== JSON.stringify(description.target) || span.scope !== description.scope || span.input_closure !== description.input_closure || typeof span.text !== 'string' || !span.text.length || Buffer.byteLength(span.text) !== description.target.captured.byte_end - description.target.captured.byte_start) throw new Error('invalid retained span');
      refused(await call('cartograph_evidence', { ...readArgs, basis_id: 'stale-basis' }));
      refused(await call('cartograph_evidence', { ...readArgs, target: { ...description.target, receipt_id: 'substituted-receipt' } }));
      refused(await call('cartograph_evidence', { ...readArgs, max_bytes: 256 }));
      refused(await call('cartograph_ingest', {}));
      child.stdin.end();
      const exit = await closed;
      if (exit.code !== 0 || exit.signal || pending.size || buffered.length || stderrBytes) throw new Error('unclean relay exit');
      return { acceptance: 'scripted-external-process-smoke', installed_client_acceptance: false, tokens: null,
        scenario: { discovery_pages: 2, candidate, target: span.target, evidence_bytes: Buffer.byteLength(span.text), evidence_sha256: createHash('sha256').update(span.text).digest('hex'), artifact_id: page.artifact_id, artifact_content_id: page.content_id, artifact_pages: 2, refused_cases: ['stale-discovery-snapshot', 'stale-context', 'substituted-artifact-content', 'stale-evidence-basis', 'substituted-receipt', 'evidence-byte-cap', 'mutation-tool'] },
        byte_definitions: { stdout_bytes: 'UTF-8 wire bytes including LF', response_bytes: 'UTF-8 JSON frame excluding LF', result_bytes: 'reserialized JSON result or error', structured_bytes: 'reserialized structuredContent', tool_input_schema_bytes: 'sum of reserialized tools/list inputSchema values' }, stdout_bytes: stdoutBytes, stderr_bytes: stderrBytes,
        responses: frames.map((frame) => ({ ...frame, jsonrpc_envelope_bytes: frame.response_bytes - frame.result_bytes, tool_schema_and_text_overhead_bytes: frame.structured_bytes ? frame.result_bytes - frame.structured_bytes : null })) };
    })()]);
  } finally { clearTimeout(timer); child.kill('SIGKILL'); }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { console.log(JSON.stringify(await smoke(optionsFrom(process.argv.slice(2))))); }
  catch { console.error('scripted stdio smoke failed'); process.exitCode = 1; }
}
