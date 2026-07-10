import React, { useEffect, useState } from 'react';
import { api } from '../../api/client';
import type { NetworkInfoResponse, MinerTypeResponse } from '../../api/types';

/**
 * W11.12 — Stock-CGI parity panel.
 *
 * Surfaces three RE2 §15.2 / competing-firmware features in one card:
 *   1. Network identity (`/api/network/info`)
 *   2. Miner identity (`/api/miner/type`)
 *   3. Support-bundle download (`/api/log/backup`)
 *
 * Lives on the About tab today; can be reused on Settings → Network or
 * Diagnostics. Read-only. Degrades gracefully when the daemon is down
 * — every section
 * shows last-known data + a clear "—" when unreachable, never a blank
 * page or a thrown error.
 */
export function NetworkInfoCard() {
  const [net, setNet] = useState<NetworkInfoResponse | null | undefined>(undefined);
  const [miner, setMiner] = useState<MinerTypeResponse | null | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api.getNetworkInfo()
      .then(r => { if (!cancelled) setNet(r); })
      .catch(() => { if (!cancelled) setNet(null); });
    api.getMinerType()
      .then(r => { if (!cancelled) setMiner(r); })
      .catch(() => { if (!cancelled) setMiner(null); });
    return () => { cancelled = true; };
  }, []);

  const handleDownloadBundle = async () => {
    setBusy(true);
    setDownloadError(null);
    try {
      const bundle = await api.fetchLogBackup();
      if (!bundle) {
        setDownloadError('Daemon does not expose /api/log/backup yet.');
        return;
      }
      // Build a Blob and trigger a save dialog.
      const blob = new Blob([bundle.text], { type: 'text/plain;charset=utf-8' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = bundle.filename;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      // Defer revoke so the browser can finish reading.
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (err) {
      setDownloadError(err instanceof Error ? err.message : 'Download failed.');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="ds-card"
      style={{
        background: 'var(--card-bg)',
        border: '1px solid var(--border)',
        borderRadius: 12,
        padding: 20,
        marginTop: 16,
      }}
    >
      <div style={{
        fontSize: '0.7rem',
        fontWeight: 700,
        letterSpacing: '0.08em',
        textTransform: 'uppercase',
        color: 'var(--text-dim)',
        marginBottom: 10,
      }}>
        Network &amp; Identity
      </div>

      <KvSection title="Network">
        <Kv label="Hostname"   value={net?.hostname} />
        <Kv label="MAC"        value={net?.mac} mono />
        <Kv label="IPv4"       value={net?.ipv4_cidr || net?.ipv4} mono />
        <Kv label="IPv6"       value={net?.ipv6} mono />
        <Kv label="Gateway"    value={net?.gateway} mono />
        <Kv label="DNS"        value={net?.dns} mono />
        <Kv label="Interface"  value={net?.primary_interface} mono />
        <Kv label="Link"       value={net?.link_state} />
        <Kv label="DHCP"       value={typeof net?.dhcp === 'boolean' ? (net.dhcp ? 'yes' : 'no') : undefined} />
      </KvSection>

      {net?.warnings && net.warnings.length > 0 && (
        <div style={{
          marginTop: 10,
          padding: 10,
          borderRadius: 8,
          border: '1px solid rgba(245, 158, 11, 0.35)',
          background: 'rgba(245, 158, 11, 0.08)',
          fontSize: '0.78rem',
          color: 'var(--amber, #F59E0B)',
        }}>
          <div style={{ fontWeight: 700, marginBottom: 4 }}>Warnings</div>
          <ul style={{ margin: 0, paddingLeft: 18 }}>
            {net.warnings.map((w, i) => <li key={i}>{w}</li>)}
          </ul>
        </div>
      )}

      <div style={{ height: 1, background: 'var(--border)', margin: '16px 0' }} />

      <KvSection title="Hardware identity">
        <Kv label="Model"          value={miner?.model} />
        <Kv label="ASIC"           value={miner?.asic} mono />
        <Kv label="Chips"          value={miner?.chip_count != null ? `${miner.chip_count}` : undefined} />
        <Kv label="Hashboards"     value={miner?.chain_count != null ? `${miner.chain_count}` : undefined} />
        <Kv label="Hashboard"      value={miner?.hashboard} mono />
        <Kv label="Control board"  value={miner?.control_board} mono />
        <Kv label="SoC"            value={miner?.soc} />
        <Kv label="Firmware"       value={miner ? `${miner.firmware} ${miner.firmware_version}` : undefined} />
      </KvSection>

      <div style={{ height: 1, background: 'var(--border)', margin: '16px 0' }} />

      <div>
        <div style={{
          fontSize: '0.78rem',
          color: 'var(--text-secondary)',
          marginBottom: 8,
          lineHeight: 1.5,
        }}>
          Download a redacted text bundle (miner snapshot + daemon log
          tail + dmesg) for support tickets. Passwords and tokens are
          scrubbed before the file leaves the miner.
        </div>
        <button
          type="button"
          className="btn btn-secondary"
          onClick={handleDownloadBundle}
          disabled={busy}
          style={{ fontSize: '0.85rem', padding: '6px 14px' }}
        >
          {busy ? 'Building bundle…' : 'Download support bundle'}
        </button>
        {downloadError && (
          <div style={{
            marginTop: 8,
            color: 'var(--red, #EF4444)',
            fontSize: '0.78rem',
          }}>
            {downloadError}
          </div>
        )}
      </div>
    </div>
  );
}

interface KvProps {
  label: string;
  value: string | undefined | null;
  mono?: boolean;
}

function Kv({ label, value, mono }: KvProps) {
  const display = value && value.length > 0 ? value : '—';
  return (
    <div style={{
      display: 'flex',
      justifyContent: 'space-between',
      gap: 16,
      padding: '4px 0',
      fontSize: '0.85rem',
    }}>
      <span style={{ color: 'var(--text-dim)' }}>{label}</span>
      <span
        style={{
          color: 'var(--text)',
          textAlign: 'right',
          fontFamily: mono ? 'var(--font-mono, monospace)' : undefined,
          wordBreak: 'break-all',
        }}
      >
        {display}
      </span>
    </div>
  );
}

function KvSection(props: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <div style={{
        fontSize: '0.65rem',
        fontWeight: 700,
        letterSpacing: '0.06em',
        textTransform: 'uppercase',
        color: 'var(--text-dim)',
        marginBottom: 6,
      }}>
        {props.title}
      </div>
      <div>{props.children}</div>
    </div>
  );
}

export default NetworkInfoCard;
