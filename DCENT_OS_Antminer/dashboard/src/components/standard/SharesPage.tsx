import React, { useState, useMemo, useEffect, useCallback } from 'react';
import { api } from '../../api/client';
import type { RecentShareEvent } from '../../api/types';
import { useMinerStore } from '../../store/miner';
import { formatPercent } from '../../utils/format';
import { MiningWorkPostureCard } from './MiningWorkPostureCard';
import { MiningPipelineManifestCard } from './MiningPipelineManifestCard';
import { useValueFlash } from '../../hooks/useValueFlash';
import { EmptyState } from '../common/EmptyState';
import { StatePanel } from '../common/StatePanel';
import { NoSharesIllustration } from '../common/illustrations';
import { PageSkeleton, SectionSkeleton } from '../common/skeletons';
import { Tooltip, InfoDot } from '../common/Tooltip';
import { glossaryText } from '../../utils/glossary';

function formatEfficiencyMetric(value?: number | null, fractionDigits = 1): string {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) {
    return '---';
  }

  return value.toFixed(fractionDigits);
}

function formatShareEventTime(ms: number) {
  if (!Number.isFinite(ms) || ms <= 0) return 'unknown time';
  const date = new Date(ms);
  return Number.isNaN(date.getTime()) ? 'unknown time' : date.toLocaleTimeString();
}

function formatShareDifficulty(value?: number | null) {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) {
    return 'not reported';
  }
  return value.toLocaleString(undefined, { maximumFractionDigits: 4 });
}

function formatTelemetryCount(value?: number | null) {
  return typeof value === 'number' && Number.isFinite(value)
    ? value.toLocaleString()
    : 'Unavailable';
}

function shareResultColor(result: string) {
  const normalized = result.toLowerCase();
  if (normalized === 'accepted' || normalized === 'lucky') return 'var(--green)';
  if (normalized === 'rejected') return 'var(--red)';
  return 'var(--yellow)';
}

function shareResultLabel(result: string) {
  if (!result) return 'unknown';
  return result.charAt(0).toUpperCase() + result.slice(1);
}

function shareEventDetails(event: RecentShareEvent) {
  const details: string[] = [];
  if (event.error_code != null) details.push(`error ${event.error_code}`);
  if (event.error_msg) details.push(event.error_msg);
  //  truth contract: pool target ≠ locally achieved difficulty. Pool target
  // now has its own column; only surface it in the details when it's unusually
  // far above the row's achieved value (helpful debug context).
  if (event.worker_name) details.push(`worker ${event.worker_name}`);
  if (event.nonce) details.push(`nonce ${event.nonce}`);
  if (event.ntime) details.push(`ntime ${event.ntime}`);
  if (event.version_bits) details.push(`version bits ${event.version_bits}`);
  if (event.protocol_meta_present) details.push('protocol metadata present');
  return details.length > 0 ? details.join(' | ') : '—';
}

/**
 *  truth contract: locally achieved difficulty ≥ 4× pool target ⇒ "lucky".
 * The 4× threshold is load-bearing — it MUST NOT be changed without a coordinated
 * memory-rule update +  co-author sign-off.
 */
const LUCKY_THRESHOLD_MULTIPLIER = 4;
function isLuckyShare(achieved?: number | null, poolTarget?: number | null): boolean {
  if (typeof achieved !== 'number' || !Number.isFinite(achieved) || achieved <= 0) return false;
  if (typeof poolTarget !== 'number' || !Number.isFinite(poolTarget) || poolTarget <= 0) return false;
  return achieved >= poolTarget * LUCKY_THRESHOLD_MULTIPLIER;
}

/**
 *  connection-state truth contract. The daemon now emits real pool states
 * (connecting / authorized / mining / rejecting / disconnected / auth_failed)
 * on PoolState.status. Map each to a FRIENDLY label, a `.ds-chip` tone class,
 * and the canonical glossary key so the Pool Connection card never renders a
 * raw token like "auth_failed". `rejecting`/`auth_failed` carry warning/error
 * tints; everything else maps to the existing connected/connecting/offline
 * vocabulary. Unknown/legacy strings fall through to a Title-cased label with a
 * neutral tone (no inferred health claim).
 */
function poolStatusDisplay(rawStatus: string): {
  label: string;
  tone: 'ds-success' | 'ds-warning' | 'ds-danger' | 'ds-neutral';
  glossaryKey: string;
  live: boolean;
} {
  const s = (rawStatus ?? '').toLowerCase();
  switch (s) {
    case 'mining':
      return { label: 'Mining', tone: 'ds-success', glossaryKey: 'state_mining', live: true };
    case 'authorized':
      return { label: 'Connected', tone: 'ds-success', glossaryKey: 'pool_authorized', live: true };
    case 'connected':
    case 'active':
    case 'alive':
    case 'donating':
    case 'mining_capable':
      return { label: 'Connected', tone: 'ds-success', glossaryKey: 'pool_connected', live: true };
    case 'connecting':
    case 'configured':
      return { label: 'Connecting', tone: 'ds-warning', glossaryKey: 'pool_connecting', live: false };
    case 'rejecting':
      return { label: 'Rejecting shares', tone: 'ds-warning', glossaryKey: 'pool_rejecting', live: false };
    case 'auth_failed':
      return { label: 'Auth failed', tone: 'ds-danger', glossaryKey: 'pool_auth_failed', live: false };
    case 'disconnected':
    case 'dead':
      return { label: 'Disconnected', tone: 'ds-danger', glossaryKey: 'pool_disconnected', live: false };
    default:
      return {
        label: rawStatus ? rawStatus.charAt(0).toUpperCase() + rawStatus.slice(1) : 'Unknown',
        tone: 'ds-neutral',
        glossaryKey: 'pool_state',
        live: false,
      };
  }
}

/**
 * FWT-1: a PoolState `*_source` of "honest_default" means the value is a
 * fresh-boot placeholder (e.g. latency 0, acceptance 100%) the firmware
 * publishes honestly — NOT a measurement. Used to gate the subtle "estimate"
 * affordance next to the value. Any other source ("stratum_status" /
 * "local_accounting") is a real reading.
 */
function isHonestDefault(source?: string): boolean {
  return source === 'honest_default';
}

interface ShareEventRowProps {
  event: RecentShareEvent;
}

const ShareEventRow = React.memo(function ShareEventRow({ event }: ShareEventRowProps) {
  const lucky = isLuckyShare(event.difficulty, event.target_difficulty);
  const achievedReported = typeof event.difficulty === 'number'
    && Number.isFinite(event.difficulty) && event.difficulty > 0;
  const targetReported = typeof event.target_difficulty === 'number'
    && Number.isFinite(event.target_difficulty) && event.target_difficulty > 0;

  return (
    <tr>
      <td className="shares-td-mono shares-td-time">
        {formatShareEventTime(event.timestamp_ms)}
      </td>
      <td className="shares-td-result" style={{ color: shareResultColor(event.result) }}>
        {/* Kit `.share-pill share-<status>` chip
            (EarningsShares.jsx:358-360, styles.css:2566-2575).
            Status keyed off the real result string; rejected
            and stale/duplicate map to the kit's rejected/stale
            pill variants. Lucky pill + aria preserved. */}
        <span
          className={`share-pill share-${
            event.result.toLowerCase() === 'accepted'
            || event.result.toLowerCase() === 'lucky'
              ? 'accepted'
              : event.result.toLowerCase() === 'rejected'
                ? 'rejected'
                : 'stale'
          }`}
        >
          <span className="dot" />
          {shareResultLabel(event.result)}
        </span>
        {lucky && (
          <Tooltip
            content={
              <>
                Achieved <b>{formatShareDifficulty(event.difficulty)}</b> ≥{' '}
                {LUCKY_THRESHOLD_MULTIPLIER}× the pool target of{' '}
                <b>{formatShareDifficulty(event.target_difficulty)}</b>. A statistically
                lucky hit — it does not change your expected earnings. The 4× threshold
                is a fixed, load-bearing constant.
              </>
            }
          >
            <span
              className="shares-lucky-pill"
              aria-label={`Lucky share: achieved difficulty ${formatShareDifficulty(event.difficulty)} is at least ${LUCKY_THRESHOLD_MULTIPLIER}× the pool target of ${formatShareDifficulty(event.target_difficulty)}`}
            >
              Lucky
            </span>
          </Tooltip>
        )}
      </td>
      <td className="shares-td-mono shares-td-job">
        {event.job_id || <span className="shares-cell-muted">not reported</span>}
      </td>
      <td className="shares-td-mono shares-td-target">
        {targetReported
          ? formatShareDifficulty(event.target_difficulty)
          : <span className="shares-cell-muted">not reported</span>}
      </td>
      <td className="shares-td-mono shares-td-achieved">
        {achievedReported
          ? formatShareDifficulty(event.difficulty)
          : <span className="shares-cell-muted">not reported</span>}
      </td>
      <td className="shares-td-details">
        {shareEventDetails(event)}
      </td>
    </tr>
  );
});

export function SharesPage() {
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  // While the primary status payload is null (very first connect, page-level
  // refresh), render the canonical page skeleton instead of an empty hero.
  if (status == null && stats == null) {
    return <PageSkeleton data-testid="page-skeleton-shares" />;
  }
  return <SharesPageInner />;
}

function SharesPageInner() {
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const [shareEvents, setShareEvents] = useState<RecentShareEvent[]>([]);
  const [shareHistoryLoading, setShareHistoryLoading] = useState(false);
  const [shareHistoryError, setShareHistoryError] = useState<string | null>(null);

  const accepted = status?.accepted ?? 0;
  const rejected = status?.rejected ?? 0;
  const total = accepted + rejected;
  const rejectRate = total > 0 ? (rejected / total) * 100 : 0;
  const uptimeS = status?.uptime_s ?? 0;

  // Live "fresh value" flashes when accepted/rejected counters tick over.
  // Operator gets a subtle visual confirmation that a new share landed.
  const acceptedFlashCls = useValueFlash(accepted);
  const rejectedFlashCls = useValueFlash(rejected);

  const pool = status?.pool;
  const shareEfficiency = status?.share_efficiency ?? pool?.share_efficiency ?? stats?.share_efficiency ?? null;
  const poolUrl = pool?.url ?? '';
  const poolStatus = pool?.status ?? 'unknown';
  const poolTargetDifficulty = pool?.difficulty ?? 0;
  const lastShareS = pool?.last_share_s ?? 0;
  const sharesUnresolved = pool?.failover?.shares_unresolved ?? pool?.failover?.unresolved_submit_count ?? null;
  const pendingSubmitDropped = pool?.failover?.pending_submit_dropped ?? null;

  //  truth contract: don't collapse connecting/connected/authorized
  // into one boolean. The pool.status string is canonical. The friendly label,
  // tone, and glossary key for the Pool Connection status chip come from the
  // canonical mapper so new states (rejecting / auth_failed / mining) render
  // honestly instead of as a raw token.
  const poolStatusInfo = poolStatusDisplay(poolStatus);

  // FWT-1 telemetry-honesty provenance. The daemon publishes `*_source` markers
  // so the UI can mark honest-default placeholders as estimates (never hide the
  // value). 0 ms ping / 100% acceptance on a fresh boot are NOT measurements.
  const latencyMs = pool?.latency_ms;
  const latencyIsEstimate = isHonestDefault(pool?.latency_ms_source);
  const rollingAcceptance = pool?.rolling_acceptance_pct_30min;
  const rollingAcceptanceIsEstimate = isHonestDefault(pool?.rolling_acceptance_source);
  const isDonating = pool?.donating ?? false;
  const donatingIsEstimate = isHonestDefault(pool?.donating_source);

  // Compute shares per minute
  const sharesPerMin = useMemo(() => {
    if (uptimeS < 60) return 0;
    return (accepted / (uptimeS / 60));
  }, [accepted, uptimeS]);

  // Per-chain HW errors from stats
  const chainStats = stats?.chains ?? [];
  const perChainSharesTracked = stats?.share_accounting?.per_chain_tracked;
  const perChainSharesReason = stats?.share_accounting?.reason
    ?? 'Per-chain share attribution is unavailable on this firmware.';
  const totalHwErrors = chainStats.reduce((s, c) => s + (c.hw_errors ?? 0), 0);
  const recentShareEvents = useMemo(
    () => [...shareEvents].sort((a, b) => b.timestamp_ms - a.timestamp_ms).slice(0, 25),
    [shareEvents],
  );

  //  lucky-share aggregate for the recent window (achieved ≥ 4× pool target).
  const luckyShareCount = useMemo(
    () => recentShareEvents.filter(e => isLuckyShare(e.difficulty, e.target_difficulty)).length,
    [recentShareEvents],
  );

  const refreshShareHistory = useCallback(async () => {
    setShareHistoryLoading(true);
    try {
      const response = await api.getShareHistory();
      setShareEvents(response.events);
      setShareHistoryError(null);
    } catch (error) {
      setShareEvents([]);
      setShareHistoryError(error instanceof Error ? error.message : 'Share history unavailable');
    } finally {
      setShareHistoryLoading(false);
    }
  }, []);

  useEffect(() => {
    void refreshShareHistory();
  }, [refreshShareHistory]);

  // Share acceptance ring chart
  const acceptPct = total > 0 ? (accepted / total) * 100 : 100;
  const ringRadius = 45;
  const ringCircum = 2 * Math.PI * ringRadius;
  const acceptDash = (acceptPct / 100) * ringCircum;

  // Last accepted-difficulty for hero KPI
  const lastDifficulty = useMemo(() => {
    for (const evt of recentShareEvents) {
      if (typeof evt.difficulty === 'number' && Number.isFinite(evt.difficulty) && evt.difficulty > 0) {
        return evt.difficulty;
      }
    }
    return poolTargetDifficulty;
  }, [recentShareEvents, poolTargetDifficulty]);

  return (
    <div className="page-content">
      <div className="page-hero-strip">
        <div className="page-hero-summary">
          <div className="page-hero-eyebrow">SHARES</div>
          <div className="page-hero-title">Share History</div>
          <div className="page-hero-stat" data-tooltip={glossaryText('earning_proof')}>
            {total > 0 ? `${acceptPct.toFixed(1)}%` : '—'}
          </div>
          <div className="page-hero-substat">
            {total > 0
              ? `${accepted.toLocaleString()} accepted / ${rejected.toLocaleString()} rejected`
              : 'Waiting for first share submission.'}
            {accepted > 0 && lastShareS > 0 && lastShareS < 600 && (
              <span className="shares-earning-ok" data-tooltip={glossaryText('earning_proof')}>
                {' · '}You ARE earning — last accepted share {lastShareS.toFixed(0)}s ago
              </span>
            )}
          </div>
        </div>
        <div className="hero-kpi-strip">
          <div className="hero-kpi">
            <div className="kpi-label kpi-label-live-wrap">
              {lastShareS > 0 && lastShareS < 60 && (
                <span className="ds-dot-live" aria-hidden="true" />
              )}
              Accepted <InfoDot term="share_accepted" />
            </div>
            <div className="kpi-value">
              <span className={`kpi-num-anim ${acceptedFlashCls}`}>{accepted.toLocaleString()}</span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Rejected <InfoDot term="share_rejected" /></div>
            <div className="kpi-value">
              <span className={`kpi-num-anim ${rejectedFlashCls}`}>{rejected.toLocaleString()}</span>
            </div>
            {total > 0 && (
              <div className="kpi-sub">{formatPercent(rejectRate)} reject rate</div>
            )}
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Last Achieved Difficulty <InfoDot term="achieved_difficulty" /></div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {lastDifficulty > 0 ? lastDifficulty.toLocaleString(undefined, { maximumFractionDigits: 0 }) : '—'}
              </span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">HW Errors</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{totalHwErrors.toLocaleString()}</span>
            </div>
          </div>
        </div>
      </div>

      <section className="section">
      <MiningWorkPostureCard />
      <MiningPipelineManifestCard />

      {/* Kit Shares KPI strip — kit `.earn-kpi-strip` / `.earn-kpi-tile`
          (EarningsShares.jsx:331-337, styles.css:2470-2495). Dual-classed
          with the production `metric-card` hooks; every value, tooltip and
          live-flash class is preserved verbatim.
          ZONE-D Wave-9: Accepted / Rejected / HW Errors tiles removed here —
          they duplicate the page hero strip above. The remaining tiles
          (Reject Rate, Shares/min, shares_unresolved, pending_submit_dropped)
          are unique to this strip. `acceptedFlashCls`/`rejectedFlashCls` are
          still consumed by the hero strip, so the value-flash wiring is intact. */}
      <div className="earn-kpi-strip" style={{ marginBottom: 20 }}>
        <div className="earn-kpi-tile metric-card centered">
          <div
            className="earn-kpi-label"
            style={{ fontSize: '0.7rem', color: 'var(--text-dim)', textTransform: 'uppercase', marginBottom: 4 }}
            data-tooltip={glossaryText('accept_rate_thresholds')}
          >
            Reject Rate
          </div>
          <div className="earn-kpi-value" style={{
            fontFamily: "var(--font-heading)",
            fontWeight: 700, fontSize: '1.6rem',
            color: rejectRate > 2 ? 'var(--red)' : rejectRate > 0.5 ? 'var(--yellow)' : 'var(--green)',
          }}>
            {formatPercent(rejectRate)}
          </div>
        </div>
        <div className="earn-kpi-tile metric-card centered">
          <div className="earn-kpi-label" style={{ fontSize: '0.7rem', color: 'var(--text-dim)', textTransform: 'uppercase', marginBottom: 4 }}>
            Shares/min
          </div>
          <div className="earn-kpi-value" style={{
            fontFamily: "var(--font-heading)",
            fontWeight: 700, fontSize: '1.6rem',
          }}>
            {sharesPerMin.toFixed(1)}
          </div>
        </div>
        <div className="earn-kpi-tile metric-card centered">
          <div
            className="earn-kpi-label"
            style={{ fontSize: '0.62rem', color: 'var(--text-dim)', textTransform: 'uppercase', marginBottom: 4, overflowWrap: 'anywhere' }}
            data-tooltip="Shares submitted to the pool that have not yet been confirmed accepted or rejected. A small transient count is normal during a pool switch or brief network blip."
          >
            shares_unresolved
          </div>
          <div className="earn-kpi-value" style={{
            fontFamily: "var(--font-heading)",
            fontWeight: 700, fontSize: '1.6rem',
            color: (sharesUnresolved ?? 0) > 0 ? 'var(--yellow)' : 'var(--text)',
          }}>
            {formatTelemetryCount(sharesUnresolved)}
          </div>
        </div>
        <div className="earn-kpi-tile metric-card centered">
          <div
            className="earn-kpi-label"
            style={{ fontSize: '0.62rem', color: 'var(--text-dim)', textTransform: 'uppercase', marginBottom: 4, overflowWrap: 'anywhere' }}
            data-tooltip="Shares the miner had queued but dropped before submission (usually because the job moved on or the pool connection reset). Differs from a rejected share — these never reached the pool."
          >
            pending_submit_dropped
          </div>
          <div className="earn-kpi-value" style={{
            fontFamily: "var(--font-heading)",
            fontWeight: 700, fontSize: '1.6rem',
            color: (pendingSubmitDropped ?? 0) > 0 ? 'var(--yellow)' : 'var(--text)',
          }}>
            {formatTelemetryCount(pendingSubmitDropped)}
          </div>
        </div>
      </div>

      {shareEfficiency && (
        <div className="page-surface" style={{ marginBottom: 20 }}>
          <div style={{
            fontFamily: "var(--font-heading)",
            fontWeight: 700,
            fontSize: '1rem',
            color: 'var(--accent)',
            marginBottom: 12,
          }}>
            Accepted Work Efficiency
          </div>
          <div className="surface-grid-auto">
            <div>
              <div style={{ fontSize: '0.7rem', color: 'var(--text-dim)', textTransform: 'uppercase', marginBottom: 4 }}>
                Target diff / kWh
              </div>
              <div style={{ fontFamily: "'JetBrains Mono', monospace", fontWeight: 700, fontSize: '1.2rem' }}>
                {formatEfficiencyMetric(shareEfficiency.accepted_difficulty_per_kwh, 0)}
              </div>
            </div>
            <div>
              <div style={{ fontSize: '0.7rem', color: 'var(--text-dim)', textTransform: 'uppercase', marginBottom: 4 }}>
                Shares / kWh
              </div>
              <div style={{ fontFamily: "'JetBrains Mono', monospace", fontWeight: 700, fontSize: '1.2rem' }}>
                {formatEfficiencyMetric(shareEfficiency.accepted_shares_per_kwh, 2)}
              </div>
            </div>
            <div>
              <div style={{ fontSize: '0.7rem', color: 'var(--text-dim)', textTransform: 'uppercase', marginBottom: 4 }}>
                Window
              </div>
              <div style={{ fontFamily: "'JetBrains Mono', monospace", fontWeight: 700, fontSize: '1.2rem' }}>
                {Math.max(1, Math.round(shareEfficiency.window_s / 60))}m
              </div>
              <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginTop: 4 }}>
                {shareEfficiency.estimated_wall_energy_kwh.toFixed(3)} kWh wall energy
              </div>
            </div>
            <div>
              <div style={{ fontSize: '0.7rem', color: 'var(--text-dim)', textTransform: 'uppercase', marginBottom: 4 }}>
                Power Source
              </div>
              <div style={{ fontFamily: "'JetBrains Mono', monospace", fontWeight: 700, fontSize: '1.05rem' }}>
                {shareEfficiency.power_source}
              </div>
              <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginTop: 4 }}>
                {shareEfficiency.calibrated ? 'Calibrated wall estimate' : 'Uncalibrated estimate'}
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Share acceptance ring + Pool info */}
      <div className="shares-ring-pool-grid">
        {/* Acceptance ring */}
         <div className="page-surface" style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center' }}>
          <div style={{ position: 'relative', width: 120, height: 120 }}>
            <svg width="120" height="120" viewBox="0 0 120 120" aria-hidden="true">
              {/* Background ring */}
              <circle
                cx="60" cy="60" r={ringRadius}
                fill="none" stroke="var(--border)" strokeWidth="8"
              />
              {/* Accept ring */}
              <circle
                cx="60" cy="60" r={ringRadius}
                fill="none"
                stroke="var(--green)"
                strokeWidth="8"
                strokeDasharray={`${acceptDash} ${ringCircum}`}
                strokeLinecap="round"
                transform="rotate(-90 60 60)"
                style={{ transition: 'stroke-dasharray 0.5s' }}
              />
              {/* Reject ring */}
              {rejectRate > 0 && (
                <circle
                  cx="60" cy="60" r={ringRadius}
                  fill="none"
                  stroke="var(--red)"
                  strokeWidth="8"
                  strokeDasharray={`${((100 - acceptPct) / 100) * ringCircum} ${ringCircum}`}
                  strokeLinecap="round"
                  transform={`rotate(${-90 + (acceptPct / 100) * 360} 60 60)`}
                  style={{ transition: 'all 0.5s' }}
                />
              )}
            </svg>
            <div style={{
              position: 'absolute', inset: 0,
              display: 'flex', flexDirection: 'column',
              alignItems: 'center', justifyContent: 'center',
            }}>
              <div style={{
                fontFamily: "var(--font-heading)",
                fontWeight: 700, fontSize: '1.4rem',
                color: 'var(--green)',
              }}>
                {acceptPct.toFixed(1)}%
              </div>
              <div style={{ fontSize: '0.6rem', color: 'var(--text-dim)' }}>accepted</div>
            </div>
          </div>
          <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginTop: 8 }}>
            {total.toLocaleString()} total shares
          </div>
        </div>

        {/* Pool info */}
         <div className="page-surface">
          <div style={{
            fontFamily: "var(--font-heading)",
            fontWeight: 700, fontSize: '1rem',
            color: 'var(--accent)', marginBottom: 12,
          }}>
            Pool Connection
          </div>
          <div style={{ display: 'grid', gap: 8, fontSize: '0.85rem' }}>
             <div className="metric-row">
              <span style={{ color: 'var(--text-dim)' }}>URL</span>
              <span style={{
                fontFamily: "'JetBrains Mono', monospace",
                fontSize: '0.75rem',
                color: 'var(--text-secondary)',
                maxWidth: '60%',
                overflow: 'hidden',
                textOverflow: 'ellipsis',
                whiteSpace: 'nowrap',
              }}>
                {poolUrl || 'Not connected'}
              </span>
            </div>
             <div className="metric-row">
              <span style={{ color: 'var(--text-dim)' }}>
                Status <InfoDot term="pool_state" />
              </span>
              {/* Wave-1 connection states: render a friendly label + honest
                  tint (rejecting → warning, auth_failed → error) so the card
                  never shows a raw "auth_failed" token. */}
              <Tooltip term={poolStatusInfo.glossaryKey}>
                <span
                  className={`ds-chip ${poolStatusInfo.tone} ${poolStatusInfo.live ? 'ds-live' : ''}`}
                >
                  <span className="ds-dot" />
                  {poolStatusInfo.label}
                </span>
              </Tooltip>
            </div>
             <div className="metric-row">
              <span style={{ color: 'var(--text-dim)' }}>
                Pool Target Difficulty <InfoDot term="pool_target_difficulty" />
              </span>
              <span style={{ fontFamily: "'JetBrains Mono', monospace" }}>
                {poolTargetDifficulty > 0 ? poolTargetDifficulty.toLocaleString() : 'not reported'}
              </span>
            </div>
            {/* FWT-1: pool ping. Shown even when it's the honest-default 0 ms,
                but marked as an estimate so it is never read as a measurement. */}
            {typeof latencyMs === 'number' && (
              <div className="metric-row">
                <span style={{ color: 'var(--text-dim)' }}>
                  Ping <InfoDot term="pool_latency_ms" />
                </span>
                <span style={{
                  fontFamily: "'JetBrains Mono', monospace",
                  display: 'inline-flex', alignItems: 'center', gap: 6,
                }}>
                  <span style={latencyIsEstimate ? { color: 'var(--text-dim)' } : undefined}>
                    {`${latencyMs.toFixed(0)} ms`}
                  </span>
                  {latencyIsEstimate && (
                    <Tooltip term="honest_default_estimate">
                      <span className="ds-chip ds-neutral" style={{ fontSize: '0.65rem' }}>
                        estimate
                      </span>
                    </Tooltip>
                  )}
                </span>
              </div>
            )}
            {/* FWT-1: rolling 30-min acceptance. 100% with an honest-default
                source means no share has been ACKed yet — mark as estimate. */}
            {typeof rollingAcceptance === 'number' && (
              <div className="metric-row">
                <span style={{ color: 'var(--text-dim)' }}>
                  Acceptance (30m) <InfoDot term="pool_rolling_acceptance" />
                </span>
                <span style={{
                  fontFamily: "'JetBrains Mono', monospace",
                  display: 'inline-flex', alignItems: 'center', gap: 6,
                }}>
                  <span style={rollingAcceptanceIsEstimate
                    ? { color: 'var(--text-dim)' }
                    : { color: 'var(--green)' }}>
                    {`${rollingAcceptance.toFixed(1)}%`}
                  </span>
                  {rollingAcceptanceIsEstimate && (
                    <Tooltip term="honest_default_estimate">
                      <span className="ds-chip ds-neutral" style={{ fontSize: '0.65rem' }}>
                        estimate
                      </span>
                    </Tooltip>
                  )}
                </span>
              </div>
            )}
            {/* FWT-1: donation routing state. Only show the estimate marker when
                the daemon reports it as an honest default (not yet observed). */}
            {pool?.donating_source != null && (
              <div className="metric-row">
                <span style={{ color: 'var(--text-dim)' }}>
                  Donating <InfoDot term="donating_indicator" />
                </span>
                <span style={{
                  fontFamily: "'JetBrains Mono', monospace",
                  display: 'inline-flex', alignItems: 'center', gap: 6,
                }}>
                  <span style={donatingIsEstimate ? { color: 'var(--text-dim)' } : undefined}>
                    {isDonating ? 'Yes' : 'No'}
                  </span>
                  {donatingIsEstimate && (
                    <Tooltip term="honest_default_estimate">
                      <span className="ds-chip ds-neutral" style={{ fontSize: '0.65rem' }}>
                        estimate
                      </span>
                    </Tooltip>
                  )}
                </span>
              </div>
            )}
             <div className="metric-row">
              <span style={{ color: 'var(--text-dim)' }}>
                Lucky shares (recent) <InfoDot term="lucky_share" />
              </span>
              <span style={{
                fontFamily: "'JetBrains Mono', monospace",
                color: luckyShareCount > 0 ? 'var(--green)' : 'var(--text-secondary)',
              }}>
                {luckyShareCount > 0
                  ? `${luckyShareCount} of last ${recentShareEvents.length}`
                  : '—'}
              </span>
            </div>
             <div className="metric-row">
              <span style={{ color: 'var(--text-dim)' }}>Last Share</span>
              <span style={{ fontFamily: "'JetBrains Mono', monospace" }}>
                {lastShareS > 0 ? `${lastShareS.toFixed(0)}s ago` : 'not reported'}
              </span>
            </div>
          </div>
        </div>
      </div>

      {/* Recent share events */}
      <div className="section">
        <div className="section-title">Recent Share Events</div>
        <div className="page-surface">
          <div className="page-toolbar" style={{ marginBottom: 12 }}>
            <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)' }}>
              Source: /api/history/shares
            </div>
            <button className="btn btn-secondary" type="button" onClick={refreshShareHistory} disabled={shareHistoryLoading}>
              {shareHistoryLoading ? 'Refreshing...' : 'Refresh'}
            </button>
          </div>

          {shareHistoryLoading && recentShareEvents.length === 0 ? (
            <SectionSkeleton rows={3} data-testid="shares-page-history-loading" />
          ) : shareHistoryError ? (
            <StatePanel
              title="Share history unavailable"
              message={shareHistoryError}
              tone="warning"
              compact
            />
          ) : recentShareEvents.length === 0 ? (
            <EmptyState
              illustration={<NoSharesIllustration />}
              title="No shares yet"
                hint="No recent share events reported by /api/history/shares. DCENT_OS will not infer per-share rows from counters or hashrate."
              data-testid="shares-page-history-empty"
            />
          ) : (
            <div className="shares-table-wrap">
              <table className="shares-table" aria-label="Recent share events — pool target difficulty vs achieved difficulty per share">
                <caption className="sr-only">Recent share events. Pool Target column shows pool-credited minimum difficulty. Achieved column shows locally proven difficulty (only lucky-share evidence).</caption>
                <thead>
                  <tr>
                    <th scope="col" className="shares-th-left">Time</th>
                    <th scope="col" className="shares-th-left">Result</th>
                    <th scope="col" className="shares-th-left">Job</th>
                    <th scope="col" className="shares-th-right">
                      <span data-tooltip={glossaryText('pool_target_difficulty')}>Pool Target</span>
                    </th>
                    <th scope="col" className="shares-th-right">
                      <span data-tooltip={glossaryText('achieved_difficulty')}>Achieved</span>
                    </th>
                    <th scope="col" className="shares-th-left">Details</th>
                  </tr>
                </thead>
                <tbody>
                  {recentShareEvents.map((event, index) => (
                    <ShareEventRow
                      key={`${event.timestamp_ms}-${event.job_id}-${index}`}
                      event={event}
                    />
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </div>

      {/* Per-chain share breakdown */}
      {chainStats.length > 0 && (() => {
        const hasPerChainData = chainStats.some(c => (c.accepted ?? 0) > 0 || (c.rejected ?? 0) > 0);
        const showUnavailableNotice = (perChainSharesTracked === false || !hasPerChainData) && total > 0;
        return (
        <div className="section perf-below-fold">
          <div className="section-title">Per-Chain Breakdown</div>
          {showUnavailableNotice && (
            <div style={{
              background: 'var(--card-bg)', borderRadius: 'var(--radius)',
              padding: '12px 16px', border: '1px solid var(--border)',
              color: 'var(--text-dim)', fontSize: '0.85rem', marginBottom: 12,
              textAlign: 'center',
            }}>
              {perChainSharesReason} Totals: {accepted} accepted, {rejected} rejected.
            </div>
          )}
          <div
            className="shares-chain-grid"
            style={{ '--shares-chain-cols': Math.min(chainStats.length, 3) } as React.CSSProperties}
          >
            {chainStats.map(chain => {
              const chainTotal = (chain.accepted ?? 0) + (chain.rejected ?? 0);
              const chainRejectRate = chainTotal > 0 ? ((chain.rejected ?? 0) / chainTotal) * 100 : 0;
              return (
                 <div key={chain.id} className="page-surface">
                  <div style={{
                    fontFamily: "var(--font-heading)",
                    fontWeight: 700, color: 'var(--accent)',
                    marginBottom: 10,
                  }}>
                    Chain {chain.id}
                  </div>
                  {/* Kit per-chain stacked share bar (EarningsShares.jsx:376-387):
                      accepted/rejected segments flex-weighted by the real chain
                      counters. Hidden when this chain has reported no shares so
                      we never render a full-green bar without per-chain share data. */}
                  {chainTotal > 0 && (
                    <div style={{
                      display: 'flex', height: 8, borderRadius: 4,
                      overflow: 'hidden', marginBottom: 10,
                      background: 'rgba(255,255,255,.05)',
                    }}>
                      <div
                        style={{ flex: chain.accepted ?? 0, background: 'var(--green)' }}
                        data-tip={`${(chain.accepted ?? 0).toLocaleString()} accepted`}
                      />
                      <div
                        style={{ flex: chain.rejected ?? 0, background: 'var(--red)' }}
                        data-tip={`${(chain.rejected ?? 0).toLocaleString()} rejected`}
                      />
                    </div>
                  )}
                  <div style={{ display: 'grid', gap: 6, fontSize: '0.8rem' }}>
                     <div className="metric-row">
                      <span style={{ color: 'var(--text-dim)' }}>Accepted</span>
                      <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--green)' }}>
                        {(chain.accepted ?? 0).toLocaleString()}
                      </span>
                    </div>
                     <div className="metric-row">
                      <span style={{ color: 'var(--text-dim)' }}>Rejected</span>
                      <span style={{
                        fontFamily: "'JetBrains Mono', monospace",
                        color: (chain.rejected ?? 0) > 0 ? 'var(--red)' : 'var(--text)',
                      }}>
                        {(chain.rejected ?? 0).toLocaleString()}
                      </span>
                    </div>
                     <div className="metric-row">
                      <span style={{ color: 'var(--text-dim)' }}>Reject Rate</span>
                      <span style={{
                        fontFamily: "'JetBrains Mono', monospace",
                        color: chainRejectRate > 2 ? 'var(--red)' : 'var(--green)',
                      }}>
                        {formatPercent(chainRejectRate)}
                      </span>
                    </div>
                     <div className="metric-row">
                      <span style={{ color: 'var(--text-dim)' }}>HW Errors</span>
                      <span style={{
                        fontFamily: "'JetBrains Mono', monospace",
                        color: (chain.hw_errors ?? 0) > 0 ? 'var(--red)' : 'var(--green)',
                      }}>
                        {(chain.hw_errors ?? 0).toLocaleString()}
                      </span>
                    </div>
                     <div className="metric-row">
                      <span style={{ color: 'var(--text-dim)' }}>Chips</span>
                      <span style={{ fontFamily: "'JetBrains Mono', monospace" }}>
                        {chain.chips}
                      </span>
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
        );
      })()}

      </section>

      {/* HW Error details */}
      {totalHwErrors > 0 && (
        <div className="section">
          <div className="section-title">Hardware Errors</div>
          <div className="page-surface" style={{ borderColor: 'var(--red)' }}>
            <div style={{
              display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8,
            }}>
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="var(--red)" strokeWidth="1.5">
                <path d="M8 1l7 13H1L8 1z" />
                <path d="M8 6v3M8 11v1" />
              </svg>
              <span style={{ color: 'var(--red)', fontWeight: 600 }}>
                {totalHwErrors} hardware error{totalHwErrors !== 1 ? 's' : ''} detected
              </span>
            </div>
            <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)' }}>
              Hardware errors indicate ASIC computation failures. Occasional HW errors are normal.
              Consistently high rates may indicate chip damage, overheating, or voltage instability.
            </div>
            <div className="chart-legend">
              {chainStats.map(c => (
                <span key={c.id} style={{
                  color: (c.hw_errors ?? 0) > 0 ? 'var(--red)' : 'var(--text-dim)',
                }}>
                  Chain {c.id}: {c.hw_errors ?? 0}
                </span>
              ))}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
