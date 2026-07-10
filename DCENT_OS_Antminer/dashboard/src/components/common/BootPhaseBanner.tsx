// BootPhaseBanner — top-of-shell strip that surfaces the cold-boot
// progression returned by `/api/boot/phase` (W13.D1 backend).
//
// Renders one of:
//   • CV1835 6-substate: vertical-style progress (PSU init → PIC DC-DC →
//     ASIC enum → MiscCtrl triple-write → first WORK_TX → awaiting nonce).
//   • Generic 3-substate: a simple chip showing Booting / Starting / Mining.
//   • HybridModeNoApi: muted note explaining server.py fallback.
//   • dcentrald-down: degrade banner with last-known phase + log tail +
//     SSH restart hint (mirrors `DaemonStatusBanner`).
//
// Auto-fades once the terminal `mining` substate (or
// `boot_awaiting_first_nonce` followed by a "live" mining state) is
// reached. Polls `/api/boot/phase` every 1s while non-terminal, then
// stops. When `/api/boot/phase` returns 404 (older daemon), synthesizes
// the 3-substate fallback from `/api/status` data per
// .
//
// Cross-references:
//   •
//   •
//   •  (uses .ds-chip tokens)

import { useEffect, useRef, useState } from 'react';
import { api } from '../../api/client';
import { useDaemonHeartbeat } from '../../hooks/useDaemonHeartbeat';
import { useMinerStore } from '../../store/miner';
import type { BootPhase, BootPhaseResponse } from '../../api/types';

// ─── CV1835 substate metadata ──────────────────────────────────────────
const CV1835_STEPS: { id: string; label: string; hint: string }[] = [
  { id: 'boot_psu_init',              label: 'PSU init',           hint: 'APW12 SMBus 5-step bring-up' },
  { id: 'boot_pic_dc_dc_enable',      label: 'PIC DC-DC enable',   hint: 'PIC1704 chain rails up' },
  { id: 'boot_asic_enum',             label: 'ASIC enum',          hint: 'BM1362 GetAddress broadcast' },
  { id: 'boot_misc_ctrl_triple_write',label: 'MiscCtrl ×3',        hint: 'BM1362 MiscCtrl 3× w/ 5 ms' },
  { id: 'boot_first_work_tx',         label: 'First WORK_TX',      hint: 'First job dispatched to chain' },
  { id: 'boot_awaiting_first_nonce',  label: 'Awaiting nonce',     hint: 'Awaiting first WORK_RX' },
];

// Generic 3-substate metadata.
const GENERIC_STEPS: { id: string; label: string }[] = [
  { id: 'booting',  label: 'Booting'  },
  { id: 'starting', label: 'Starting' },
  { id: 'mining',   label: 'Mining'   },
];

const POLL_MS = 1000;
// Once we hit a terminal substate we keep the banner visible briefly so
// the user sees the "Mining" confirmation, then fade out.
const TERMINAL_FADE_MS = 4000;

// Synthesize a generic 3-substate from /api/status when /api/boot/phase
// is not available (older daemons / non-CV1835 platforms without
// backend wiring). The synthesis rules are the conservative version of
// the rules in :
//   • daemon dead/unreachable + no status      → 'booting'
//   • status present, no nonces in last 60s   → 'starting'
//   • status present, nonces in last 60s      → 'mining'
function synthesizeGenericPhase(opts: {
  daemonAlive: boolean;
  hasStatus: boolean;
  recentNonceMs: number | null;
}): BootPhase {
  if (!opts.daemonAlive || !opts.hasStatus) {
    return { kind: 'generic', phase: 'booting' };
  }
  const now = Date.now();
  if (opts.recentNonceMs && now - opts.recentNonceMs < 60_000) {
    return { kind: 'generic', phase: 'mining' };
  }
  return { kind: 'generic', phase: 'starting' };
}

function isTerminalPhase(phase: BootPhase): boolean {
  if (phase.kind === 'generic') return phase.phase === 'mining';
  // CV1835 boot is "done" once we leave `awaiting_first_nonce` — but the
  // backend doesn't transition into a synthetic post-boot state, so we
  // treat `awaiting_first_nonce` as the last visible step and let the
  // status hashrate fade us out.
  return false;
}

function isBootPhase(value: unknown): value is BootPhase {
  if (!value || typeof value !== 'object') return false;
  const phase = value as { kind?: unknown; phase?: unknown };
  if (phase.kind === 'hybrid_mode_no_api') return true;
  return (phase.kind === 'cv1835' || phase.kind === 'generic') && typeof phase.phase === 'string';
}

function isBootPhaseResponse(value: unknown): value is BootPhaseResponse {
  if (!value || typeof value !== 'object') return false;
  return isBootPhase((value as { phase?: unknown }).phase);
}

export interface BootPhaseBannerProps {
  /** Override the auto-poll for tests. */
  pollMs?: number;
}

export function BootPhaseBanner({ pollMs = POLL_MS }: BootPhaseBannerProps = {}) {
  const daemon = useDaemonHeartbeat();
  const status = useMinerStore(s => s.status);

  const [resp, setResp] = useState<BootPhaseResponse | null | undefined>(undefined);
  const [endpointMissing, setEndpointMissing] = useState(false);
  const [hidden, setHidden] = useState(false);
  // Last-known phase across daemon disconnects (for graceful degrade).
  const lastKnownRef = useRef<BootPhaseResponse | null>(null);

  // Pick a synthesized fallback we can show when the endpoint is missing
  // or while we wait for the first poll to land.
  const recentNonceMs = (() => {
    // Mining store exposes hashrate-ish telemetry; treat any chain with
    // freq>0 + accepted shares > 0 in the last minute as "mining".
    if (!status) return null;
    const accepted = status.accepted ?? 0;
    if (accepted > 0) return Date.now();
    return null;
  })();
  const synthesized = synthesizeGenericPhase({
    daemonAlive: daemon.state === 'alive',
    hasStatus: !!status,
    recentNonceMs,
  });

  // Polling
  useEffect(() => {
    if (hidden) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const poll = async () => {
      if (cancelled) return;
      try {
        const r = await api.getBootPhase();
        if (cancelled) return;
        if (r === null || !isBootPhaseResponse(r)) {
          // Endpoint missing or malformed -> synthesize.
          setEndpointMissing(true);
          setResp(null);
        } else {
          setEndpointMissing(false);
          setResp(r);
          lastKnownRef.current = r;
          if (isTerminalPhase(r.phase)) {
            timer = setTimeout(() => setHidden(true), TERMINAL_FADE_MS);
            return;
          }
        }
      } catch {
        // Network/fetch error — keep last-known, just retry next tick.
      }
      timer = setTimeout(poll, pollMs);
    };
    poll();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [pollMs, hidden]);

  if (hidden) return null;

  const isDaemonDown = daemon.state === 'dead';
  const isStarting = daemon.state === 'starting';

  // Pick the phase to render. Priority:
  // 1. Live response (when available + daemon alive)
  // 2. Last-known response (degrade banner during disconnect)
  // 3. Synthesized fallback
  let displayPhase: BootPhase;
  let isLive = true;
  let degraded = false;
  if (resp && isBootPhaseResponse(resp) && !endpointMissing) {
    displayPhase = resp.phase;
    isLive = !!resp.is_live;
  } else if (lastKnownRef.current) {
    displayPhase = lastKnownRef.current.phase;
    isLive = false;
    degraded = true;
  } else {
    displayPhase = synthesized;
    isLive = !endpointMissing;
  }

  // Once we've reached terminal mining state via synthesis, hide the
  // banner — we don't have a backend phase signal to drive it any longer.
  if (endpointMissing && displayPhase.kind === 'generic' && displayPhase.phase === 'mining') {
    return null;
  }

  return (
    <div
      role="status"
      data-testid="boot-phase-banner"
      data-phase-kind={displayPhase.kind}
      data-phase={displayPhase.kind === 'hybrid_mode_no_api' ? 'hybrid_mode_no_api' : (displayPhase as { phase: string }).phase}
      style={{
        background: isDaemonDown
          ? 'rgba(239, 68, 68, 0.10)'
          : isStarting
            ? 'rgba(255, 178, 0, 0.10)'
            : 'rgba(250, 165, 0, 0.08)',
        borderBottom: `1px solid ${
          isDaemonDown ? 'rgba(239, 68, 68, 0.45)' :
          isStarting ? 'rgba(255, 178, 0, 0.40)' :
          'rgba(250, 165, 0, 0.35)'
        }`,
        color: 'var(--text, #E8E8E8)',
        fontFamily: "'Inter', sans-serif",
        padding: '10px 20px',
        position: 'sticky',
        top: 0,
        zIndex: 8500,
        backdropFilter: 'blur(8px)',
      }}
    >
      <div style={{ display: 'flex', alignItems: 'center', gap: 16, flexWrap: 'wrap' }}>
        <div style={{
          fontSize: '0.7rem',
          fontWeight: 700,
          letterSpacing: '0.08em',
          textTransform: 'uppercase',
          color: 'var(--text-dim, #B5B5BD)',
          minWidth: 100,
        }}>
          Boot Phase
        </div>
        {displayPhase.kind === 'cv1835' && (
          <Cv1835Progress current={displayPhase.phase} />
        )}
        {displayPhase.kind === 'generic' && (
          <GenericChipRow current={displayPhase.phase} />
        )}
        {displayPhase.kind === 'hybrid_mode_no_api' && (
          <div style={{
            fontSize: '0.78rem',
            color: 'var(--text-dim, #B5B5BD)',
            fontStyle: 'italic',
          }}>
            Hybrid mode: API not available; status from server.py.
          </div>
        )}
        {(degraded || !isLive) && (
          <span
            className="ds-chip"
            style={{
              fontSize: '0.65rem',
              padding: '2px 8px',
              borderRadius: 6,
              background: 'rgba(239, 68, 68, 0.15)',
              color: '#EF4444',
              border: '1px solid rgba(239, 68, 68, 0.35)',
              letterSpacing: '0.06em',
              textTransform: 'uppercase',
              fontWeight: 700,
            }}
            title="Last-known phase; daemon is not currently reachable."
          >
            Last-known
          </span>
        )}
        {endpointMissing && (
          <span
            style={{
              fontSize: '0.7rem',
              color: 'var(--text-dim, #B5B5BD)',
              fontStyle: 'italic',
            }}
            title="Backend has no boot-phase tracker; phase synthesized from /api/status."
          >
            (synthesized)
          </span>
        )}
      </div>

      {isDaemonDown && (
        <DegradeFooter
          lastLogLines={daemon.lastLogLines}
        />
      )}
    </div>
  );
}

// ─── Subcomponents ─────────────────────────────────────────────────────

function Cv1835Progress({ current }: { current: string }) {
  const idx = CV1835_STEPS.findIndex(s => s.id === current);
  return (
    <div style={{
      display: 'flex',
      alignItems: 'center',
      gap: 6,
      flexWrap: 'wrap',
      flex: 1,
    }}>
      {CV1835_STEPS.map((step, i) => {
        const isCurrent = i === idx;
        const isDone = idx >= 0 && i < idx;
        return (
          <span key={step.id} style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <span
              className="ds-chip"
              data-testid={`boot-substate-${step.id}`}
              data-active={isCurrent ? 'true' : 'false'}
              data-done={isDone ? 'true' : 'false'}
              title={step.hint}
              style={{
                fontSize: '0.7rem',
                padding: '3px 9px',
                borderRadius: 6,
                background: isCurrent
                  ? 'rgba(250, 165, 0, 0.22)'
                  : isDone
                    ? 'rgba(16, 185, 129, 0.12)'
                    : 'rgba(255, 255, 255, 0.04)',
                color: isCurrent ? 'var(--accent, #FAA500)' : isDone ? 'var(--green, #10B981)' : 'var(--text-dim, #8B8B9E)',
                border: `1px solid ${
                  isCurrent ? 'rgba(250, 165, 0, 0.45)'
                  : isDone ? 'rgba(16, 185, 129, 0.25)'
                  : 'rgba(255, 255, 255, 0.08)'
                }`,
                fontWeight: isCurrent ? 700 : 500,
                whiteSpace: 'nowrap',
              }}
            >
              {step.label}
            </span>
            {i < CV1835_STEPS.length - 1 && (
              <span style={{ color: 'var(--text-dim, #5a5a66)', fontSize: '0.7rem' }} aria-hidden>→</span>
            )}
          </span>
        );
      })}
    </div>
  );
}

function GenericChipRow({ current }: { current: string }) {
  const idx = GENERIC_STEPS.findIndex(s => s.id === current);
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
      {GENERIC_STEPS.map((step, i) => {
        const isCurrent = i === idx;
        const isDone = idx >= 0 && i < idx;
        return (
          <span key={step.id} style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <span
              className="ds-chip"
              data-testid={`boot-substate-${step.id}`}
              data-active={isCurrent ? 'true' : 'false'}
              style={{
                fontSize: '0.78rem',
                padding: '4px 12px',
                borderRadius: 999,
                background: isCurrent
                  ? 'rgba(250, 165, 0, 0.22)'
                  : isDone
                    ? 'rgba(16, 185, 129, 0.12)'
                    : 'rgba(255, 255, 255, 0.04)',
                color: isCurrent ? 'var(--accent, #FAA500)' : isDone ? 'var(--green, #10B981)' : 'var(--text-dim, #8B8B9E)',
                border: `1px solid ${
                  isCurrent ? 'rgba(250, 165, 0, 0.45)' : 'rgba(255, 255, 255, 0.08)'
                }`,
                fontWeight: isCurrent ? 700 : 500,
              }}
            >
              {step.label}
            </span>
            {i < GENERIC_STEPS.length - 1 && (
              <span style={{ color: 'var(--text-dim, #5a5a66)', fontSize: '0.7rem' }} aria-hidden>→</span>
            )}
          </span>
        );
      })}
    </div>
  );
}

function DegradeFooter({ lastLogLines }: { lastLogLines: string[] }) {
  return (
    <div style={{
      marginTop: 10,
      padding: 10,
      borderRadius: 6,
      background: 'rgba(0, 0, 0, 0.35)',
      border: '1px solid rgba(255, 255, 255, 0.05)',
      fontFamily: "'JetBrains Mono', monospace",
      fontSize: '0.7rem',
      color: 'var(--text-dim, #B5B5BD)',
    }}>
      <div style={{ marginBottom: 6, color: '#EF4444' }}>
        dcentrald is not reachable — showing last-known boot phase.
      </div>
      {lastLogLines.length > 0 && (
        <pre
          data-testid="boot-degrade-log-tail"
          style={{
            margin: 0,
            padding: 6,
            background: '#0a0a0f',
            borderRadius: 4,
            maxHeight: 120,
            overflow: 'auto',
            whiteSpace: 'pre-wrap',
            wordBreak: 'break-all',
            fontSize: '0.68rem',
            color: '#B5B5BD',
            border: '1px solid rgba(255,255,255,0.05)',
          }}
        >
          {lastLogLines.slice(-8).join('\n')}
        </pre>
      )}
      <div style={{ marginTop: 6 }}>
        Restart hint:{' '}
        <code style={{ background: 'rgba(0,0,0,0.5)', padding: '2px 6px', borderRadius: 3 }}>
          ssh root@&lt;miner-ip&gt; /etc/init.d/S82dcentrald restart
        </code>
      </div>
    </div>
  );
}

export default BootPhaseBanner;
