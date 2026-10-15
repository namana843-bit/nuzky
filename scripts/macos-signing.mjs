import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, symlinkSync } from 'node:fs';
import path from 'node:path';

export function macosSigning(env) {
  const certificateKeys = ['APPLE_CERTIFICATE', 'APPLE_CERTIFICATE_PASSWORD'];
  const notaryKeys = ['APPLE_ID', 'APPLE_TEAM_ID', 'APPLE_APP_PASSWORD'];
  const certificate = certificateKeys.some(key => env[key]);
  const notary = notaryKeys.some(key => env[key]);
  const missing = [
    ...(certificate ? certificateKeys.filter(key => !env[key]) : []),
    ...(notary ? notaryKeys.filter(key => !env[key]) : []),
    ...((certificate || notary) && !env.APPLE_SIGNING_IDENTITY ? ['APPLE_SIGNING_IDENTITY'] : []),
  ];
  if (missing.length) throw new Error(`Missing macOS signing variables: ${missing.join(', ')}`);
  return {
    identity: env.APPLE_SIGNING_IDENTITY || '-',
    certificate: certificate ? { base64: env.APPLE_CERTIFICATE, password: env.APPLE_CERTIFICATE_PASSWORD } : null,
    notary: notary ? { appleId: env.APPLE_ID, teamId: env.APPLE_TEAM_ID, password: env.APPLE_APP_PASSWORD } : null,
  };
}

export function createDmg(app, dmg, target) {
  mkdirSync(path.dirname(dmg), { recursive: true });
  const stage = mkdtempSync(path.join(target, 'dmg-stage-'));
  try {
    execFileSync('ditto', [app, path.join(stage, 'Nuzky.app')], { stdio: 'inherit' });
    symlinkSync('/Applications', path.join(stage, 'Applications'));
    execFileSync('hdiutil', ['create', '-ov', '-format', 'UDZO', '-volname', 'Nuzky', '-srcfolder', stage, dmg], { stdio: 'inherit' });
  } finally {
    rmSync(stage, { recursive: true, force: true });
  }
}
