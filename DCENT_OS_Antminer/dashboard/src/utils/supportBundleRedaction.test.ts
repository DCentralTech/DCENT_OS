import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { redactSupportBundlePayload, SUPPORT_BUNDLE_REDACTED } from './supportBundleRedaction';

describe('support bundle redaction', () => {
  it('redacts credential fields, bearer headers, and credential-bearing URLs', () => {
    const payload = {
      current: {
        password: 'owner-secret',
        apiToken: 'session-token-secret',
        auth: {
          password_set: true,
          token_issued: true,
        },
      },
      alerts: [
        {
          message: 'pool=stratum+tcp://bc1qworker:pool-pass@pool.example:3333 password=hunter2',
        },
        {
          message: 'Authorization: Bearer abc.def.ghi token: cli-token',
        },
      ],
      logs: [
        'api_key=abcd1234 secret=raw-secret',
        'basic header Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==',
      ],
    };

    const redacted = redactSupportBundlePayload(payload);
    const encoded = JSON.stringify(redacted);

    expect(encoded).not.toContain('owner-secret');
    expect(encoded).not.toContain('session-token-secret');
    expect(encoded).not.toContain('pool-pass');
    expect(encoded).not.toContain('hunter2');
    expect(encoded).not.toContain('abc.def.ghi');
    expect(encoded).not.toContain('cli-token');
    expect(encoded).not.toContain('abcd1234');
    expect(encoded).not.toContain('raw-secret');
    expect(encoded).not.toContain('QWxhZGRpbjpvcGVuIHNlc2FtZQ');
    expect(encoded).toContain(SUPPORT_BUNDLE_REDACTED);
    expect(redacted.current.auth.password_set).toBe(true);
    expect(redacted.current.auth.token_issued).toBe(true);
  });

  it('redacts non-credential PII: wallet/worker, MAC, and device serial', () => {
    const payload = {
      pool: {
        // Bitcoin worker string is `<btc-payout-address>.<rig>` — the wallet
        // must never ship in a "safe to share" bundle.
        worker_name: 'bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq.rig1',
        worker: 'bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq.rig2',
      },
      system: {
        mac: 'aa:bb:cc:dd:ee:ff',
        macAddr: '32304127f6ab', // colonless field value
        miner_serial: 'ANTS9-DEADBEEF-0001',
        mac_present: true, // boolean must survive (only string values redact)
      },
      logs: [
        'first-light: enumerated 189 chips for bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq',
        'net: link up on aa:bb:cc:dd:ee:ff',
      ],
    };

    const redacted = redactSupportBundlePayload(payload);
    const encoded = JSON.stringify(redacted);

    expect(encoded).not.toContain('bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq');
    expect(encoded).not.toContain('aa:bb:cc:dd:ee:ff');
    expect(encoded).not.toContain('32304127f6ab');
    expect(encoded).not.toContain('ANTS9-DEADBEEF-0001');
    expect(encoded).toContain(SUPPORT_BUNDLE_REDACTED);
    // Boolean presence flags are not PII and must survive.
    expect(redacted.system.mac_present).toBe(true);
  });

  it('keeps dashboard session-export copy aligned with the implemented bundle', () => {
    const advancedDashboard = readFileSync('src/components/advanced/AdvancedDashboard.tsx', 'utf8');
    const sessionExport = readFileSync('src/components/advanced/SessionShareExportPanel.tsx', 'utf8');

    expect(advancedDashboard).toContain('Redacted browser-session bundle');
    expect(advancedDashboard).not.toMatch(/dmesg/i);
    expect(sessionExport).toContain('redactSupportBundlePayload');
    expect(sessionExport).toContain('redaction');
  });
});
