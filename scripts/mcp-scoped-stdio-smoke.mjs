#!/usr/bin/env node
// #755: independent scripted scoped client; not installed-client/SDK acceptance.
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
const options = {};
for (let i=2;i<process.argv.length;i+=2) options[process.argv[i].slice(2)] = process.argv[i+1];
for (const key of ['max-frame-bytes','timeout-ms','max-output-bytes','max-stderr-bytes']) {
  assert.match(options[key] ?? '', /^[1-9][0-9]*$/u);
  options[key] = Number(options[key]); assert.ok(Number.isSafeInteger(options[key]));
}
assert.ok(options.relay?.startsWith('/') && options.record?.startsWith('/') && options.fact);
async function run() {
  let stage = 'initialize';
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
          request_bytes: waiter.request_bytes,
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
    const response = notification ? Promise.resolve() : new Promise((resolve) => pending.set(id, { resolve, method, request_bytes: Buffer.byteLength(line) }));
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
      const initialized = await send('initialize', {protocolVersion:'2025-11-25', capabilities:{}, clientInfo:{name:'cartograph-scoped-scripted',version:'1'}});
      assert.equal(initialized.result?.protocolVersion, '2025-11-25');
      await send('notifications/initialized', undefined, true);
      const call = (name,args) => send('tools/call',{name,arguments:args});
      stage='capabilities';
      const caps=structured(await call('cartograph_capabilities',{}));
      assert.equal(caps.mode,'scoped'); assert.equal(caps.complete_system,false); assert.equal(caps.h3_curated_available,false);
      const identity={session_id:caps.session_id,session_basis_id:caps.session_basis_id};
      stage='partition';
      const partition=structured(await call('cartograph_read_artifact',{...identity,kind:'rule-evidence',root:options.fact,max_bytes:65536})).partition;
      assert.equal(partition.complete_system,false); assert.equal(partition.absence_claim,false);
      assert.equal(partition.artifact.schema,'cartograph.spec-partition/rule-evidence@1');
      assert.match(partition.artifact.content_id,/^spec-partition-artifact-v1:/u);
      const assertion=partition.artifact.assertions.find(a=>a.subject_kind==='BusinessRule' && a.subject_id===options.fact);
      assert.ok(assertion?.provenance?.evidence?.length);
      stage='evidence-description';
      const common={...identity,kind:'retained_primary_source',max_bytes:65536};
      const description=structured(await call('cartograph_evidence',{...common,mode:'describe',fact:{kind:'node',id:assertion.subject_id},ordinal:0})).evidence;
      assert.equal(description.basis_id,caps.session_basis_id);assert.equal(description.target.fact.id,options.fact);
      assert.match(description.target.receipt_id,/^ts-primary-v3:[0-9a-f]{64}$/u);
      stage='evidence-read';
      const read={...common,mode:'read',target:description.target};
      const span=structured(await call('cartograph_evidence',read)).evidence;
      assert.deepEqual(span.target,description.target);assert.equal(span.scope,'primary_source_only');assert.equal(span.input_closure,'input_closure_not_established');assert.ok(span.text.length);
      assert.equal(Buffer.byteLength(span.text),span.target.captured.byte_end-span.target.captured.byte_start);
      stage='refusals';
      const stale=await call('cartograph_evidence',{...read,session_basis_id:`mcp-scoped-session-v1:${'0'.repeat(64)}`});
      assert.equal(stale.result?.structuredContent?.error,'stale_basis');assert.equal(stale.result?.isError,true);
      const limited=await call('cartograph_evidence',{...read,max_bytes:256});
      assert.equal(limited.result?.structuredContent?.error ?? limited.error?.message,'response_limit');
      refused(limited);
      stage='relay-close';child.stdin.end();const exit=await closed;
      assert.equal(exit.code,0);assert.equal(exit.signal,null);assert.equal(stderrBytes,0);assert.equal(buffered.length,0);assert.equal(pending.size,0);
      return {acceptance:'scoped-scripted-external-process',installed_client_acceptance:false,tokens:null,
        basis_id:caps.session_basis_id,partition_content_id:partition.artifact.content_id,target:span.target,
        evidence_bytes:Buffer.byteLength(span.text),evidence_sha256:createHash('sha256').update(span.text).digest('hex'),
        refused_cases:['stale-session-basis-with-original-target','evidence-response-limit'],relay_exit:exit,
        stdout_bytes:stdoutBytes,stderr_bytes:stderrBytes,responses:frames};
    })()]);
  } catch (error) { throw new Error(`scoped client failed at ${stage}: ${error.message}`); }
  finally {clearTimeout(timer);child.kill('SIGKILL');}
}
try {console.log(JSON.stringify(await run()));} catch(error) {console.error(error.message);process.exitCode=1;}
