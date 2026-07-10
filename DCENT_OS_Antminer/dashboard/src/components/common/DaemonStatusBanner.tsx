// DaemonStatusBanner — top-of-page banner that shows dcentrald liveness.
//
// Hidden when the daemon is healthy (alive + recent heartbeat). Shows a
// yellow "starting" banner during the first ~30s after boot, and a red
// "DEAD" banner with last log lines + restart instructions otherwise.
//
// Designed to NEVER unmount the rest of the dashboard — the dashboard keeps
// rendering with whatever cached data it has, while the user gets clear,
// actionable feedback that the daemon is the broken link.

import { useState } from 'react';
import { useDaemonHeartbeat } from '../../hooks/useDaemonHeartbeat';
import type { DaemonHealth } from '../../hooks/useDaemonHeartbeat';

const SSH_HINT = 'ssh root@<miner-ip> /etc/init.d/S82dcentrald restart';

export function DaemonStatusBanner() {
  const health = useDaemonHeartbeat();
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);

  // Healthy daemon → don't render anything, the dashboard speaks for itself.
  if (health.state === 'alive') {
    return null;
  }

  // While we haven't probed yet at all, stay quiet so we don't flash a
  // misleading red banner during the very first 0-1s of page load.
  if (health.state === 'unknown' && !health.lastProbeTs) {
    return null;
  }

  const isStarting = health.state === 'starting';
  const isDead = health.state === 'dead';
  // server.py being down is its own special case — usually means the whole
  // miner is offline / SSH-only. Surface that distinctly.
  const isServerPyDown = !health.serverPyAlive && !!health.lastProbeTs;

  const accent = isStarting ? 'var(--amber, #F59E0B)' : 'var(--red, #EF4444)';
  const tint = isStarting ? 'rgba(255, 178, 0, 0.10)' : 'rgba(239, 68, 68, 0.10)';
  const border = isStarting ? 'rgba(255, 178, 0, 0.45)' : 'rgba(239, 68, 68, 0.45)';

  let title: string;
  if (isServerPyDown) {
    title = 'Dashboard offline';
  } else if (isStarting) {
    title = 'dcentrald: starting';
  } else {
    title = 'dcentrald: NOT RESPONDING';
  }

  let detail: string;
  if (isServerPyDown) {
    detail = 'Could not reach the miner web server. Check network / power.';
  } else if (isStarting) {
    detail = `Daemon coming up${health.pid ? ` (pid ${health.pid})` : ''} — boot can take ~30s.`;
  } else if (health.pidAlive) {
    detail = `Process is running${health.pid ? ` (pid ${health.pid})` : ''} but its API is not answering. Last seen ${formatLastSeen(health.lastSeenSec)}.`;
  } else {
    detail = `No dcentrald process found. Last seen ${formatLastSeen(health.lastSeenSec)}.`;
  }

  const handleCopySsh = async () => {
    try {
      await navigator.clipboard.writeText(SSH_HINT);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard not available */
    }
  };

  return (
    <div
      role="alert"
      style={{
        background: tint,
        borderBottom: `1px solid ${border}`,
        borderLeft: `3px solid ${accent}`,
        color: 'var(--text, #E8E8E8)',
        fontFamily: "'Inter', sans-serif",
        padding: '10px 20px',
        position: 'sticky',
        top: 0,
        zIndex: 9000,
        backdropFilter: 'blur(10px) saturate(1.1)',
        WebkitBackdropFilter: 'blur(10px) saturate(1.1)',
        fontVariantNumeric: 'tabular-nums',
      }}
      data-testid="daemon-status-banner"
    >
      <div style={{
        display: 'flex',
        alignItems: 'center',
        gap: 16,
        flexWrap: 'wrap',
      }}>
        <span
          aria-hidden
          style={{
            display: 'inline-block',
            width: 10,
            height: 10,
            borderRadius: '50%',
            background: accent,
            boxShadow: `0 0 8px ${accent}`,
            animation: isDead ? 'dcentos-pulse 1.4s ease-in-out infinite' : undefined,
            flexShrink: 0,
          }}
        />
        <div style={{ flex: 1, minWidth: 200 }}>
          <div style={{
            fontWeight: 700,
            fontSize: '0.95rem',
            color: accent,
            letterSpacing: 0.2,
          }}>
            {title}
          </div>
          <div style={{
            fontSize: '0.8rem',
            color: 'var(--text-dim, #B5B5BD)',
            marginTop: 2,
          }}>
            {detail}
          </div>
        </div>

        {(isDead || isServerPyDown) && (
          <>
            <button
              onClick={handleCopySsh}
              style={{
                fontSize: '0.75rem',
                fontFamily: "'JetBrains Mono', monospace",
                padding: '6px 12px',
                borderRadius: 6,
                border: '1px solid var(--border, #333)',
                background: 'rgba(0, 0, 0, 0.25)',
                color: 'var(--text, #E8E8E8)',
                cursor: 'pointer',
                whiteSpace: 'nowrap',
              }}
              title="Copy SSH command to clipboard"
            >
              {copied ? 'Copied!' : 'Copy SSH restart cmd'}
            </button>
            <button
              onClick={() => setExpanded(v => !v)}
              style={{
                fontSize: '0.75rem',
                padding: '6px 12px',
                borderRadius: 6,
                border: 'none',
                background: accent,
                color: '#0a0a0f',
                fontWeight: 700,
                cursor: 'pointer',
                whiteSpace: 'nowrap',
              }}
            >
              {expanded ? 'Hide details' : 'Show details'}
            </button>
          </>
        )}
      </div>

      {expanded && (
        <div style={{
          marginTop: 12,
          padding: 12,
          borderRadius: 6,
          background: 'rgba(0, 0, 0, 0.35)',
          border: '1px solid rgba(255, 255, 255, 0.06)',
          fontFamily: "'JetBrains Mono', monospace",
          fontSize: '0.72rem',
          color: 'var(--text-dim, #B5B5BD)',
        }}>
          <div style={{ marginBottom: 8 }}>
            <strong style={{ color: 'var(--text, #E8E8E8)' }}>How to restart:</strong>{' '}
            run <code style={{ background: 'rgba(0,0,0,0.5)', padding: '2px 6px', borderRadius: 3 }}>
              {SSH_HINT}
            </code>
            {' '}from another machine on the same network.
          </div>
          {health.lastError && (
            <div style={{ marginBottom: 8, color: 'var(--red, #EF4444)' }}>
              Last error: {health.lastError}
            </div>
          )}
          {health.uptimeSec != null && (
            <div style={{ marginBottom: 8 }}>
              dcentrald uptime: {formatUptime(health.uptimeSec)}
            </div>
          )}
          {health.lastLogLines.length > 0 ? (
            <>
              <div style={{ marginBottom: 4, color: 'var(--text, #E8E8E8)' }}>
                Last log lines:
              </div>
              <pre style={{
                margin: 0,
                padding: 8,
                background: '#0a0a0f',
                borderRadius: 4,
                maxHeight: 220,
                overflow: 'auto',
                whiteSpace: 'pre-wrap',
                wordBreak: 'break-all',
                fontSize: '0.7rem',
                color: '#B5B5BD',
                border: '1px solid rgba(255,255,255,0.05)',
              }}>
                {health.lastLogLines.join('\n')}
              </pre>
            </>
          ) : (
            <div className="cp-empty-note">No log lines available.</div>
          )}
        </div>
      )}

      <style>{`
        @keyframes dcentos-pulse {
          0%, 100% { opacity: 1; }
          50% { opacity: 0.35; }
        }
      `}</style>
    </div>
  );
}

function formatLastSeen(sec: number | null): string {
  if (sec == null) return 'never (since dashboard load)';
  if (sec < 5) return 'just now';
  if (sec < 60) return `${sec}s ago`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m ago`;
  return `${Math.floor(sec / 3600)}h ago`;
}

function formatUptime(sec: number): string {
  if (sec < 60) return `${sec}s`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m ${sec % 60}s`;
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  return `${h}h ${m}m`;
}
