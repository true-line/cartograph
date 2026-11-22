import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { spawnSync } from 'node:child_process';
import { claudeCodeRequirement } from './build-agent-runtime.mjs';

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
// A workflow's top-level `on:` block, which alone decides what triggers it.
const triggers = (workflow) => workflow.match(/^on:\n((?: .*\n|\n)*)/mu)?.[1] ?? '';
const PLAYBOOK_RUNTIME_PIN = '64ec289604e2409eb92fd718495df8a10761a20a';

test('version-cut preserves review, evidence, and immutable-tag gates', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/version-cut.yml'), 'utf8');
  assert.match(workflow, /npm run changeset:version/u);
  assert.doesNotMatch(workflow, /gh workflow run ci\.yml/u);
  assert.match(workflow, /event=push&head_sha=\$cut_commit/u);
  assert.match(workflow, /select\(\.path == "\.github\/workflows\/ci\.yml" and \.conclusion == "success"\)/u);
  // Tagging starts from the cut commit's own completed CI run instead of a
  // job polling for it while holding a runner (#1378).
  assert.match(triggers(workflow), /^\s{2}workflow_run:\n\s{4}workflows: \[CI\]\n\s{4}types: \[completed\]\n\s{4}branches: \[main\]/mu);
  assert.match(workflow, /github\.event\.workflow_run\.event == 'push'/u);
  assert.match(workflow, /github\.event\.workflow_run\.head_repository\.full_name == github\.repository/u);
  assert.match(workflow, /ref: \$\{\{ github\.event\.workflow_run\.head_sha \|\| github\.sha \}\}/u);
  assert.match(workflow, /"\$CI_CONCLUSION" != success/u);
  assert.match(workflow, /"\$cut_commit" != "\$CI_SHA"/u);
  assert.doesNotMatch(workflow, /for _ in \$\(seq/u);
  assert.doesNotMatch(workflow, /sleep 30/u);
  assert.doesNotMatch(workflow, /workflow_runs\[\].*\.name == "CI"/u);
  assert.match(workflow, /Manual recovery requires an existing \$tag tag/u);
  // A cut with changesets still pending fails the tag job instead of exiting
  // successfully untagged, as five versions did (#1762).
  assert.match(workflow, /changesets still pending \(\$pending\); refusing to tag\. See docs\/releasing\.md\."\n\s+exit 1/u);
  assert.doesNotMatch(workflow, /Changesets remain pending \(\$pending\); nothing to tag/u);
  assert.match(workflow, /commits\/\$cut_commit\/pulls/u);
  assert.match(workflow, /\.head\.ref == "changeset-release\/main"/u);
  assert.match(workflow, /git tag -a "\$TAG" "\$CUT_COMMIT"/u);
  assert.match(workflow, /tagged_commit.*!=.*cut_commit/u);
  assert.match(workflow, /gh workflow run release\.yml --ref main -f tag="\$TAG"/u);
  assert.match(workflow, /actions\/create-github-app-token@[0-9a-f]{40}/u);
  assert.match(workflow, /secrets\.CHORES_DUMB_CLIENT_ID/u);
  assert.match(workflow, /secrets\.CHORES_DUMB_PRIVATE_KEY/u);
  assert.doesNotMatch(workflow, /RELEASE_TOKEN|secrets\.GITHUB_TOKEN/u);
  // The version branch must be written through the git data API so the commit
  // is GitHub-signed; a runner-authored push cannot satisfy required_signatures.
  assert.match(workflow, /node scripts\/version-commit\.mjs publish/u);
  assert.match(workflow, /--base "\$GITHUB_SHA"/u);
  assert.doesNotMatch(workflow, /git push --force origin "\$BRANCH"/u);
  // A no-op release regeneration must not force-push a new head SHA onto the
  // open Version packages PR — that would re-run a full exact-SHA suite on
  // every open PR for no content change (#347). patch-id compares the diff
  // content itself, so it matches across commits with different parents.
  assert.match(workflow, /git diff-tree -p HEAD \| git patch-id --stable/u);
  assert.match(workflow, /git diff-tree -p "\$open_sha" \| git patch-id --stable/u);
  assert.match(workflow, /skipping the force-push/u);
  // The version PR opens and stays ready for review, so its complete suite
  // runs with no manual undraft; one left in draft is made ready.
  assert.match(workflow, /gh pr create --base main --head "\$BRANCH"/u);
  assert.doesNotMatch(workflow, /--draft/u);
  assert.doesNotMatch(workflow, /gh pr ready [^\n]*--undo/u);
  assert.match(workflow, /gh pr ready "\$open_draft"/u);
  assert.ok(
    workflow.indexOf('gh pr ready "$open_draft"') < workflow.indexOf('skipping the force-push'),
    'a draft version PR is made ready even when its patch is unchanged',
  );
  // checkout removes its credentials, so the open-PR fetch must authenticate
  // per command and never wait on a prompt (#667).
  assert.match(workflow, /GIT_TERMINAL_PROMPT: "0"/u);
  assert.match(workflow, /git -c "http\.https:\/\/github\.com\/\.extraheader=AUTHORIZATION: basic \$auth" \\\n\s+fetch origin "\$open_sha"/u);
});

test('CI enforces the governed lifecycle without draft jobs', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  assert.match(
    workflow,
    /^\s{2}pull_request:\n\s{4}branches: \[main\]\n\s{4}types: \[opened, synchronize, reopened, ready_for_review\]/mu,
  );
  assert.match(workflow, /^\s{2}merge_group:\n\s{4}types: \[checks_requested\]/mu);
  assert.match(workflow, /^\s{2}workflow_dispatch:/mu);
  assert.match(workflow, /exact-sha-preflight/u);
  assert.match(workflow, /format\('pr-\{0\}', github\.event\.pull_request\.number\)/u);
  assert.match(workflow, /cancel-in-progress: \$\{\{ github\.event_name != 'push' \}\}/u);
  assert.match(workflow, /github\.event\.pull_request\.draft == false/u);
  assert.match(
    workflow,
    new RegExp(`true-line/trueline-agent-ci/\\.github/actions/ci-policy@${PLAYBOOK_RUNTIME_PIN}`, 'u'),
  );
  assert.match(workflow, /head_sha=\$TARGET_SHA/u);
  assert.match(workflow, /head_sha=\$GITHUB_SHA/u);
  assert.match(workflow, /name: Complete suite/u);
  assert.match(workflow, /name: CI/u);
  // Without a merge queue, a main push reuses the merged PR's passing ready
  // run only when the trees are byte-identical, and never for the Version
  // packages PR, whose cut commit needs its own complete-suite evidence.
  assert.match(workflow, /commits\/\$GITHUB_SHA\/pulls/u);
  assert.match(workflow, /\[ "\$head_tree" = "\$tree" \] \|\| continue/u);
  assert.match(workflow, /\.head\.ref != "changeset-release\/main"/u);
  assert.match(workflow, /event=pull_request&head_sha=\$head/u);
  // Actions runs bash with errexit; a failed lookup must fall back to the
  // complete suite, not fail the main push.
  assert.match(workflow, /run: \|\n\s+set \+e\n\s+run_id=\$\(gh api "repos\/\$GITHUB_REPOSITORY\/actions\/runs\?event=merge_group/u);
});

test('Advanced CodeQL runs the security-and-quality suite', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/codeql.yml'), 'utf8');
  assert.match(workflow, /^\s+queries: security-and-quality$/mu);
});

test('required Advanced CodeQL contexts report on every non-draft PR run, even on the evidence path (#440)', () => {
  // The main ruleset requires the three `Advanced CodeQL / Analyze (…)` contexts
  // and evaluates them on the PR's own check rollup. Contexts from the
  // workflow_dispatch preflight are not part of that rollup, so skipping CodeQL
  // when preflight evidence exists leaves an approved, green PR BLOCKED.
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const codeql = workflow.match(/^ {2}codeql:\n(?: {4}.*\n| *\n)+/mu)?.[0] ?? '';
  assert.match(codeql, /name: Advanced CodeQL/u);
  assert.match(codeql, /needs\.policy\.outputs\.run_full == 'true'/u);
  assert.doesNotMatch(codeql, /preflight-evidence\.outputs\.validated/u);

  const gate = workflow.match(/^ {2}gate:\n(?: {4}.*\n| *\n)+/mu)?.[0] ?? '';
  const full = gate.match(/ full\)\n([\s\S]*?);;/u)?.[1] ?? '';
  assert.match(full, /test "\$CODEQL" = success/u);
});

test('queued PR lanes never cancel each other, and package installs serialize on one host lock (#669)', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  // A repo-wide lane group keeps one pending job, so a third queued PR
  // cancelled the waiting one. Only the workflow-level per-PR group may
  // supersede a run.
  assert.doesNotMatch(workflow, /ci-lane-/u);
  assert.match(workflow, /format\('pr-\{0\}', github\.event\.pull_request\.number\)/u);
  // Concurrent lanes share one runner host: a package install would hold the
  // same flock and wait for it within its bounded step timeout. No lane
  // installs packages per run: the Rust lanes use the CI image and the
  // frontend lane Playwright's image.
  const calls = [
    ...workflow.matchAll(
      /executable: flock\n\s+arguments-json: '\["-w","(\d+)","\/tmp\/cartograph-ci-packages\.lock",(.*?)\]'\n\s+(?:working-directory: .*\n\s+)?timeout-seconds: '(\d+)'/gu,
    ),
  ];
  for (const [, wait, , step] of calls) assert.ok(Number(wait) < Number(step));
  const locked = calls.map(([, , command]) => command);
  assert.equal(locked.filter((command) => command.startsWith('"sudo","apt-get"')).length, 0);
  assert.equal(locked.filter((command) => command.includes('"--with-deps"')).length, 0);
  assert.equal([...workflow.matchAll(/arguments-json: .*(?:"apt-get"|"--with-deps")/gu)].length, calls.length);
});

test('heavy Rust jobs hold one of two shared-host slots and release it (#1242)', () => {
  // The Linux runners share one VM's memory; overlapping Rust lanes and
  // CodeQL Rust extractions exhausted it and took every runner down.
  const script = readFileSync(path.join(root, '.github/scripts/heavy-slot.sh'), 'utf8');
  assert.match(script, /^slots=2$/mu);
  assert.match(script, /flock -n "\/tmp\/cartograph-ci-heavy-\$slot\.lock"/u);
  const take = /- name: Take a host heavy-build slot\n(?: +if: (.*)\n)? +run: bash \.github\/scripts\/heavy-slot\.sh (\d+)\n/u;
  const release = /- name: Release the host heavy-build slot\n +if: \$\{\{ !cancelled\(\)(.*) \}\}\n +run: rm -f "\$RUNNER_TEMP\/cartograph-heavy-slot"\n/u;
  const ci = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const rust = ci.match(/^ {2}rust:\n(?: {4}.*\n| *\n)+/mu)?.[0] ?? '';
  const [, , rustWait] = rust.match(take) ?? [];
  // A job's timeout counts its slot wait, so it leaves the work its own
  // budget beyond the longest wait (#1373).
  assert.match(rust, /^ {4}timeout-minutes: 89$/mu);
  assert.ok(89 * 60 - Number(rustWait) >= 45 * 60);
  assert.ok(rust.indexOf('Take a host heavy-build slot') < rust.indexOf('cargo test $PACKAGES'));
  assert.ok(rust.indexOf('cargo test $PACKAGES') < rust.search(release));
  const codeql = readFileSync(path.join(root, '.github/workflows/codeql.yml'), 'utf8');
  const [, when, codeqlWait] = codeql.match(take) ?? [];
  assert.equal(when, "matrix.language == 'rust'");
  assert.match(codeql, /^ {4}timeout-minutes: 120$/mu);
  assert.ok(120 * 60 - Number(codeqlWait) >= 90 * 60);
  // One Rust extraction at a time repo-wide; other languages per run.
  assert.match(codeql, /group: codeql-\$\{\{ matrix\.language == 'rust' && 'rust' \|\| github\.run_id \}\}-\$\{\{ matrix\.language \}\}\n +cancel-in-progress: false\n/u);
  assert.ok(codeql.indexOf('Take a host heavy-build slot') < codeql.indexOf('codeql-action/init@'));
  assert.equal(codeql.match(release)?.[1], " && matrix.language == 'rust'");
  assert.ok(codeql.indexOf('codeql-action/analyze@') < codeql.search(release));
});

test('install-action never shares its download directory across runners (#674)', () => {
  // install-action rm -rfs $HOME/.install-action/tmp after every install and
  // the self-hosted runners share one HOME, so each step gets its own.
  for (const file of ['.github/workflows/ci.yml', '.github/workflows/package.yml']) {
    const workflow = readFileSync(path.join(root, file), 'utf8');
    const steps = workflow.split(/\n(?= +- name: )/u).filter((step) => /uses: taiki-e\/install-action@/u.test(step));
    assert.ok(steps.length > 0, file);
    for (const step of steps) {
      assert.match(step, /\n +env:\n +HOME: \$\{\{ runner\.temp \}\}\/install-action-home\n/u, step);
    }
  }
  // A cargo-binstall fallback lands in that home's .cargo/bin, which the
  // action leaves off PATH when cargo is present.
  const ci = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  assert.match(ci, /tool: zizmor@[\d.]+\n\n(?: +#.*\n)+ +- name: Put installed tools on PATH\n +run: echo "\$RUNNER_TEMP\/install-action-home\/\.cargo\/bin" >> "\$GITHUB_PATH"/u);
});

test('direct non-CI entrypoints authorize before checkout and reusable calls inherit policy', () => {
  const workflows = [
    '.github/workflows/close-linked-issues.yml',
    '.github/workflows/package.yml',
    '.github/workflows/release.yml',
    '.github/workflows/version-cut.yml',
    '.github/workflows/help-wiki-sync.yml',
  ].map((file) => readFileSync(path.join(root, file), 'utf8'));
  for (const workflow of workflows) {
    assert.match(workflow, new RegExp(`ci-policy@${PLAYBOOK_RUNTIME_PIN}`, 'u'));
    assert.match(workflow, /authorization-only: 'true'/u);
    assert.match(workflow, /name: Action Policy/u);
    const policyPosition = workflow.indexOf('name: Action Policy');
    const checkoutPosition = workflow.indexOf('actions/checkout@');
    assert.ok(policyPosition >= 0 && policyPosition < checkoutPosition);
  }
  const packageWorkflow = workflows[1];
  assert.match(packageWorkflow, /if: github\.event_name != 'workflow_call'/u);
  assert.match(packageWorkflow, /github\.event_name == 'workflow_call' \|\| needs\.policy\.result == 'success'/u);
});

test('the help wiki follows main post-merge and never gates a PR (#470)', () => {
  const ci = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  assert.doesNotMatch(ci, /\.wiki\.git/u);
  assert.match(ci, /node scripts\/check-help-mirror\.mjs --validate/u);

  const sync = readFileSync(path.join(root, '.github/workflows/help-wiki-sync.yml'), 'utf8');
  assert.match(sync, /on:\n  push:\n    branches:\n      - main\n/u);
  assert.doesNotMatch(triggers(sync), /pull_request/u);
  assert.match(sync, /if: github\.ref == 'refs\/heads\/main'/u);
  assert.match(sync, /cancel-in-progress: false/u);
  // Least privilege: the repository token only reads; the wiki write uses
  // the dedicated secret, fail-closed, and never persists checkout creds.
  assert.doesNotMatch(sync, /: write/u);
  assert.match(sync, /HAS_WIKI_TOKEN: \$\{\{ secrets\.HELP_WIKI_TOKEN != '' \}\}/u);
  assert.match(sync, /persist-credentials: false/u);
  assert.match(sync, /node scripts\/check-help-mirror\.mjs --sync/u);
  assert.ok(sync.indexOf('Require the wiki credential') < sync.indexOf('actions/checkout@'));
});

test('the immutable policy action contract covers both actor fields and fork refusal', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const guidance = readFileSync(path.join(root, 'AGENTS.md'), 'utf8');
  assert.match(workflow, new RegExp(`ci-policy@${PLAYBOOK_RUNTIME_PIN}`, 'u'));
  assert.match(guidance, /github\.triggering_actor/u);
  assert.match(guidance, /github\.actor/u);
  assert.match(guidance, /Public-fork workflows are\s+never approved or run/u);
});

test('complete suite retains every existing Cartograph gate', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  assert.match(
    workflow,
    /taiki-e\/install-action@288e746965032cfcc232e09af2daf5f23c14d780/u,
  );
  const commands = [
    /node scripts\/check-traceability\.mjs/u,
    /node scripts\/generate-notices\.mjs --check/u,
    /npm run version:check/u,
    /npm run test:version/u,
    /cargo fmt --all --check/u,
    /cargo clippy --workspace --all-targets -- -D warnings/u,
    /cargo test \$PACKAGES/u,
    /--workspace --exclude app --exclude agents --exclude spec/u,
    /cargo deny check/u,
    /npm run lint/u,
    /npm run typecheck/u,
    /npm run test/u,
    /npm run build/u,
  ];
  for (const command of commands) assert.match(workflow, command);
  for (const context of ['Docs & traceability', 'Rust', 'License & supply-chain', 'Frontend']) {
    assert.match(workflow, new RegExp(`name: ${context}`, 'u'));
  }
});

test('superseded runs cancel every job, so the current head never queues behind them (#1230)', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  // always() keeps a job of a cancelled run waiting for a runner, holding the
  // per-PR group; only the evidence step of a manual dispatch may use it.
  const uses = [...workflow.matchAll(/always\(\)/gu)].map(({ index }) => workflow.slice(workflow.lastIndexOf('\n', index) + 1, workflow.indexOf('\n', index)));
  assert.deepEqual(uses, ["        if: always() && matrix.group == 'core' && github.event_name == 'workflow_dispatch' && inputs.local_provider_acceptance"]);
  assert.equal([...workflow.matchAll(/!cancelled\(\) && /gu)].length, 15);
});

test('Windows ownership tests are required by the same exact-SHA complete-suite policy', () => {
  // AC-0157 / AC-0159: native lock and process-exit coverage must not become
  // optional, silently skipped, or detached from the governed validation SHA.
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const job = (name) => {
    const contents = workflow.split(`\n  ${name}:\n`)[1]?.split(/\n  [a-z][a-z-]*:\n/u)[0];
    assert.ok(contents, `missing ${name} job`);
    return contents;
  };
  const windows = job('windows-job-ownership');
  const rust = job('rust');
  const complete = job('complete');
  // Comments between a job's condition and its runner are not part of it.
  const condition = (contents) =>
    contents
      .match(/    if: >-\n([\s\S]*?)    runs-on:/u)?.[1]
      .replace(/^ *#.*\n/gmu, '')
      .trim();
  assert.ok(condition(rust));
  assert.equal(condition(windows), condition(rust));
  assert.equal(condition(complete), condition(rust));
  assert.match(windows, /needs: \[policy, merge-evidence, preflight-evidence\]/u);
  assert.match(windows, /runs-on: windows-latest$/mu);
  assert.match(windows, /timeout-minutes: 25/u);
  assert.match(windows, /actions\/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1/u);
  assert.match(
    windows,
    /ref: \$\{\{ github\.event_name == 'pull_request' && github\.event\.pull_request\.head\.sha \|\| github\.sha \}\}/u,
  );
  assert.match(windows, /persist-credentials: false/u);
  assert.match(windows, /Get-Content rust-toolchain\.toml -Raw/u);
  assert.match(windows, /executable: rustup\.exe/u);
  assert.match(windows, /"\$\{\{ steps\.toolchain\.outputs\.channel \}\}"/u);
  assert.match(windows, /timeout-seconds: '300'/u);
  assert.match(windows, /executable: cargo\.exe/u);
  assert.match(
    windows,
    /arguments-json: '\["test","--locked","-p","job-execution-tests","--test","ownership","--","--nocapture"\]'/u,
  );
  assert.match(windows, /timeout-seconds: '900'/u);
  assert.equal(
    windows.match(new RegExp(`bounded-command@${PLAYBOOK_RUNTIME_PIN}`, 'gu'))?.length,
    2,
  );
  assert.doesNotMatch(windows, /continue-on-error|--skip|--ignored|attempts:/u);
  assert.match(complete, /needs: \[[^\]\n]*windows-job-ownership[^\]\n]*\]/u);
  assert.match(complete, /WINDOWS_JOB_OWNERSHIP: \$\{\{ needs\.windows-job-ownership\.result \}\}/u);
  assert.match(complete, /test "\$WINDOWS_JOB_OWNERSHIP" = success/u);
});

test('app unit tests run in required shards that jointly cover the binary (#1924)', () => {
  // The Rust job excludes only the app package; the shards run its one test
  // binary. Dropping a shard, or the gate on them, would silently drop tests.
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const job = (name) => workflow.split(`\n  ${name}:\n`)[1]?.split(/\n  [a-z][a-z-]*:\n/u)[0] ?? '';
  const condition = (contents) =>
    contents
      .match(/    if: >-\n([\s\S]*?)    runs-on:/u)?.[1]
      .replace(/^ *#.*\n/gmu, '')
      .trim();
  const rust = job('rust');
  const shards = job('rust-app-tests');
  const complete = job('complete');
  // The Rust job's groups jointly run every package but app, agents and spec,
  // each exactly once: the named groups' packages are exactly the ones the
  // `core` group excludes besides those three (#1924).
  assert.match(rust, /env:\n +PACKAGES: \$\{\{ matrix\.packages \}\}\n(?: +#.*\n)* +run: cargo test \$PACKAGES\n/u);
  assert.match(rust, /fail-fast: false/u);
  // Each group is a `- group:` line, then `packages:` and the folded lines
  // indented deeper than it.
  const groups = [];
  const lines = (rust.match(/^ {8}include:\n((?: {10}.*\n)+)/mu)?.[1] ?? '').split('\n');
  for (const line of lines) {
    const group = /^ {10}- group: ([a-z]+)$/u.exec(line);
    if (group) groups.push({ name: group[1], args: [] });
    else if (line.startsWith('            packages: ')) {
      groups.at(-1).args.push(...line.slice('            packages: '.length).split(' ').filter((arg) => arg !== '>-'));
    } else if (line.startsWith('              ')) groups.at(-1).args.push(...line.trim().split(' '));
  }
  assert.deepEqual(groups.map(({ name }) => name), ['adapters', 'ingest', 'core']);
  const named = groups.filter(({ name }) => name !== 'core').flatMap(({ args }) => {
    assert.equal(args.length % 2, 0);
    return args.filter((_, index) => index % 2 === 1).map((name, index) => {
      assert.equal(args[index * 2], '-p');
      return name;
    });
  });
  assert.equal(new Set(named).size, named.length, 'a package runs in two Rust groups');
  const core = groups.find(({ name }) => name === 'core').args;
  assert.equal(core[0], '--workspace');
  const excluded = core.slice(1).filter((_, index) => index % 2 === 1);
  assert.ok(core.slice(1).every((arg, index) => index % 2 === 1 || arg === '--exclude'));
  assert.deepEqual(excluded.slice(0, 3), ['app', 'agents', 'spec']);
  assert.deepEqual([...excluded.slice(3)].sort(), [...named].sort());
  const members = readdirSync(path.join(root, 'crates'));
  for (const name of named) assert.ok(members.includes(name), `${name} is not a workspace crate`);
  assert.ok(condition(shards));
  assert.equal(condition(shards), condition(rust));
  assert.match(shards, /needs: \[policy, merge-evidence, preflight-evidence, ci-image\]/u);
  assert.match(shards, /runs-on: ubuntu-latest$/mu);
  assert.match(shards, /^ {4}timeout-minutes: 60$/mu);
  assert.match(shards, /fail-fast: false/u);
  const [, list] = shards.match(/matrix:\n +shard: \[([\d, ]+)\]\n/u) ?? [];
  const indexes = (list ?? '').split(', ').map(Number);
  const [, count, target] =
    shards.match(/run: node scripts\/rust-test-shard\.mjs "\$SHARD\/(\d+)" -- (.*)\n/u) ?? [];
  assert.equal(target, '-p app --bin app');
  assert.deepEqual(indexes, Array.from({ length: Number(count) }, (_, index) => index + 1));
  assert.match(shards, new RegExp(`name: Rust app tests \\(\\$\\{\\{ matrix\\.shard \\}\\}/${count}\\)`, 'u'));
  assert.match(shards, /SHARD: \$\{\{ matrix\.shard \}\}/u);
  assert.match(shards, /ref: \$\{\{ github\.event_name == 'pull_request' && github\.event\.pull_request\.head\.sha \|\| github\.sha \}\}/u);
  assert.doesNotMatch(shards, /continue-on-error/u);
  assert.match(complete, /needs: \[.*\brust-app-tests\b.*\]/u);
  assert.match(complete, /RUST_APP_TESTS: \$\{\{ needs\.rust-app-tests\.result \}\}/u);
  assert.match(complete, /test "\$RUST_APP_TESTS" = success/u);
  // The packages the Rust job excludes besides app run whole in their own lane.
  const agents = job('rust-heavy-tests');
  assert.equal(condition(agents), condition(rust));
  assert.match(agents, /needs: \[policy, merge-evidence, preflight-evidence, ci-image\]/u);
  assert.match(agents, /runs-on: ubuntu-latest$/mu);
  assert.match(agents, /run: cargo test -p agents -p spec\n/u);
  assert.doesNotMatch(agents, /continue-on-error|--skip|--exclude/u);
  assert.match(complete, /needs: \[.*\brust-heavy-tests\b.*\]/u);
  assert.match(complete, /RUST_HEAVY_TESTS: \$\{\{ needs\.rust-heavy-tests\.result \}\}/u);
  assert.match(complete, /test "\$RUST_HEAVY_TESTS" = success/u);
  // Clippy runs in its own required lane, on the same condition.
  const clippy = job('rust-clippy');
  assert.equal(condition(clippy), condition(rust));
  assert.match(clippy, /run: cargo clippy --workspace --all-targets -- -D warnings\n/u);
  assert.doesNotMatch(rust, /cargo clippy/u);
  assert.match(complete, /needs: \[.*\brust-clippy\b.*\]/u);
  assert.match(complete, /test "\$RUST_CLIPPY" = success/u);
  // `app` has no library, integration-test or doc-test target the exclusion
  // could drop: its tests are the binary's unit tests.
  const manifest = readFileSync(path.join(root, 'src-tauri/Cargo.toml'), 'utf8');
  assert.match(manifest, /^name = "app"$/mu);
  assert.doesNotMatch(manifest, /^\[lib\]|^\[\[(?:test|bin|example|bench)\]\]/mu);
  assert.deepEqual(readdirSync(path.join(root, 'src-tauri')).filter((entry) => ['tests', 'examples', 'benches'].includes(entry)), []);
  assert.deepEqual(readdirSync(path.join(root, 'src-tauri/src')).filter((entry) => entry === 'lib.rs' || entry === 'bin'), []);
});

test('real-model acceptance is explicit, bounded, and retains evidence after failures', () => {
  // AC-0191: routine CI cannot silently run a model or hide a failed opt-in run.
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  assert.match(workflow, /local_provider_acceptance:\n\s+description:.*\n\s+required: false\n\s+type: boolean\n\s+default: false/u);
  const rust = workflow.split('\n  rust:\n')[1].split('\n  windows-job-ownership:\n')[0];
  const model = rust.split('- name: Run explicit local-provider acceptance')[1]
    .split('- name: Retain local-provider acceptance evidence')[0];
  // One Rust group runs it, so a dispatch starts one model run, not three.
  assert.match(model, /if: matrix\.group == 'core' && github\.event_name == 'workflow_dispatch' && inputs\.local_provider_acceptance/u);
  assert.match(model, new RegExp(`bounded-command@${PLAYBOOK_RUNTIME_PIN}`, 'u'));
  assert.match(model, /timeout-seconds: '2400'/u);
  assert.doesNotMatch(model, /continue-on-error|attempts:/u);
  const artifact = rust.split('- name: Retain local-provider acceptance evidence')[1];
  assert.match(artifact, /if: always\(\) && matrix\.group == 'core' && github\.event_name == 'workflow_dispatch' && inputs\.local_provider_acceptance/u);
  assert.match(artifact, /cartograph-local-acceptance\/evidence/u);
  assert.match(artifact, /include-hidden-files: true/u);
  assert.doesNotMatch(artifact, /cartograph-local-acceptance\/runtime/u);
});

test('the native ownership harness includes production modules without a Tauri dependency', () => {
  // AC-0157: exercise the same source and child-process test entrypoint on each
  // platform; a separate implementation would not establish host behavior.
  const manifest = readFileSync(path.join(root, 'crates/job-execution-tests/Cargo.toml'), 'utf8');
  const harness = readFileSync(path.join(root, 'crates/job-execution-tests/tests/ownership.rs'), 'utf8');
  assert.match(manifest, /version\.workspace = true/u);
  assert.match(manifest, /\[\[test\]\]\nname = "ownership"\npath = "tests\/ownership\.rs"/u);
  assert.doesNotMatch(manifest, /^\s*(?:app|semantic|tauri(?:-build)?|wasmtime|usearch)\s*=|\[build-dependencies\]/imu);
  for (const name of ['jobs', 'job_execution']) {
    assert.match(
      harness,
      new RegExp(`#\\[path = "\\.\\./\\.\\./\\.\\./src-tauri/src/${name}\\.rs"\\]\\nmod ${name};`, 'u'),
    );
  }
  assert.match(harness, /extern crate self as semantic;/u);
  assert.match(harness, /#\[path = "\.\.\/\.\.\/\.\.\/crates\/semantic\/src\/eval_report\.rs"\]\nmod eval_report;/u);
  assert.match(harness, /pub use eval_report::EvalReport;/u);
  const semantic = readFileSync(path.join(root, 'crates/semantic/src/lib.rs'), 'utf8');
  assert.match(semantic, /mod eval_report;\npub use eval_report::EvalReport;/u);
  assert.doesNotMatch(semantic, /struct EvalReport/u);
  for (const [file, directory, modules] of [
    ['jobs.rs', 'jobs', ['execution', 'ownership_tests']],
    ['job_execution.rs', 'job_execution', ['tests', 'windows']],
  ]) {
    const source = readFileSync(path.join(root, 'src-tauri/src', file), 'utf8');
    for (const name of modules) {
      assert.match(source, new RegExp(`#\\[path = "${directory}/${name}\\.rs"\\]\\nmod ${name};`, 'u'));
    }
  }
  assert.doesNotMatch(harness, /\b(?:fn|struct|enum|impl)\s|include_str!|include_bytes!|allow\(warnings\)/u);
  const guidance = readFileSync(path.join(root, 'AGENTS.md'), 'utf8');
  assert.match(guidance, /native job ownership tests on Windows:\s+`cargo test --locked -p job-execution-tests --test ownership`/u);
  assert.match(guidance, /no job a pull request can start runs on a\s+self-hosted runner/u);
});

test('workflow runtime policy is pinned and installer processes are centrally bounded', () => {
  const workflowNames = [
    'ci.yml',
    'close-linked-issues.yml',
    'codeql.yml',
    'codeql-rust.yml',
    'package.yml',
    'release.yml',
    'version-cut.yml',
  ];
  const workflows = workflowNames.map((name) =>
    readFileSync(path.join(root, '.github/workflows', name), 'utf8'),
  );
  const combined = workflows.join('\n');

  assert.match(combined, new RegExp(`bounded-command@${PLAYBOOK_RUNTIME_PIN}`, 'u'));
  assert.match(workflows[0], /name: Workflow runtime policy/u);
  assert.match(workflows[0], new RegExp(`ref: ${PLAYBOOK_RUNTIME_PIN}`, 'u'));
  assert.match(workflows[0], /runtime-policy\.mjs --root "\$GITHUB_WORKSPACE"/u);
  assert.doesNotMatch(combined, /workflow_runs\[\].*\.name == "CI"/u);
});

test('Advanced CodeQL is governed, immutable, and preserves Rust coverage', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/codeql.yml'), 'utf8');
  const dependabot = readFileSync(path.join(root, '.github/dependabot.yml'), 'utf8');
  assert.match(workflow, /^\s{2}workflow_call:/mu);
  assert.doesNotMatch(workflow, /^\s{2}(push|pull_request|schedule):/mu);
  // #634: Rust analysis runs on pushes to main only, in its own workflow run;
  // every CI lane keeps the rest.
  assert.match(
    workflow,
    /language: \$\{\{ fromJSON\(inputs\.rust && '\["rust"\]' \|\| '\["actions", "javascript-typescript"\]'\) \}\}/u,
  );
  assert.match(
    workflow,
    /github\/codeql-action\/init@ff2f1c621b7f889edc0d3c761ac2e6a3f8cdb0dd/u,
  );
  assert.match(
    workflow,
    /github\/codeql-action\/analyze@ff2f1c621b7f889edc0d3c761ac2e6a3f8cdb0dd/u,
  );
  assert.match(workflow, /build-mode: none/u);
  assert.match(
    dependabot,
    /codeql-action:\n\s+patterns:\n\s+- github\/codeql-action\/\*/u,
  );
});

test('a superseded Rust CodeQL analysis never fails a main commit\'s CI, so version merges get tagged', () => {
  // Rust extractions run one at a time and the newest pending commit replaces
  // an older pending one. Inside ci.yml that cancellation failed the older
  // commit's CI gate, and version-cut refuses to tag a commit whose CI failed.
  const ci = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const codeql = ci.match(/^ {2}codeql:\n(?: {4}.*\n| *\n)+/mu)?.[0] ?? '';
  assert.match(codeql, /uses: \.\/\.github\/workflows\/codeql\.yml\n/u);
  assert.doesNotMatch(codeql, /rust:/u);
  const rust = readFileSync(path.join(root, '.github/workflows/codeql-rust.yml'), 'utf8');
  assert.match(triggers(rust), /push:\n +branches:\n +- main\n/u);
  assert.doesNotMatch(triggers(rust), /pull_request|workflow_run/u);
  assert.match(rust, /uses: \.\/\.github\/workflows\/codeql\.yml\n +with:\n +image: \$\{\{ needs\.ci-image\.outputs\.image \}\}\n +rust: true\n/u);
  assert.match(rust, /security-events: write/u);
  const version = readFileSync(path.join(root, '.github/workflows/version-cut.yml'), 'utf8');
  assert.match(version, /workflow_run:\n +workflows: \[CI\]\n/u);
});

test('Dependabot batches minor/patch bumps and skips auto-rebase fan-out', () => {
  const dependabot = readFileSync(path.join(root, '.github/dependabot.yml'), 'utf8');
  // Auto-rebase mints a new head SHA per push to main, which re-runs a full
  // exact-SHA suite on every open PR; the governed lifecycle re-validates the
  // exact merge candidate anyway, so rebase-on-push buys nothing (#347).
  const rebaseDisabledCount = (dependabot.match(/rebase-strategy: disabled/gu) ?? []).length;
  assert.equal(rebaseDisabledCount, 4);
  assert.doesNotMatch(dependabot, /rebase-strategy: auto/u);
  // codeql-action must stay its own atomic group (#327) and be listed before
  // the catch-all, since Dependabot assigns each update to the first
  // matching group.
  assert.match(
    dependabot,
    /codeql-action:\n\s+patterns:\n\s+- github\/codeql-action\/\*\n\s+#[^\n]*\n(?:\s+#[^\n]*\n)*\s+github-actions-minor-patch:\n\s+patterns:\n\s+- '\*'/u,
  );
});

test('macOS packaging is exact-SHA, universal, fail-closed, and verified', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/package.yml'), 'utf8');

  assert.match(
    workflow,
    /taiki-e\/install-action@288e746965032cfcc232e09af2daf5f23c14d780/u,
  );
  assert.match(workflow, /name: Verify exact CI evidence/u);
  assert.match(workflow, /head_sha=\$PACKAGE_SHA/u);
  assert.match(workflow, /ref: \$\{\{ needs\.evidence\.outputs\.sha \}\}/u);
  assert.match(workflow, /node scripts\/signing-secrets\.mjs/u);
  assert.match(workflow, /APPLE_CERTIFICATE: \$\{\{ secrets\.CSC_LINK \}\}/u);
  assert.match(workflow, /APPLE_CERTIFICATE_PASSWORD: \$\{\{ secrets\.CSC_KEY_PASSWORD \}\}/u);
  assert.match(workflow, /APPLE_API_KEY_PATH/u);
  assert.match(workflow, /APPLE_API_KEY_ID: \$\{\{ secrets\.APPLE_API_KEY_ID \}\}/u);
  assert.match(workflow, /APPLE_API_ISSUER: \$\{\{ secrets\.APPLE_API_ISSUER \}\}/u);
  assert.match(workflow, /APPLE_SIGNING_IDENTITY: '-'/u);
  assert.match(workflow, /universal-apple-darwin/u);
  assert.match(workflow, /codesign --verify --deep --strict/u);
  assert.match(workflow, /spctl --assess/u);
  assert.match(workflow, /xcrun notarytool submit/u);
  assert.match(workflow, /xcrun stapler staple/u);
  assert.match(workflow, /xcrun stapler validate/u);
  assert.match(workflow, /Cartograph_\$\{version\}_universal_\$\{SIGNING_MODE\}/u);
  assert.doesNotMatch(workflow, /Run required repository gates/u);
});

test('macOS packaging compiles with no signing secrets in the environment (#480)', () => {
  // Cargo expands env!/option_env! and runs build scripts and proc macros, so
  // any signing input visible while compiling could be embedded in the binary.
  const workflow = readFileSync(path.join(root, '.github/workflows/package.yml'), 'utf8');
  const manifest = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8'));
  const steps = workflow.split(/\n(?= {6}- name: )/u);
  const step = (name) => steps.find((block) => block.startsWith(`      - name: ${name}`)) ?? '';
  const index = (name) => steps.findIndex((block) => block.startsWith(`      - name: ${name}`));

  assert.match(manifest.scripts['package:macos:compile'], /^node scripts\/build-mcp-relay\.mjs && node scripts\/build-agent-runtime\.mjs universal-apple-darwin && tauri build --no-bundle --target universal-apple-darwin$/u);
  assert.match(manifest.scripts['package:macos:bundle'], /^tauri bundle --bundles app,dmg --target universal-apple-darwin --config src-tauri\/tauri\.mcp-bundle\.conf\.json$/u);

  const compile = step('Compile universal binary without signing secrets');
  assert.match(compile, /npm run package:macos:compile/u);
  assert.doesNotMatch(compile, /^\s+env:/mu);
  assert.doesNotMatch(compile, /secrets\./u);

  // No step may export signing material into later steps' environment.
  assert.doesNotMatch(workflow, /APPLE_[A-Z_]*=.*GITHUB_ENV/u);
  assert.doesNotMatch(workflow, />>\s*"?\$GITHUB_ENV/u);

  // Fail-closed mode selection precedes the compile; every step that holds a
  // signing secret other than mode selection runs after it.
  const compileAt = index('Compile universal binary without signing secrets');
  assert.ok(index('Select signing mode') < compileAt);
  for (const name of [
    'Decode App Store Connect key',
    'Bundle and sign universal app',
    'Bundle ad-hoc signed unsigned-dev universal app',
    'Sign agent runtime with its own entitlements',
    'Notarize and staple disk image',
  ]) {
    assert.ok(index(name) > compileAt, `${name} must run after compilation`);
  }
  steps.forEach((block, position) => {
    if (/secrets\.(?:CSC_|APPLE_)/u.test(block) && !block.startsWith('      - name: Select signing mode')) {
      assert.ok(position > compileAt, `signing secret visible before compilation in: ${block.split('\n')[0]}`);
    }
  });

  for (const name of ['Bundle and sign universal app', 'Bundle ad-hoc signed unsigned-dev universal app']) {
    assert.match(step(name), /npm run package:macos:bundle/u);
    // Notarization follows the agent runtime's re-signing, so Tauri gets no
    // App Store Connect input.
    assert.doesNotMatch(step(name), /APPLE_API_/u);
  }
  assert.match(step('Notarize and staple disk image'), /APPLE_API_KEY_PATH: \$\{\{ steps\.apple_key\.outputs\.key_path \}\}/u);
  assert.doesNotMatch(workflow, /npm run package:macos\n/u);
});

test('only the agent runtime holds allow-jit, signed before notarization (ADR-0085, AC-1427)', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/package.yml'), 'utf8');
  const steps = workflow.split(/\n(?= {6}- name: )/u);
  const step = (name) => steps.find((block) => block.startsWith(`      - name: ${name}`)) ?? '';
  const index = (name) => steps.findIndex((block) => block.startsWith(`      - name: ${name}`));
  const resign = 'Sign agent runtime with its own entitlements';
  assert.ok(index('Bundle ad-hoc signed unsigned-dev universal app') < index(resign));
  assert.ok(index(resign) < index('Locate bundles'), 'the ready check runs the re-signed sidecar');
  assert.ok(index(resign) < index('Notarize and staple disk image'), 'notarization covers the final signatures');
  assert.match(step(resign), /bash scripts\/macos-sign-agent-runtime\.sh "\$SIGNING_MODE"/u);
  assert.doesNotMatch(step(resign), /^\s+if:/mu, 'both signing modes re-sign the sidecar');
  assert.match(step('Notarize and staple disk image'), /xcrun stapler staple "\$\{\{ steps\.bundles\.outputs\.app \}\}"/u);

  const macos = JSON.parse(readFileSync(path.join(root, 'src-tauri/tauri.macos.conf.json'), 'utf8'));
  assert.equal(macos.bundle.macOS.entitlements, undefined, 'the app carries no entitlements');
  assert.equal(macos.bundle.macOS.hardenedRuntime, true);
  const plist = readFileSync(path.join(root, 'src-tauri/agent-runtime.entitlements.plist'), 'utf8');
  assert.deepEqual([...plist.matchAll(/<key>([^<]+)<\/key>/gu)].map((m) => m[1]), ['com.apple.security.cs.allow-jit']);

  const script = readFileSync(path.join(root, 'scripts/macos-sign-agent-runtime.sh'), 'utf8');
  const signing = script.split('\n').filter((line) => /codesign\b.*--sign|"\$\{sign\[@\]\}"/u.test(line));
  assert.ok(signing.length >= 3);
  for (const line of signing) assert.doesNotMatch(line, /--deep/u, line);
  const sidecar = script.indexOf('"${sign[@]}" --entitlements "$entitlements" "$agent"');
  const app = script.indexOf('"${sign[@]}" --preserve-metadata=entitlements,requirements "$app"');
  assert.ok(sidecar > 0 && app > sidecar, 'the sidecar is signed before the app that seals it');
  assert.match(script, /sidecar=\$\(codesign -d --entitlements - --xml "\$bundle\/Contents\/MacOS\/cartograph-agent"/u);
  assert.match(script, /app=\$\(codesign -d --entitlements - --xml "\$bundle" /u);
  assert.match(script, /if \[\[ "\$app" == \*"\$jit"\* \]\]; then\n\s+echo "\$bundle: the app must not hold/u);
  assert.match(script, /security delete-keychain "\$keychain"/u);
  // AC-1424: Claude Code ships as a resource under Anthropic's signature, and
  // the script checks it with the same requirement as the build and the app.
  const overlay = JSON.parse(readFileSync(path.join(root, 'src-tauri/tauri.mcp-bundle.conf.json'), 'utf8'));
  assert.deepEqual(overlay.bundle.resources, { '../target/agent-runtime/claude-code/claude': 'claude-code/claude' });
  assert.ok(!overlay.bundle.externalBin.some((bin) => bin.includes('claude')), 'never an external binary, which Tauri re-signs');
  const requirement = script.split('\n').find((line) => line.startsWith('claude_requirement='))
    .slice('claude_requirement="'.length, -1).replaceAll('\\"', '"').replace('$claude_team', 'Q6L2SF6YDW');
  assert.equal(requirement, claudeCodeRequirement('Q6L2SF6YDW'));
  assert.match(script, /codesign --verify --strict -R="=\$claude_requirement" "\$bundle\/Contents\/Resources\/claude-code\/claude"/u);
  const result = spawnSync('bash', ['-n', path.join(root, 'scripts/macos-sign-agent-runtime.sh')], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
});

test('release publication is reviewed, exact-evidence-only, and idempotent', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/release.yml'), 'utf8');

  assert.match(workflow, /^\s{2}push:\n\s{4}tags:/mu);
  assert.match(workflow, /^\s{2}workflow_dispatch:/mu);
  assert.match(workflow, /group: release-\$\{\{ inputs\.tag \|\| github\.ref_name \}\}/u);
  assert.match(workflow, /cancel-in-progress: false/u);
  assert.match(workflow, /git cat-file -t "refs\/tags\/\$TAG"/u);
  assert.match(workflow, /expected=\$\(node scripts\/release-version\.mjs tag\)/u);
  assert.match(workflow, /commits\/\$tag_commit\/pulls/u);
  assert.match(workflow, /\.head\.ref == "changeset-release\/main"/u);
  assert.match(workflow, /event=merge_group&head_sha=\$tag_commit/u);
  assert.match(workflow, /name == "Complete suite" and \.conclusion == "success"/u);
  assert.match(workflow, /event=pull_request&head_sha=\$pr_head/u);
  assert.match(workflow, /uses: \.\/\.github\/workflows\/package\.yml/u);
  assert.match(workflow, /ref: \$\{\{ needs\.validate\.outputs\.tag \}\}/u);
  assert.match(workflow, /name: \$\{\{ needs\.build\.outputs\.artifact_name \}\}/u);
  assert.match(workflow, /SIGNING_MODE: \$\{\{ needs\.build\.outputs\.signing_mode \}\}/u);
  assert.match(workflow, /release-metadata\.mjs notes "\$VERSION" CHANGELOG\.md/u);
  assert.match(workflow, /gh release edit "\$TAG"/u);
  assert.match(workflow, /gh release delete-asset/u);
  assert.match(workflow, /gh release upload "\$TAG" dist\/\* --clobber/u);
  assert.match(workflow, /permissions:\n\s{6}actions: read\n\s{6}contents: read/u);
  assert.match(workflow, /actions\/create-github-app-token@[0-9a-f]{40}/u);
  assert.match(workflow, /secrets\.CHORES_DUMB_CLIENT_ID/u);
  assert.match(workflow, /secrets\.CHORES_DUMB_PRIVATE_KEY/u);
  const publish = workflow.split('- name: Create or update release without duplicate assets')[1] ?? '';
  assert.doesNotMatch(workflow, /RELEASE_TOKEN/u);
  assert.doesNotMatch(publish, /GH_TOKEN: \$\{\{ github\.token \}\}/u);
});

// Credential-bearing workflows never share a persistent self-hosted runner
// with pull-request builds, which run repository-controlled scripts (#619).

test('Rust lanes run in the CI image their inputs name, built only when missing', () => {
  // Every lane apt-installed ~150 packages per run, and an index from one
  // mirror named archives another had replaced, failing every lane. The image
  // carries the libraries and toolchain; its tag hashes its inputs, so a
  // Dockerfile or toolchain change builds a new one and nothing else does.
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const job = (name) => workflow.split(`\n  ${name}:\n`)[1]?.split(/\n  [a-z][a-z-]*:\n/u)[0] ?? '';
  const image = job('ci-image');
  assert.match(image, /run: echo "image=\$\(node scripts\/ci-image-tag\.mjs\)" >> "\$GITHUB_OUTPUT"/u);
  assert.match(image, /if docker manifest inspect "\$IMAGE" > \/dev\/null 2>&1; then/u);
  assert.match(image, /tar -c ci\/linux\.Dockerfile rust-toolchain\.toml \|/u);
  assert.match(image, /^ {6}packages: write$/mu);
  // Old images are pruned on main only, keeping the newest few and never
  // the tag the run is using.
  assert.match(image, /- name: Prune old CI images\n +if: github\.event_name == 'push' && github\.ref == 'refs\/heads\/main'\n/u);
  assert.match(image, /select\(\.metadata\.container\.tags \| any\(\.\[\]; \. == env\.TAG\) \| not\)/u);
  assert.match(image, /KEEP: '3'/u);
  // A concurrent main run may already have removed a listed version.
  assert.match(image, /- name: Prune old CI images\n +if: [^\n]+\n +continue-on-error: true\n/u);
  assert.match(image, /gh api -X DELETE "\$PACKAGE\/versions\/\$id" \|\| echo/u);
  for (const name of ['rust', 'rust-app-tests', 'rust-heavy-tests', 'rust-clippy', 'compliance']) {
    const lane = job(name);
    assert.match(lane, /needs: \[[^\]]*\bci-image\b[^\]]*\]/u, name);
    assert.match(lane, /container:\n(?: +#.*\n)* +image: \$\{\{ needs\.ci-image\.outputs\.image \}\} # zizmor: ignore\[unpinned-images\]\n/u, name);
    assert.match(lane, /options: --user 1001\n/u, name);
    assert.match(lane, /volumes:\n +- \/home\/runner\/work:\/home\/runner\/work\n/u, name);
    assert.doesNotMatch(lane, /apt-get|apt-mirrors/u, name);
  }
  const complete = job('complete');
  assert.match(complete, /needs: \[.*\bci-image\b.*\]/u);
  assert.match(complete, /test "\$CI_IMAGE" = success/u);
  assert.match(image, /echo "image=\$IMAGE@\$digest" >> "\$GITHUB_OUTPUT"/u);
  // The frontend lane runs in Playwright's image, pinned by digest, whose
  // version must be the locked one: its browsers are built for that release.
  const frontend = job('frontend');
  const lock = JSON.parse(readFileSync(path.join(root, 'ui/package-lock.json'), 'utf8'));
  const playwright = lock.packages['node_modules/playwright'].version;
  assert.match(frontend, new RegExp(`image: mcr\\.microsoft\\.com/playwright:v${playwright.replaceAll('.', '\\.')}-noble@sha256:[0-9a-f]{64}\n`, 'u'));
  assert.doesNotMatch(frontend, /apt-get|apt-mirrors|playwright install/u);
  // bounded-command needs the host work path itself, mounted, not linked.
  assert.match(frontend, /volumes:\n +- \/home\/runner\/work:\/home\/runner\/work\n/u);
  assert.doesNotMatch(workflow, /ln -s/u);
  // Advanced CodeQL, Rust extraction included, runs in the CI image too.
  const codeql = readFileSync(path.join(root, '.github/workflows/codeql.yml'), 'utf8');
  assert.match(codeql, /container:\n +image: \$\{\{ inputs\.image \}\} # zizmor: ignore\[unpinned-images\]\n/u);
  assert.doesNotMatch(codeql, /apt-get/u);
  assert.match(job('codeql'), /with:\n +image: \$\{\{ needs\.ci-image\.outputs\.image \}\}\n/u);
  const dockerfile = readFileSync(path.join(root, 'ci/linux.Dockerfile'), 'utf8');
  assert.match(dockerfile, /^FROM ubuntu:26\.04$/mu);
  assert.match(dockerfile, /COPY rust-toolchain\.toml /u);
  assert.match(dockerfile, /^USER 1001$/mu);
  for (const pkg of ['libwebkit2gtk-4.1-dev', 'libxdo-dev', 'libssl-dev', 'libayatana-appindicator3-dev', 'librsvg2-dev', 'rustup', 'zstd']) {
    assert.match(dockerfile, new RegExp(`\\b${pkg.replaceAll('.', '\\.')}\\b`, 'u'), pkg);
  }
});

test('the app crate compiles incrementally from state main saves, and only main saves it (#1924)', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const job = (name) => workflow.split(`\n  ${name}:\n`)[1]?.split(/\n  [a-z][a-z-]*:\n/u)[0] ?? '';
  const shards = job('rust-app-tests');
  const seed = job('app-incremental');
  const key = /echo "key=app-incremental-\$LOCK_HASH-\$\(date -u \+%Y%m%d%H\)" >> "\$GITHUB_OUTPUT"/u;
  for (const contents of [shards, seed]) {
    assert.match(contents, /RUSTC_WORKSPACE_WRAPPER=\$GITHUB_WORKSPACE\/scripts\/rustc-app-incremental\.sh/u);
    assert.match(contents, /SCCACHE_PATH=\$wrapper/u);
    // A pull request branched before the wrapper checks out no script.
    assert.match(contents, /if \[ -x scripts\/rustc-app-incremental\.sh \]; then\n.*\n +else\n +echo "RUSTC_WORKSPACE_WRAPPER=\$wrapper" >> "\$GITHUB_ENV"\n +fi\n/u);
    assert.match(contents, /LOCK_HASH: \$\{\{ hashFiles\('rust-toolchain\.toml', 'Cargo\.lock'\) \}\}/u);
    assert.match(contents, /APP_INCREMENTAL_DIR=\$RUNNER_TEMP\/app-incremental/u);
    assert.match(contents, key);
    assert.match(contents, /uses: actions\/cache\/restore@caa296126883cff596d87d8935842f9db880ef25 # v5\.1\.0/u);
  }
  // A shard restores and never writes: a pull request's cache would be
  // visible to that pull request alone, and six writers a run would evict
  // the other lanes' caches.
  assert.doesNotMatch(shards, /actions\/cache(?:\/save)?@/u);
  assert.match(seed, /if: "!cancelled\(\) && github\.event_name == 'push' && needs\.ci-image\.result == 'success'"/u);
  assert.match(seed, /uses: actions\/cache\/save@caa296126883cff596d87d8935842f9db880ef25 # v5\.1\.0/u);
  assert.match(seed, /run: cargo test -p app --bin app --no-run/u);
  assert.match(seed, /save-if: false/u);
  // Each hour saves a new key, so the job deletes the older entries.
  assert.match(seed, /actions: write/u);
  assert.match(seed, /deleteActionsCacheById/u);
  assert.doesNotMatch(shards, /actions: write/u);
});

test('every job runs on GitHub-hosted runners except release packaging', () => {
  // The repository is public, so no job a pull request can start may run on
  // a self-hosted runner (fork PRs would execute on it), and a workflow that
  // reads secrets never runs pull-request code (#631). macOS packaging alone
  // runs on the owner's Mac; its workflow has no pull-request trigger.
  const dir = path.join(root, '.github/workflows');
  for (const name of readdirSync(dir).filter((file) => file.endsWith('.yml'))) {
    const workflow = readFileSync(path.join(dir, name), 'utf8');
    if (/secrets\.[A-Z]/u.test(workflow)) {
      assert.doesNotMatch(triggers(workflow), /pull_request/u, `${name}: secrets with a PR trigger`);
    }
    const hosted = /runs-on: (ubuntu|windows|macos)-latest$/u;
    const packaging = /runs-on: \[self-hosted, macOS, ARM64\]$/u;
    for (const [line] of workflow.matchAll(/^\s*runs-on:.*$/gmu)) {
      if ((name === 'package.yml' || name === 'scheduled.yml') && packaging.test(line)) {
        assert.doesNotMatch(triggers(workflow), /pull_request|issue_comment/u, `${name}: self-hosted with a PR trigger`);
        continue;
      }
      assert.match(line, hosted, `${name}: ${line.trim()}`);
    }
  }
});

test('release symbols persist with versioned assets and short-lived Actions copies (#1676)', () => {
  const packaging = readFileSync(path.join(root, '.github/workflows/package.yml'), 'utf8');
  const release = readFileSync(path.join(root, '.github/workflows/release.yml'), 'utf8');
  assert.match(packaging, /DSYM_PATH: \$\{\{ steps\.dsym\.outputs\.path \}\}/u);
  assert.ok(packaging.includes('ditto -c -k --sequesterRsrc --keepParent "$DSYM_PATH"'));
  assert.ok(packaging.includes('"dist/Cartograph_${version}_universal_${SIGNING_MODE}.dSYM.zip"'));
  for (const name of ['Upload versioned artifacts', 'Upload dSYM']) {
    const block = packaging.split(`      - name: ${name}\n`)[1]?.split(/\n      - name:/u)[0] ?? '';
    assert.match(block, /retention-days: 1(?:\n|$)/u);
    assert.match(block, /if-no-files-found: error/u);
  }
  assert.match(release, /for extension in app\.zip dmg dSYM\.zip; do/u);
  assert.match(release, /gh release upload "\$TAG" dist\/\* --clobber/u);
  assert.match(release, /LC_ALL=C sort/u);
  // Exercise the actual publisher guard, including missing or unexpected symbols.
  const declarations = release.match(/          expected_app=.*\n          expected_dmg=.*\n          expected_dsym=.*\n/u)?.[0];
  const guard = release.match(/          if \[ "\$\{#assets\[@\]\}"[\s\S]*?          fi\n/u)?.[0];
  assert.ok(declarations && guard);
  for (const mode of ['signed', 'unsigned-dev']) {
    const prefix = `Cartograph_0.63.0_universal_${mode}`;
    const valid = [`${prefix}.app.zip`, `${prefix}.dSYM.zip`, `${prefix}.dmg`];
    for (const [assets, pass] of [
      [valid, true],
      [valid.filter((name) => !name.endsWith('.dSYM.zip')), false],
      [[...valid, 'extra.zip'], false],
      [[valid[0], 'Cartograph_0.62.0_universal_signed.dSYM.zip', valid[2]], false],
    ]) {
      const script = `${declarations}\nassets=(${assets.map((name) => `'${name}'`).join(' ')})\n${guard}`;
      const result = spawnSync('bash', ['-c', script], {
        env: { ...process.env, VERSION: '0.63.0', SIGNING_MODE: mode }, encoding: 'utf8',
      });
      assert.equal(result.status, pass ? 0 : 1, result.stderr || result.stdout);
    }
  }
});

test('macOS release packaging runs on the owner\'s self-hosted Mac', () => {
  // A hosted macos-latest runner did not finish the universal compile in 90
  // minutes (v0.71.0); the self-hosted Mac packages in about 15.
  const workflow = readFileSync(path.join(root, '.github/workflows/package.yml'), 'utf8');
  const job = workflow.split(/\n(?= {2}[a-z][\w-]*:\n)/u).find((block) => block.startsWith('  package:')) ?? '';
  assert.match(job, /^ {4}runs-on: \[self-hosted, macOS, ARM64\]$/mu);
  assert.match(job, /^ {4}timeout-minutes: 90$/mu);
  assert.doesNotMatch(triggers(workflow), /pull_request|issue_comment/u);
});

test('the privacy guard keeps its terms in a secret and never runs PR code with it', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/privacy-guard.yml'), 'utf8');
  assert.match(workflow, /DENYLIST: \$\{\{ secrets\.PRIVACY_DENYLIST \}\}/u);
  assert.match(triggers(workflow), /^\s{2}workflow_run:\n\s{4}workflows: \[CI\]/mu);
  assert.match(workflow, /github\.event\.workflow_run\.head_repository\.full_name == github\.repository/u);
  assert.match(workflow, /ref: main\n/u);
  assert.match(workflow, /persist-credentials: false/u);
  assert.match(workflow, /grep -iwFf "\$terms" >\/dev\/null/u);
  assert.doesNotMatch(workflow, /grep -q/u);
  assert.match(workflow, /read -r -d '' file/u);
  assert.match(workflow, /context=privacy-guard/u);
  // A push to main also starts CI; one shared group would let the skipped
  // workflow_run cancel the push scan.
  assert.match(workflow, /group: privacy-guard-\$\{\{ github\.event_name \}\}-/u);
});

test('scheduled real-repository lanes stay off pull requests and share one Mac slot (#1768, #1770, #1812, #1762)', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/scheduled.yml'), 'utf8');
  const on = triggers(workflow);
  assert.doesNotMatch(on, /pull_request|issue_comment|push:|workflow_run/u);
  assert.match(on, /^ {2}schedule:\n(?: {4}#.*\n| {4}- cron: '[^']+'\n)+/mu);
  assert.match(on, /^ {2}workflow_dispatch:/mu);
  // No repository secret: the self-hosted Macs never see one, and the issue
  // writes use the workflow's own token.
  assert.doesNotMatch(workflow, /secrets\./u);
  const jobs = Object.fromEntries(
    workflow.split(/^jobs:\n/mu)[1].split(/\n(?= {2}[a-z][\w-]*:\n)/u).map((block) => [block.match(/^ {2}([\w-]+):/u)[1], block]),
  );
  assert.deepEqual(Object.keys(jobs), ['policy', 'release-tags', 'macos-tests', 'simplelogin-showcase', 'scale-showcase']);
  for (const name of ['macos-tests', 'simplelogin-showcase', 'scale-showcase']) {
    const job = jobs[name];
    assert.match(job, /^ {4}runs-on: \[self-hosted, macOS, ARM64\]$/mu, name);
    assert.match(job, /^ {4}concurrency:\n {6}group: scheduled-macos\n {6}cancel-in-progress: false$/mu, name);
    assert.match(job, /^ {4}needs: policy$/mu, name);
    // The build stays incremental between runs and bounded on disk.
    assert.ok(job.includes('dir="$HOME/.cartograph-scheduled/target"'), name);
    assert.ok(job.includes('-gt 62914560 ]; then rm -rf "$dir"; fi'), name);
  }
  assert.match(jobs['release-tags'], /^ {4}runs-on: ubuntu-latest$/mu);
  // Every lane reports through one tracking issue per lane.
  for (const [name, title] of [
    ['release-tags', 'Scheduled: changelog versions without a release tag'],
    ['macos-tests', 'Scheduled: macOS Rust tests or determinism failing'],
    ['simplelogin-showcase', 'Scheduled: SimpleLogin showcase below its thresholds'],
    ['scale-showcase', 'Scheduled: scale showcase below its thresholds'],
  ]) {
    assert.ok(jobs[name].includes(`node scripts/scheduled-issue.mjs "$mode" --title '${title}'`), name);
    assert.match(jobs[name], /^ {6}issues: write$/mu, name);
  }
  // #1770: the whole workspace on macOS, app in four processes (#1621).
  const mac = jobs['macos-tests'];
  assert.ok(mac.includes('cargo test --locked --workspace --exclude app'));
  assert.ok(mac.includes('node scripts/rust-test-shard.mjs "$shard/4" -- --locked -p app --bin app'));
  // #1812: the ignored determinism test, exactly, at the pinned commit.
  assert.match(mac, /DETERMINISM_COMMIT: [0-9a-f]{40}\n/u);
  assert.ok(mac.includes('-- --ignored --exact \\\n            registered_source_tests::pinned_repository_reingest_yields_identical_graph_hash'));
  assert.ok(jobs['simplelogin-showcase'].includes('node scripts/showcase-watch.mjs simplelogin'));
  assert.ok(jobs['scale-showcase'].includes('node scripts/showcase-watch.mjs scale'));
  assert.ok(jobs['release-tags'].includes('node scripts/release-tags.mjs --window-hours 3'));
});
