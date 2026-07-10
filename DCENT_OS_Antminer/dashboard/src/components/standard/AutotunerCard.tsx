import React, { useEffect, useRef, useState } from 'react';

import { useMinerStore } from '../../store/miner';
import { InfoDot } from '../common/Tooltip';
import type { CanonicalTunerMode } from '../../utils/autotunerModes';

/**
 * COMP-AUTOTUNER contract surface (DCENT Design Language —
 * component-contract.md §8). This `AutotunerCardProps` type ADVERTISES the §8
 * contract surface so the OS AutotunerCard is contract-legible against axe's
 * emission; it is a pure type advertisement — the render is unchanged (the
 * 4-step PHASE_RIBBON ladder + STATE_LABELS + the honest `ribbonIndexFor(-1)`
 * non-ladder index below ARE the contract emission).
 *
 * §8 contract props (substrate-neutral):
 *   { mode: CanonicalTunerMode, state: TunerState, phaseRibbon, effJth, freq,
 *     chipsTuned/Total, grades, limitingFactor, safetyOverride, healthBackoff }
 *
 * `mode` reuses the canonical 4-mode set from utils/autotunerModes.ts
 * (`max_hashrate | best_efficiency | target_watts | target_temp`). OS-only
 * policy presets (Quiet/Balanced/Advanced + the formatPolicyValue synonym set)
 * stay `[OS-only]` and are NOT in the shared `CanonicalTunerMode` enum (§8
 * KEEP-UNIQUE). The per-chip A/B/C/D grade GRID is `[OS-only]`; only the
 * derived-grade `grades` VOCABULARY is shared (axe MAY omit it).
 */
export type AutotunerTunerState =
  | 'idle'
  | 'characterizing'
  | 'verifying'
  | 'thermal_refinement'
  | 'tuned'
  | 'partially_tuned'
  | 'failed'
  | 'background_adjust';

export interface AutotunerCardProps {
  /** Canonical 4-mode set (utils/autotunerModes.ts). */
  mode: CanonicalTunerMode;
  /** The 8 daemon TunerState variants (dcentrald-autotuner tuner.rs:61-79). */
  state: AutotunerTunerState;
  /** The rendered 4-step ladder (characterizing→verifying→thermal_refinement→tuned). */
  phaseRibbon: ReadonlyArray<{ key: string; label: string }>;
  /** Tuner-estimated J/TH; null ⇒ "--" (never a fabricated 0). */
  effJth?: number | null;
  freq?: number | null;
  chipsTuned?: number;
  chipsTotal?: number;
  /** OS-richer per-chip grade distribution; axe MAY omit (GRID UI is [OS-only]). */
  grades?: { a: number; b: number; c: number; d: number } | null;
  limitingFactor?: string | null;
  safetyOverride?: string | null;
  /** The shared bidirectional health-backoff channel (axe↔OS) — formalized. */
  healthBackoff?: boolean;
}

const STATE_COLORS: Record<string, string> = {
  disabled: 'var(--text-dim)',
  idle: 'var(--text-dim)',
  waiting: 'var(--text-dim)',
  characterizing: 'var(--blue)',
  verifying: 'var(--yellow)',
  thermal_refinement: 'var(--accent)',
  tuned: 'var(--green)',
  partially_tuned: 'var(--yellow)',
  failed: 'var(--red)',
  background_adjust: 'var(--blue-lighter)',
};

const STATE_LABELS: Record<string, string> = {
  disabled: 'Disabled',
  idle: 'Idle',
  waiting: 'Waiting',
  characterizing: 'Characterizing',
  verifying: 'Verifying',
  thermal_refinement: 'Thermal Soak',
  tuned: 'Tuned',
  partially_tuned: 'Partial Success',
  failed: 'Failed',
  background_adjust: 'Adjusting',
};

function normalizeState(value?: string): string {
  return (value || 'disabled').replace(/([a-z])([A-Z])/g, '$1_$2').toLowerCase();
}

/* Kit AutotunerCard.jsx signature phase ribbon: a canonical 4-step ladder
   (Characterizing → Verifying → Thermal Soak → Tuned). The kit auto-cycles a
   demo; production maps the REAL daemon state onto the ladder. Non-ladder
   states (idle/disabled/failed/background_adjust) report an honest ribbon
   index (failed → -1 so no step is "now"; tuned → last).

   COMP-AUTOTUNER §8 contract emission: this 4-step ladder IS the §8
   `phaseRibbon` (characterizing → verifying → thermal_refinement → tuned), and
   `ribbonIndexFor` below IS the §8 honest off-ladder projection — failed/idle/
   disabled/waiting ⇒ -1 (no "now" step; the whole ribbon reads pending —
   `failed` NEVER shows a fake "now"), partially_tuned/background_adjust ⇒ last.
   No render change this pass; the existing ladder already matches the §8 rule. */
const PHASE_RIBBON = [
  { key: 'characterizing', label: 'Characterizing', color: 'var(--blue, #3B82F6)' },
  { key: 'verifying', label: 'Verifying', color: 'var(--yellow, #EAB308)' },
  { key: 'thermal_refinement', label: 'Thermal Soak', color: 'var(--accent, #FAA500)' },
  { key: 'tuned', label: 'Tuned', color: 'var(--green, #22C55E)' },
] as const;

function ribbonIndexFor(state: string): number {
  switch (state) {
    case 'characterizing':
      return 0;
    case 'verifying':
      return 1;
    case 'thermal_refinement':
      return 2;
    case 'tuned':
    case 'partially_tuned':
    case 'background_adjust':
      return 3;
    default:
      // disabled / idle / waiting / failed → ribbon shows all pending.
      return -1;
  }
}

function formatEta(seconds?: number | null): string {
  if (!seconds || seconds <= 0) return 'estimating';
  if (seconds < 60) return `~${Math.round(seconds)}s`;
  const m = Math.floor(seconds / 60);
  const s = Math.round(seconds % 60);
  return `~${m}m ${s}s`;
}

function formatPolicyValue(value?: string | null): string | null {
  if (!value) return null;
  switch (value) {
    case 'quiet_home':
      return 'Quiet Home';
    case 'balanced_home':
      return 'Balanced Home';
    case 'efficiency_max':
      return 'Efficiency Max';
    case 'hashrate_max':
      return 'Hashrate Max';
    case 'watt_cap':
      return 'Watt Cap';
    case 'heat':
      return 'Heat / Space Heater';
    case 'night_quiet':
      return 'Night Quiet';
    case 'offgrid':
      return 'Off-Grid / Direct-DC';
    case 'advanced_manual':
      return 'Advanced Manual';
    case 'quiet':
      return 'Quiet';
    case 'balanced':
      return 'Balanced';
    case 'efficiency':
      return 'Use less power';
    case 'hashrate':
      return 'More heat';
    case 'power_cap':
      return 'Stay within power limit';
    case 'thermal':
      return 'Thermal protection';
    case 'hashrate_target':
      return 'Hold target hashrate';
    case 'manual':
      return 'Manual tuning';
    case 'quiet_mode':
      return 'Quiet mode';
    case 'off_grid':
      return 'Off-grid battery protection';
    case 'fan_clamp':
      return 'Fan/noise limit';
    case 'sensor_safety':
      return 'Sensor safety';
    case 'missing_temperature':
      return 'Missing temperature';
    default:
      return value
        .split('_')
        .map(part => part.charAt(0).toUpperCase() + part.slice(1))
        .join(' ');
  }
}

function GradeBar({ label, count, total, color }: {
  label: string; count: number; total: number; color: string;
}) {
  const pct = total > 0 ? (count / total * 100) : 0;
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 4 }}>
      <span style={{
        fontFamily: "'JetBrains Mono', monospace", fontWeight: 700,
        fontSize: '0.75rem', color, width: 16, textAlign: 'center',
      }}>
        {label}
      </span>
      <div style={{
        flex: 1, height: 8, borderRadius: 4,
        background: 'var(--border)', overflow: 'hidden',
      }}>
        <div style={{
          width: `${pct}%`, height: '100%', borderRadius: 4,
          background: color, transition: 'width 0.5s ease',
        }} />
      </div>
      <span style={{
        fontFamily: "'JetBrains Mono', monospace",
        fontSize: '0.7rem', color: 'var(--text-dim)', width: 28, textAlign: 'right',
      }}>
        {count}
      </span>
    </div>
  );
}

export function AutotunerCard() {
  const status = useMinerStore(s => s.autotunerStatus);
  // Per-chain freq/voltage for the kit ramp rails come from REAL miner
  // telemetry (status.chains) — never the autotuner DTO (which has no
  // per-chain freq/voltage). No chains telemetry ⇒ no rails rendered.
  const minerChains = useMinerStore(s => s.status?.chains ?? []);
  // P3-7: route the disabled-state CTA to the operator-facing autotuner page.
  const setCurrentPage = useMinerStore(s => s.setCurrentPage);

  //  (Agent W3): flash the completedChips counter when a new
  // convergence step lands. Track via last_update_s changes — never
  // fabricate; only flash when the daemon actually advanced.
  const [stepTick, setStepTick] = useState(0);
  const lastUpdateRef = useRef<number>(0);
  useEffect(() => {
    const lu = status?.last_update_s ?? 0;
    if (lu > 0 && lu !== lastUpdateRef.current) {
      lastUpdateRef.current = lu;
      setStepTick(t => t + 1);
    }
  }, [status?.last_update_s]);

  if (!status) return null;

  // P3-7: when the autotuner is OFF, collapse the whole card down to a single
  // honest "Autotuner OFF — Enable" CTA instead of rendering a full card of
  // "—" / "Unavailable" placeholders that read like a broken tuner. `enabled`
  // is the daemon's authoritative on/off flag (idle/waiting stay the full card
  // because those are enabled-but-not-running states).
  if (status.enabled === false) {
    return (
      <section className="section" style={{ marginBottom: 16 }}>
        <div
          className="ds-glass-card"
          style={{
            padding: 16, display: 'flex', alignItems: 'center',
            justifyContent: 'space-between', gap: 16, flexWrap: 'wrap',
          }}
        >
          <div style={{ minWidth: 0 }}>
            <div className="page-hero-eyebrow">AUTOTUNER</div>
            <div style={{ fontSize: '1.05rem', fontWeight: 700, color: 'var(--text)', marginTop: 2 }}>
              Autotuner OFF
            </div>
            <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)', marginTop: 4, maxWidth: 520 }}>
              The miner is running at its configured fixed frequency and voltage.
              Enable the autotuner to optimize efficiency and per-chip health automatically.
            </div>
          </div>
          <button
            type="button"
            className="ds-btn primary"
            onClick={() => setCurrentPage('autotuner')}
          >
            Enable
          </button>
        </div>
      </section>
    );
  }

  const state = normalizeState(status.phase || status.state);
  const color = STATE_COLORS[state] || 'var(--text-dim)';
  const label = STATE_LABELS[state] || status.phase || status.state;
  const isActive = ['characterizing', 'verifying', 'thermal_refinement', 'background_adjust'].includes(state);
  const grades = status.silicon_grades;
  const gradeTotal = grades ? grades.a + grades.b + grades.c + grades.d : 0;
  const ageS = status.last_update_s > 0
    ? Math.max(0, Math.floor(Date.now() / 1000) - status.last_update_s)
    : status.age_s;
  const isBootstrap = status.source === 'runtime_bootstrap';
  const isStale = status.stale || (!isBootstrap && ageS > 15);
  const runtimeLabel = isStale
    ? `Stale (${ageS}s old)`
    : status.live_runtime
      ? 'Live runtime'
      : isBootstrap
        ? 'Preparing runtime'
        : 'Not live';
  const requestedPresetRaw = status.policy?.requested_preset ?? null;
  const requestedPreset = status.policy?.requested_preset_display_name || formatPolicyValue(requestedPresetRaw);
  const effectivePresetRaw = status.policy?.effective_preset ?? null;
  const effectivePreset = status.policy?.effective_preset_display_name || formatPolicyValue(effectivePresetRaw);
  const requestedPresetSupported = status.policy?.requested_preset_supported;
  const presetReason = formatPolicyValue(status.policy?.requested_preset_reason);
  const degradedPreset = status.policy?.degraded_from_requested === true;
  const activeObjective = !isStale ? formatPolicyValue(status.policy?.active_objective) : null;
  const limitingFactor = !isStale ? formatPolicyValue(status.policy?.active_limiting_factor) : null;
  const safetyOverride = !isStale ? formatPolicyValue(status.policy?.safety_override) : null;
  // Daemon / preview can return a partial autotuner shape — coalesce the
  // numeric counters so the UI shows "0" rather than "undefined/undefined".
  const completedChips = status.completed_chips ?? 0;
  const totalChips = status.total_chips ?? 0;
  const tunedChains = status.tuned_chains ?? 0;
  const targetChains = status.target_chains ?? 0;
  const failedChains = status.failed_chains ?? 0;
  const progressLabel = status.active_chain_id != null && status.active_chain_total_chips != null
    ? `Chain ${status.active_chain_id}: ${status.active_chips ?? 0}/${status.active_chain_total_chips} chips active`
    : `${completedChips}/${totalChips} chips complete`;

  // ── Kit signature surfaces (honest data only) ─────────────────────────
  const ribbonIdx = ribbonIndexFor(state);
  // Per-chain target ceiling from the real dispatcher limits; fall back to
  // the real avg frequency. Never invent a target.
  const ceilingForChain = (chainId: number): number | null => {
    const lim = status.dispatcher_limits?.find(l => l.chain_id === chainId);
    const v = lim?.effective_ceiling_mhz;
    return typeof v === 'number' && Number.isFinite(v) && v > 0 ? v : null;
  };
  const avgFreq = typeof status.avg_frequency_mhz === 'number' && status.avg_frequency_mhz > 0
    ? status.avg_frequency_mhz
    : null;
  // Voltage target = highest real per-chain voltage observed (a measured
  // reference, not a fabricated setpoint). Null when no telemetry.
  const maxChainVoltageMv = minerChains.reduce(
    (m, c) => Math.max(m, c.voltage_mv > 0 ? c.voltage_mv : 0), 0,
  );

  return (
    <>
      <div className="page-hero-strip">
        <div className="page-hero-summary">
          <div
            className="page-hero-eyebrow"
            style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}
          >
            {isActive && <span className="ds-dot-live accent" aria-hidden="true" />}
            <span>AUTOTUNER</span>
          </div>
          <div className="page-hero-title">
            Runtime <InfoDot term="autotuner_phase" size={12} />
          </div>
          <div className="page-hero-stat">{label}</div>
          <div className="page-hero-substat">
            {effectivePreset || requestedPreset || 'No preset selected'}
            {' · '}{progressLabel}
          </div>
        </div>
        <div className="hero-kpi-strip">
          <div className="hero-kpi">
            <div className="kpi-label">Frequency</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {typeof status.avg_frequency_mhz === 'number' && status.avg_frequency_mhz > 0
                  ? `${status.avg_frequency_mhz.toFixed(0)} MHz`
                  : '—'}
              </span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">
              Chips Tuned <InfoDot term="autotuner_transitions" size={12} />
            </div>
            <div className="kpi-value">
              <span
                key={stepTick}
                className={`kpi-num-anim${isActive ? ' ds-value-flash' : ''}`}
              >
                {completedChips}/{totalChips}
              </span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">
              Tuned Chains <InfoDot term="autotuner_convergence" size={12} />
            </div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{tunedChains}/{targetChains}</span>
            </div>
            {failedChains > 0 && (
              <div className="kpi-sub">{failedChains} failed</div>
            )}
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Est. J/TH <InfoDot term="efficiency_jth" size={12} /></div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {typeof status.efficiency_jth === 'number' && status.efficiency_jth > 0
                  ? status.efficiency_jth.toFixed(1)
                  : '—'}
              </span>
            </div>
          </div>
        </div>
      </div>

    <section className="section" style={{ marginBottom: 16 }}>
      <div className="ds-glass-card" style={{ padding: 16 }}>
        <div style={{
          display: 'flex', justifyContent: 'space-between', gap: 12,
          marginBottom: 12, fontSize: '0.72rem', color: 'var(--text-dim)',
          flexWrap: 'wrap',
        }}>
          <span>Source: {status.source}</span>
          <span>{runtimeLabel}</span>
          {/* Wave-13: removed the "Chains: N/M tuned" span — the hero "Tuned
              Chains" KPI tile already shows it. Failed-chain count surfaces
              below in the per-chain cards. */}
        </div>

        {(requestedPreset || effectivePreset || activeObjective || limitingFactor || safetyOverride || presetReason) && (
          <div className="autotuner-card-policy-grid">
            {requestedPreset && (
              <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, fontSize: '0.8rem' }}>
                <span style={{ color: 'var(--text-dim)' }}>Requested preset</span>
                <span style={{ color: 'var(--text)', fontWeight: 600, textAlign: 'right' }}>
                  {requestedPreset}
                </span>
              </div>
            )}
            {effectivePreset && (
              <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, fontSize: '0.8rem' }}>
                <span style={{ color: 'var(--text-dim)' }}>Effective preset</span>
                <span style={{ color: degradedPreset ? 'var(--yellow)' : 'var(--text)', fontWeight: 600, textAlign: 'right' }}>
                  {effectivePreset}
                </span>
              </div>
            )}
            {presetReason && (
              <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, fontSize: '0.8rem' }}>
                <span style={{ color: 'var(--text-dim)' }}>Preset resolution</span>
                <span style={{ color: degradedPreset ? 'var(--yellow)' : 'var(--text)', fontWeight: 600, textAlign: 'right' }}>
                  {presetReason}
                </span>
              </div>
            )}
            {activeObjective && (
              <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, fontSize: '0.8rem' }}>
                <span style={{ color: 'var(--text-dim)' }}>Optimizing for</span>
                <span style={{ color: 'var(--text)', fontWeight: 600, textAlign: 'right' }}>{activeObjective}</span>
              </div>
            )}
            {limitingFactor && (
              <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, fontSize: '0.8rem' }}>
                <span
                  style={{ color: 'var(--text-dim)' }}
                  data-tooltip="The constraint currently capping the tuner — e.g. a thermal ceiling, a power cap, or a fan/noise limit. It is why the tuner is not pushing further; not an error."
                >
                  Held back by
                </span>
                <span style={{ color: 'var(--text)', fontWeight: 600, textAlign: 'right' }}>{limitingFactor}</span>
              </div>
            )}
            {safetyOverride && (
              <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, fontSize: '0.8rem' }}>
                <span
                  style={{ color: 'var(--text-dim)' }}
                  data-tooltip="A safety rule has overridden the requested tuning (e.g. missing temperature data, sensor safety, or the home fan/noise clamp). The miner stays safe — this is the protection working, not a fault."
                >
                  Safety override
                </span>
                <span style={{ color: 'var(--red)', fontWeight: 700, textAlign: 'right' }}>{safetyOverride}</span>
              </div>
            )}
          </div>
        )}

        {/* Kit signature: 4-step phase ribbon (AutotunerCard.jsx ribbon).
            Driven by the REAL daemon state; failed/idle ⇒ all pending. */}
        <div className="atc-ribbon" role="list" aria-label="Autotuner phase progress">
          {PHASE_RIBBON.map((p, i) => {
            const cls = ribbonIdx < 0
              ? 'pending'
              : i < ribbonIdx ? 'done' : i === ribbonIdx ? 'now' : 'pending';
            return (
              <div key={p.key} className={`atc-ribbon-step ${cls}`} role="listitem">
                <span
                  className="atc-ribbon-dot"
                  aria-hidden="true"
                  style={{ background: ribbonIdx >= 0 && i <= ribbonIdx ? p.color : 'rgba(255,255,255,.1)' }}
                />
                <span>{p.label}</span>
              </div>
            );
          })}
        </div>

        {/* Kit signature: per-chain FREQ/VOLT ramp rails with target markers.
            All values are REAL miner telemetry; rendered only when present. */}
        {minerChains.length > 0 && (
          <div className="atc-chains">
            {minerChains.map(chain => {
              const targetFreq = ceilingForChain(chain.id) ?? avgFreq ?? null;
              const fPct = targetFreq && chain.frequency_mhz > 0
                ? Math.max(0, Math.min(1, chain.frequency_mhz / targetFreq))
                : 0;
              const targetVoltMv = maxChainVoltageMv > 0 ? maxChainVoltageMv : null;
              const vPct = targetVoltMv && chain.voltage_mv > 0
                ? Math.max(0, Math.min(1, chain.voltage_mv / targetVoltMv))
                : 0;
              const fLabel = chain.frequency_mhz > 0 ? `${chain.frequency_mhz.toFixed(0)} MHz` : '—';
              const vLabel = chain.voltage_mv > 0 ? `${(chain.voltage_mv / 1000).toFixed(2)} V` : '—';
              return (
                <div key={chain.id} className="atc-chain">
                  <div className="atc-chain-head">
                    <span className="atc-chain-id">CH{chain.id}</span>
                    <span className="atc-chain-detail">{fLabel} @ {vLabel}</span>
                  </div>
                  <div className="atc-chain-rails">
                    <div className="atc-rail">
                      <span className="atc-rail-label">FREQ</span>
                      <div className="atc-rail-track">
                        <div
                          className="atc-rail-fill"
                          style={{ width: `${fPct * 100}%`, background: 'var(--accent-gradient)' }}
                        />
                        {targetFreq != null && <div className="atc-rail-target" style={{ left: '100%' }} />}
                      </div>
                      <span className="atc-rail-target-val">
                        {targetFreq != null ? targetFreq.toFixed(0) : '—'}
                      </span>
                    </div>
                    <div className="atc-rail">
                      <span className="atc-rail-label">VOLT</span>
                      <div className="atc-rail-track">
                        <div
                          className="atc-rail-fill"
                          style={{ width: `${vPct * 100}%`, background: 'linear-gradient(90deg, var(--blue, #3B82F6), var(--accent, #FAA500))' }}
                        />
                        {targetVoltMv != null && <div className="atc-rail-target" style={{ left: '100%' }} />}
                      </div>
                      <span className="atc-rail-target-val">
                        {targetVoltMv != null ? (targetVoltMv / 1000).toFixed(2) : '—'}
                      </span>
                    </div>
                  </div>
                  {/* Wave-13: removed the per-chain grade track — it rendered
                      the GLOBAL grade distribution inside every chain card,
                      duplicating the single "Silicon Grades" GradeBar below
                      (and it omitted grade D). The standalone GradeBar is the
                      authoritative grade view. */}
                </div>
              );
            })}
          </div>
        )}

        {isActive && (
          <div style={{ marginBottom: 16 }}>
            <div style={{
              display: 'flex', justifyContent: 'space-between', marginBottom: 6,
              fontSize: '0.75rem', color: 'var(--text-dim)',
            }}>
              <span>{progressLabel}</span>
              <span>{formatEta(status.estimated_remaining_s)}</span>
            </div>
            <div style={{
              height: 10, borderRadius: 5, background: 'var(--border)',
              overflow: 'hidden',
            }}>
              <div style={{
                width: `${status.percent_complete ?? 0}%`, height: '100%', borderRadius: 5,
                background: color, transition: 'width 0.5s ease',
              }} />
            </div>
            <div style={{
              textAlign: 'center', marginTop: 4,
              fontFamily: "'JetBrains Mono', monospace",
              fontSize: '0.85rem', fontWeight: 700, color,
            }}>
              {(status.percent_complete ?? 0).toFixed(0)}%
            </div>
            {/* Wave-13: removed the "{completed}/{total} chips complete" line —
                progressLabel above already says exactly that. */}
          </div>
        )}

        {grades && gradeTotal > 0 && (
          <div style={{ marginBottom: 16 }}>
            <div style={{
              fontSize: '0.7rem', color: 'var(--text-dim)', marginBottom: 8, fontWeight: 600,
            }}>
              Silicon Grades
            </div>
            <GradeBar label="A" count={grades.a} total={gradeTotal} color="var(--green)" />
            <GradeBar label="B" count={grades.b} total={gradeTotal} color="var(--blue-lighter, #60A5FA)" />
            <GradeBar label="C" count={grades.c} total={gradeTotal} color="var(--yellow)" />
            <GradeBar label="D" count={grades.d} total={gradeTotal} color="var(--red)" />
          </div>
        )}

        {/* Wave-13: removed the centered Efficiency / Avg Frequency block —
            both values are already in the hero KPI strip (J/TH + Frequency)
            at the top of this card. */}

        <div style={{
          textAlign: 'center', color: 'var(--text-dim)', fontSize: '0.8rem',
          padding: '8px 0',
        }}>
          {status.message}
        </div>

        {/* Kit signature: live "last step" strip. The value re-keys on
            stepTick — which only increments when last_update_s actually
            changes (a genuine daemon advance), so the flash is honest. */}
        <div className="atc-step-strip">
          <span className="atc-step-label">last step</span>
          <span key={stepTick} className="atc-step-val">{progressLabel}</span>
          <span className="atc-step-time">
            {ageS > 0 ? `${ageS}s ago` : 'just now'}
          </span>
        </div>
      </div>
    </section>
    </>
  );
}
