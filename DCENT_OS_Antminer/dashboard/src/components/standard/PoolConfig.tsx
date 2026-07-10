import React, { useState, useEffect, useMemo } from 'react';
import { api, ApiError } from '../../api/client';
import { useMinerStore } from '../../store/miner';
import { ActionButton } from '../common/ActionButton';
import { StatePanel } from '../common/StatePanel';
import { TaskHandoffBanner } from '../common/TaskHandoffBanner';
import { StatusPill, type StatusState } from '../common/StatusPill';
import { EmptyState } from '../common/EmptyState';
import { NoPoolIllustration } from '../common/illustrations';
import type { HashrateSplitState, PoolFailoverStatus, PoolInfo } from '../../api/types';
import { POOL_TEMPLATES } from '../../utils/constants';
import { glossaryText } from '../../utils/glossary';
import { InfoDot } from '../common/Tooltip';
import { PoolLatencyBadge } from './PoolLatencyBadge';
// COMP-POOLCARD §6 PoolState→StatusState projection — now lifted into the shared
// utils/poolState.ts so PoolConfig (live predicates, below) and the thin
// common/PoolStatus card consume ONE projection. The pure rung→state switch is
// no longer inlined here; PoolConfig's live-predicate logic
// (`poolLivenessStatusState`) stays intact and routes through it.
import {
  poolStateToStatusState,
  type PoolStateRung,
} from '../../utils/poolState';

interface PoolEntry {
  url: string;
  worker: string;
  password: string;
  priority: number;
  protocol?: string;
  sv2_url?: string;
}

const MAX_POOLS = 3;

const emptyPool = (): PoolEntry => ({
  url: '',
  worker: '',
  password: '',
  priority: 0,
  protocol: 'auto',
  sv2_url: '',
});

const normalizePools = (entries: PoolEntry[]): PoolEntry[] => {
  const sorted = [...entries]
    .sort((a, b) => a.priority - b.priority)
    .slice(0, MAX_POOLS)
    .map((pool, index) => ({
      ...emptyPool(),
      ...pool,
      priority: index,
      protocol: pool.protocol ?? 'auto',
      sv2_url: pool.sv2_url ?? '',
    }));

  return sorted.length > 0 ? sorted : [emptyPool()];
};

function formatEfficiencyMetric(value?: number | null, fractionDigits = 1): string {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) {
    return '---';
  }

  return value.toFixed(fractionDigits);
}

function formatTelemetryCount(value?: number | null): string {
  return typeof value === 'number' && Number.isFinite(value)
    ? value.toLocaleString()
    : 'Unavailable';
}

const reprioritizePools = (entries: PoolEntry[], from: number, to: number): PoolEntry[] => {
  const next = [...entries];
  const [moved] = next.splice(from, 1);
  next.splice(Math.max(0, Math.min(to, next.length)), 0, moved);
  return next.map((pool, index) => ({ ...pool, priority: index }));
};

/** Validate a stratum pool URL. Returns null if valid, or an error message string. */
function validateStratumUrl(url: string): string | null {
  if (!url) return null; // empty is allowed (not filled in yet)
  const trimmed = url.trim();

  // Must match stratum+tcp:// or stratum+ssl:// or stratum2+tcp:// patterns
  const stratumPattern = /^stratum\+?(2\+)?(tcp|ssl):\/\/.+:\d{1,5}$/i;
  // Also allow plain hostname:port for power users
  const hostPortPattern = /^[\w.-]+:\d{1,5}$/;

  if (!stratumPattern.test(trimmed) && !hostPortPattern.test(trimmed)) {
    return 'Invalid URL. Expected format: stratum+tcp://hostname:port';
  }

  // Extract port and validate range
  const portMatch = trimmed.match(/:(\d+)$/);
  if (portMatch) {
    const port = parseInt(portMatch[1], 10);
    if (port < 1 || port > 65535) {
      return 'Port must be between 1 and 65535';
    }
  }

  return null;
}

/**
 * Derive the embedded liveness pill's `StatusState` from the live `/api/pools`
 * predicates (the same fields rest.rs exposes: stratum_active / status /
 * fallback / accepted). Live rungs map to a §6 `PoolStateRung` and route through
 * the shared `poolStateToStatusState` projection (utils/poolState.ts) so
 * `connecting ≠ connected ≠ mining_capable` is preserved; the genuinely-down
 * case stays an honest `error` (it is NOT in the §6 ladder, so it must NOT be
 * forced onto a connecting/online rung). This is the additive convergence: the
 * pill can no longer render a connecting pool as connected, nor claim `mining`
 * before accepted shares prove work is flowing.
 */
function poolLivenessStatusState(p: PoolInfo): StatusState {
  const status = (p.status ?? '').toLowerCase();
  // Classify the live pool onto a §6 `PoolStateRung` first (the typed ladder),
  // then project to the pill's `StatusState` via the shared projection. Naming
  // the rung makes the connecting/connected/mining_capable distinction explicit.
  let rung: PoolStateRung | null;
  if (p.auto_fallback_active || (p.failover_switch_count ?? 0) > 0) {
    rung = 'failover';
  } else if (p.stratum_active) {
    // subscribe+authorize complete. Only claim mining-capable (→ `mining` pill)
    // when accepted shares prove work is flowing; otherwise the session is up
    // but not yet hashing → `authorized` → `online`.
    rung = (p.accepted ?? 0) > 0 ? 'mining_capable' : 'authorized';
  } else if (status === 'connecting' || status === 'configured' || status === 'pending') {
    // Not subscribed/authorized yet. A handshake-in-progress status is the
    // `connecting` rung.
    rung = 'connecting';
  } else {
    // No usable session and not a recognised handshake state — NOT in the §6
    // ladder, so it must NOT be forced onto a connecting/online rung. Stays an
    // honest `error`.
    rung = null;
  }
  return rung == null ? 'error' : poolStateToStatusState(rung);
}

export function PoolConfig() {
  const addAlert = useMinerStore(s => s.addAlert);
  const addToast = useMinerStore(s => s.addToast);
  const [pools, setPools] = useState<PoolEntry[]>([emptyPool()]);
  const [livePools, setLivePools] = useState<PoolInfo[]>([]);
  const [failoverState, setFailoverState] = useState<PoolFailoverStatus | null>(null);
  const [hashrateSplit, setHashrateSplit] = useState<HashrateSplitState | null>(null);
  const [splitEnabled, setSplitEnabled] = useState(false);
  const [splitSecondaryPct, setSplitSecondaryPct] = useState(20);
  const [splitCycleMinutes, setSplitCycleMinutes] = useState(30);
  const [testing, setTesting] = useState<number | null>(null);
  const [testResults, setTestResults] = useState<Record<number, 'ok' | 'fail'>>({});
  const [loadingPools, setLoadingPools] = useState(true);
  const [poolLoadError, setPoolLoadError] = useState('');
  //  truth contract: don't claim "Pool switched" until the new pool is
  // connected, authorized, and the first job arrived. We surface a tri-state
  // pendingSave: 'idle' (no pending action), 'applying' (POST in flight),
  // 'pending' (POST completed but daemon hasn't yet reported connected).
  const [pendingSave, setPendingSave] = useState<'idle' | 'applying' | 'pending'>('idle');
  const [pendingTargetUrl, setPendingTargetUrl] = useState<string | null>(null);
  // Kit PoolsPage subtab bar (Pages.jsx:19-25): Pools / Shares / SV2 Status.
  // Reorganizes the existing fully-wired surfaces under the kit's
  // `.standard-subtab-bar` without dropping any control or telemetry.
  // : the card-level pool sub-tabs were dropped (duplicated the page
  // tabs). The editor + Active Pool Health now render inline, so no tab state.

  // Load current pool config
  useEffect(() => {
    api.getPools().then(res => {
      setPoolLoadError('');
      setLivePools(res.pools);
      setFailoverState(res.failover ?? null);
      const split = res.hashrate_split ?? res.failover?.hashrate_split ?? null;
      setHashrateSplit(split);
      if (split) {
        setSplitEnabled(Boolean(split.enabled));
        const secondaryPct = split.secondary_pct ?? ((split.secondary_bps ?? 2000) / 100);
        setSplitSecondaryPct(Math.max(1, Math.min(99, Math.round(secondaryPct))));
        setSplitCycleMinutes(Math.max(2, Math.round((split.cycle_duration_s ?? 1800) / 60)));
      }
      if (res.pools.length > 0) {
        const loadedPools = res.pools.slice(0, MAX_POOLS).map(p => ({
          url: p.url,
          worker: p.worker,
          password: '',
          priority: p.priority,
          protocol: p.protocol ?? 'auto',
          sv2_url: p.sv2_url ?? '',
        }));
        setPools(normalizePools(loadedPools));
      }
    }).catch(() => {
      setPoolLoadError('The miner did not return current pool configuration. You can still configure and save a new pool below.');
      addToast('Failed to load pool configuration', 'error');
    }).finally(() => {
      setLoadingPools(false);
    });
  }, [addToast]);

  const updatePool = (index: number, field: keyof PoolEntry, value: string | number | undefined) => {
    setPools(prev => {
      if (field === 'priority' && typeof value === 'number') {
        return reprioritizePools(prev, index, value);
      }
      return prev.map((p, i) => i === index ? { ...p, [field]: value } : p);
    });
  };

  const addPool = () => {
    if (pools.length < MAX_POOLS) {
      setPools(prev => [...prev, { ...emptyPool(), priority: prev.length }]);
    }
  };

  const removePool = (index: number) => {
    if (pools.length > 1) {
      setPools(prev => prev.filter((_, i) => i !== index).map((p, i) => ({ ...p, priority: i })));
    }
  };

  // Per-pool URL validation errors
  const urlErrors = useMemo(() =>
    pools.map(p => validateStratumUrl(p.url)),
    [pools]
  );

  // Can we save? No pool with a URL should have a validation error
  const hasValidationErrors = pools.some((p, i) => p.url && urlErrors[i] !== null);
  const configuredPoolCount = normalizePools(pools).filter(pool => pool.url.trim()).length;
  const splitNeedsSecondPool = splitEnabled && configuredPoolCount < 2;
  const splitCycleTooShort = splitEnabled && Math.round(splitCycleMinutes * 60 * Math.min(splitSecondaryPct, 100 - splitSecondaryPct) / 100) < 60;

  const testConnection = async (index: number) => {
    setTesting(index);
    setTestResults(prev => { const n = { ...prev }; delete n[index]; return n; });
    try {
      await api.testPoolConnection({
        url: pools[index].url,
        worker: pools[index].worker,
        password: pools[index].password || 'x',
        priority: pools[index].priority,
        ...(pools[index].protocol && pools[index].protocol !== 'auto' ? { protocol: pools[index].protocol } : {}),
        ...(pools[index].sv2_url ? { sv2_url: pools[index].sv2_url } : {}),
      });
      setTestResults(prev => ({ ...prev, [index]: 'ok' }));
    } catch {
      setTestResults(prev => ({ ...prev, [index]: 'fail' }));
    }
    setTesting(null);
  };

  const saveAll = async () => {
    if (hasValidationErrors) {
      addToast('Fix URL errors before saving', 'warning');
      return;
    }
    const configuredPools = normalizePools(pools)
      .filter(pool => pool.url.trim())
      .map((pool, priority) => ({
        url: pool.url.trim(),
        worker: pool.worker,
        password: pool.password || 'x',
        priority,
        ...(pool.protocol && pool.protocol !== 'auto' ? { protocol: pool.protocol } : {}),
        ...(pool.sv2_url?.trim() ? { sv2_url: pool.sv2_url.trim() } : {}),
      }));

    if (configuredPools.length === 0) {
      addToast('Enter a pool URL before saving', 'warning');
      return;
    }
    if (splitNeedsSecondPool) {
      addToast('Hashrate splitting needs Pool #1 and Pool #2', 'warning');
      return;
    }
    if (splitCycleTooShort) {
      addToast('Hashrate split windows must be at least 60 seconds', 'warning');
      return;
    }
    if (splitEnabled && configuredPools.slice(0, 2).some(pool => pool.protocol === 'sv2' || pool.sv2_url)) {
      addToast('Hashrate splitting is V1-only in this build', 'warning');
      return;
    }

    setPendingSave('applying');
    setPendingTargetUrl(configuredPools[0].url);
    try {
      await api.configurePools({
        pools: configuredPools,
        hashrate_split: {
          enabled: splitEnabled,
          secondary_pool_index: 1,
          secondary_pct: splitSecondaryPct,
          cycle_duration_s: Math.max(120, Math.round(splitCycleMinutes * 60)),
        },
      });
    } catch (err) {
      setPendingSave('idle');
      setPendingTargetUrl(null);
      const detail = err instanceof ApiError
        ? (err.suggestion || err.message)
        : configuredPools[0].url;
      addToast(`Failed to save pools: ${detail}`, 'error');
      return;
    }
    setPools(normalizePools(configuredPools.map(pool => ({ ...pool, password: '' }))));
    // : do NOT toast "Pool switched". Daemon still has to subscribe,
    // authorize, and pull the first job. We say "applying" → "pending" until
    // /api/pools reports stratum_active === true for the new URL.
    setPendingSave('pending');
    addToast('Pool configuration saved — applying', 'success');
  };

  // : clear pending-save when the daemon confirms the new pool is
  // actually connected (stratum_active true for the saved primary URL).
  useEffect(() => {
    if (pendingSave !== 'pending' || pendingTargetUrl == null) return;
    const matched = livePools.find(p => p.url === pendingTargetUrl);
    if (matched?.stratum_active) {
      setPendingSave('idle');
      setPendingTargetUrl(null);
    }
  }, [pendingSave, pendingTargetUrl, livePools]);

  // F003 fix: while a save is pending confirmation, actively re-poll /api/pools
  // so livePools refreshes (the initial load only runs once on mount, so the
  // confirm effect above would otherwise watch a stale snapshot forever and
  // leave "Save Pools" stuck disabled). A bounded timeout guarantees the button
  // re-enables even when the daemon never reports stratum_active (e.g. a wrong
  // or unreachable pool URL) by clearing pendingSave and surfacing an error.
  useEffect(() => {
    if (pendingSave !== 'pending' || pendingTargetUrl == null) return;
    let cancelled = false;
    const refresh = () => {
      api.getPools().then(res => {
        if (cancelled) return;
        setLivePools(res.pools);
        setFailoverState(res.failover ?? null);
      }).catch(() => {
        // Transient poll failure — keep trying until the timeout fires.
      });
    };
    refresh();
    const interval = setInterval(refresh, 5000);
    const timeout = setTimeout(() => {
      if (cancelled) return;
      setPendingSave('idle');
      setPendingTargetUrl(null);
      addToast(
        `Pools saved, but the miner did not confirm a connection to ${pendingTargetUrl} in time. Check the pool URL and miner status.`,
        'error',
      );
    }, 45000);
    return () => {
      cancelled = true;
      clearInterval(interval);
      clearTimeout(timeout);
    };
  }, [pendingSave, pendingTargetUrl, addToast]);

  // Hero strip aggregates from live pools
  const activeLivePool = useMemo<PoolInfo | null>(() => {
    if (livePools.length === 0) return null;
    return livePools.find(p => p.stratum_active) ?? livePools[0];
  }, [livePools]);
  const heroAccepted = livePools.reduce((s, p) => s + (p.accepted ?? 0), 0);
  const heroRejected = livePools.reduce((s, p) => s + (p.rejected ?? 0), 0);
  const heroTotalShares = heroAccepted + heroRejected;
  const heroAcceptRate = heroTotalShares > 0 ? (heroAccepted / heroTotalShares) * 100 : 0;
  const heroLastShareS = activeLivePool?.last_share_s ?? null;
  const heroLastShareDisplay = heroLastShareS == null
    ? '—'
    : heroLastShareS <= 0 ? 'never'
    : heroLastShareS < 60 ? `${Math.round(heroLastShareS)}s`
    : heroLastShareS < 3600 ? `${Math.round(heroLastShareS / 60)}m`
    : `${Math.round(heroLastShareS / 3600)}h`;
  const heroFailoverSwitches = failoverState?.switch_count ?? 0;
  const heroPoolHost = activeLivePool?.url ?? (pools[0]?.url || 'Not configured');
  const heroBadgeTone: 'good' | 'warn' | 'muted' = !activeLivePool
    ? 'muted'
    : activeLivePool.stratum_active
      ? heroAcceptRate >= 95 || heroTotalShares === 0 ? 'good' : 'warn'
      : 'warn';
  //  truth contract: stratum_active means the daemon completed
  // subscribe+authorize. We don't conflate that with "mining" — we say
  // "awaiting first share" until the first accepted share arrives.
  const heroPoolStatus = (activeLivePool?.status ?? '').toLowerCase();
  const heroBadgeLabel = !activeLivePool
    ? 'no telemetry'
    : activeLivePool.stratum_active
      ? heroTotalShares === 0 ? 'authorized · awaiting first share'
        : heroAcceptRate >= 95 ? 'mining · healthy' : 'mining · degraded accept'
      : heroPoolStatus === 'connecting' || heroPoolStatus === 'configured' ? 'connecting'
      : (activeLivePool.status ?? 'offline');

  const splitPrimaryPct = 100 - splitSecondaryPct;
  const splitActiveLabel = hashrateSplit?.active_route
    ? hashrateSplit.active_route.replace(/_/g, ' ')
    : splitEnabled ? 'configured' : 'disabled';
  const splitRuntimeStatus: 'online' | 'warning' | 'error' =
    !splitEnabled ? 'warning' :
    hashrateSplit?.runtime_active ? 'online' : 'warning';
  const splitEffectivePrimary = hashrateSplit?.configured_effective_primary_pct ?? splitPrimaryPct;
  const splitEffectiveSecondary = hashrateSplit?.configured_effective_secondary_pct ?? splitSecondaryPct;
  const sharesUnresolved = failoverState?.shares_unresolved ?? failoverState?.unresolved_submit_count ?? null;
  const pendingSubmitDropped = failoverState?.pending_submit_dropped ?? null;

  return (
    <div className="page-content">
      <div className="page-hero-strip">
        <div className="page-hero-summary">
          <div className="page-hero-eyebrow">POOL</div>
          <div className="page-hero-title">Stratum Pools</div>
          <div className="page-hero-stat" style={{ fontSize: '1.2rem', wordBreak: 'break-all' }}>
            {heroPoolHost}
          </div>
          <div className="page-hero-substat">
            {activeLivePool
              ? `Active priority ${activeLivePool.priority ?? 0} · ${activeLivePool.status ?? 'unknown'}`
              : 'Save a pool below to begin submitting shares.'}
          </div>
        </div>
        <div className="hero-kpi-strip">
          <div className="hero-kpi">
            <div className="kpi-label">Accept Rate</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {heroTotalShares > 0 ? `${heroAcceptRate.toFixed(1)}%` : '—'}
              </span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Accepted / Rejected</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {heroAccepted.toLocaleString()} / {heroRejected.toLocaleString()}
              </span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Last Share</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{heroLastShareDisplay}</span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Failover Switches</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{heroFailoverSwitches.toLocaleString()}</span>
            </div>
          </div>
        </div>
      </div>

      <section className="section">
      <div className="page-toolbar" style={{ marginBottom: 16 }}>
        <div className="section-title" style={{ margin: 0 }}>
          Pool Configuration
          <span
            className={`small-tag ${heroBadgeTone}`}
            data-tooltip={glossaryText('pool_state')}
          >
            {heroBadgeLabel}
          </span>
        </div>
        <div className="page-toolbar-actions" style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          {pendingSave !== 'idle' && (
            <span
              className="ds-chip ds-warning ds-live"
              aria-live="polite"
              title={pendingTargetUrl ?? undefined}
            >
              <span className="ds-dot" />
              {pendingSave === 'applying' ? 'Applying…' : 'Pending — connecting to new pool'}
            </span>
          )}
          <ActionButton
            label={pendingSave === 'idle' ? 'Save Pools' : 'Saving…'}
            onClick={saveAll}
            disabled={hasValidationErrors || splitNeedsSecondPool || splitCycleTooShort || pendingSave !== 'idle'}
          />
        </div>
      </div>

      {/* Wave-13: the card-level sub-tab bar (Pools / Pool Health / SV2 Status)
          was dropped — it duplicated the PAGE-level tabs (Pools / Shares /
          SV2 Status / Own Templates in StandardDashboard), so "Pools" and "SV2
          Status" appeared at two levels (operator's "repetition" report). The
          Pool editor + the unique Active Pool Health telemetry now render
          inline (stacked) below. The per-pool SV2 surface was removed here
          because the page already has a dedicated "SV2 Status" tab. */}

      <TaskHandoffBanner
        expectedMode="standard"
        title="Pool task opened from Heater mode"
        copy="Finish pool commissioning here, then jump straight back to the simpler heat view when you are done."
      />

      {loadingPools && (
        <StatePanel
          title="Loading pool telemetry"
          message="Fetching saved pool settings, share health, and protocol status from dcentrald."
          tone="info"
          compact
        />
      )}

      {!loadingPools && poolLoadError && (
        <StatePanel
          title="Could not load current pool state"
          message={poolLoadError}
          tone="danger"
          compact
        />
      )}

      {!loadingPools && !poolLoadError && livePools.length === 0 && (
        pools.some(pool => pool.url.trim()) ? (
          <StatePanel
            title="Pool saved locally, waiting for miner telemetry"
            message="A pool URL is present in the editor, but the miner is not reporting active pool telemetry yet. Save the pool or check connectivity."
            tone="warning"
            compact
          />
        ) : (
          <EmptyState
            illustration={<NoPoolIllustration />}
            title="No pool configured"
            hint="Set up your Stratum endpoint to start mining."
            data-testid="pool-config-empty"
          />
        )
      )}

      {/* Active Pool Health — live accept-rate / failover / donation telemetry.
          Unique to this section (no other home), rendered inline. */}
      {livePools.length > 0 && (
        <div className="page-surface" style={{ marginBottom: 20 }}>
          <div className="page-surface-header">
            <div>
              <div className="page-surface-title" style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
                Active Pool Health
                <InfoDot term="pool_job_fresh" size={12} label="Pool routing ladder: authorized, job-fresh, share-accepted" />
              </div>
              <div className="page-surface-copy">
                Read-only pool health from /api/pools. This view does not switch pools or trigger failover.
              </div>
            </div>
          </div>
          {failoverState && (
            <div className="pool-live-card" style={{ marginBottom: 12 }}>
              <div className="pool-live-header">
                <StatusPill
                  status={failoverState.current_pool_role === 'user_primary' ? 'online' : failoverState.current_pool_role === 'user_failover' ? 'warning' : 'unknown'}
                  label={failoverState.current_pool_role?.replace(/_/g, ' ') ?? 'failover state'}
                />
                <span className="pool-live-url">
                  Active pool {failoverState.active_pool_priority}: {failoverState.active_pool_host ?? failoverState.active_pool_url}
                </span>
              </div>
              <div className="pool-live-health">
                <span style={{ color: 'var(--text-dim)' }}>
                  Switches: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                    {failoverState.switch_count}
                  </span>
                </span>
                <span style={{ color: 'var(--text-dim)' }}>
                  Last reason: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                    {failoverState.last_switch_reason ?? 'Unavailable'}
                  </span>
                </span>
                <span style={{ color: 'var(--text-dim)' }}>
                  Backoff: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                    {failoverState.backoff_ms ? `${Math.round(failoverState.backoff_ms / 1000)}s` : 'None'}
                  </span>
                </span>
                <span style={{ color: 'var(--text-dim)' }}>
                  Stale flush: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: failoverState.stale_jobs_flushed_on_switch ? 'var(--green)' : 'var(--text-secondary)' }}>
                    {failoverState.stale_jobs_flushed_on_switch ? 'proved' : 'not observed'}
                  </span>
                </span>
                <span style={{ color: 'var(--text-dim)' }}>
                  shares_unresolved: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                    {formatTelemetryCount(sharesUnresolved)}
                  </span>
                </span>
                <span style={{ color: 'var(--text-dim)' }}>
                  pending_submit_dropped: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: pendingSubmitDropped ? 'var(--yellow)' : 'var(--text-secondary)' }}>
                    {formatTelemetryCount(pendingSubmitDropped)}
                  </span>
                </span>
              </div>
              <div className="pool-live-health" style={{ borderTop: '1px solid var(--border)', paddingTop: 10, marginTop: 10 }}>
                <span style={{ color: 'var(--text-dim)' }} data-tooltip={glossaryText('donation')}>
                  Donation: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: failoverState.donation?.active ? 'var(--yellow)' : 'var(--text-secondary)' }}>
                    {failoverState.donation?.active ? 'active' : failoverState.donation?.enabled ? 'scheduled' : 'disabled'}
                  </span>
                </span>
                <span style={{ color: 'var(--text-dim)' }}>
                  Donation backup: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: failoverState.donation?.fallback_enabled ? 'var(--green)' : 'var(--text-secondary)' }}>
                    {failoverState.donation?.fallback_enabled ? failoverState.donation?.fallback_pool_host ?? 'enabled' : 'disabled'}
                  </span>
                </span>
                <span style={{ color: 'var(--text-dim)' }}>
                  Secrets: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: failoverState.secrets_included ? 'var(--red)' : 'var(--green)' }}>
                    {failoverState.secrets_included ? 'included' : 'redacted'}
                  </span>
                </span>
                <span style={{ color: 'var(--text-dim)' }}>
                  Source: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                    {failoverState.telemetry_source}
                  </span>
                </span>
              </div>
            </div>
          )}
          <div className="pool-live-list">
          {livePools.map(p => {
            const totalShares = (p.accepted ?? 0) + (p.rejected ?? 0);
            const acceptRate = totalShares > 0 ? ((p.accepted ?? 0) / totalShares) * 100 : 0;
            const acceptStatus: 'online' | 'warning' | 'error' =
              totalShares === 0 ? 'warning' :
              acceptRate >= 98 ? 'online' :
              acceptRate >= 90 ? 'warning' : 'error';
            const lastShareAge = p.last_share_s ?? 0;
            const lastShareDisplay = lastShareAge <= 0 ? 'never'
              : lastShareAge < 60 ? `${Math.round(lastShareAge)}s ago`
              : lastShareAge < 3600 ? `${Math.round(lastShareAge / 60)}m ago`
              : `${Math.round(lastShareAge / 3600)}h ago`;
            const shareEfficiency = p.share_efficiency;
            const notifyAgeDisplay = typeof p.no_notify_age_s === 'number'
              ? `${Math.round(p.no_notify_age_s)}s`
              : 'No mining.notify age reported';
            // : stale-job indicator. Stratum V1 pools typically send a
            // fresh `mining.notify` within ~60s; older is a real anomaly.
            const jobStale = typeof p.no_notify_age_s === 'number' && p.no_notify_age_s > 60;
            const fallbackDisplay = p.auto_fallback_active
              ? `Fallback active${p.auto_fallback_reason ? `: ${p.auto_fallback_reason}` : ''}`
              : 'No automatic fallback active';
            const failoverDisplay = p.failover_switch_count && p.failover_switch_count > 0
              ? `Pool ${((p.failover_active_pool_index ?? 0) + 1)} / ${p.failover_last_switch_reason ?? 'switch observed'}`
              : fallbackDisplay;

            return (
              <div key={p.id} className="pool-live-card">
                <div className="pool-live-header">
                  {/* Embedded liveness pill — routed through the component-
                      contract §6 PoolState→StatusState projection so a
                      connecting pool never renders as connected and only an
                      accepted-share-flowing pool claims `mining`. The visible
                      `label` stays the daemon's own status string (operator
                      truth), only the pill TONE/STATE is projected. */}
                  <StatusPill
                    status={poolLivenessStatusState(p)}
                    label={p.status}
                    pulse={p.stratum_active}
                  />
                  {(p.protocol ?? 'sv1') === 'sv2' && (
                    <span className="protocol-badge sv2">
                      SV2 {'\u{1F512}'}
                    </span>
                  )}
                  {(p.protocol ?? 'sv1') === 'sv1' && p.encrypted === true && (
                    <span className="protocol-badge tls">
                      TLS
                    </span>
                  )}
                  {jobStale && (
                    <span
                      className="ds-chip ds-warning"
                      data-tooltip={`No mining.notify for ${Math.round(p.no_notify_age_s ?? 0)}s — the pool job may be stale. The miner keeps hashing the last job; judge health by accepted shares.`}
                    >
                      <span className="ds-dot" />
                      Stale job · {Math.round(p.no_notify_age_s ?? 0)}s
                    </span>
                  )}
                  <span className="pool-live-url">
                    {p.url}
                  </span>
                </div>
                {/* Pool health row */}
                <div className="pool-live-health">
                  {/* Acceptance rate */}
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                    <StatusPill
                      status={acceptStatus}
                      label={totalShares > 0 ? `${acceptRate.toFixed(1)}%` : 'N/A'}
                    />
                    <span style={{ color: 'var(--text-dim)' }}>accept</span>
                  </div>
                  {/* Shares count */}
                  <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                    {(p.accepted ?? 0).toLocaleString()}A / {(p.rejected ?? 0).toLocaleString()}R
                  </span>
                  {/* Last share */}
                  <span style={{ color: 'var(--text-dim)' }}>
                    Last share: <span style={{
                      fontFamily: "'JetBrains Mono', monospace",
                      color: lastShareAge > 300 ? 'var(--yellow)' : 'var(--text-secondary)',
                    }}>
                      {lastShareDisplay}
                    </span>
                  </span>
                  {/* Difficulty */}
                  {(p.difficulty ?? 0) > 0 && (
                    <span style={{ color: 'var(--text-dim)' }}>
                      Diff: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                        {p.difficulty >= 1000 ? `${(p.difficulty / 1000).toFixed(1)}K` : p.difficulty}
                      </span>
                    </span>
                  )}
                  {/* Per-pool latency (HLA-9 truthfulness): only the active pool
                      has a measured submit->response RTT; inactive pools render an
                      honest "—", never a fake 0 ms. */}
                  <PoolLatencyBadge
                    latencyMs={p.latency_ms}
                    latencyMeasured={p.latency_measured}
                    latencyMsSource={p.latency_ms_source}
                    poolId={p.id}
                  />
                </div>
                <div className="pool-live-health" style={{ borderTop: '1px solid var(--border)', paddingTop: 10, marginTop: 10 }}>
                  <span style={{ color: 'var(--text-dim)' }}>
                    Telemetry: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                      {p.telemetry_source ?? (p.stratum_active ? 'runtime_state' : 'configured_pool')}
                    </span>
                  </span>
                  <span style={{ color: 'var(--text-dim)' }}>
                    Notify: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                      {notifyAgeDisplay}
                    </span>
                  </span>
                  <span style={{ color: 'var(--text-dim)' }}>
                    Failover: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                      {failoverDisplay}
                    </span>
                  </span>
                  <span style={{ color: 'var(--text-dim)' }}>
                    shares_unresolved: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                      {formatTelemetryCount(p.shares_unresolved)}
                    </span>
                  </span>
                  <span style={{ color: 'var(--text-dim)' }}>
                    pending_submit_dropped: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: (p.pending_submit_dropped ?? 0) > 0 ? 'var(--yellow)' : 'var(--text-secondary)' }}>
                      {formatTelemetryCount(p.pending_submit_dropped)}
                    </span>
                  </span>
                  <span style={{ color: 'var(--text-dim)' }}>
                    Policy: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                      {p.failover_policy ?? 'observability_only'}
                    </span>
                  </span>
                </div>
                {p.health_limitations && p.health_limitations.length > 0 && (
                  <div className="pool-live-health" style={{ color: 'var(--text-dim)' }}>
                    {p.health_limitations.slice(0, 2).map((limitation, limitationIndex) => (
                      <span key={limitationIndex}>{limitation}</span>
                    ))}
                  </div>
                )}
                {shareEfficiency && (
                  <div className="pool-live-health" style={{ borderTop: '1px solid var(--border)', paddingTop: 10, marginTop: 10 }}>
                    <span style={{ color: 'var(--text-dim)' }}>
                      Target diff / kWh: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                        {formatEfficiencyMetric(shareEfficiency.accepted_difficulty_per_kwh, 0)}
                      </span>
                    </span>
                    <span style={{ color: 'var(--text-dim)' }}>
                      Shares / kWh: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                        {formatEfficiencyMetric(shareEfficiency.accepted_shares_per_kwh, 2)}
                      </span>
                    </span>
                    <span style={{ color: 'var(--text-dim)' }}>
                      Window: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                        {Math.max(1, Math.round(shareEfficiency.window_s / 60))}m / {shareEfficiency.estimated_wall_energy_kwh.toFixed(3)} kWh
                      </span>
                    </span>
                    <span style={{ color: 'var(--text-dim)' }}>
                      Source: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                        {shareEfficiency.power_source}{shareEfficiency.calibrated ? ' calibrated' : ''}
                      </span>
                    </span>
                  </div>
                )}
              </div>
            );
          })}
          </div>
        </div>
      )}

      {/* Kit "Pools" subtab — the editable pool/group config, hashrate
          split, add-pool, and quick-add templates. */}
      {(<>
      <div className="page-surface" style={{ marginBottom: 20 }}>
        <div className="page-surface-header">
          <div>
            <div className="page-surface-title">Hashrate Splitting</div>
            <div className="page-surface-copy">
              Weighted V1 route between Pool #1 and Pool #2. Donation time remains separate.
            </div>
          </div>
          <StatusPill
            status={splitRuntimeStatus}
            label={splitEnabled ? splitActiveLabel : 'disabled'}
          />
        </div>

        <div className="surface-stack">
          <label className="field-label" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <input
              type="checkbox"
              checked={splitEnabled}
              onChange={e => setSplitEnabled(e.target.checked)}
              style={{ width: 'auto' }}
            />
            Enable pool1/pool2 split
          </label>

          <div className="standard-grid-2" style={{ gap: 12 }}>
            <div>
              <label className="field-label">
                Pool #2 allocation
              </label>
              <input
                type="range"
                min={1}
                max={99}
                value={splitSecondaryPct}
                disabled={!splitEnabled}
                onChange={e => setSplitSecondaryPct(Number(e.target.value))}
                aria-label="Pool #2 hashrate allocation percent"
                aria-valuetext={`Pool #2 ${splitSecondaryPct}%, Pool #1 ${splitPrimaryPct}%`}
              />
              <div className="pool-live-health" style={{ marginTop: 8 }}>
                <span style={{ color: 'var(--text-dim)' }}>
                  Pool #1: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                    {splitPrimaryPct}%
                  </span>
                </span>
                <span style={{ color: 'var(--text-dim)' }}>
                  Pool #2: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                    {splitSecondaryPct}%
                  </span>
                </span>
              </div>
            </div>
            <div>
              <label className="field-label">
                Cycle minutes
              </label>
              <input
                type="number"
                min={2}
                max={1440}
                value={splitCycleMinutes}
                disabled={!splitEnabled}
                onChange={e => setSplitCycleMinutes(Math.max(2, Number(e.target.value) || 2))}
                aria-label="Hashrate split cycle minutes"
              />
              <div className="pool-live-health" style={{ marginTop: 8 }}>
                <span style={{ color: 'var(--text-dim)' }}>
                  Next route: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                    {hashrateSplit?.cycle_remaining_s ? `${Math.round(hashrateSplit.cycle_remaining_s)}s` : 'after reconnect'}
                  </span>
                </span>
                <span style={{ color: 'var(--text-dim)' }}>
                  Switches: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                    {hashrateSplit?.switch_count ?? 0}
                  </span>
                </span>
              </div>
            </div>
          </div>

          {splitEnabled && (
            <div className="pool-live-health" style={{ borderTop: '1px solid var(--border)', paddingTop: 10 }}>
              <span style={{ color: 'var(--text-dim)' }}>
                Effective pool #1: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                  {splitEffectivePrimary.toFixed(1)}%
                </span>
              </span>
              <span style={{ color: 'var(--text-dim)' }}>
                Effective pool #2: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                  {splitEffectiveSecondary.toFixed(1)}%
                </span>
              </span>
              <span style={{ color: 'var(--text-dim)' }}>
                Donation: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: hashrateSplit?.donation_composed ? 'var(--yellow)' : 'var(--text-secondary)' }}>
                  {hashrateSplit?.donation_composed ? `${hashrateSplit.donation_pct?.toFixed(1) ?? '2.0'}% composed` : 'not composed'}
                </span>
              </span>
              <span style={{ color: 'var(--text-dim)' }}>
                Secondary shares: <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--text-secondary)' }}>
                  {hashrateSplit?.secondary_shares ?? 0}
                </span>
              </span>
            </div>
          )}

          {splitNeedsSecondPool && (
            <StatePanel
              title="Pool #2 required"
              message="Add a second V1 pool before saving hashrate splitting."
              tone="warning"
              compact
            />
          )}
          {splitCycleTooShort && (
            <StatePanel
              title="Cycle too short"
              message="Increase the cycle or use a less extreme split so each route window is at least 60 seconds."
              tone="warning"
              compact
            />
          )}
        </div>
      </div>

      {/* Pool editor */}
      {pools.map((pool, i) => (
        <div key={i} className="page-surface" style={{
          borderColor: urlErrors[i] ? 'var(--red)' : 'var(--border)', marginBottom: 12,
        }}>
          <div className="page-surface-header">
            <div>
              <div className="page-surface-title">Pool #{i + 1}</div>
              <div className="page-surface-copy">
                Configure the worker address, protocol preference, and optional SV2 transport details.
              </div>
            </div>
            <div className="page-surface-actions">
              {testResults[i] && (
                <StatusPill
                  status={testResults[i] === 'ok' ? 'online' : 'error'}
                  label={testResults[i] === 'ok' ? 'OK' : 'Failed'}
                />
              )}
              {pools.length > 1 && (
                <button
                  className="btn btn-secondary"
                  onClick={() => removePool(i)}
                  style={{ padding: '4px 10px', fontSize: '0.8rem' }}
                >
                  Remove
                </button>
              )}
            </div>
          </div>

          <div className="surface-stack">
            <div>
              <label className="field-label">
                Stratum URL
              </label>
              <input
                type="url"
                value={pool.url}
                onChange={e => updatePool(i, 'url', e.target.value)}
                placeholder="stratum+tcp://pool.example.com:3333"
                style={urlErrors[i] ? { borderColor: 'var(--red)' } : undefined}
              />
              {urlErrors[i] && (
                <div style={{
                  fontSize: '0.75rem', color: 'var(--red)', marginTop: 4,
                }}>
                  {urlErrors[i]}
                </div>
              )}
            </div>
            <div>
              <label className="field-label">
                Protocol
              </label>
              <select
                value={pool.protocol ?? 'auto'}
                onChange={e => updatePool(i, 'protocol', e.target.value)}
                style={{ width: 'auto', fontSize: '0.85rem' }}
                aria-label={`Pool ${i + 1} protocol`}
              >
                <option value="auto">Auto (prefer SV2)</option>
                <option value="sv1">Stratum V1 only</option>
                <option value="sv2">Stratum V2 only</option>
              </select>
            </div>
            {(pool.protocol === 'sv2' || pool.protocol === 'auto') && (
              <div>
                <label className="field-label">
                  SV2 URL <span style={{ opacity: 0.5 }}>(optional — auto-discovered if blank)</span>
                </label>
                <input
                  type="text"
                  placeholder="stratum2+tcp://pool:port"
                  value={pool.sv2_url || ''}
                  onChange={e => updatePool(i, 'sv2_url', e.target.value)}
                />
              </div>
            )}
            <div className="standard-grid-2" style={{ gap: 10 }}>
              <div>
                <label className="field-label">
                  Worker
                </label>
                <input
                  type="text"
                  value={pool.worker}
                  onChange={e => updatePool(i, 'worker', e.target.value)}
                  placeholder="bc1q...worker1"
                />
              </div>
              <div>
                <label className="field-label">
                  Password
                </label>
                <input
                  type="password"
                  value={pool.password}
                  onChange={e => updatePool(i, 'password', e.target.value)}
                  placeholder="x"
                />
              </div>
            </div>
            <div className="standard-inline-actions">
              {/* Wave 4: explicit up/down failover-order arrows (drag would
                  need a new dep). The existing priority dropdown stays as
                  the canonical control for users who want a direct value. */}
              <button
                type="button"
                className="btn btn-secondary"
                aria-label="Move pool up in failover order"
                title="Move up"
                onClick={() => updatePool(i, 'priority', Math.max(0, i - 1))}
                disabled={i === 0}
                style={{ padding: '4px 9px', fontSize: '0.85rem' }}
              >
                ↑
              </button>
              <button
                type="button"
                className="btn btn-secondary"
                aria-label="Move pool down in failover order"
                title="Move down"
                onClick={() => updatePool(i, 'priority', Math.min(pools.length - 1, i + 1))}
                disabled={i === pools.length - 1}
                style={{ padding: '4px 9px', fontSize: '0.85rem' }}
              >
                ↓
              </button>
              <select
                value={pool.priority}
                onChange={e => updatePool(i, 'priority', Number(e.target.value))}
                style={{ width: 'auto' }}
                aria-label="Set pool priority directly"
              >
                  {pools.slice(0, MAX_POOLS).map((_, pi) => (
                    <option key={pi} value={pi}>Priority {pi + 1}</option>
                  ))}
                </select>
              <button
                className="btn btn-secondary"
                onClick={() => testConnection(i)}
                disabled={!pool.url || testing === i || !!urlErrors[i]}
                style={{ fontSize: '0.85rem' }}
              >
                {testing === i ? 'Testing…' : 'Test Connection'}
              </button>
            </div>
          </div>
        </div>
      ))}

      <div className="page-action-row" style={{ marginBottom: 24 }}>
        {pools.length < MAX_POOLS && (
          <button className="btn btn-secondary" onClick={addPool}>
            + Add Pool
          </button>
        )}
      </div>

      {/* Quick pool templates */}
      <div className="page-surface">
        <div className="page-surface-header">
          <div>
            <div className="page-surface-title">Quick Add Templates</div>
            <div className="page-surface-copy">
              Load a known-good pool preset, then add your wallet and worker details before saving.
            </div>
          </div>
        </div>
        <div className="template-grid">
        {POOL_TEMPLATES.map(tpl => (
          <button
            key={tpl.name}
            onClick={() => {
              const emptyIdx = pools.findIndex(p => !p.url);
              if (emptyIdx >= 0) {
                updatePool(emptyIdx, 'url', tpl.url);
                if (tpl.sv2_supported && tpl.sv2_url) {
                  updatePool(emptyIdx, 'sv2_url', tpl.sv2_url);
                  updatePool(emptyIdx, 'protocol', 'auto');
                }
              } else if (pools.length < MAX_POOLS) {
                setPools(prev => [...prev, {
                  url: tpl.url, worker: '', password: '', priority: prev.length,
                  protocol: tpl.sv2_supported ? 'auto' : 'sv1',
                  sv2_url: tpl.sv2_url ?? '',
                }]);
              }
            }}
            className={`template-card ${tpl.highlighted ? 'highlighted' : ''}`}
          >
            <div className="template-card-title">
              {tpl.name}
              {tpl.sv2_supported && (
                <span className="protocol-badge sv2">
                  SV2
                </span>
              )}
            </div>
            <div className="template-card-copy">
              {tpl.description}
            </div>
          </button>
        ))}
        </div>
      </div>
      </>)}

      </section>
    </div>
  );
}
