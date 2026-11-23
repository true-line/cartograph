import { pathToFileURL } from 'node:url';

// The macOS Developer ID set (ADR-0016).
export const SIGNING_SECRET_NAMES = Object.freeze([
  'CSC_LINK',
  'CSC_KEY_PASSWORD',
  'APPLE_API_KEY',
  'APPLE_API_KEY_ID',
  'APPLE_API_ISSUER',
]);

// The Windows Authenticode set: the Azure service principal that holds the
// Artifact Signing Certificate Profile Signer role (ADR-0093, #227). The
// account coordinates are not secret and live in windows-installer.yml.
export const WINDOWS_SIGNING_SECRET_NAMES = Object.freeze([
  'AZURE_TENANT_ID',
  'AZURE_CLIENT_ID',
  'AZURE_CLIENT_SECRET',
]);

const PLATFORMS = Object.freeze({
  macos: { names: SIGNING_SECRET_NAMES, label: 'Apple' },
  windows: { names: WINDOWS_SIGNING_SECRET_NAMES, label: 'Windows Authenticode' },
});

// No credential is `unsigned-dev`, the whole set is `signed`, and any part of
// it fails closed naming only what is missing, never a value.
export function signingMode(env = process.env, platform = 'macos') {
  const set = PLATFORMS[platform];
  if (!set) {
    throw new Error(`unknown signing platform: ${platform}`);
  }
  const present = set.names.filter((name) => String(env[name] ?? '').length > 0);

  if (present.length === 0) {
    return { mode: 'unsigned-dev', signed: false };
  }

  if (present.length !== set.names.length) {
    const missing = set.names.filter((name) => !present.includes(name));
    throw new Error(`partial ${set.label} signing credential set; missing: ${missing.join(', ')}`);
  }

  return { mode: 'signed', signed: true };
}

function main() {
  try {
    const result = signingMode(process.env, process.argv[2] ?? 'macos');
    process.stdout.write(`mode=${result.mode}\nsigned=${result.signed}\n`);
  } catch (error) {
    process.stderr.write(`::error::${error.message}\n`);
    process.exitCode = 1;
  }
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
