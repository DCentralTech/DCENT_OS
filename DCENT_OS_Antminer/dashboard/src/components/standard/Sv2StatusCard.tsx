import React from 'react';
import { useMinerStore } from '../../store/miner';
import type { Sv2SessionInfo } from '../../api/types';
import { glossaryText } from '../../utils/glossary';

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatFingerprint(fp: string): string {
  if (fp.length <= 12) return fp;
  return fp.slice(0, 8) + '...' + fp.slice(-4);
}

function formatCertDate(epoch: number): string {
  if (!epoch) return 'Unknown';
  const d = new Date(epoch * 1000);
  return d.toISOString().slice(0, 10);
}

function CertStatus({ notAfter }: { notAfter: number }) {
  const now = Date.now() / 1000;
  const daysLeft = Math.floor((notAfter - now) / 86400);
  const expired = daysLeft < 0;
  const expiring = daysLeft >= 0 && daysLeft < 30;
  const color = expired ? 'var(--red)' : expiring ? 'var(--yellow)' : 'var(--green)';
  const label = expired
    ? 'Expired'
    : `Valid until ${formatCertDate(notAfter)}`;

  return (
    <span style={{ color, fontWeight: 600 }}>{label}</span>
  );
}

const ROW_STYLE: React.CSSProperties = {
  display: 'flex',
  justifyContent: 'space-between',
  alignItems: 'center',
  padding: '5px 0',
  fontSize: '0.82rem',
};

const LABEL_STYLE: React.CSSProperties = {
  color: 'var(--text-dim)',
  fontSize: '0.78rem',
};

const VALUE_STYLE: React.CSSProperties = {
  fontFamily: "'JetBrains Mono', monospace",
  fontSize: '0.8rem',
  color: 'var(--text)',
};

function SessionDetails({ session }: { session: Sv2SessionInfo }) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
      <div style={ROW_STYLE}>
        <span style={LABEL_STYLE}>Cipher</span>
        <span style={VALUE_STYLE}>{session.cipher_suite || 'Noise_NX + ChaChaPoly1305'}</span>
      </div>
      <div style={ROW_STYLE}>
        <span style={LABEL_STYLE}>Handshake</span>
        <span style={VALUE_STYLE}>{session.handshake_latency_ms}ms</span>
      </div>
      <div style={ROW_STYLE}>
        <span style={LABEL_STYLE}>Channel</span>
        <span style={VALUE_STYLE}>
          Standard {session.channel_id != null ? `#${session.channel_id}` : ''}
        </span>
      </div>
      <div style={ROW_STYLE}>
        <span style={LABEL_STYLE}>Pool Key</span>
        <span
          style={{ ...VALUE_STYLE, cursor: 'default' }}
          title={session.pool_pubkey_fingerprint}
        >
          {formatFingerprint(session.pool_pubkey_fingerprint)}
        </span>
      </div>
      <div style={ROW_STYLE}>
        <span style={LABEL_STYLE}>Certificate</span>
        <CertStatus notAfter={session.certificate_not_after} />
      </div>

      <div style={{
        borderTop: '1px solid var(--border)',
        margin: '4px 0',
      }} />

      <div style={ROW_STYLE}>
        <span style={LABEL_STYLE}>Nonces</span>
        <span style={VALUE_STYLE}>
          TX: {session.noise_nonce_tx.toLocaleString()} / RX: {session.noise_nonce_rx.toLocaleString()}
        </span>
      </div>
      <div style={ROW_STYLE}>
        <span style={LABEL_STYLE}>Encrypted</span>
        <span style={VALUE_STYLE}>
          {formatBytes(session.bytes_encrypted)} sent / {formatBytes(session.bytes_decrypted)} recv
        </span>
      </div>
      {/* Wave-13: removed the "Share Latency" row — it bound to
          handshake_latency_ms, the same value as the "Handshake" row above
          (no share_submit_latency_ms exists), so it was a mislabeled duplicate. */}
    </div>
  );
}

export function Sv2StatusCard() {
  const sv2Session = useMinerStore(s => s.status?.pool?.sv2_session);
  const protocol = useMinerStore(s => s.status?.pool?.protocol);
  const autoFallbackActive = useMinerStore(s => s.status?.pool?.auto_fallback_active);
  const autoRetryAfterS = useMinerStore(s => s.status?.pool?.auto_retry_sv2_after_s);
  const autoFallbackReason = useMinerStore(s => s.status?.pool?.auto_fallback_reason);
  //  truth contract: an SV2 *session* is the only evidence the daemon
  // actually completed Noise handshake + channel negotiation. Protocol can
  // be advertised "sv2" while the session is still null (mid-handshake).
  const isSv2 = protocol === 'sv2' && sv2Session != null;
  //  truth: if `auto_fallback_active`, we are running V1 but SV2 was
  // attempted and fell back. Surface that distinct state explicitly.
  const sv2FellBack = !isSv2 && autoFallbackActive === true;

  return (
    <div style={{
      background: 'var(--card-bg)',
      borderRadius: 'var(--radius)',
      border: `1px solid ${isSv2 ? 'rgba(34,197,94,0.3)' : 'var(--border)'}`,
      padding: '14px 16px',
      transition: 'border-color 0.2s',
    }}>
      {/* Header */}
      <div style={{
        display: 'flex',
        alignItems: 'center',
        gap: 8,
        marginBottom: isSv2 ? 10 : 0,
      }}>
        <svg
          width="16" height="16" viewBox="0 0 16 16" fill="none"
          style={{ flexShrink: 0 }}
        >
          {isSv2 ? (
            /* Locked padlock — green */
            <path
              d="M4.5 7V5a3.5 3.5 0 1 1 7 0v2h.5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1h.5Zm1.5 0h4V5a2 2 0 1 0-4 0v2Z"
              fill="var(--green)"
            />
          ) : (
            /* Open padlock — dim */
            <path
              d="M4.5 7V5a3.5 3.5 0 0 1 6.65-1.5M11.5 5v2h.5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1h.5V5"
              stroke="var(--text-dim)" strokeWidth="1.2" fill="none"
            />
          )}
        </svg>
        <span style={{
          fontFamily: "var(--font-heading)",
          fontWeight: 700,
          fontSize: '0.95rem',
          color: isSv2 ? 'var(--green)' : 'var(--text-dim)',
        }}>
          {isSv2 ? 'Stratum V2 Session' : 'Stratum V2'}
        </span>
        {isSv2 && (
          <span style={{
            marginLeft: 'auto',
            background: 'rgba(34,197,94,0.15)',
            color: 'var(--green)',
            padding: '1px 8px',
            borderRadius: 8,
            fontSize: '0.7rem',
            fontWeight: 700,
            fontFamily: "'JetBrains Mono', monospace",
          }}>
            ENCRYPTED
          </span>
        )}
      </div>

      {isSv2 ? (
        <SessionDetails session={sv2Session} />
      ) : sv2FellBack ? (
        <div style={{
          color: 'var(--text-dim)',
          fontSize: '0.82rem',
          lineHeight: 1.5,
          marginTop: 6,
        }}>
          <div
            style={{ color: 'var(--yellow)', fontWeight: 600, marginBottom: 4 }}
            data-tooltip={glossaryText('sv2_fellback')}
          >
            Stratum V1 (auto-fallback active)
          </div>
          SV2 handshake failed{autoFallbackReason ? `: ${autoFallbackReason}` : ''}.
          {typeof autoRetryAfterS === 'number' && autoRetryAfterS > 0
            && ` Auto-retry in ~${Math.round(autoRetryAfterS)}s.`}
        </div>
      ) : (
        <div style={{
          color: 'var(--text-dim)',
          fontSize: '0.82rem',
          lineHeight: 1.5,
          marginTop: 6,
        }}>
          Stratum V1 only. Connect to an SV2-capable pool to
          enable encrypted, low-latency mining with job negotiation.
        </div>
      )}
    </div>
  );
}
