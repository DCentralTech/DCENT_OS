// AutoTunerPanel — operator control surface for the live autotuner.
// W15-B (wave 15) — closes the dashboard gap left by W13-A's
// backend-only mode-change runtime wiring.
//  Finish (Agent W3): DS migration (.ds-glass-card / .ds-btn),
// per-chip drill-in via /api/autotuner/chip-health, safe-envelope
// display from dispatcher_limits.
//
// Sections:
//   1. Mode + step controls (current mode + ± hashrate / ± power /
//      reset-to-default).
//   2. Per-chain rows from /api/stats (chain id, chip count, freq,
//      voltage, status icon, active silicon profile + change button).
//      Each row is collapsible to reveal per-chip health cards.
//   3. Profile selector dropdown (per-chain change → silicon profile
//      setActive).
//
// Polling: 2s on mount, cleared on unmount.
// Per-chip: fetched on chain-row expand (lazy), 15s auto-refresh.
//
// Dependencies — all already in tree:
//   `autotunerApi`              ./api/autotuner.ts
//   `siliconProfilesApi`        ./api/profiles-silicon.ts (W8-D)
//   `api.getStats`              ./api/client.ts (chain freq/voltage)
//   `api.getAutotunerChipHealth`  ./api/client.ts (chip health)
//   `<StatusIcon />`            ./common/StatusIcon.tsx (W13-D)
//
// Honest gap: the active silicon profile per (model, hashboard) is not
// surfaced by /api/autotuner/status today — `active_silicon_profile_ids`
// stays inside `dcentrald-autotuner::AutoTuner`. The panel reads the
// imported profile list and lets the operator pick one to apply per
// hashboard; it shows "—" for the active profile column and relies on
// the backend ack ("registered + applied/deferred/rejected") for
// feedback. Surfacing the live active id is wave-16 backlog.

import React, { useEffect, useMemo, useState } from 'react';
import api from '../../api/client';
import { autotunerApi } from '../../api/autotuner';
import type {
  AutotunerChipHealthStatus,
  AutotunerStatusResponse,
  StatsChain,
  StatsResponse,
  SystemInfoResponse,
} from '../../api/types';
import { siliconProfilesApi, type SiliconProfileSummary } from '../../api/profiles-silicon';
import { StatusIcon, type StatusIconState } from '../common/StatusIcon';
import { Tooltip, InfoDot } from '../common/Tooltip';

const POLL_INTERVAL_MS = 2000;
const CHIP_HEALTH_REFRESH_MS = 15000;

// Per-chip health status colours — mirrors the backend health_score range.
// health_score: 0–100 (100 = perfect). trend: 'stable'|'improving'|'degrading'.
function chipHealthColor(chip: AutotunerChipHealthStatus): string {
  if (chip.status === 'failed') return 'var(--red, #EF4444)';
  if (chip.status === 'warning' || chip.health_score < 60) return 'var(--yellow, #F59E0B)';
  if (chip.health_score >= 85) return 'var(--green, #10B981)';
  return 'var(--text-dim)';
}

// trend is a numeric enum from the backend: >0 = improving, <0 = degrading, 0 = stable.
function chipTrendGlyph(trend: number | undefined): string {
  if (trend == null) return '•';
  if (trend > 0) return '↑';
  if (trend < 0) return '↓';
  return '•';
}

interface ToastState {
  kind: 'ok' | 'warn' | 'error';
  text: string;
}

function chainStatusIcon(chain: StatsChain, autotuner: AutotunerStatusResponse | null): StatusIconState {
  if (chain.errors > 5) return 'fail';
  if (chain.status === 'failed') return 'fail';
  if (chain.hw_errors > 0) return 'warn';
  if (autotuner && (autotuner.tuned_chain_ids ?? []).includes(chain.id)) return 'ok';
  if (autotuner && (autotuner.failed_chain_ids ?? []).includes(chain.id)) return 'fail';
  if (autotuner && autotuner.active_chain_id === chain.id) return 'warn'; // converging
  if (chain.hashrate_ths > 0) return 'ok';
  return 'pending';
}

function formatFreq(mhz: number | null | undefined): string {
  if (mhz == null || mhz <= 0) return '—';
  return `${mhz.toFixed(0)} MHz`;
}

function formatVoltage(volts: number | null | undefined): string {
  if (volts == null || volts <= 0) return '—';
  return `${volts.toFixed(2)} V`;
}

function modeDisplay(status: AutotunerStatusResponse | null): string {
  if (!status) return 'Loading…';
  const obj = status.policy?.active_objective;
  switch (obj) {
    case 'efficiency': return 'Efficiency (Use less power)';
    case 'hashrate': return 'Hashrate (More heat)';
    case 'hashrate_target': return 'Hashrate target';
    case 'power_cap': return 'Power cap';
    case 'manual': return 'Manual';
    default:
      return status.policy?.requested_preset_display_name
        || status.policy?.requested_preset
        || status.phase
        || 'Unknown';
  }
}

// Stable-for timer derived from the REAL last_update_s timestamp. Shown only once
// the daemon reports a settled phase (tuned / partially_tuned) — see isTuned below.
function formatStableSince(lastUpdateS: number): string {
  if (!lastUpdateS || lastUpdateS <= 0) return 'Tuned';
  const ageS = Math.max(0, Math.floor(Date.now() / 1000) - lastUpdateS);
  if (ageS < 5) return 'Just converged';
  if (ageS < 60) return `Stable for ${ageS}s`;
  if (ageS < 3600) return `Stable for ${Math.floor(ageS / 60)}m`;
  return `Stable for ${Math.floor(ageS / 3600)}h`;
}

interface ChainProfileRow {
  chain: StatsChain;
  matchingProfiles: SiliconProfileSummary[];
}

// ── Per-chip drill-in component ────────────────────────────────────────────
// Fetched lazily when the operator expands a chain row.
// Data comes from /api/autotuner/chip-health (same endpoint used by
// ChipFreqMap.tsx in the advanced view). Shows health_score, freq, trend,
// error rate, and backoff count per chip — read-only, no mutations.
function ChipHealthDrillIn({ chainId }: { chainId: number }) {
  const [chips, setChips] = useState<AutotunerChipHealthStatus[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;

    const load = async () => {
      try {
        const res = await api.getAutotunerChipHealth();
        if (cancelled) return;
        setChips(res.chips.filter(c => c.chain_id === chainId));
        setError(null);
      } catch (e) {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : 'chip-health unavailable');
      } finally {
        if (!cancelled) {
          setLoading(false);
          timer = window.setTimeout(load, CHIP_HEALTH_REFRESH_MS);
        }
      }
    };

    void load();
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [chainId]);

  if (loading && chips === null) {
    return (
      <div style={{ padding: '8px 12px', color: 'var(--text-dim)', fontSize: '0.78rem' }}>
        Loading chip health…
      </div>
    );
  }

  if (error) {
    return (
      <div style={{ padding: '8px 12px', fontSize: '0.78rem', color: 'var(--text-dim)' }}>
        Per-chip detail unavailable: {error}
      </div>
    );
  }

  if (!chips || chips.length === 0) {
    return (
      <div style={{ padding: '8px 12px', fontSize: '0.78rem', color: 'var(--text-dim)' }}>
        Per-chip detail unavailable — no chip-health data reported for this chain.
        This is normal when the autotuner has not yet profiled the chain.
      </div>
    );
  }

  const failedCount = chips.filter(c => c.status === 'failed').length;
  const warnCount = chips.filter(c => c.status === 'warning' || c.health_score < 60).length;

  return (
    <>
    <div className="autotuner-chip-legend" aria-hidden="true">
      <span>Per-chip health · chain {chainId}</span>
      <span className="autotuner-chip-legend-key"><i className="acl-ok" /> ≥85%</span>
      <span className="autotuner-chip-legend-key"><i className="acl-warn" /> &lt;60%</span>
      <span className="autotuner-chip-legend-key"><i className="acl-fail" /> failed</span>
      {(failedCount > 0 || warnCount > 0) && (
        <span className="autotuner-chip-legend-flag">
          {failedCount > 0 && `${failedCount} failed`}
          {failedCount > 0 && warnCount > 0 && ' · '}
          {warnCount > 0 && `${warnCount} low`}
        </span>
      )}
    </div>
    <div
      style={{
        padding: '10px 12px 4px',
        display: 'grid',
        gridTemplateColumns: 'repeat(auto-fill, minmax(80px, 1fr))',
        gap: 6,
      }}
      aria-label={`Per-chip health for chain ${chainId}: ${chips.length} chips, ${failedCount} failed, ${warnCount} low`}
    >
      {chips.map(chip => {
        const color = chipHealthColor(chip);
        const glyph = chipTrendGlyph(chip.trend);
        const trendWord = chip.trend == null
          ? 'stable'
          : chip.trend > 0 ? 'improving' : chip.trend < 0 ? 'degrading' : 'stable';
        return (
          <Tooltip
            key={chip.chip_index}
            content={
              <>
                <b>Chip #{chip.chip_index}</b> — {chip.health_score}% health ({trendWord})
                <br />
                Frequency: {chip.freq_mhz?.toFixed(0) ?? '—'} MHz
                <br />
                Error rate: {chip.error_rate_pct?.toFixed(1) ?? '—'}%
                <br />
                Backoffs: {chip.backoff_count ?? 0}
                {chip.status === 'failed' && (
                  <><br /><span style={{ color: 'var(--red,#EF4444)' }}>This chip is flagged failed.</span></>
                )}
              </>
            }
          >
            <div
              className="autotuner-chip-cell"
              style={{
                borderRadius: 6,
                border: `1px solid ${color}33`,
                background: `${color}11`,
                padding: '5px 6px',
                textAlign: 'center',
                fontSize: '0.68rem',
                cursor: 'help',
              }}
            >
              <div style={{ fontFamily: 'JetBrains Mono, monospace', fontWeight: 700, color, fontSize: '0.7rem' }}>
                {glyph} {chip.health_score}%
              </div>
              <div style={{ color: 'var(--text-dim)', lineHeight: 1.3 }}>
                #{chip.chip_index}
              </div>
              {chip.freq_mhz != null && (
                <div style={{ color: 'var(--text-dim)', fontSize: '0.62rem' }}>
                  {chip.freq_mhz.toFixed(0)} MHz
                </div>
              )}
            </div>
          </Tooltip>
        );
      })}
    </div>
    </>
  );
}

// ── Safe-envelope banner ────────────────────────────────────────────────────
// Shows the effective ceiling from dispatcher_limits if present.
// Never claims target = applied (truth contract).
function SafeEnvelopeBanner({ status }: { status: AutotunerStatusResponse | null }) {
  // STD-A-06 — dispatcher_limits is a per-chain ARRAY (types.ts), not a single
  // object, and the daemon only emits `effective_ceiling_mhz` per chain (there is
  // NO effective_floor_mhz / voltage_ceiling_mv field). The old single-object cast
  // meant the banner never rendered. Read the real array form (same shape as
  // AutotunerCard) and aggregate the real per-chain ceilings — never invent a
  // floor or a voltage envelope the daemon does not report.
  const ceilings = (status?.dispatcher_limits ?? [])
    .map(l => l.effective_ceiling_mhz)
    .filter((v): v is number => typeof v === 'number' && Number.isFinite(v) && v > 0);

  if (ceilings.length === 0) return null;

  const minCeiling = Math.min(...ceilings);
  const maxCeiling = Math.max(...ceilings);

  return (
    <div
      style={{
        display: 'flex',
        gap: 16,
        flexWrap: 'wrap',
        padding: '6px 10px',
        borderRadius: 6,
        background: 'rgba(250,165,0,0.07)',
        border: '1px solid rgba(250,165,0,0.18)',
        fontSize: '0.75rem',
        color: 'var(--text-dim)',
        marginTop: 6,
      }}
      aria-label="Autotuner safe envelope limits"
    >
      <span style={{ fontWeight: 600, color: 'var(--accent)' }}>Safe envelope</span>
      {minCeiling === maxCeiling
        ? <span>Freq ceiling: {maxCeiling.toFixed(0)} MHz</span>
        : <span>Freq ceiling: {minCeiling.toFixed(0)}–{maxCeiling.toFixed(0)} MHz</span>}
    </div>
  );
}

// ── Main panel ──────────────────────────────────────────────────────────────
export function AutoTunerPanel() {
  const [status, setStatus] = useState<AutotunerStatusResponse | null>(null);
  const [stats, setStats] = useState<StatsResponse | null>(null);
  const [systemInfo, setSystemInfo] = useState<SystemInfoResponse | null>(null);
  const [profiles, setProfiles] = useState<SiliconProfileSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [toast, setToast] = useState<ToastState | null>(null);
  const [pendingMode, setPendingMode] = useState<string | null>(null);
  // Per-chain expand state: Set of chain IDs currently expanded for chip drill-in.
  const [expandedChains, setExpandedChains] = useState<Set<number>>(new Set());

  // Poll status + stats every 2s.
  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      try {
        const [s, st, info] = await Promise.all([
          autotunerApi.getStatus(),
          api.getStats(),
          systemInfo ? Promise.resolve(systemInfo) : api.getSystemInfo(),
        ]);
        if (cancelled) return;
        setStatus(s);
        setStats(st);
        if (!systemInfo) setSystemInfo(info);
        setError(null);
      } catch (e) {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : 'autotuner status fetch failed');
      }
    };
    void poll();
    const id = window.setInterval(poll, POLL_INTERVAL_MS);
    return () => { cancelled = true; window.clearInterval(id); };
  }, [systemInfo]);

  // Load profile list once on mount + after a setActive call.
  const reloadProfiles = async () => {
    try {
      const list = await siliconProfilesApi.list();
      setProfiles(list);
    } catch (e) {
      setProfiles([]);
      setError(e instanceof Error ? e.message : 'profile list fetch failed');
    }
  };
  useEffect(() => { void reloadProfiles(); }, []);

  // Auto-clear toast after 4s.
  useEffect(() => {
    if (!toast) return;
    const id = window.setTimeout(() => setToast(null), 4000);
    return () => window.clearTimeout(id);
  }, [toast]);

  const handleMode = async (label: string, fn: () => Promise<{ runtime?: { status?: string; message?: string } }>) => {
    setPendingMode(label);
    try {
      const res = await fn();
      const ack = res.runtime?.status ?? 'persisted';
      const kind: ToastState['kind'] =
        ack === 'applied' ? 'ok' :
        ack === 'deferred' ? 'warn' :
        ack === 'rejected' ? 'warn' :
        'ok';
      setToast({ kind, text: `${label}: ${ack}${res.runtime?.message ? ' — ' + res.runtime.message : ''}` });
    } catch (e) {
      setToast({ kind: 'error', text: e instanceof Error ? e.message : `${label} failed` });
    } finally {
      setPendingMode(null);
    }
  };

  const handleProfileChange = async (model: string, hashboard: string, profileId: string) => {
    if (!profileId) return;
    try {
      const r = await siliconProfilesApi.setActive(model, hashboard, profileId);
      setToast({ kind: 'ok', text: r.note ?? `Profile applied to ${hashboard}` });
      void reloadProfiles();
    } catch (e) {
      setToast({ kind: 'error', text: e instanceof Error ? e.message : 'setActive failed' });
    }
  };

  const toggleChainExpand = (chainId: number) => {
    setExpandedChains(prev => {
      const next = new Set(prev);
      if (next.has(chainId)) {
        next.delete(chainId);
      } else {
        next.add(chainId);
      }
      return next;
    });
  };

  const minerModel = systemInfo?.model ?? '';
  const hbType = systemInfo?.hardware?.hb_type ?? '';

  const chainRows: ChainProfileRow[] = useMemo(() => {
    const chains = stats?.chains ?? [];
    return chains.map(chain => ({
      chain,
      matchingProfiles: profiles.filter(p => p.hashboard === hbType),
    }));
  }, [stats, profiles, hbType]);

  // Same phantom-field class as the AutoTunerPage fix (STD-A-05): `transitions` is
  // NOT a field on AutotunerStatusResponse, so the old cast made "stable since"
  // permanently read "No transitions yet" and the transition count never rendered.
  // Derive the settled state from the REAL daemon phase instead.
  const isConverging = !!status && (
    status.phase === 'characterizing'
    || status.phase === 'verifying'
    || status.phase === 'background_adjust'
    || status.phase === 'thermal_refinement'
  );
  const isTuned = !!status && (status.phase === 'tuned' || status.phase === 'partially_tuned');
  const stable = isTuned ? formatStableSince(status?.last_update_s ?? 0) : 'Idle';

  return (
    <div className="page-content" style={{ padding: 0 }} data-testid="autotuner-panel">
      {/* The page title + description are rendered by StandardDashboard's
          per-page header (page-title / page-desc) — no inner <h2> here, or
          it double-renders "Autotuner" twice. */}
      {error && (
        <div role="alert" className="autotuner-panel-error-banner" data-testid="autotuner-error">
          {error}
        </div>
      )}

      {/* Section 1: Mode controls */}
      <div className="section ds-glass-card autotuner-panel-section" data-testid="autotuner-mode-section">
        <div style={{ display: 'flex', gap: 16, flexWrap: 'wrap', alignItems: 'flex-start' }}>
          <div style={{ flex: 1, minWidth: 240 }}>
            <div className="autotuner-panel-eyebrow">
              {isConverging && <span className="ds-dot-live accent" aria-hidden="true" />}
              <span>Current mode <InfoDot term="autotuner_phase" size={12} /></span>
            </div>
            <div className="autotuner-panel-mode-label" data-testid="autotuner-current-mode">
              {modeDisplay(status)}
            </div>
            <div
              className="autotuner-panel-stable"
              data-testid="autotuner-stable-since"
              data-tooltip={isConverging
                ? 'The tuner is actively stepping frequency/voltage. Early hashrate is unstable and not final — this is expected and normal across all firmware.'
                : undefined}
            >
              {isConverging ? 'Converging…' : stable}
            </div>
            <SafeEnvelopeBanner status={status} />
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8, minWidth: 240 }}>
            <div style={{ display: 'flex', gap: 8 }}>
              <button
                className="ds-btn ds-btn--ghost ds-btn--sm autotuner-panel-step-btn"
                onClick={() => handleMode('Hashrate ↑', autotunerApi.incrementHashrate)}
                disabled={pendingMode !== null}
                data-testid="autotuner-hashrate-up"
              >
                Hashrate ↑
              </button>
              <button
                className="ds-btn ds-btn--ghost ds-btn--sm autotuner-panel-step-btn"
                onClick={() => handleMode('Hashrate ↓', autotunerApi.decrementHashrate)}
                disabled={pendingMode !== null}
                data-testid="autotuner-hashrate-down"
              >
                Hashrate ↓
              </button>
            </div>
            <div style={{ display: 'flex', gap: 8 }}>
              <button
                className="ds-btn ds-btn--ghost ds-btn--sm autotuner-panel-step-btn"
                onClick={() => handleMode('Power ↑', autotunerApi.incrementPower)}
                disabled={pendingMode !== null}
                data-testid="autotuner-power-up"
              >
                Power ↑
              </button>
              <button
                className="ds-btn ds-btn--ghost ds-btn--sm autotuner-panel-step-btn"
                onClick={() => handleMode('Power ↓', autotunerApi.decrementPower)}
                disabled={pendingMode !== null}
                data-testid="autotuner-power-down"
              >
                Power ↓
              </button>
            </div>
            <button
              className="ds-btn ds-btn--secondary ds-btn--sm"
              onClick={() => handleMode('Reset target', autotunerApi.setDefaultHashrateTarget)}
              disabled={pendingMode !== null}
              data-testid="autotuner-reset"
            >
              Reset to default
            </button>
            <button
              className="ds-btn ds-btn--ghost ds-btn--sm"
              onClick={async () => {
                try {
                  const name = await autotunerApi.downloadTelemetryCsv();
                  if (name === null) {
                    setToast({ kind: 'warn', text: 'No tuning telemetry to export yet — run autotuning first' });
                  } else {
                    setToast({ kind: 'ok', text: `Telemetry downloaded (${name})` });
                  }
                } catch (e) {
                  setToast({ kind: 'error', text: e instanceof Error ? e.message : 'CSV export failed' });
                }
              }}
              data-testid="autotuner-telemetry-csv"
              title="Download the last tuning run's per-iteration telemetry as CSV"
            >
              Download telemetry CSV
            </button>
          </div>
        </div>
      </div>

      {/* Section 2: Per-chain table with collapsible chip drill-in */}
      <div className="section ds-glass-card autotuner-panel-section" data-testid="autotuner-chains-section">
        <div className="autotuner-panel-section-title">Per-chain status</div>
        {chainRows.length === 0 && (
          <div style={{ color: 'var(--text-dim)', fontSize: '0.82rem' }}>
            No chains reporting. Daemon may be starting, or no hashboards detected.
          </div>
        )}
        {chainRows.map(row => {
          const chain = row.chain;
          const icon = chainStatusIcon(chain, status);
          const isExpanded = expandedChains.has(chain.id);
          return (
            <div
              key={chain.id}
              className="autotuner-panel-chain-group"
              data-testid={`autotuner-chain-${chain.id}`}
              data-chain-id={chain.id}
            >
              {/* Chain summary row */}
              <div className="autotuner-panel-chain-row">
                <div style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 90 }}>
                  <StatusIcon state={icon} />
                  <span style={{ fontWeight: 700 }}>Chain {chain.id}</span>
                </div>
                <div style={{ minWidth: 80, color: 'var(--text-dim)', fontSize: '0.82rem' }}>
                  {chain.chips} chips
                </div>
                <div
                  style={{ minWidth: 110, fontFamily: 'JetBrains Mono, monospace', fontSize: '0.85rem' }}
                  data-testid={`chain-${chain.id}-freq`}
                >
                  {formatFreq(chain.frequency_mhz)}
                </div>
                <div
                  style={{ minWidth: 90, fontFamily: 'JetBrains Mono, monospace', fontSize: '0.85rem' }}
                  data-testid={`chain-${chain.id}-voltage`}
                >
                  {formatVoltage(chain.voltage_v)}
                </div>
                <div style={{ flex: 1, minWidth: 140 }}>
                  <select
                    className="autotuner-panel-select"
                    value=""
                    disabled={row.matchingProfiles.length === 0}
                    aria-label={`Apply imported profile to chain ${chain.id}`}
                    onChange={e => {
                      if (e.target.value) {
                        void handleProfileChange(minerModel, hbType, e.target.value);
                        e.target.value = '';
                      }
                    }}
                    data-testid={`chain-${chain.id}-profile-select`}
                  >
                    <option value="" disabled>
                      {row.matchingProfiles.length === 0 ? 'No profiles imported' : 'Apply profile…'}
                    </option>
                    {row.matchingProfiles.map(p => (
                      <option key={p.id} value={p.id}>
                        {p.chip} / {p.source_class} ({p.preset_count} steps)
                      </option>
                    ))}
                  </select>
                </div>
                {/* Per-chip drill-in toggle */}
                <Tooltip content={isExpanded
                  ? 'Hide the per-chip health grid for this chain.'
                  : 'Show per-chip health: each chip’s health score, frequency, error rate and trend. Spotting a hot or dead chip is the premium diagnostic no board-level firmware exposes.'}>
                  <button
                    className="ds-btn ds-btn--icon ds-btn--sm autotuner-panel-chip-toggle"
                    onClick={() => toggleChainExpand(chain.id)}
                    aria-expanded={isExpanded}
                    aria-controls={`chip-drill-${chain.id}`}
                    aria-label={isExpanded ? 'Hide per-chip detail' : 'Show per-chip detail'}
                  >
                    {isExpanded ? '▲' : '▼'}
                  </button>
                </Tooltip>
              </div>

              {/* Collapsible per-chip drill-in */}
              {isExpanded && (
                <div
                  id={`chip-drill-${chain.id}`}
                  className="autotuner-panel-chip-drill"
                  aria-label={`Per-chip health for chain ${chain.id}`}
                >
                  <ChipHealthDrillIn chainId={chain.id} />
                </div>
              )}
            </div>
          );
        })}
      </div>

      {/* Toast — error uses role=alert (assertive), ok/warn use role=status
          (polite) so VoiceOver/NVDA announce them appropriately. M-03
          (Wave-6): visual grammar converged to the canonical common/Toast
          (glass + border-left tone stripe + circular glyph badge +
          ds-slideUp). It stays a local-state toast (the store is frozen and
          this is a panel-scoped, non-dismissible status), but is now
          pixel-faithful to the canonical Toast tone vocabulary. */}
      {toast && (
        <div
          role={toast.kind === 'error' ? 'alert' : 'status'}
          aria-live={toast.kind === 'error' ? 'assertive' : 'polite'}
          aria-atomic="true"
          style={toastStyle(toast.kind)}
          data-testid={`autotuner-toast-${toast.kind}`}
        >
          <span aria-hidden="true" style={toastGlyphStyle(toast.kind)}>
            {toast.kind === 'ok' ? '✓' : toast.kind === 'error' ? '✕' : '!'}
          </span>
          <span style={{ flex: 1, minWidth: 0 }}>{toast.text}</span>
        </div>
      )}
    </div>
  );
}

// The cardStyle / btnStyle / resetBtnStyle / chainRowStyle / selectStyle inline
// style objects have been migrated to CSS classes in standard.css:
//   .ds-glass-card                    — replaces cardStyle
//   .autotuner-panel-section          — padding/margin
//   .autotuner-panel-error-banner     — replaces errorBanner
//   .ds-btn / .ds-btn--ghost / .ds-btn--secondary / .ds-btn--sm
//                                     — replaces btnStyle / resetBtnStyle
//   .autotuner-panel-chain-row        — replaces chainRowStyle
//   .autotuner-panel-select           — replaces selectStyle
//
// These classes are defined below (injected via a <style> block) so they are
// co-located with the component and don't require a separate CSS file edit.
// They are scoped under .autotuner-panel-* to avoid global collisions.

// M-03 (): converged to the canonical common/Toast `ToastItem` visual
// grammar — glass bg, 1px glass border + 3px left tone stripe, --radius-sm,
// blur(14px) saturate, --elevation-overlay, ds-slideUp entrance. Tone tokens
// match the canonical Toast (success/warning/error) so the autotuner toast
// is pixel-faithful to every other toast. ok→success, warn→warning,
// error→error.
const TOAST_TONE: Record<ToastState['kind'], string> = {
  ok: 'var(--green, #22C55E)',
  warn: 'var(--yellow, #EAB308)',
  error: 'var(--red, #EF4444)',
};

function toastStyle(kind: ToastState['kind']): React.CSSProperties {
  const tone = TOAST_TONE[kind];
  return {
    position: 'fixed',
    right: 24,
    bottom: 24,
    maxWidth: 400,
    zIndex: 10000,
    display: 'flex',
    alignItems: 'center',
    gap: 12,
    padding: '10px 16px',
    background: 'rgba(20, 20, 31, 0.82)',
    border: '1px solid var(--border-glass, rgba(255,255,255,0.08))',
    borderLeft: `3px solid ${tone}`,
    borderRadius: 'var(--radius-sm, 8px)',
    color: 'var(--fg-primary, #f0f0f0)',
    fontSize: '0.85rem',
    fontWeight: 500,
    backdropFilter: 'blur(14px) saturate(1.15)',
    WebkitBackdropFilter: 'blur(14px) saturate(1.15)',
    boxShadow: 'var(--elevation-overlay, 0 8px 32px rgba(0,0,0,0.4))',
    animation: 'ds-slideUp 0.22s var(--ease-standard, cubic-bezier(.2,0,0,1)) both',
    fontVariantNumeric: 'tabular-nums',
  };
}

function toastGlyphStyle(kind: ToastState['kind']): React.CSSProperties {
  const tone = TOAST_TONE[kind];
  return {
    display: 'inline-flex',
    alignItems: 'center',
    justifyContent: 'center',
    width: 22,
    height: 22,
    flexShrink: 0,
    borderRadius: '50%',
    background: `${tone === TOAST_TONE.ok ? 'rgba(34,197,94,0.12)' : tone === TOAST_TONE.error ? 'rgba(239,68,68,0.12)' : 'rgba(234,179,8,0.12)'}`,
    color: tone,
    fontSize: '0.78rem',
    fontWeight: 800,
    boxShadow: `0 0 8px ${tone === TOAST_TONE.ok ? 'rgba(34,197,94,0.2)' : tone === TOAST_TONE.error ? 'rgba(239,68,68,0.2)' : 'rgba(234,179,8,0.2)'}`,
  };
}
