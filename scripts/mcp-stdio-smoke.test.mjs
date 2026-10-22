import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, chmodSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { optionsFrom, smoke } from './mcp-stdio-smoke.mjs';
function fixture(t, body) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'cartograph-stdio-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const relay = path.join(root, 'fake relay');
  writeFileSync(relay, `#!${process.execPath}\n${body}`); chmodSync(relay, 0o700);
  return { relay, record: path.join(root, 'record.json'), 'max-frame-bytes': 65536, 'timeout-ms': 1000, 'max-output-bytes': 524288, 'max-stderr-bytes': 4096 };
}
test('requires explicit absolute paths and all positive finite limits', () => {
  const args = ['--relay', '/relay', '--record', '/record', '--max-frame-bytes', '4096', '--timeout-ms', '100', '--max-output-bytes', '8192', '--max-stderr-bytes', '128'];
  assert.equal(optionsFrom(args)['timeout-ms'], 100);
  for (const bad of [[], args.slice(0, -2), [...args, '--relay', '/again'], args.map((x) => x === '100' ? '0' : x), args.map((x) => x === '/relay' ? 'relative' : x)]) assert.throws(() => optionsFrom(bad));
});
function protocolRelay(fault = '') {
  return `const readline=require('node:readline');
const names=['cartograph_capabilities','cartograph_discover','cartograph_artifacts','cartograph_read_artifact','cartograph_evidence'];
const fault=${JSON.stringify(fault)};
const source={repo:'fixture/repo',path:'source.ts',commit_sha:'commit'};
const candidate=n=>({reference:{kind:'node',id:'rule:'+n},confidence_tier:'confirmed',source});
const target={fact:{kind:'node',id:'rule:1'},source,captured:{byte_start:0,byte_end:4},receipt_id:'receipt'};
readline.createInterface({input:process.stdin}).on('line',line=>{const q=JSON.parse(line);if(!('id' in q))return;
const error=()=>process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:q.id,error:{code:-32602,message:'refused'}})+'\\n');
let result;if(q.method==='initialize')result={protocolVersion:'2025-11-25'};
else if(q.method==='tools/list')result={tools:names.map(name=>({name,inputSchema:{type:'object'},annotations:{readOnlyHint:true}}))};
else {const name=q.params.name;const a=q.params.arguments;let data;
if(name==='cartograph_capabilities')data={session_id:'session',basis_id:'basis',h3_curated_available:false};
else if(name==='cartograph_discover'){
 const r=a.request.request;const cursor=r.cursor;
 if((cursor&&cursor.snapshot_id!=='snapshot')||(cursor&&a.request.context_id!=='context'))return error();
 data={context:{context_id:'context',coverage:{exhaustive:false},response:{snapshot_id:'snapshot',total_candidates:2,candidates:[candidate(cursor?2:1)],next_cursor:cursor?null:{snapshot_id:'snapshot',query_id:'query',policy:'discovery-v1',offset:1}}}};
 if(fault==='discovery')data.context.response.candidates.push(candidate(3));
}
else if(name==='cartograph_artifacts')data={artifacts:[{id:'rule-evidence'}]};
else if(name==='cartograph_read_artifact'){
 if(a.expected_content_id&&a.expected_content_id!=='content')return error();
 data={artifact:{artifact_id:'rule-evidence',basis_id:'basis',content_id:'content',content:'x'.repeat(64),partial:true,offset:a.offset,next_offset:a.offset+64,structured_assertions_included:false}};
 if(fault==='artifact')data.artifact.content='x'.repeat(65);
}
else if(name==='cartograph_evidence'){
 if(a.basis_id!=='basis'||(a.target&&a.target.receipt_id!=='receipt')||a.max_bytes<1024)return error();
 const common={basis_id:'basis',scope:'primary_source_only',input_closure:'input_closure_not_established',target};
 data=a.mode==='describe'?{description:{...common,total_ranges:1,next_ordinal:null}}:{span:{...common,text:'🧭'}};
 if(fault==='evidence'&&data.span)data.span.text='wrong';
 if(fault==='provenance'&&data.description)data.description={...data.description,target:{...target,source:{...source,path:'other.ts'}}};
}
else return error();
result={structuredContent:data,content:[{type:'text',text:'summary'}]};}
const bytes=Buffer.from(JSON.stringify({jsonrpc:'2.0',id:q.id,result})+'\\n');process.stdout.write(bytes.subarray(0,3));process.stdout.write(bytes.subarray(3));});`;
}
test('independent protocol client measures discovery artifact and retained evidence frames', async (t) => {
  const report = await smoke(fixture(t, protocolRelay()));
  assert.equal(report.responses.length, 17);
  assert.equal(report.tokens, null);
  assert.equal(report.scenario.evidence_bytes, 4);
  assert.equal(report.scenario.evidence_sha256, 'c3dafe97536ac80c9176a8de7ff941aed2f56d03daca2e35ceae1d5c979925cd');
  assert.equal(report.scenario.refused_cases.length, 7);
  assert.equal(report.stdout_bytes, report.responses.reduce((sum, frame) => sum + frame.response_bytes + 1, 0));
  assert.ok(report.responses[1].tool_input_schema_bytes > 0);
  assert.ok(report.responses[8].tool_schema_and_text_overhead_bytes > 0);
});
test('scenario rejects unbounded pages and evidence/provenance substitutions', async (t) => {
  for (const fault of ['discovery', 'artifact', 'evidence', 'provenance']) await assert.rejects(smoke(fixture(t, protocolRelay(fault))));
});
test('oversized stdout, stderr, invalid UTF8, unsolicited responses, EOF and timeout fail closed', async (t) => {
  for (const body of ["process.stdout.write('x'.repeat(70000));", "process.stderr.write('x'.repeat(5000));", "process.stdout.write(Buffer.from([255,10]));", "process.stdout.write('{\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{}}\\n');", 'process.exit(0);', 'setInterval(()=>{},1000);']) {
    const options = fixture(t, body); options['timeout-ms'] = 200;
    await assert.rejects(smoke(options));
  }
});
