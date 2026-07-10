// AutoTunerPage — hash route #/autotuner.
//
//  (Agent W3): hero layout above the existing AutoTunerPanel.
//  Finish: premium 2-column glass-card hero with full design-system
// primitives (.ds-glass-card, .ds-dot-live, .ds-chip, .ds-pill-numeric).
//
// Two columns:
//   left  = "Current Profile" — name, freq, voltage, fan PWM, efficiency.
//           State shown truthfully: never claims convergence without evidence.
//   right = "Convergence" — daemon phase, real progress counters (chips tuned /
//           chains tuned), and a stable-for timer derived from last_update_s once
//           the daemon reports a settled phase. Converging shows amber glow + dot.
//
// The hero reads /api/autotuner/status (same source as AutoTunerPanel).
// When status is null we show "Loading…" without fabricating numbers.
// "Stable" is derived from the REAL phase (tuned / partially_tuned), NEVER from a
// phantom transitions counter the daemon does not emit (STD-A-05).
//
// The hero is additive; AutoTunerPanel below is unchanged in contract.

import React, { useEffect, useState } from 'react';
import { autotunerApi } from '../../api/autotuner';
import api from '../../api/client';
import type { AutotunerStatusResponse, StatsResponse } from '../../api/types';
import { AutoTunerPanel } from './AutoTunerPanel';
import { ConvergenceTimeline } from './ConvergenceTimeline';
import { InfoBanner } from '../common/InfoBanner';
import { InfoDot } from '../common/Tooltip';
import { glossary } from '../../utils/glossary';

const HERO_POLL_MS = 4000;

function fmtFreq(mhz: number | null | undefined): string {
  if (mhz == null || mhz <= 0) return '—';
  return `${mhz.toFixed(0)} MHz`;
}

function fmtVolt(volts: number | null | undefined): string {
  if (volts == null || volts <= 0) return '—';
  return `${volts.toFixed(2)} V`;
}

function fmtPwm(pct: number | null | undefined): string {
  if (pct == null || pct < 0) return '—';
  return `${pct.toFixed(0)}%`;
}

function presetDisplay(s: AutotunerStatusResponse | null): string {
  if (!s) return 'Loading…';
  return (
    s.policy?.effective_preset_display_name
    || s.policy?.requested_preset_display_name
    || s.policy?.effective_preset
    || s.policy?.requested_preset
    || s.phase
    || 'Unknown'
  );
}

function phaseDisplay(s: AutotunerStatusResponse | null): string {
  if (!s) return '—';
  return s.phase || s.state || '—';
}

// Stable-for timer derived from the REAL last_update_s timestamp. Shown only once
// the daemon reports a settled phase (tuned / partially_tuned) — see isTuned below.
function stableForText(lastUpdateS: number): string {
  if (!lastUpdateS || lastUpdateS <= 0) return 'Tuned';
  const ageS = Math.max(0, Math.floor(Date.now() / 1000) - lastUpdateS);
  if (ageS < 5) return 'Just converged';
  if (ageS < 60) return `Stable for ${ageS}s`;
  if (ageS < 3600) return `Stable for ${Math.floor(ageS / 60)}m`;
  return `Stable for ${Math.floor(ageS / 3600)}h`;
}

function chainAverageVoltage(stats: StatsResponse | null): number | null {
  const chains = stats?.chains ?? [];
  if (chains.length === 0) return null;
  const sum = chains.reduce((a, c) => a + (c.voltage_v ?? 0), 0);
  const n = chains.filter(c => (c.voltage_v ?? 0) > 0).length;
  return n > 0 ? sum / n : null;
}

function chainAverageFreq(stats: StatsResponse | null, fallback: number | null | undefined): number | null {
  const chains = stats?.chains ?? [];
  if (chains.length === 0) return fallback ?? null;
  const sum = chains.reduce((a, c) => a + (c.frequency_mhz ?? 0), 0);
  const n = chains.filter(c => (c.frequency_mhz ?? 0) > 0).length;
  return n > 0 ? sum / n : (fallback ?? null);
}

export function AutoTunerPage() {
  const [status, setStatus] = useState<AutotunerStatusResponse | null>(null);
  const [stats, setStats] = useState<StatsResponse | null>(null);

  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      try {
        const [s, st] = await Promise.all([
          autotunerApi.getStatus(),
          api.getStats(),
        ]);
        if (cancelled) return;
        setStatus(s);
        setStats(st);
      } catch {
        /* keep last-known-good; AutoTunerPanel surfaces its own error banner */
      }
    };
    void poll();
    const id = window.setInterval(poll, HERO_POLL_MS);
    return () => { cancelled = true; window.clearInterval(id); };
  }, []);

  // STD-A-05 — `transitions` and `last_delta_hashrate_ths` are NOT fields on
  // AutotunerStatusResponse (the daemon never emits them), so reading them via
  // `as unknown` casts produced a permanent 0 / '—' and the chip could never reach
  // 'Stable'. Removed. Convergence state is now derived from the REAL daemon phase
  // and the real progress counters the status response actually carries.
  const phase = phaseDisplay(status);
  const isConverging = !!status && (
    phase === 'characterizing'
    || phase === 'verifying'
    || phase === 'background_adjust'
    || phase === 'thermal_refinement'
  );
  // A settled tuner reports phase 'tuned' / 'partially_tuned' — the honest "Stable"
  // signal (NOT a phantom transitions count).
  const isTuned = !!status && (phase === 'tuned' || phase === 'partially_tuned');
  const stable = isTuned ? stableForText(status?.last_update_s ?? 0) : null;
  const preset = presetDisplay(status);

  const freq = chainAverageFreq(stats, status?.avg_frequency_mhz);
  const volt = chainAverageVoltage(stats);
  // Fan PWM lives on stats.thermal — best-effort, may be null.
  const fanPwm = (stats as unknown as { thermal?: { fan_pwm_pct?: number } } | null)?.thermal?.fan_pwm_pct ?? null;
  const efficiency = status?.efficiency_jth ?? null;

  // Convergence right-card metrics — REAL progress counters from
  // AutotunerStatusResponse (chips tuned / chains tuned), replacing the phantom
  // transitions + last-delta surfaces.
  const completedChips = status?.completed_chips ?? null;
  const totalChips = status?.total_chips ?? null;
  const tunedChains = status?.tuned_chains ?? null;
  const targetChains = status?.target_chains ?? null;

  // Convergence state chip label — truthful: searching ≠ stable ≠ idle.
  const convergenceChipLabel = isConverging ? 'Searching' : (isTuned ? 'Stable' : 'Idle');
  const convergenceChipTone = isConverging ? 'warn' : (isTuned ? 'ok' : 'muted');
  const chipToneStyle: Record<string, React.CSSProperties> = {
    warn: { background: 'rgba(250,165,0,0.14)', color: 'var(--accent)', borderColor: 'rgba(250,165,0,0.3)' },
    ok:   { background: 'rgba(16,185,129,0.10)', color: 'var(--green,#10B981)', borderColor: 'rgba(16,185,129,0.28)' },
    muted:{ background: 'rgba(255,255,255,0.04)', color: 'var(--text-dim)', borderColor: 'rgba(255,255,255,0.08)' },
  };

  // R1 #3 — the single biggest cross-firmware trust gap is the opaque
  // autotuning black box: users panic at the temporary half-rate and the
  // surprise reboots because no firmware tells them it's expected. We show
  // an honest "this is expected" banner the WHOLE time the tuner is
  // actively iterating (never a false convergence claim — W3 contract).
  const expectation = glossary('autotuner_expectation');

  return (
    <div className="page-content" style={{ padding: '0 20px' }}>
      {isConverging && (
        <InfoBanner
          tone="info"
          title={expectation?.term ?? 'Tuning takes time — this is expected'}
          className="autotuner-expectation-banner"
        >
          {expectation?.body ?? (
            'Autotuning legitimately takes hours and the hashrate is unstable '
            + 'while it explores. Early numbers are not final and a tuning '
            + 'reboot can happen. This is normal across all firmware — do not '
            + 'panic at a temporary half-rate.'
          )}
        </InfoBanner>
      )}
      <section
        className="autotuner-hero"
        aria-label="Autotuner current profile and convergence summary"
      >
        {/* ── Current Profile card ── */}
        <div className="autotuner-hero-card ds-glass-card" data-testid="autotuner-hero-profile">
          <div className="autotuner-hero-eyebrow">
            <span>Current profile</span>
            {status && (
              <span
                className="ds-chip"
                style={{ marginLeft: 'auto', fontSize: '0.62rem', padding: '2px 7px' }}
              >
                {status.enabled ? 'Enabled' : 'Disabled'}
              </span>
            )}
          </div>
          <div className="autotuner-hero-stat" data-testid="autotuner-hero-preset">
            {preset}
          </div>
          <div className="autotuner-hero-substat">
            {status?.message || 'Targets shown below.'}
          </div>
          <div className="autotuner-hero-row" style={{ marginTop: 8 }}>
            <div>
              <div className="ahr-label">Freq</div>
              <div className="ahr-value">{fmtFreq(freq)}</div>
            </div>
            <div>
              <div className="ahr-label">Voltage</div>
              <div className="ahr-value">{fmtVolt(volt)}</div>
            </div>
            <div>
              <div className="ahr-label">Fan PWM <InfoDot term="fan_pwm" size={12} /></div>
              <div className="ahr-value">{fmtPwm(fanPwm)}</div>
            </div>
            <div>
              <div className="ahr-label">Est. J/TH <InfoDot term="efficiency_jth" size={12} /></div>
              <div className="ahr-value">
                {typeof efficiency === 'number' && efficiency > 0
                  ? `${efficiency.toFixed(1)}`
                  : '—'}
              </div>
            </div>
          </div>
        </div>

        {/* ── Convergence card ── */}
        <div
          className={`autotuner-hero-card ds-glass-card${isConverging ? ' is-converging' : ''}`}
          data-testid="autotuner-hero-convergence"
        >
          <div className="autotuner-hero-eyebrow">
            {isConverging && <span className="ds-dot-live accent" aria-hidden="true" />}
            <span>Convergence <InfoDot term="autotuner_convergence" size={12} /></span>
            {/* Status chip — "Searching" ≠ "Stable" ≠ "Idle" — truthful state only */}
            <span
              className="ds-chip"
              style={{ marginLeft: 'auto', fontSize: '0.62rem', padding: '2px 7px', ...chipToneStyle[convergenceChipTone] }}
            >
              {convergenceChipLabel}
            </span>
          </div>
          <div className="autotuner-hero-stat" data-testid="autotuner-hero-phase">
            {isConverging ? 'Iterating' : (isTuned ? 'Stable' : 'Idle')}
          </div>
          <div className="autotuner-hero-substat" data-testid="autotuner-hero-stable">
            {isConverging
              ? 'Autotuner is stepping toward target…'
              : isTuned
                ? stable
                : 'Idle — no active tuning'}
          </div>
          <div className="autotuner-hero-row" style={{ marginTop: 8 }}>
            <div>
              <div className="ahr-label">Chips tuned</div>
              <div className="ahr-value" data-testid="autotuner-hero-chips">
                {/* Real progress counter from /api/autotuner/status — never fabricated. */}
                <span className="ds-pill-numeric" style={{ fontSize: '0.88rem' }}>
                  {completedChips != null && totalChips != null && totalChips > 0
                    ? `${completedChips}/${totalChips}`
                    : '—'}
                </span>
              </div>
            </div>
            <div>
              <div className="ahr-label">Chains tuned</div>
              <div className="ahr-value" data-testid="autotuner-hero-chains">
                {tunedChains != null && targetChains != null && targetChains > 0
                  ? `${tunedChains}/${targetChains}`
                  : '—'}
              </div>
            </div>
            <div>
              <div className="ahr-label">Phase <InfoDot term="autotuner_phase" size={12} /></div>
              <div className="ahr-value" style={{ fontSize: '0.78rem', lineHeight: 1.2 }}>
                {/* Never show "converged" or "applied" unless the data says so.
                    phase is raw daemon state — honest, never prettified here. */}
                {phase}
              </div>
            </div>
          </div>
        </div>
      </section>

      <ConvergenceTimeline status={status} />

      <AutoTunerPanel />
    </div>
  );
}
