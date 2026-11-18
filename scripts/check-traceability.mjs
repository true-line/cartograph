#!/usr/bin/env node
// Traceability gate: keeps docs/user_stories.md, docs/US-TM.md, docs/adr/,
// and docs/test-map.md mutually consistent. Runs in CI on every PR (see
// .github/workflows/ci.yml).
//
// Checks:
//   1. Every US in user_stories.md has a row in the US-TM matrix, and vice versa.
//   2. AC ids are globally unique; every US defines at least one AC.
//   3. Each matrix row's AC range covers exactly the ACs its US defines.
//   4. Every ADR referenced in the matrix exists as a file in docs/adr/.
//   5. Every ADR file has a Status line, and appears in the US-TM ADR index.
//   6. Every T-XXXX reserved in US-TM has a row in docs/test-map.md, and every
//      automated reference there names a test/story that actually exists;
//      manual references name a procedure in docs/manual-tests.md.
//   7. Every story's Status is a value of the Status enum in user_stories.md.
//   8. A T-id whose every `rust:` test is #[ignore]d fails unless its note
//      names where and when the test runs (a `manual:MT-...` procedure or a
//      `ci:<job>` lane that runs ignored tests); a T-id with some ignored
//      tests is listed. Pre-existing violations live in an only-shrinking
//      baseline (#1758).

import { readFile, readdir } from 'node:fs/promises';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const errors = [];
const fail = (msg) => errors.push(msg);

// --- user_stories.md ---------------------------------------------------------
const storiesSrc = await readFile(path.join(ROOT, 'docs/user_stories.md'), 'utf8');

/** @type {Map<string, Set<string>>} US id -> AC ids defined under it */
const storyAcs = new Map();
let currentUs = null;
const seenAcs = new Map(); // AC id -> owning US
const statusEnumLine = storiesSrc.match(/^\*\*Status enum:\*\*\s*`([^`]+)`/mu);
const statusEnum = new Set(
  (statusEnumLine?.[1] ?? '')
    .split('·')
    .map((s) => s.trim())
    .filter(Boolean),
);
if (statusEnum.size === 0) fail('user_stories.md has no "**Status enum:** `...`" line');
const storyStatuses = new Map(); // US id -> [status values]
for (const line of storiesSrc.split('\n')) {
  const usHeading = line.match(/^###\s+(US-\d{4})\s+—/u);
  if (usHeading) {
    currentUs = usHeading[1];
    if (storyAcs.has(currentUs)) fail(`Duplicate story heading ${currentUs} in user_stories.md`);
    storyAcs.set(currentUs, new Set());
    storyStatuses.set(currentUs, []);
    continue;
  }
  const status = currentUs && line.match(/^-\s.*\*\*Status:\*\*\s*([^\s·]+)/u);
  if (status) storyStatuses.get(currentUs).push(status[1]);
  for (const ac of line.matchAll(/\*\*(AC-\d{4})\*\*/gu) ?? []) {
    if (!currentUs) {
      fail(`${ac[1]} appears before any US heading in user_stories.md`);
      continue;
    }
    if (seenAcs.has(ac[1])) fail(`${ac[1]} defined under both ${seenAcs.get(ac[1])} and ${currentUs}`);
    seenAcs.set(ac[1], currentUs);
    storyAcs.get(currentUs).add(ac[1]);
  }
}
for (const [us, acs] of storyAcs) {
  if (acs.size === 0) fail(`${us} defines no acceptance criteria`);
}
for (const [us, statuses] of storyStatuses) {
  if (statuses.length !== 1) {
    fail(`${us} has ${statuses.length} "**Status:**" values; expected exactly one`);
  } else if (!statusEnum.has(statuses[0])) {
    fail(
      `${us} has Status "${statuses[0]}", which is not in the Status enum (${[...statusEnum].join(' · ')})`,
    );
  }
}

// --- US-TM.md ----------------------------------------------------------------
const matrixSrc = await readFile(path.join(ROOT, 'docs/US-TM.md'), 'utf8');

const expandAcRange = (cell) => {
  // "AC-0001..0003" or "AC-0013..0014" or a single "AC-0007"
  const acs = new Set();
  for (const m of cell.matchAll(/AC-(\d{4})(?:\.\.(\d{4}))?/gu)) {
    const start = Number(m[1]);
    const end = Number(m[2] ?? m[1]);
    for (let i = start; i <= end; i++) acs.add(`AC-${String(i).padStart(4, '0')}`);
  }
  return acs;
};

const matrixUs = new Map(); // US id -> { acs, adrs }
const matrixAdrRefs = new Set();
for (const line of matrixSrc.split('\n')) {
  const row = line.match(/^\|\s*(US-\d{4})\s*\|([^|]*)\|/u);
  if (!row) continue;
  const us = row[1];
  const adrs = new Set([...line.matchAll(/ADR-\d{4}/gu)].map((m) => m[0]));
  for (const adr of adrs) matrixAdrRefs.add(adr);
  matrixUs.set(us, { acs: expandAcRange(row[2]), adrs });
}

for (const us of storyAcs.keys()) {
  if (!matrixUs.has(us)) fail(`${us} exists in user_stories.md but has no row in US-TM.md`);
}
for (const us of matrixUs.keys()) {
  if (!storyAcs.has(us)) fail(`${us} has a row in US-TM.md but no story in user_stories.md`);
}
for (const [us, { acs }] of matrixUs) {
  const defined = storyAcs.get(us);
  if (!defined) continue;
  for (const ac of acs) {
    if (!defined.has(ac)) fail(`US-TM.md maps ${ac} to ${us}, but ${us} does not define it`);
  }
  for (const ac of defined) {
    if (!acs.has(ac)) fail(`${us} defines ${ac}, but the US-TM.md row does not cover it`);
  }
}

// --- docs/adr/ ---------------------------------------------------------------
const adrDir = path.join(ROOT, 'docs/adr');
const adrFiles = (await readdir(adrDir)).filter((f) => f.endsWith('.md'));
const adrIds = new Set();
for (const file of adrFiles) {
  const m = file.match(/^(ADR-\d{4})-[a-z0-9-]+\.md$/u);
  if (!m) {
    fail(`ADR file name "${file}" does not match ADR-NNNN-kebab-slug.md`);
    continue;
  }
  if (adrIds.has(m[1])) fail(`Duplicate ADR id ${m[1]} in docs/adr/`);
  adrIds.add(m[1]);
  const body = await readFile(path.join(adrDir, file), 'utf8');
  if (!/^-\s+\*\*Status:\*\*/mu.test(body)) fail(`${file} has no "- **Status:**" line`);
}

for (const adr of matrixAdrRefs) {
  if (!adrIds.has(adr)) fail(`US-TM.md references ${adr}, but docs/adr/ has no such file`);
}
// The ADR index table at the bottom of US-TM.md must list every ADR on disk.
const indexedAdrs = new Set([...matrixSrc.matchAll(/^\|\s*(ADR-\d{4})\s*\|/gmu)].map((m) => m[1]));
for (const adr of adrIds) {
  if (!indexedAdrs.has(adr)) fail(`${adr} exists in docs/adr/ but is missing from the US-TM.md ADR index`);
}
for (const adr of indexedAdrs) {
  if (!adrIds.has(adr)) fail(`US-TM.md ADR index lists ${adr}, but docs/adr/ has no such file`);
}

// --- docs/test-map.md (T-XXXX -> realized tests) -------------------------------
const expandTestRange = (cell) => {
  const ids = new Set();
  for (const m of cell.matchAll(/T-(\d{4})(?:\.\.(\d{4}))?/gu)) {
    const start = Number(m[1]);
    const end = Number(m[2] ?? m[1]);
    for (let i = start; i <= end; i++) ids.add(`T-${String(i).padStart(4, '0')}`);
  }
  return ids;
};

// US-TM reserves AC and T ranges in parallel order (nth T realizes nth AC),
// so the expected T -> AC pairing is derivable and enforced below.
const reservedTestIds = new Set();
const expectedAcForTest = new Map();
for (const line of matrixSrc.split('\n')) {
  const row = line.match(/^\|\s*(US-\d{4})\s*\|([^|]*)\|/u);
  if (!row) continue;
  const acs = [...expandAcRange(row[2])];
  const tids = [...expandTestRange(line)];
  if (tids.length > 0 && tids.length !== acs.length) {
    fail(
      `${row[1]}: US-TM reserves ${tids.length} test ids for ${acs.length} ACs — ranges must pair 1:1`,
    );
  }
  tids.forEach((tid, i) => {
    reservedTestIds.add(tid);
    if (acs[i]) expectedAcForTest.set(tid, acs[i]);
  });
}

const mapSrc = await readFile(path.join(ROOT, 'docs/test-map.md'), 'utf8');
const manualSrc = await readFile(path.join(ROOT, 'docs/manual-tests.md'), 'utf8');
const manualIds = new Set([...manualSrc.matchAll(/^##\s+(MT-[A-Z0-9-]+)/gmu)].map((m) => m[1]));

// Collect the realizable universe once: rust test fns and storybook stories.
async function collectFiles(dir, suffix, out) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const p = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === 'node_modules' || entry.name === 'target' || entry.name.startsWith('.'))
        continue;
      await collectFiles(p, suffix, out);
    } else if (entry.name.endsWith(suffix)) {
      out.push(p);
    }
  }
}

// Only #[test]-annotated functions count as realized coverage — a `rust:`
// ref pointing at a production/helper fn must NOT satisfy the gate.
// Each name maps to whether it is #[ignore]d: true only when every
// same-named #[test] fn in the crate carries #[ignore] (CI skips it).
const rustFns = new Map(); // crate name -> Map<#[test] fn name, ignored>
for (const base of ['crates', 'src-tauri']) {
  const files = [];
  await collectFiles(path.join(ROOT, base), '.rs', files);
  for (const file of files) {
    const crate =
      base === 'src-tauri' ? 'app' : path.relative(path.join(ROOT, 'crates'), file).split(path.sep)[0];
    const fns = rustFns.get(crate) ?? new Map();
    let armed = false; // saw a #[test]-like attribute; other attrs/comments may sit between
    let ignored = false; // saw #[ignore] in the same attribute block (before or after #[test])
    for (const line of (await readFile(file, 'utf8')).split('\n')) {
      if (/^\s*#\[(?:[\w:]+::)?test(?:[\](]|$)/u.test(line)) {
        armed = true;
        continue;
      }
      if (/^\s*#\[ignore\b/u.test(line)) {
        ignored = true;
        continue;
      }
      if (/^\s*(#\[|\/\/)/u.test(line) || line.trim() === '') continue;
      const m = line.match(/\bfn\s+(\w+)\s*[(<]/u);
      if (m && armed) fns.set(m[1], (fns.get(m[1]) ?? true) && ignored);
      armed = false;
      ignored = false;
    }
    rustFns.set(crate, fns);
  }
}

const storyFiles = [];
await collectFiles(path.join(ROOT, 'ui/src'), '.stories.tsx', storyFiles);
const stories = new Set(); // "Title/Export"
for (const file of storyFiles) {
  const src = await readFile(file, 'utf8');
  const title = src.match(/title:\s*'([^']+)'/u)?.[1];
  if (!title) continue;
  for (const m of src.matchAll(/^export const (\w+)/gmu)) {
    stories.add(`${title}/${m[1]}`);
  }
}

// CI lanes a test-map note may name with `ci:<job>`: workflow jobs whose
// body runs ignored tests (`--ignored` / `--include-ignored`).
const ignoredLanes = new Set();
const workflowDir = path.join(ROOT, '.github/workflows');
for (const file of (await readdir(workflowDir)).filter((f) => /\.ya?ml$/u.test(f))) {
  const lines = (await readFile(path.join(workflowDir, file), 'utf8')).split('\n');
  const jobsAt = lines.findIndex((l) => /^jobs:\s*$/u.test(l));
  if (jobsAt < 0) continue;
  let job = null;
  for (const line of lines.slice(jobsAt + 1)) {
    if (/^\S/u.test(line)) break;
    const key = line.match(/^ {2}([\w-]+):\s*$/u);
    if (key) job = key[1];
    else if (job && /--(?:include-)?ignored\b/u.test(line)) ignoredLanes.add(job);
  }
}

// T-ids on main at #1758 whose every bound rust test is #[ignore]d and whose
// row names no lane that runs it. This list may only shrink: an entry that no
// longer violates fails the gate until it is removed, and new ids must not be
// added — name a `manual:MT-...` procedure or `ci:<job>` lane instead.
const IGNORED_ONLY_BASELINE = new Set([
  'T-0329', // opt-in source-built CARTOGRAPH_TEST_MCP_RELAY relay cases; no lane runs them yet
]);

/** T-id -> { total, ignored: [refs] } for kind=rust rows. */
const rustCoverage = new Map();

const checkRef = (tid, kind, ref) => {
  if (kind === 'rust') {
    const [crate, fn] = ref.split('::');
    if (!crate || !fn) return fail(`${tid}: rust ref "${ref}" is not crate::test_fn`);
    const cov = rustCoverage.get(tid) ?? { total: 0, ignored: [] };
    rustCoverage.set(tid, cov);
    cov.total += 1;
    if (!rustFns.get(crate)?.has(fn)) fail(`${tid}: rust test ${ref} not found in the tree`);
    else if (rustFns.get(crate).get(fn)) cov.ignored.push(ref);
  } else if (kind === 'story') {
    if (!stories.has(ref)) fail(`${tid}: story ${ref} not found in ui/src/**/*.stories.tsx`);
  } else if (kind === 'manual') {
    if (!manualIds.has(ref)) fail(`${tid}: manual procedure ${ref} not in docs/manual-tests.md`);
  } else {
    fail(`${tid}: unknown kind "${kind}"`);
  }
};

const mappedTestIds = new Set();
const runLaneNamed = new Set(); // T-ids whose note names a manual procedure or ci lane
for (const line of mapSrc.split('\n')) {
  const row = line.match(
    /^\|\s*(T-\d{4})\s*\|\s*(AC-\d{4})\s*\|\s*(\w+)\s*\|\s*([^|]+)\|\s*([^|]*)\|/u,
  );
  if (!row) continue;
  const [, tid, ac, kind, refCell, note] = row;
  if (mappedTestIds.has(tid)) fail(`Duplicate row for ${tid} in test-map.md`);
  mappedTestIds.add(tid);
  if (!reservedTestIds.has(tid)) fail(`${tid} is in test-map.md but reserved by no US-TM row`);
  if (!seenAcs.has(ac)) fail(`${tid} maps to ${ac}, which no story defines`);
  const expected = expectedAcForTest.get(tid);
  if (expected && expected !== ac) {
    fail(`${tid} maps to ${ac}, but US-TM reserves it for ${expected}`);
  }
  const refs = refCell.trim();
  if (kind === 'reserved') {
    if (refs !== '—' && refs !== '-') fail(`${tid} is reserved but has a reference "${refs}"`);
  } else {
    for (const ref of refs.split(',').map((r) => r.trim()).filter(Boolean)) {
      checkRef(tid, kind, ref);
    }
  }
  // Secondary realizations noted as story:X/Y or manual:MT-... are validated too.
  const secondaryRefs = [...note.matchAll(/\b(story|manual):([\w/ -]+?)(?=[,)]|\s+and\b|$)/gu)];
  for (const m of secondaryRefs) {
    checkRef(tid, m[1], m[2].trim());
    if (m[1] === 'manual' && manualIds.has(m[2].trim())) runLaneNamed.add(tid);
  }
  for (const m of note.matchAll(/\bci:([\w-]+)/gu)) {
    if (ignoredLanes.has(m[1])) runLaneNamed.add(tid);
    else fail(`${tid}: ci:${m[1]} names no workflow job that runs ignored tests (--ignored)`);
  }
  // A ref the pattern cannot terminate would otherwise be silently skipped —
  // a claimed realization the gate never checks. Reject it instead.
  const declared = [...note.matchAll(/\b(?:story|manual):/gu)].length;
  if (declared > secondaryRefs.length) {
    fail(
      `${tid} note has a story:/manual: reference the gate cannot parse — ` +
        'end it with ",", ")", " and", or end-of-line',
    );
  }
}
for (const tid of reservedTestIds) {
  if (!mappedTestIds.has(tid)) fail(`${tid} reserved in US-TM.md but missing from test-map.md`);
}

// --- ignored tests (#1758) ---------------------------------------------------
const partlyIgnored = [];
const ignoredOnly = new Set();
for (const [tid, { total, ignored }] of rustCoverage) {
  if (ignored.length === 0) continue;
  if (ignored.length < total) {
    partlyIgnored.push(`${tid}: ${ignored.length}/${total} ignored (${ignored.join(', ')})`);
    continue;
  }
  if (runLaneNamed.has(tid)) continue;
  ignoredOnly.add(tid);
  if (!IGNORED_ONLY_BASELINE.has(tid)) {
    fail(
      `${tid}: every bound rust test is #[ignore]d (${ignored.join(', ')}) and CI never runs it — ` +
        'name where and when it runs in the note (manual:MT-... or ci:<job>), or bind a test CI runs',
    );
  }
}
for (const tid of IGNORED_ONLY_BASELINE) {
  if (!ignoredOnly.has(tid)) {
    fail(
      `${tid} is in the ignored-test baseline but no longer violates — remove it (the baseline only shrinks)`,
    );
  }
}

// --- verdict -----------------------------------------------------------------
if (errors.length > 0) {
  console.error(`Traceability check failed (${errors.length} error${errors.length === 1 ? '' : 's'}):\n`);
  for (const e of errors) console.error(`  ✗ ${e}`);
  if (process.env.GITHUB_ACTIONS) {
    // Workflow commands render as annotations in the run summary UI, so the
    // failure reason is visible without opening the raw log.
    for (const e of errors) console.log(`::error title=Traceability::${e}`);
  }
  process.exit(1);
}
const realized = [...mappedTestIds].length;
if (partlyIgnored.length > 0 || ignoredOnly.size > 0) {
  console.log('T-ids bound to #[ignore]d rust tests (CI skips them):');
  for (const line of partlyIgnored) console.log(`  ~ ${line}`);
  for (const tid of ignoredOnly) console.log(`  ! ${tid}: all bound tests ignored (baselined, #1758)`);
}
console.log(
  `Traceability check passed: ${storyAcs.size} stories, ${seenAcs.size} ACs, ${adrIds.size} ADRs, ${realized} test ids mapped — consistent.`,
);
