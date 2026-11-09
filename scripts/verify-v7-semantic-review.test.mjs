import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile, symlink } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { LISTING_PROJECTION, main, strictJson, validateBinding, validateSemanticReview, validateLatestReview, verifyReview } from './verify-v7-semantic-review.mjs';

function fixture(verdict = 'pass') {
  const binding = { schema: 'cartograph-v7-semantic-review-binding@1', protocol_sha256: 'a'.repeat(64),
    run_id: 'v7-analyst-20261020', role: 'analyst', receipt_sha256: 'b'.repeat(64), rubric_sha256: 'c'.repeat(64) };
  const review = { ...binding, schema: 'cartograph-v7-semantic-review@1', verdict, reasons: ['Scoped evidence supports the recorded rubric assessment.'] };
  const body = `Reviewed the sealed artifact.\n\n\`\`\`json\n${JSON.stringify(review)}\n\`\`\`\n`;
  const viewer = { data: { viewer: { __typename: 'User', login: 'trueline-codex-agent[bot]' } } };
  const commentId = 123456789;
  const url = `https://github.com/true-line/cartograph/issues/652#issuecomment-${commentId}`;
  const rest = { id: commentId, node_id: 'IC_authenticated', issue_url: 'https://api.github.com/repos/true-line/cartograph/issues/652',
    html_url: url, body, created_at: '2026-10-20T16:30:00Z', updated_at: '2026-10-20T16:30:00Z',
    user: { login: 'trueline-claude-agent[bot]', type: 'Bot' }, performed_via_github_app: { slug: 'trueline-claude-agent' } };
  const graphql = { data: { node: { __typename: 'IssueComment', id: rest.node_id, databaseId: commentId, url,
    body, createdAt: rest.created_at, lastEditedAt: null, includesCreatedEdit: false,
    author: { __typename: 'Bot', login: 'trueline-claude-agent' }, issue: { number: 652, repository: { nameWithOwner: 'true-line/cartograph' } } } } };
  return { binding, commentId, viewer, rest, graphql };
}
function replaceReview(f, change) {
  const review = JSON.parse(f.rest.body.split('```json\n')[1].split('\n```')[0]);
  change(review);
  f.rest.body = `\`\`\`json\n${JSON.stringify(review)}\n\`\`\``;
  f.graphql.data.node.body = f.rest.body;
}
function transportFor(f, calls = []) {
  return async (args) => {
    calls.push(args);
    if (calls.length === 1) return f.viewer;
    if (calls.length === 2) return f.rest;
    if (calls.length === 3) return f.graphql;
    if (calls.length === 4) return [f.rest];
    throw new Error('Unexpected additional request');
  };
}

test('AC-0267: exact authenticated representations and all verdicts yield content-bound attestations', () => {
  for (const verdict of ['pass', 'fail', 'unreviewable']) {
    const f = fixture(verdict); const result = validateSemanticReview(f);
    assert.equal(result.schema, 'cartograph-v7-semantic-review-attestation@1');
    assert.equal(result.review.verdict, verdict);
    assert.deepEqual(result.binding, f.binding);
    assert.equal(result.comment.database_id, f.commentId);
    assert.equal(result.comment.url, f.rest.html_url);
    assert.equal(result.comment.created_at, f.rest.created_at);
    assert.equal(result.comment.body_sha256, createHash('sha256').update(f.rest.body).digest('hex'));
    f.binding.run_id = 'later-mutation'; assert.notEqual(result.binding.run_id, f.binding.run_id);
  }
});

test('AC-0267: author, App, issue, pristine flags and API agreement fail closed', () => {
  const mutations = [
    (f) => { f.viewer.data.viewer.login = 'a-human'; },
    (f) => { f.viewer.data.viewer.__typename = 'Bot'; },
    (f) => { f.rest.user.login = 'trueline-claude-agent'; },
    (f) => { f.rest.user.type = 'User'; },
    (f) => { delete f.rest.performed_via_github_app; },
    (f) => { f.rest.performed_via_github_app.slug = 'other-app'; },
    (f) => { f.graphql.data.node.author.login = 'trueline-claude-agent[bot]'; },
    (f) => { f.graphql.data.node.author.__typename = 'User'; },
    (f) => { f.rest.issue_url = 'https://api.github.com/repos/other/cartograph/issues/652'; },
    (f) => { f.graphql.data.node.issue.number = 653; },
    (f) => { f.graphql.data.node.issue.repository.nameWithOwner = 'other/cartograph'; },
    (f) => { f.graphql.data.node.lastEditedAt = '2026-10-20T16:31:00Z'; },
    (f) => { f.graphql.data.node.includesCreatedEdit = true; },
    (f) => { delete f.graphql.data.node.includesCreatedEdit; },
    (f) => { f.rest.updated_at = '2026-10-20T16:31:00Z'; },
    (f) => { f.graphql.data.node.databaseId += 1; },
    (f) => { f.graphql.data.node.id = 'IC_other'; },
    (f) => { f.graphql.data.node.__typename = 'PullRequestReviewComment'; },
    (f) => { f.graphql.data.node.url += '?edited'; },
    (f) => { f.graphql.data.node.createdAt = '2026-10-20T16:31:00Z'; },
    (f) => { f.graphql.data.node.body += '\n'; },
    (f) => { f.graphql.errors = [{ message: 'Deleted or unavailable' }]; },
    (f) => { f.graphql.data.node = null; },
  ];
  for (const mutate of mutations) { const f = fixture(); mutate(f); assert.throws(() => validateSemanticReview(f)); }
});

test('AC-0267: strict frozen bindings and review fields cannot be aliased or rebound', () => {
  for (const field of ['protocol_sha256', 'receipt_sha256', 'rubric_sha256', 'run_id', 'role']) {
    const f = fixture(); replaceReview(f, (review) => { review[field] = field.endsWith('sha256') ? 'f'.repeat(64) : 'different'; });
    assert.throws(() => validateSemanticReview(f));
  }
  for (const change of [(r) => { r.extra = true; }, (r) => { delete r.role; }, (r) => { r.verdict = 'approved'; },
    (r) => { r.reasons = []; }, (r) => { r.reasons = [' ']; }, (r) => { r.reasons = ['é'.repeat(513)]; },
    (r) => { r.reasons = Array(13).fill('reason'); }, (r) => { r.reasons = Array(5).fill('a'.repeat(1024)); }]) {
    const f = fixture(); replaceReview(f, change); assert.throws(() => validateSemanticReview(f));
  }
  for (const run_id of ['', '.', '..', '../escape', 'with spaces', 'é', 'a'.repeat(129)])
    assert.throws(() => validateBinding({ ...fixture().binding, run_id }));
  assert.throws(() => validateBinding({ ...fixture().binding, owner_approved: true }));
});

test('AC-0267: exactly one strict fenced block, duplicate-key and byte limits are enforced', () => {
  for (const alter of [(body) => body + '\n```json\n{}\n```', (body) => body.replace('```json', '```JSON'),
    (body) => body.replace('```json', '```json extra'), (body) => body.replace('```json', '```'),
    (body) => body.replace('"verdict":"pass"', '"verdict":"fail","verdict":"pass"'),
    () => 'a'.repeat(32769)]) {
    const f = fixture(); f.rest.body = alter(f.rest.body); f.graphql.data.node.body = f.rest.body;
    assert.throws(() => validateSemanticReview(f));
  }
  assert.throws(() => strictJson('{"role":"analyst","r\\u006fle":"auditor"}'), /Duplicate/u);
  assert.throws(() => strictJson('{"nested":{"a":1,"a":2}}'), /Duplicate/u);
  assert.throws(() => strictJson('['.repeat(18) + '0' + ']'.repeat(18)), /structure/u);
  assert.throws(() => strictJson('"' + 'a'.repeat(8192) + '"'), /bound/u);
  const f = fixture(); f.graphql.padding = 'a'.repeat(128 * 1024); assert.throws(() => validateSemanticReview(f), /bound/u);
});

test('AC-0267: transport verifies viewer first then only reads the selected REST and GraphQL node', async () => {
  const f = fixture(); const calls = [];
  const result = await verifyReview({ ...f, transport: transportFor(f, calls) });
  assert.equal(result.review.verdict, 'pass'); assert.equal(calls.length, 4);
  assert.equal(calls[0][0], 'graphql'); assert.match(calls[0][2], /viewer/u);
  assert.deepEqual(calls[1], [`repos/true-line/cartograph/issues/comments/${f.commentId}`]);
  assert.equal(calls[2].at(-1), `id=${f.rest.node_id}`);
  assert.deepEqual(calls[3], ['repos/true-line/cartograph/issues/652/comments?per_page=20&page=1', '--jq', LISTING_PROJECTION]);
  for (const args of calls) assert.ok(!args.some((arg) => /mutation|--method|PATCH|DELETE/u.test(arg)));
  const bad = fixture(); bad.viewer.data.viewer.login = 'human'; const badCalls = [];
  await assert.rejects(verifyReview({ ...bad, transport: transportFor(bad, badCalls) }));
  assert.equal(badCalls.length, 1);
  await assert.rejects(verifyReview({ ...f, transport: async () => { throw new Error('Deleted'); } }));
  const oversized = fixture(); oversized.viewer.padding = 'a'.repeat(128 * 1024); const oversizedCalls = [];
  await assert.rejects(verifyReview({ ...oversized, transport: transportFor(oversized, oversizedCalls) }), /bound/u);
  assert.equal(oversizedCalls.length, 1);
  const changed = fixture(); const changedCalls = [];
  const transport = transportFor(changed, changedCalls);
  const snapshotResult = await verifyReview({ ...changed, transport: async (args) => {
    if (!changedCalls.length) changed.binding.run_id = 'mutated-during-fetch';
    return transport(args);
  } });
  assert.equal(snapshotResult.binding.run_id, 'v7-analyst-20261020');
});

test('AC-0267: CLI writes an exclusive fresh attestation and never treats local data as identity proof', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'v7-review-test-'));
  try {
    const f = fixture(); const bindingPath = path.join(directory, 'sealed.json'); const output = path.join(directory, 'attestation.json');
    await writeFile(bindingPath, JSON.stringify(f.binding));
    const argv = ['--binding', bindingPath, '--comment-id', String(f.commentId), '--output', output];
    const result = await main(argv, { transport: transportFor(f) });
    assert.deepEqual(JSON.parse(await readFile(output, 'utf8')), result);
    await assert.rejects(main(argv, { transport: transportFor(f) }), /EEXIST/u);
    const target = path.join(directory, 'target'); await writeFile(target, 'unchanged');
    const link = path.join(directory, 'link'); await symlink(target, link);
    await assert.rejects(main([...argv.slice(0, 5), link], { transport: transportFor(f) }), /EEXIST/u);
    assert.equal(await readFile(target, 'utf8'), 'unchanged');
    await writeFile(bindingPath, JSON.stringify(f.binding).replace('"role":"analyst"', '"role":"analyst","role":"auditor"'));
    let fetched = false;
    await assert.rejects(main(argv, { transport: async () => { fetched = true; } }), /Duplicate/u);
    assert.equal(fetched, false);
    await writeFile(bindingPath, JSON.stringify(f.binding));
    const invalidOutput = path.join(directory, 'must-not-exist.json');
    const edited = fixture(); edited.graphql.data.node.lastEditedAt = edited.rest.created_at;
    await assert.rejects(main([...argv.slice(0, 5), invalidOutput], { transport: transportFor(edited) }), /Edited/u);
    await assert.rejects(readFile(invalidOutput), /ENOENT/u);
    await assert.rejects(main(['--binding', bindingPath, '--comment-id', '1', '--comment-id', '2']));
  } finally { await rm(directory, { recursive: true, force: true }); }
});

function laterComment(f, verdict, { sameTime = false, change } = {}) {
  const later = structuredClone(f);
  later.rest.id += 1;
  later.rest.node_id = 'IC_newer';
  later.rest.html_url = `https://github.com/true-line/cartograph/issues/652#issuecomment-${later.rest.id}`;
  later.rest.created_at = sameTime ? f.rest.created_at : '2026-10-20T16:31:00Z';
  later.rest.updated_at = later.rest.created_at;
  replaceReview(later, (review) => { review.verdict = verdict; change?.(review); });
  return later.rest;
}

test('AC-0267: newest matching Claude App verdict supersedes earlier pass or fail', () => {
  for (const [oldVerdict, newVerdict] of [['pass', 'fail'], ['fail', 'pass'], ['pass', 'unreviewable']]) {
    const f = fixture(oldVerdict); const later = laterComment(f, newVerdict);
    assert.throws(() => validateLatestReview({ ...f, comments: [f.rest, later] }), /superseded/u);
    validateLatestReview({ binding: f.binding, commentId: later.id, rest: later, comments: [f.rest, later] });
  }
  const f = fixture(); const sameTime = laterComment(f, 'fail', { sameTime: true });
  assert.throws(() => validateLatestReview({ ...f, comments: [sameTime, f.rest] }), /superseded/u);
  const edited = laterComment(f, 'fail'); edited.updated_at = '2026-10-20T16:32:00Z';
  assert.throws(() => validateLatestReview({ ...f, comments: [f.rest, edited] }), /superseded/u);
});

test('AC-0267: other runs, roles, hashes and authors do not supersede a matching review', () => {
  for (const change of [(r) => { r.run_id = 'different-run'; }, (r) => { r.role = 'auditor'; },
    (r) => { r.protocol_sha256 = 'f'.repeat(64); }, (r) => { r.receipt_sha256 = 'f'.repeat(64); },
    (r) => { r.rubric_sha256 = 'f'.repeat(64); }]) {
    const f = fixture(); validateLatestReview({ ...f, comments: [f.rest, laterComment(f, 'fail', { change })] });
  }
  const f = fixture(); const other = laterComment(f, 'fail'); other.performed_via_github_app.slug = 'other-app';
  validateLatestReview({ ...f, comments: [f.rest, other] });
  other.performed_via_github_app.slug = 'trueline-claude-agent'; other.body = 'Not a valid semantic review';
  validateLatestReview({ ...f, comments: [f.rest, other] });
});

test('AC-0267: paginated listings are complete and listing failures or bounds stop attestation', async () => {
  const f = fixture(); const unrelated = (id) => ({ id, body: 'Ordinary issue discussion', user: { login: 'other' } });
  const first = [f.rest, ...Array.from({ length: 19 }, (_, i) => unrelated(i + 1))];
  const calls = []; const base = transportFor(f, calls);
  const newer = laterComment(f, 'fail');
  await assert.rejects(verifyReview({ ...f, transport: async (args) => {
    if (args[0].includes('/652/comments?')) { calls.push(args); return args[0].endsWith('page=1') ? first : [newer]; }
    return base(args);
  } }), /superseded/u);
  assert.ok(calls.at(-1)[0].endsWith('page=2'));
  for (const listing of [async () => { throw new Error('Listing failed'); }, async () => ({ message: 'Unavailable' }),
    async () => Array(21).fill(f.rest), async () => [{ ...f.rest, body: 'a'.repeat(128 * 1024) }],
    async () => []]) {
    const localCalls = []; const normal = transportFor(f, localCalls);
    await assert.rejects(verifyReview({ ...f, transport: (args) => args[0].includes('/652/comments?') ? listing() : normal(args) }));
  }
  const boundCalls = []; const normal = transportFor(f, boundCalls); let pages = 0;
  await assert.rejects(verifyReview({ ...f, transport: (args) => {
    if (!args[0].includes('/652/comments?')) return normal(args);
    pages += 1; return Array.from({ length: 20 }, (_, i) => unrelated(pages * 20 + i));
  } }), /pagination bound/u);
  assert.equal(pages, 20);
  assert.throws(() => validateLatestReview({ ...f, comments: [f.rest, f.rest] }), /duplicate/u);
  const changed = structuredClone(f.rest); changed.body += '\n';
  assert.throws(() => validateLatestReview({ ...f, comments: [changed] }), /changed/u);
});

// AC-0267: the listing projection keeps a long issue inside the unchanged page
// bound without dropping any comment or any admissible review body.
test('AC-0267: listing projection keeps every comment and only admissible review bodies', { skip: spawnSync('jq', ['--version']).status !== 0 }, () => {
  const f = fixture();
  const noise = { reactions: { total_count: 3 }, author_association: 'MEMBER', body_html: 'x'.repeat(4096) };
  const page = [
    { ...f.rest, ...noise, user: { ...f.rest.user, id: 1, avatar_url: 'https://example.invalid/a' },
      performed_via_github_app: { slug: 'trueline-claude-agent', id: 9, permissions: {} } },
    { ...f.rest, ...noise, id: 2, body: 'y'.repeat(100 * 1024), user: { login: 'someone', type: 'User' }, performed_via_github_app: null },
    { ...f.rest, ...noise, id: 3, body: 'z'.repeat(32 * 1024 + 1) },
    { ...f.rest, ...noise, id: 4, performed_via_github_app: { slug: 'other-app' } },
  ];
  assert.ok(Buffer.byteLength(JSON.stringify(page)) > 128 * 1024);
  const run = spawnSync('jq', ['-c', LISTING_PROJECTION], { input: JSON.stringify(page), encoding: 'utf8' });
  assert.equal(run.status, 0, run.stderr);
  const projected = JSON.parse(run.stdout);
  assert.ok(Buffer.byteLength(run.stdout) < 128 * 1024);
  assert.deepEqual(projected[0], f.rest);
  assert.deepEqual(projected.map((comment) => comment.id), [f.commentId, 2, 3, 4]);
  assert.deepEqual(projected.slice(1).map((comment) => comment.body), [null, null, null]);
  assert.deepEqual(projected[1].performed_via_github_app, null);
  // The selected review is still found and still the latest matching one.
  validateLatestReview({ ...f, comments: projected });
  // A later matching review inside the bound keeps its body and supersedes.
  const newer = laterComment(f, 'fail');
  const again = JSON.parse(spawnSync('jq', ['-c', LISTING_PROJECTION], { input: JSON.stringify([...page, { ...newer, ...noise }]), encoding: 'utf8' }).stdout);
  assert.throws(() => validateLatestReview({ ...f, comments: again }), /superseded/u);
});

// AC-0464: explicit issue selection preserves all authenticated review checks.
test('AC-0464: captured review selects issue598 and rejects foreign or malformed issue', async () => {
  const f = fixture(); f.issue = 598;
  f.rest.issue_url = f.rest.issue_url.replace('652', '598');
  f.rest.html_url = f.rest.html_url.replace('652', '598');
  f.graphql.data.node.url = f.rest.html_url; f.graphql.data.node.issue.number = 598;
  const calls = []; const actual = await verifyReview({ ...f, transport: transportFor(f, calls) });
  assert.equal(actual.comment.url, f.rest.html_url);
  assert.ok(calls[3][0].includes('/598/comments?'));
  assert.throws(() => validateSemanticReview({ ...f, issue: 652 }));
  const newer = structuredClone(f.rest); newer.id += 1; newer.created_at = '2026-10-21T16:30:00Z';
  newer.body = newer.body.replace('\"verdict\":\"pass\"', '\"verdict\":\"fail\"');
  assert.throws(() => validateLatestReview({ ...f, comments: [f.rest, newer] }), /superseded/u);
  for (const issue of [0, -1, 1.5, NaN, Infinity, '598'])
    await assert.rejects(verifyReview({ ...f, issue, transport: async () => { throw new Error('must not fetch'); } }), /Invalid issue/u);
});
