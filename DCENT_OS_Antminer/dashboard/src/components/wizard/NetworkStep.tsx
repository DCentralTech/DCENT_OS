// DCENT_OS Setup Wizard — Network step.
//
// Structural recreation of the kit `NetworkStep` (ui_kits/wizard/Wizard.jsx):
// the radio stack (Ethernet / Wi-Fi via Expansion Pack / DCENT Expansion
// Pack) + the "Local-first" info note.
//
// HONESTY (per brief): production has NO setup-network endpoint. We do NOT
// fabricate a network-config call. This step:
//   • is marked OPTIONAL on the rail and ALWAYS skippable;
//   • reads REAL network facts (IP / hostname / MAC) via api.getSystemInfo()
//     — read-only, the same call NetworkInfoPreview uses below;
//   • the radio choice is purely informational/local and is NEVER persisted
//     through a made-up endpoint.
// NetworkInfoPreview is still exported (NameStep folds it in).

import React, { useState, useEffect } from 'react';
import { api } from '../../api/client';

interface NetworkStepProps {
  value: string;
  onChange: (value: string) => void;
}

interface NetOption {
  id: string;
  l: string;
  sub: string;
  detail: string;
}

export function NetworkStep({ value, onChange }: NetworkStepProps) {
  const [ip, setIp] = useState<string>('');
  const [mac, setMac] = useState<string>('');

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const sys = await api.getSystemInfo();
        if (!cancelled) {
          setIp(window.location.hostname || '');
          setMac(sys.mac || '');
        }
      } catch {
        if (!cancelled) setIp(window.location.hostname || '');
      }
    })();
    return () => { cancelled = true; };
  }, []);

  const opts: NetOption[] = [
    {
      id: 'eth',
      l: 'Ethernet',
      sub: ip ? `Connected · ${ip}${mac ? ` · ${mac}` : ''}` : 'Connected via Ethernet',
      detail: "You're already online via Ethernet. This is the most reliable path.",
    },
    {
      id: 'wifi',
      l: 'Wi-Fi',
      sub: 'Requires DCENT Expansion Pack (ESP32-C6).',
      detail: 'The miner itself has no Wi-Fi radio. Plug in the DCENT Expansion Pack to add it.',
    },
    {
      id: 'xpack',
      l: 'DCENT Expansion Pack',
      sub: 'Wi-Fi + OLED + external thermostat.',
      detail: 'Adds Wi-Fi, an OLED status screen, and a JST temperature port. Pairs via one QR code.',
    },
  ];

  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">Network</h2>
      <p className="wiz-lede">
        How is this miner online? This step is informational — DCENT_OS detects the
        connection automatically, so you can skip it.
      </p>

      <div className="wiz-radio-stack" role="radiogroup" aria-label="Network connection">
        {opts.map(o => {
          const active = value === o.id;
          return (
            <label key={o.id} className={`wiz-radio${active ? ' active' : ''}`}>
              <input
                type="radio"
                name="wiz-net"
                value={o.id}
                checked={active}
                onChange={() => onChange(o.id)}
              />
              <span className="wiz-radio-dot" />
              <span className="wiz-radio-text">
                <strong>{o.l}</strong>
                <span>{o.sub}</span>
                <small>{o.detail}</small>
              </span>
            </label>
          );
        })}
      </div>

      <div className="wiz-info">
        <strong>Local-first.</strong> DCENT_OS doesn&apos;t phone home. Your network
        choice stays on this device — it isn&apos;t sent anywhere and isn&apos;t
        required to finish setup.
      </div>
    </div>
  );
}

// ─── Compact Network Info Preview (folded into NameStep) ───────────────
interface NetworkInfoPreviewProps {
  hostname: string;
}

export function NetworkInfoPreview({ hostname }: NetworkInfoPreviewProps) {
  const [ip, setIp] = useState<string>('');

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const sysInfo = await api.getSystemInfo();
        if (!cancelled) setIp(window.location.hostname || sysInfo.hostname || '');
      } catch {
        if (!cancelled) setIp(window.location.hostname || '');
      }
    })();
    return () => { cancelled = true; };
  }, []);

  const derivedHostname = hostname || 'dcentos';

  return (
    <div
      style={{
        background: 'var(--wz-surface-glass-card)',
        borderRadius: 'var(--wz-radius-sm)',
        border: '1px solid var(--wz-border-glass)',
        padding: '14px 18px',
      }}
    >
      <div
        style={{
          color: 'var(--wz-fg-dim)',
          fontSize: '.72rem',
          fontWeight: 700,
          textTransform: 'uppercase',
          letterSpacing: '.05em',
          marginBottom: 10,
          fontFamily: 'var(--wz-font-mono)',
        }}
      >
        Network
      </div>
      <dl style={{ display: 'flex', flexDirection: 'column', gap: 6, margin: 0 }}>
        {ip && (
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
            <dt style={{ color: 'var(--wz-fg-secondary)', fontSize: '.82rem' }}>IP Address</dt>
            <dd style={{ fontFamily: 'var(--wz-font-mono)', color: 'var(--wz-fg-primary)', fontSize: '.88rem', fontWeight: 600, margin: 0 }}>
              {ip}
            </dd>
          </div>
        )}
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <dt style={{ color: 'var(--wz-fg-secondary)', fontSize: '.82rem' }}>Hostname</dt>
          <dd style={{ fontFamily: 'var(--wz-font-mono)', color: 'var(--wz-fg-primary)', fontSize: '.88rem', fontWeight: 600, margin: 0 }}>
            {derivedHostname}.local
          </dd>
        </div>
      </dl>
    </div>
  );
}
