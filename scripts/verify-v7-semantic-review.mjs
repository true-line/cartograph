// AC-0267: authenticated, read-only review orchestration. This attestation binds
// content; a local JSON file never proves reviewer identity or the owner's go.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { writeFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

const REPO = 'true-line/cartograph';
const ISSUE = 652;
const validIssue = (issue) => assert.ok(Number.isSafeInteger(issue) && issue > 0, 'Invalid issue identity');
const API_BYTES = 128 * 1024;
const BODY_BYTES = 32 * 1024;
const BINDING_BYTES = 8 * 1024;
const COMMENTS_PER_PAGE = 20;
const MAX_COMMENT_PAGES = 20;
const MAX_LISTING_BYTES = 2 * 1024 * 1024;
const sha256 = (value) => createHash('sha256').update(value, 'utf8').digest('hex');
const hash = (value) => typeof value === 'string' && /^[a-f0-9]{64}$/u.test(value);
const keys = (value, names) => value && typeof value === 'object' && !Array.isArray(value) &&
  Object.keys(value).length === names.length && names.every((name) => Object.hasOwn(value, name));
const idValid = (id) => Number.isSafeInteger(id) && id > 0;
const bounded = (value, maximum) => {
  assert.ok(typeof value === 'string' && Buffer.byteLength(value, 'utf8') <= maximum, 'Payload exceeds its UTF-8 bound');
  return value;
};

// JSON.parse alone silently keeps the last duplicate key. Scan the original
// bounded bytes too, including escaped keys; syntax is admitted by JSON.parse.
export function strictJson(raw, maximum = BINDING_BYTES) {
  bounded(raw, maximum);
  const result = JSON.parse(raw);
  let offset = 0;
  let count = 0;
  const whitespace = () => { while (/\s/u.test(raw[offset] ?? '') && offset < raw.length) offset += 1; };
  const string = () => {
    const begin = offset++;
    while (offset < raw.length) {
      const ch = raw[offset++];
      if (ch === '\\') offset += 1;
      else if (ch === '"') return JSON.parse(raw.slice(begin, offset));
    }
    throw new Error('Unterminated JSON string');
  };
  const value = (depth) => {
    assert.ok(depth <= 16 && ++count <= 4096, 'JSON structure exceeds its bound');
    whitespace();
    if (raw[offset] === '{') {
      offset += 1; whitespace();
      const seen = new Set();
      if (raw[offset] !== '}') for (;;) {
        const key = string();
        assert.ok(!seen.has(key), 'Duplicate JSON key'); seen.add(key);
        whitespace(); offset += 1; value(depth + 1); whitespace();
        if (raw[offset] !== ',') break;
        offset += 1; whitespace();
      }
      offset += 1;
    } else if (raw[offset] === '[') {
      offset += 1; whitespace();
      if (raw[offset] !== ']') for (;;) {
        value(depth + 1); whitespace();
        if (raw[offset] !== ',') break;
        offset += 1;
      }
      offset += 1;
    } else if (raw[offset] === '"') string();
    else {
      const token = /^(?:true|false|null|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/u.exec(raw.slice(offset));
      assert.ok(token, 'Invalid JSON token'); offset += token[0].length;
    }
  };
  value(0); whitespace(); assert.equal(offset, raw.length, 'Trailing JSON');
  return result;
}

export function validateBinding(binding) {
  assert.ok(keys(binding, ['schema', 'protocol_sha256', 'run_id', 'role', 'receipt_sha256', 'rubric_sha256']), 'Unknown binding shape');
  assert.equal(binding.schema, 'cartograph-v7-semantic-review-binding@1');
  assert.ok(['analyst', 'auditor'].includes(binding.role), 'Invalid role');
  assert.ok(typeof binding.run_id === 'string' && /^[A-Za-z0-9._-]{1,128}$/u.test(binding.run_id) &&
    !['.', '..'].includes(binding.run_id), 'Invalid run identity');
  for (const field of ['protocol_sha256', 'receipt_sha256', 'rubric_sha256']) assert.ok(hash(binding[field]), `Invalid ${field}`);
  bounded(JSON.stringify(binding), BINDING_BYTES);
  return binding;
}

export function validateViewer(viewerPayload) {
  assert.ok(!viewerPayload.errors, 'GitHub viewer query failed');
  const viewer = viewerPayload.data?.viewer;
  // Observed installation-token viewer representation differs from Bot authors.
  assert.equal(viewer?.__typename, 'User', 'Unexpected authenticated viewer type');
  assert.equal(viewer.login, 'trueline-codex-agent[bot]', 'Wrong authenticated viewer');
}

function reviewBlock(body) {
  bounded(body, BODY_BYTES);
  const fences = [...body.matchAll(/^```([^\r\n]*)\r?$/gmu)];
  assert.equal(fences.length, 2, 'Expected exactly one fenced JSON block');
  assert.equal(fences[0][1], 'json', 'Expected a strict json fence');
  assert.equal(fences[1][1], '', 'Invalid closing fence');
  const begin = fences[0].index + fences[0][0].length;
  const raw = body.slice(begin, fences[1].index).trim();
  const review = strictJson(raw, BODY_BYTES);
  assert.ok(keys(review, ['schema', 'protocol_sha256', 'run_id', 'role', 'receipt_sha256', 'rubric_sha256', 'verdict', 'reasons']), 'Unknown review shape');
  assert.equal(review.schema, 'cartograph-v7-semantic-review@1');
  assert.ok(['pass', 'fail', 'unreviewable'].includes(review.verdict), 'Invalid verdict');
  assert.ok(Array.isArray(review.reasons) && review.reasons.length >= 1 && review.reasons.length <= 12, 'Invalid reasons');
  let reasonBytes = 0;
  for (const reason of review.reasons) {
    bounded(reason, 1024); assert.ok(reason.trim().length > 0, 'Empty reason');
    reasonBytes += Buffer.byteLength(reason, 'utf8');
  }
  assert.ok(reasonBytes <= 4096, 'Reasons exceed aggregate bound');
  return review;
}

// Pure validation accepts API-shaped inputs for deterministic offline tests.
// Authentication is established by verifyReview's live transport, not this JSON.
export function validateSemanticReview({ binding, commentId, viewer, rest, graphql, issue = ISSUE }) {
  validIssue(issue); validateBinding(binding); validateViewer(viewer);
  assert.ok(idValid(commentId), 'Invalid comment identity');
  for (const payload of [viewer, rest, graphql]) bounded(JSON.stringify(payload), API_BYTES);
  assert.ok(!graphql.errors, 'GitHub comment query failed');
  const node = graphql.data?.node;
  assert.equal(rest.id, commentId, 'REST comment mismatch');
  assert.equal(rest.issue_url, `https://api.github.com/repos/${REPO}/issues/${issue}`, 'Wrong issue/repository');
  assert.equal(rest.user?.login, 'trueline-claude-agent[bot]', 'Wrong REST reviewer');
  assert.equal(rest.user?.type, 'Bot', 'Reviewer is not a bot');
  assert.equal(rest.performed_via_github_app?.slug, 'trueline-claude-agent', 'Wrong reviewer App');
  assert.equal(node?.__typename, 'IssueComment', 'Wrong GraphQL node kind');
  assert.ok(typeof rest.node_id === 'string' && rest.node_id.length <= 256 && rest.node_id.length > 0, 'Invalid node identity');
  assert.equal(node.id, rest.node_id, 'GraphQL node mismatch');
  assert.equal(node.databaseId, rest.id, 'Comment database identity mismatch');
  assert.equal(node.author?.__typename, 'Bot', 'Wrong GraphQL author type');
  assert.equal(node.author?.login, 'trueline-claude-agent', 'Wrong GraphQL reviewer');
  assert.equal(node.issue?.number, issue, 'Wrong GraphQL issue');
  assert.equal(node.issue?.repository?.nameWithOwner, REPO, 'Wrong GraphQL repository');
  const url = `https://github.com/${REPO}/issues/${issue}#issuecomment-${commentId}`;
  assert.equal(rest.html_url, url, 'Wrong comment URL'); assert.equal(node.url, url, 'API URL disagreement');
  assert.ok(typeof rest.created_at === 'string' && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/u.test(rest.created_at) &&
    Number.isFinite(Date.parse(rest.created_at)) && new Date(rest.created_at).toISOString().replace('.000Z', 'Z') === rest.created_at, 'Invalid creation timestamp');
  assert.equal(node.createdAt, rest.created_at, 'API creation-time disagreement');
  assert.equal(node.lastEditedAt, null, 'Edited review');
  assert.equal(node.includesCreatedEdit, false, 'Created edit review');
  assert.equal(rest.updated_at, rest.created_at, 'REST reports an edited comment');
  assert.equal(node.body, rest.body, 'API body disagreement');
  const review = reviewBlock(rest.body);
  for (const field of ['protocol_sha256', 'run_id', 'role', 'receipt_sha256', 'rubric_sha256'])
    assert.equal(review[field], binding[field], `Review ${field} does not match sealed binding`);
  return {
    schema: 'cartograph-v7-semantic-review-attestation@1', binding: structuredClone(binding),
    comment: { database_id: rest.id, url, created_at: rest.created_at, body_sha256: sha256(rest.body) },
    review,
  };
}

const VIEWER_QUERY = 'query { viewer { __typename login } }';
const COMMENT_QUERY = 'query($id: ID!) { node(id: $id) { __typename ... on IssueComment { id databaseId url createdAt body lastEditedAt includesCreatedEdit author { __typename login } issue { number repository { nameWithOwner } } } } }';

function matchesBinding(review, binding) {
  return ['protocol_sha256', 'run_id', 'role', 'receipt_sha256', 'rubric_sha256'].every((field) => review[field] === binding[field]);
}

// Corrections append new comments. A later matching verdict supersedes every
// earlier verdict, including a favorable one; edited candidates cannot revive it.
export function validateLatestReview({ binding, commentId, rest, comments, issue = ISSUE }) {
  validIssue(issue); validateBinding(binding);
  assert.ok(Array.isArray(comments) && comments.length <= COMMENTS_PER_PAGE * MAX_COMMENT_PAGES, 'Comment listing exceeds count bound');
  bounded(JSON.stringify(comments), MAX_LISTING_BYTES);
  const ids = new Set();
  let selected;
  let latest;
  for (const comment of comments) {
    assert.ok(comment && idValid(comment.id) && !ids.has(comment.id), 'Invalid/duplicate listed comment identity');
    ids.add(comment.id);
    if (comment.id === commentId) selected = comment;
    if (comment.user?.login !== 'trueline-claude-agent[bot]' || comment.user?.type !== 'Bot' ||
        comment.performed_via_github_app?.slug !== 'trueline-claude-agent') continue;
    let review;
    try { review = reviewBlock(comment.body); } catch { continue; }
    if (!matchesBinding(review, binding)) continue;
    assert.equal(comment.issue_url, `https://api.github.com/repos/${REPO}/issues/${issue}`, 'Listed review belongs to another issue');
    assert.ok(typeof comment.created_at === 'string' && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/u.test(comment.created_at) &&
      Number.isFinite(Date.parse(comment.created_at)) && new Date(comment.created_at).toISOString().replace('.000Z', 'Z') === comment.created_at, 'Invalid listed review timestamp');
    if (!latest || comment.created_at > latest.created_at ||
        (comment.created_at === latest.created_at && comment.id > latest.id)) latest = comment;
  }
  assert.ok(selected, 'Selected comment is absent from the complete issue listing');
  for (const field of ['node_id', 'html_url', 'body', 'created_at', 'updated_at'])
    assert.equal(selected[field], rest[field], 'Selected comment changed during review verification');
  assert.equal(latest?.id, commentId, 'Selected semantic review has been superseded');
}

async function listIssueComments(transport, issue) {
  const comments = [];
  let bytes = 0;
  for (let page = 1; page <= MAX_COMMENT_PAGES; page += 1) {
    const result = await transport([`repos/${REPO}/issues/${issue}/comments?per_page=${COMMENTS_PER_PAGE}&page=${page}`]);
    const encoded = JSON.stringify(result);
    bounded(encoded, API_BYTES); bytes += Buffer.byteLength(encoded, 'utf8');
    assert.ok(bytes <= MAX_LISTING_BYTES, 'Comment listing exceeds aggregate byte bound');
    assert.ok(Array.isArray(result) && result.length <= COMMENTS_PER_PAGE, 'Invalid comment listing page');
    comments.push(...result);
    if (result.length < COMMENTS_PER_PAGE) return comments;
  }
  throw new Error('Comment listing exceeds pagination bound');
}

export async function verifyReview({ binding, commentId, transport, issue = ISSUE }) {
  validIssue(issue); binding = structuredClone(validateBinding(binding)); assert.ok(idValid(commentId), 'Invalid comment identity');
  const viewer = await transport(['graphql', '-f', `query=${VIEWER_QUERY}`]);
  bounded(JSON.stringify(viewer), API_BYTES);
  validateViewer(viewer); // Do not even fetch the review using an ambient human token.
  const rest = await transport([`repos/${REPO}/issues/comments/${commentId}`]);
  bounded(JSON.stringify(rest), API_BYTES);
  assert.ok(typeof rest.node_id === 'string' && rest.node_id.length > 0 && rest.node_id.length <= 256, 'Missing REST node identity');
  const graphql = await transport(['graphql', '-f', `query=${COMMENT_QUERY}`, '-f', `id=${rest.node_id}`]);
  const attestation = validateSemanticReview({ binding, commentId, viewer, rest, graphql, issue });
  const comments = await listIssueComments(transport, issue);
  validateLatestReview({ binding, commentId, rest, comments, issue });
  return attestation;
}

export function githubTransport(args) {
  return new Promise((resolve, reject) => {
    const child = spawn('gh', ['api', '--hostname', 'github.com', ...args], {
      env: { ...process.env, GH_HOST: 'github.com', GH_AGENT_APP: 'trueline-codex-agent', GH_PROMPT_DISABLED: '1' },
      stdio: ['ignore', 'pipe', 'pipe'], timeout: 30_000,
    });
    let size = 0;
    const chunks = [];
    let failure;
    child.stdout.on('data', (chunk) => {
      size += chunk.length;
      if (size > API_BYTES) { failure = new Error('GitHub API payload exceeds bound'); child.kill(); }
      else chunks.push(chunk);
    });
    // Drain but never print possibly sensitive authentication diagnostics.
    child.stderr.on('data', () => {});
    child.once('error', reject);
    child.once('close', (code) => {
      if (failure) reject(failure);
      else if (code !== 0) reject(new Error('Read-only GitHub API request failed'));
      else {
        try { resolve(strictJson(Buffer.concat(chunks).toString('utf8'), API_BYTES)); }
        catch (error) { reject(error); }
      }
    });
  });
}

async function readBinding(file) {
  const chunks = []; let size = 0;
  for await (const chunk of createReadStream(file)) {
    size += chunk.length; assert.ok(size <= BINDING_BYTES, 'Binding exceeds bound'); chunks.push(chunk);
  }
  return validateBinding(strictJson(Buffer.concat(chunks).toString('utf8')));
}

export async function main(argv, { transport = githubTransport } = {}) {
  assert.ok([6, 8].includes(argv.length), 'Usage: --binding FILE --comment-id ID --output NEW_FILE [--issue NUMBER]');
  const options = new Map();
  for (let i = 0; i < argv.length; i += 2) {
    assert.ok(['--binding', '--comment-id', '--output', '--issue'].includes(argv[i]) && !options.has(argv[i]), 'Unknown/duplicate CLI option');
    options.set(argv[i], argv[i + 1]);
  }
  assert.ok(/^[1-9]\d{0,15}$/u.test(options.get('--comment-id')), 'Invalid comment identity');
  assert.ok(['--binding', '--comment-id', '--output'].every((key) => options.has(key)), 'Missing required option');
  const rawIssue = options.get('--issue') ?? String(ISSUE);
  assert.ok(/^[1-9]\d{0,15}$/u.test(rawIssue), 'Invalid issue identity');
  const issue = Number(rawIssue); validIssue(issue);
  const binding = await readBinding(options.get('--binding'));
  const attestation = await verifyReview({ binding, commentId: Number(options.get('--comment-id')), transport, issue });
  await writeFile(options.get('--output'), `${JSON.stringify(attestation, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  return attestation;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).catch((error) => { console.error(error.message); process.exitCode = 1; });
}
