import type { CSSProperties } from 'react';
import type { BosminerBlocker, ChainRailVerdict, SystemHealthResponse } from '../../../api/types';
import {
  getHonestModeState,
  getLastSeenAgeMs,
  type HonestModeState,
  SystemHealthProvider,
  useSystemHealth,
} from './SystemHealthContext';

export { SystemHealthProvider, useSystemHealth, getHonestModeState };
export type { HonestModeState };

interface HonestModeCardProps {
  compact?: boolean;
  style?: CSSProperties;
}

const STATE_TONE: Record<HonestModeState, {
  title: string;
  accent: string;
  background: string;
  border: string;
}> = {
  unknown: {
    title: 'Health endpoint unavailable',
    accent: '#9CA3AF',
    background: 'rgba(156, 163, 175, 0.08)',
    border: 'rgba(156, 163, 175, 0.22)',
  },
  native: {
    title: 'Native chain driver',
    accent: 'var(--green, #2DD4A0)',
    background: 'rgba(45, 212, 160, 0.08)',
    border: 'rgba(45, 212, 160, 0.25)',
  },
  proxy_alive: {
    title: 'Proxy mode: bosminer alive',
    accent: 'var(--amber, #F59E0B)',
    background: 'rgba(245, 158, 11, 0.10)',
    border: 'rgba(245, 158, 11, 0.32)',
  },
  proxy_degraded: {
    title: 'Proxy degraded: reconnecting',
    accent: 'var(--accent-deep, #F97316)',
    background: 'rgba(249, 115, 22, 0.11)',
    border: 'rgba(249, 115, 22, 0.35)',
  },
  hardware_blocked: {
    title: 'Hardware blocked',
    accent: 'var(--red, #EF4444)',
    background: 'rgba(239, 68, 68, 0.12)',
    border: 'rgba(239, 68, 68, 0.45)',
  },
  //  HIGH-1 (2026-05-24): `a lab unit`-class XIL S19j Pro running the
  //  bosminer-handoff mining recipe. Orange-green hybrid treatment —
  // green to signal "actively mining" but with an orange undertone for the
  // AC-cycle dependency operator caveat (the session survives daemon-uptime
  // but does NOT survive reboot).
  handoff_mining: {
    title: 'Handoff (bosminer pre-engaged)',
    accent: 'var(--green, #2DD4A0)',
    background: 'linear-gradient(135deg, rgba(45, 212, 160, 0.10) 0%, rgba(250, 165, 0, 0.10) 100%)',
    border: 'rgba(250, 165, 0, 0.38)',
  },
};

export function HonestModeBanner() {
  const { health, endpointAvailable, state } = useSystemHealth();

  // `handoff_mining` is the  "happy path" on `a lab unit` — actively mining,
  // no operator action required, no banner needed.
  if (
    !endpointAvailable ||
    !health ||
    state === 'native' ||
    state === 'proxy_alive' ||
    state === 'handoff_mining' ||
    state === 'unknown'
  ) {
    return null;
  }

  const tone = STATE_TONE[state];
  const age = formatAge(getLastSeenAgeMs(health));

  return (
    <div
      role="status"
      style={{
        position: 'sticky',
        top: 0,
        zIndex: 8999,
        padding: '8px 20px',
        background: tone.background,
        borderBottom: `1px solid ${tone.border}`,
        color: 'var(--text, #E8E8E8)',
        fontFamily: "'Inter', sans-serif",
        backdropFilter: 'blur(8px)',
      }}
      data-testid="honest-mode-banner"
    >
      <div style={{ display: 'flex', gap: 12, alignItems: 'center', flexWrap: 'wrap' }}>
        <StatusDot color={tone.accent} pulse={state === 'proxy_degraded'} />
        <strong style={{ color: tone.accent, fontSize: '0.85rem' }}>{tone.title}</strong>
        <span style={{ color: 'var(--text-secondary, #B5B5BD)', fontSize: '0.78rem' }}>
          {state === 'hardware_blocked'
            ? 'No hashrate is claimed. Check rail/UART recovery before native takeover.'
            : `Bosminer API is not live${age ? `; last seen ${age}` : ''}. Last-known stats may be stale.`}
        </span>
        <InlineFacts health={health} />
      </div>
    </div>
  );
}

export function HonestModeCard({ compact = false, style }: HonestModeCardProps) {
  const { health, endpointAvailable, state } = useSystemHealth();

  if (!endpointAvailable || !health || state === 'unknown') {
    return null;
  }

  const tone = STATE_TONE[state];
  const detail = getStateDetail(health, state);
  const blockers = health.bosminer?.blockers ?? [];
  const nextAction = health.recovery?.next_action ?? null;
  const rail = health.rail;

  return (
    <section
      aria-label="Mining telemetry source"
      style={{
        ...style,
        background: tone.background,
        border: `1px solid ${tone.border}`,
        borderRadius: 8,
        padding: compact ? '10px 12px' : 14,
        color: 'var(--text, #E8E8E8)',
        fontFamily: "'Inter', sans-serif",
      }}
      data-testid="honest-mode-card"
    >
      <div style={{ display: 'flex', gap: 12, alignItems: 'flex-start', justifyContent: 'space-between', flexWrap: 'wrap' }}>
        <div style={{ display: 'flex', gap: 10, alignItems: 'flex-start', minWidth: 220, flex: 1 }}>
          <StatusDot color={tone.accent} pulse={state === 'proxy_degraded'} />
          <div>
            <div style={{ color: tone.accent, fontWeight: 800, fontSize: compact ? '0.86rem' : '0.95rem' }}>
              {tone.title}
            </div>
            <div style={{ color: 'var(--text-secondary, #B5B5BD)', fontSize: compact ? '0.76rem' : '0.82rem', marginTop: 3, lineHeight: 1.45 }}>
              {detail}
            </div>
          </div>
        </div>

        <InlineFacts health={health} />
      </div>

      {(blockers.length > 0 || rail || nextAction) && state !== 'native' && (
        <div style={{
          display: 'flex',
          gap: 8,
          flexWrap: 'wrap',
          alignItems: 'center',
          marginTop: 10,
          fontSize: '0.72rem',
          color: 'var(--text-secondary, #B5B5BD)',
        }}>
          {rail && (
            <FactPill label="rail" value={formatRailVerdict(rail.verdict)} tone={rail.verdict === 'ALIVE' ? '#2DD4A0' : tone.accent} />
          )}
          {rail && (
            <FactPill label="uart rx" value={String(rail.uart_rx_bytes_post_enable ?? 0)} />
          )}
          {blockers.map(blocker => (
            <FactPill key={blocker} label="blocker" value={formatBlocker(blocker)} tone={tone.accent} />
          ))}
          {nextAction?.kind && (
            <FactPill label="next" value={formatAction(nextAction.kind)} tone={tone.accent} />
          )}
          {nextAction && 'doc_url' in nextAction && nextAction.doc_url && (
            <a
              href={nextAction.doc_url}
              style={{ color: tone.accent, fontWeight: 700, textDecoration: 'none' }}
            >
              Open playbook
            </a>
          )}
        </div>
      )}
    </section>
  );
}

function InlineFacts({ health }: { health: SystemHealthResponse }) {
  const mode = String(health.mode ?? 'unknown');
  const age = formatAge(getLastSeenAgeMs(health));
  const bosminer = health.bosminer;

  return (
    <div style={{
      display: 'flex',
      gap: 6,
      alignItems: 'center',
      flexWrap: 'wrap',
      fontSize: '0.7rem',
      fontFamily: "'JetBrains Mono', monospace",
    }}>
      <FactPill label="mode" value={mode} />
      {bosminer && <FactPill label="bosminer" value={bosminer.alive ? 'alive' : 'down'} tone={bosminer.alive ? '#2DD4A0' : '#EF4444'} />}
      {age && <FactPill label="last" value={age} />}
    </div>
  );
}

function StatusDot({ color, pulse = false }: { color: string; pulse?: boolean }) {
  return (
    <span
      aria-hidden
      style={{
        display: 'inline-block',
        width: 9,
        height: 9,
        borderRadius: '50%',
        background: color,
        boxShadow: `0 0 8px ${color}`,
        marginTop: 5,
        flexShrink: 0,
        animation: pulse ? 'dcentos-pulse 1.4s ease-in-out infinite' : undefined,
      }}
    />
  );
}

function FactPill({ label, value, tone }: { label: string; value: string; tone?: string }) {
  return (
    <span style={{
      display: 'inline-flex',
      gap: 5,
      alignItems: 'center',
      border: '1px solid rgba(255,255,255,0.10)',
      borderRadius: 6,
      padding: '3px 6px',
      background: 'rgba(0,0,0,0.18)',
      color: tone ?? 'var(--text-secondary, #B5B5BD)',
      whiteSpace: 'nowrap',
    }}>
      <span style={{ color: 'var(--text-dim, #7C7C86)' }}>{label}</span>
      <strong style={{ color: tone ?? 'var(--text, #E8E8E8)' }}>{value}</strong>
    </span>
  );
}

function getStateDetail(health: SystemHealthResponse, state: HonestModeState): string {
  const age = formatAge(getLastSeenAgeMs(health));

  switch (state) {
    case 'native':
      return 'DCENT_OS owns the chain driver. Hashrate shown elsewhere is native daemon telemetry.';
    case 'proxy_alive':
      return 'dcentrald is routing Stratum only. Hardware ownership stays with bosminer; any miner stats are bosminer/cgminer telemetry, not native chain takeover.';
    case 'proxy_degraded':
      return `Bosminer is not answering${age ? `; last seen ${age}` : ''}. The dashboard should treat any old values as stale and make no live hashrate claim.`;
    case 'hardware_blocked':
      return 'Native chain takeover is blocked by the current rail/UART or bosminer failure state. No hashrate is claimed until recovery clears the blocker.';
    case 'handoff_mining':
      return 'Handoff path: bosminer pre-engaged the PSU/dsPIC/Loki spoof on cold-boot, then DCENT_OS took over the chain driver and is currently dispatching work. This mining session survives daemon uptime but does NOT survive a reboot — re-running the recipe requires an AC power cycle. See the documentation.';
    default:
      return 'System health endpoint has not reported a mining source yet.';
  }
}

function formatAge(ageMs: number | null): string | null {
  if (ageMs == null) {
    return null;
  }
  const sec = Math.floor(ageMs / 1000);
  if (sec < 5) return 'just now';
  if (sec < 60) return `${sec}s ago`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ago`;
  return `${Math.floor(min / 60)}h ${min % 60}m ago`;
}

function formatBlocker(blocker: BosminerBlocker): string {
  switch (blocker) {
    case 'missing_license':
      return 'missing license';
    case 'dead_pools':
      return 'dead pools';
    case 'fw_86_rejection':
      return 'fw 0x86 rejection';
    case 'license_cycle':
      return 'license cycle';
    default:
      return String(blocker).replace(/_/g, ' ');
  }
}

function formatRailVerdict(verdict: ChainRailVerdict): string {
  return String(verdict || 'unknown').toLowerCase();
}

function formatAction(kind: string): string {
  return kind.replace(/_/g, ' ');
}
