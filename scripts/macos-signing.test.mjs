import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { macosSigning } from './macos-signing.mjs';

const identity = 'Developer ID Application: Example (TEAM123456)';
const certificate = { APPLE_CERTIFICATE: 'cDEy', APPLE_CERTIFICATE_PASSWORD: 'certificate-password' };
const notary = { APPLE_ID: 'apple@example.com', APPLE_TEAM_ID: 'TEAM123456', APPLE_APP_PASSWORD: 'app-password' };

test('without variables, or with the empty values of unset GitHub secrets, the build signs ad-hoc', () => {
  const adHoc = { identity: '-', certificate: null, notary: null };
  assert.deepEqual(macosSigning({}), adHoc);
  assert.deepEqual(macosSigning({
    APPLE_SIGNING_IDENTITY: '', APPLE_CERTIFICATE: '', APPLE_CERTIFICATE_PASSWORD: '',
    APPLE_ID: '', APPLE_TEAM_ID: '', APPLE_APP_PASSWORD: '',
  }), adHoc);
});

test('all variables sign with the Developer ID and notarize', () => {
  assert.deepEqual(macosSigning({ APPLE_SIGNING_IDENTITY: identity, ...certificate, ...notary }), {
    identity,
    certificate: { base64: 'cDEy', password: 'certificate-password' },
    notary: { appleId: 'apple@example.com', teamId: 'TEAM123456', password: 'app-password' },
  });
});

test('an identity without notary credentials signs and does not notarize', () => {
  assert.deepEqual(macosSigning({ APPLE_SIGNING_IDENTITY: identity, ...certificate }), {
    identity, certificate: { base64: 'cDEy', password: 'certificate-password' }, notary: null,
  });
  assert.deepEqual(macosSigning({ APPLE_SIGNING_IDENTITY: identity }), { identity, certificate: null, notary: null });
});

test('an incomplete group stops the build and names only the missing variables', () => {
  assert.throws(() => macosSigning({ APPLE_SIGNING_IDENTITY: identity, APPLE_ID: 'apple@example.com' }),
    { message: 'Missing macOS signing variables: APPLE_TEAM_ID, APPLE_APP_PASSWORD' });
  assert.throws(() => macosSigning({ APPLE_SIGNING_IDENTITY: identity, APPLE_CERTIFICATE_PASSWORD: 'certificate-password' }),
    { message: 'Missing macOS signing variables: APPLE_CERTIFICATE' });
  assert.throws(() => macosSigning({ ...certificate, ...notary }),
    { message: 'Missing macOS signing variables: APPLE_SIGNING_IDENTITY' });
});

test('the release notes have the one macOS line that release.yml drops for a notarized dmg', () => {
  const notes = readFileSync(new URL('release-notes.md', import.meta.url), 'utf8');
  const lines = notes.split('\n').filter((line) => line.startsWith('- **macOS**:'));
  assert.equal(lines.length, 1);
  assert.match(lines[0], /System Settings → Privacy & Security, click Open Anyway/);
});
