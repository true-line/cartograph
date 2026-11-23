import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { SIGNING_SECRET_NAMES, WINDOWS_SIGNING_SECRET_NAMES, signingMode } from './signing-secrets.mjs';

const SETS = [
  ['macos', SIGNING_SECRET_NAMES],
  ['windows', WINDOWS_SIGNING_SECRET_NAMES],
];

test('an empty signing environment is visibly unsigned', () => {
  assert.deepEqual(signingMode({}), { mode: 'unsigned-dev', signed: false });
  assert.deepEqual(signingMode({}, 'windows'), { mode: 'unsigned-dev', signed: false });
});

test('every complete signing set enables the production path', () => {
  for (const [platform, names] of SETS) {
    const env = Object.fromEntries(names.map((name) => [name, `${name}-value`]));
    assert.deepEqual(signingMode(env, platform), { mode: 'signed', signed: true });
  }
});

test('every partial signing set fails closed without exposing values', () => {
  for (const [platform, names] of SETS) {
    for (const omitted of names) {
      const env = Object.fromEntries(
        names.filter((name) => name !== omitted).map((name) => [name, 'sensitive']),
      );

      assert.throws(
        () => signingMode(env, platform),
        (error) => error.message.includes(omitted) && !error.message.includes('sensitive'),
      );
    }
  }
});

// ADR-0093 (#227): one platform's credentials never sign for the other.
test('the macOS and Windows sets are disjoint and classified apart', () => {
  assert.equal(SIGNING_SECRET_NAMES.filter((name) => WINDOWS_SIGNING_SECRET_NAMES.includes(name)).length, 0);
  const apple = Object.fromEntries(SIGNING_SECRET_NAMES.map((name) => [name, 'x']));
  assert.deepEqual(signingMode(apple, 'windows'), { mode: 'unsigned-dev', signed: false });
  const azure = Object.fromEntries(WINDOWS_SIGNING_SECRET_NAMES.map((name) => [name, 'x']));
  assert.deepEqual(signingMode(azure, 'macos'), { mode: 'unsigned-dev', signed: false });
  assert.throws(() => signingMode({}, 'linux'), /unknown signing platform/u);
});

test('the command prints the mode for the named platform and defaults to macOS', () => {
  const script = fileURLToPath(new URL('./signing-secrets.mjs', import.meta.url));
  const env = { PATH: process.env.PATH };
  assert.equal(execFileSync(process.execPath, [script], { env, encoding: 'utf8' }), 'mode=unsigned-dev\nsigned=false\n');
  for (const name of WINDOWS_SIGNING_SECRET_NAMES) env[name] = 'x';
  assert.equal(
    execFileSync(process.execPath, [script, 'windows'], { env, encoding: 'utf8' }),
    'mode=signed\nsigned=true\n',
  );
});
