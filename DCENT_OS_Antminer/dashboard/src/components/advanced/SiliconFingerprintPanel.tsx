import React, { useMemo, useState, useEffect } from 'react';
import { useMinerStore } from '../../store/miner';
import { api } from '../../api/client';
import type { SiliconReportResponse, ThermalSupervisorSnapshot } from '../../api/types';
import { classifyThermalSupervisor } from '../../utils/thermalSupervisor';

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

const GRADE_TONE: Record<string, string> = {
  A: 'var(--green)',
  B: 'var(--accent)',
  C: 'var(--yellow)',
  D: 'var(--red, #ff6b6b)',
};

/**
 * Silicon Grade Report — surfaces the live `/api/autotuner/silicon-report`
 * (W9/W15) and the DIAGNOSTIC chip-imbalance telemetry from
 * `/api/thermal/supervisor` (Wave-G). Both are PURE TELEMETRY.
 *
 * W15 honesty contract (load-bearing): when `report.characterized === false`
 * the autotuner has not yet measured these chips — the grade distribution is
 * NOT a silicon-quality verdict and MUST NOT be presented as one. In that
 * state we show the "Not Characterized" tier + a run-tuning hint and SUPPRESS
 * the grade-distribution bars entirely (rather than render fabricated grades).
 */
function SiliconGradeReportSection() {
  const [report, setReport] = useState<SiliconReportResponse | null>(null);
  const [thermal, setThermal] = useState<ThermalSupervisorSnapshot | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;
    const load = async () => {
      const [rep, th] = await Promise.allSettled([
        api.getAutotunerSiliconReport(),
        api.getThermalSupervisor(),
      ]);
      if (cancelled) return;
      if (rep.status === 'fulfilled') setReport(rep.value);
      if (th.status === 'fulfilled') setThermal(th.value);
      // Only an error if BOTH telemetry sources are unreachable; either alone
      // still gives the operator something useful.
      if (rep.status === 'rejected' && th.status === 'rejected') {
        setError(rep.reason instanceof Error ? rep.reason.message : 'telemetry unavailable');
      } else {
        setError(null);
      }
      setLoaded(true);
      timer = window.setTimeout(() => { void load(); }, 10000);
    };
    void load();
    return () => { cancelled = true; if (timer !== undefined) window.clearTimeout(timer); };
  }, []);

  // P3-37: honest disabled/active state + die-fallback caveat. The supervisor
  // is an OPTIONAL diagnostic/escalation layer that is OFF by default — render
  // that truthfully instead of an imbalance KPI that implies active protection.
  const supervisor = useMemo(() => classifyThermalSupervisor(thermal), [thermal]);
  const imbalance = supervisor.imbalance;

  if (!loaded) {
    return (
      <section className="sf-report-section" aria-busy="true">
        <div className="sf-empty">Loading silicon grade report…</div>
      </section>
    );
  }

  if (!report && error) {
    return (
      <section className="sf-report-section">
        <div className="sf-empty">Silicon grade report unavailable — {error}.</div>
      </section>
    );
  }

  const characterized = report?.characterized ?? false;
  const grades: { letter: string; count: number; pct: number }[] = report
    ? [
        { letter: 'A', count: report.grade_a_count, pct: report.grade_a_pct },
        { letter: 'B', count: report.grade_b_count, pct: report.grade_b_pct },
        { letter: 'C', count: report.grade_c_count, pct: report.grade_c_pct },
        { letter: 'D', count: report.grade_d_count, pct: report.grade_d_pct },
      ]
    : [];

  return (
    <section className="sf-report-section">
      <header className="sf-report-head">
        <div className="hacker-inspector-eyebrow">// silicon grade report</div>
        <span className={`hacker-status-chip ${characterized ? 'success' : 'info'}`}>
          {report ? (characterized ? (report.quality_tier || 'Characterized').toUpperCase() : 'NOT CHARACTERIZED') : 'NO PROFILE DATA'}
        </span>
      </header>

      {report && !characterized && (
        <div className="sf-report-note" role="note">
          The autotuner has not measured these chips yet
          {report.not_characterized_chips > 0 ? ` (${report.not_characterized_chips} of ${report.total_chips} chips un-measured)` : ''}.
          Grades are not fabricated — run autotuning to characterize the silicon before
          reading a quality verdict.
        </div>
      )}

      {report && characterized && (
        <>
          <div className="sf-kpi-grid">
            <div className="glass-card sf-kpi">
              <span className="sf-kpi-label">Quality score</span>
              <span className="sf-kpi-value" style={{ color: 'var(--accent-orange)' }}>{report.quality_score.toFixed(0)}/100</span>
            </div>
            <div className="glass-card sf-kpi">
              <span className="sf-kpi-label">Chips graded</span>
              <span className="sf-kpi-value" style={{ color: 'var(--accent)' }}>{report.total_chips - report.not_characterized_chips}/{report.total_chips}</span>
            </div>
            <div className="glass-card sf-kpi">
              <span className="sf-kpi-label">Avg max-stable</span>
              <span className="sf-kpi-value" style={{ color: 'var(--text)' }}>{report.avg_max_stable_mhz.toFixed(0)} MHz</span>
            </div>
            <div className="glass-card sf-kpi">
              <span className="sf-kpi-label">Best / worst</span>
              <span className="sf-kpi-value" style={{ color: 'var(--accent)' }}>{report.best_chip_mhz}/{report.worst_chip_mhz} MHz</span>
            </div>
          </div>

          <div className="sf-grade-row" role="group" aria-label="Effective grade distribution">
            {grades.map(g => (
              <div key={g.letter} className="glass-card sf-grade-chip">
                <span className="sf-grade-letter" style={{ color: GRADE_TONE[g.letter] }}>{g.letter}</span>
                <span className="sf-grade-count">{g.count}</span>
                <span className="sf-grade-pct">{g.pct.toFixed(0)}%</span>
              </div>
            ))}
          </div>

          {report.not_characterized_chips > 0 && (
            <div className="sf-report-note" role="note">
              {report.not_characterized_chips} chip{report.not_characterized_chips === 1 ? '' : 's'} not yet
              measured — excluded from the grades above (never counted as grade D).
            </div>
          )}

          {report.chain_reports.length > 0 && (
            <div className="sf-chain-grades">
              {report.chain_reports.map(cr => (
                <div key={cr.chain_id} className="register-inspector sf-chain-grade">
                  <span className="sf-chain-name">Chain {cr.chain_id}</span>
                  <span className="sf-chain-grade-dist">
                    {(['A', 'B', 'C', 'D'] as const).map((letter, i) => (
                      <span key={letter} style={{ color: GRADE_TONE[letter] }}>
                        {letter}:{cr.grade_distribution[i]}{i < 3 ? ' ' : ''}
                      </span>
                    ))}
                  </span>
                  <span className="sf-chain-grade-meta">{cr.avg_max_stable_mhz.toFixed(0)} MHz · q{cr.quality_score.toFixed(0)}</span>
                </div>
              ))}
            </div>
          )}
        </>
      )}

      {supervisor.availability !== 'unavailable' && (
        <div className="sf-supervisor" role="group" aria-label="Thermal supervisor state">
          <header className="sf-report-head">
            <div className="hacker-inspector-eyebrow">// thermal supervisor</div>
            <span className={`hacker-status-chip ${supervisor.availability === 'active' ? 'success' : 'info'}`}>
              {supervisor.availability === 'active' ? 'ACTIVE' : 'DISABLED'}
            </span>
          </header>

          {supervisor.availability === 'disabled' && (
            <div className="sf-report-note" role="note">
              The thermal supervisor is disabled (the default — its config is empty or
              [thermal.supervisor].enabled = false). It is an optional extra layer
              (board/chip-panic escalation + chip-imbalance diagnostics) and does not
              replace the always-on thermal controller, which stays in charge. With the
              supervisor off, that extra escalation and these diagnostics are not running.
            </div>
          )}

          {supervisor.dieFallbackCaveat && (
            <div className="sf-report-note" role="note">
              No per-board PCB/chip temperature sensors are being read, so the SoC-die
              fallback is the only temperature source. The die runs far cooler than the
              chip junction, so the supervisor's ≥70 °C "dangerous" board-panic alert
              cannot fire from die temperature alone — treat die-only temps as a coarse
              safety proxy, not a board-temperature guarantee.
            </div>
          )}

          {imbalance && (
            <div className="sf-imbalance" role="group" aria-label="Inter-chip temperature imbalance (diagnostic)">
              <div className="glass-card sf-kpi">
                <span className="sf-kpi-label">Worst chip imbalance <span className="sf-diag-tag">diagnostic</span></span>
                <span className="sf-kpi-value" style={{ color: imbalance.flagged ? 'var(--yellow)' : 'var(--green)' }}>
                  {imbalance.worst !== null && imbalance.worst !== undefined ? `${imbalance.worst.toFixed(1)} °C` : 'no multi-sensor data'}
                  {imbalance.threshold != null && (
                    <span className="sf-imbalance-threshold">{' '}/ {imbalance.threshold.toFixed(0)} °C</span>
                  )}
                  {imbalance.worst !== null && imbalance.worst !== undefined && (
                    <span className="sf-imbalance-threshold">{' '}· {imbalance.flagged ? 'over threshold' : 'within threshold'}</span>
                  )}
                </span>
              </div>
              {imbalance.flagged && (
                <div className="sf-report-note" role="note">
                  Inter-chip temperature spread exceeded the diagnostic threshold. This is a
                  uniformity signal only — it does not throttle or shut down the miner.
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </section>
  );
}

function getOverallPersona(hottest: number, tempSpread: number, totalErrors: number, maxFreq: number, activeChains: number) {
  if (activeChains <= 1) return 'Lone Wolf';
  if (tempSpread > 8) return 'Uneven Silicon';
  if (hottest >= 68) return 'Hot Rod';
  if (maxFreq >= 700) return 'Clock Chaser';
  if (totalErrors === 0) return 'Balanced Die';
  return 'Field Mix';
}

export function SiliconFingerprintPanel() {
  const status = useMinerStore(s => s.status);
  const autotunerStatus = useMinerStore(s => s.autotunerStatus);
  const chains = (Array.isArray(status?.chains) ? status.chains : [])
    .filter(chain => chain.chips > 0);

  const fingerprint = useMemo(() => {
    if (chains.length === 0) {
      return null;
    }

    const temps = chains.map(chain => chain.temp_c);
    const hashrates = chains.map(chain => chain.hashrate_ghs);
    const freqs = chains.map(chain => chain.frequency_mhz);
    const errors = chains.map(chain => chain.errors);
    const hottest = Math.max(...temps);
    const coolest = Math.min(...temps);
    const tempSpread = hottest - coolest;
    const maxHashrate = Math.max(...hashrates);
    const minHashrate = Math.min(...hashrates);
    const maxFreq = Math.max(...freqs);
    const totalErrors = errors.reduce((sum, value) => sum + value, 0);

    const cards = chains.map(chain => {
      const normalizedHash = maxHashrate > 0 ? chain.hashrate_ghs / maxHashrate : 0;
      const stress = clamp(((chain.temp_c - coolest) / Math.max(tempSpread, 1)) * 0.45 + (chain.errors > 0 ? 0.35 : 0) + (1 - normalizedHash) * 0.2, 0, 1);
      const persona = stress > 0.72
        ? 'Thermal Rebel'
        : chain.errors === 0 && normalizedHash > 0.9
          ? 'Clean Sprinter'
          : chain.frequency_mhz === maxFreq
            ? 'Overclocker'
            : chain.temp_c === hottest
              ? 'Space Heater'
              : 'Steady Hand';
      return {
        chain,
        stress,
        persona,
      };
    });

    return {
      hottest,
      coolest,
      tempSpread,
      hashrateSpread: maxHashrate - minHashrate,
      maxFreq,
      totalErrors,
      persona: getOverallPersona(hottest, tempSpread, totalErrors, maxFreq, chains.length),
      cards,
    };
  }, [chains]);

  if (!fingerprint) {
    return (
      <div className="hacker-inspector">
        <header className="hacker-inspector-header">
          <div className="hacker-inspector-title-group">
            <div className="hacker-inspector-eyebrow">// silicon fingerprint</div>
            <h2 className="hacker-inspector-title">Chain Personality Scan</h2>
          </div>
          <div className="hacker-inspector-actions">
            <span className="hacker-inspector-status neutral">NO TELEMETRY</span>
          </div>
        </header>
        <div className="hacker-inspector-body">
          <div className="sf-empty">
            Waiting for active chain telemetry before fingerprinting the silicon personality.
          </div>
          <SiliconGradeReportSection />
        </div>
        <footer className="hacker-inspector-footer">
          <div className="hacker-inspector-stats"><span>0 chains</span></div>
        </footer>
      </div>
    );
  }

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// silicon fingerprint</div>
          <h2 className="hacker-inspector-title">Chain Personality Scan</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className="hacker-inspector-status">{fingerprint.persona.toUpperCase()}</span>
        </div>
      </header>

      <div className="hacker-inspector-body">
      <div className="sf-kpi-grid">
        {[
          { label: 'Overall persona', value: fingerprint.persona, tone: 'var(--accent-orange)' },
          { label: 'Temp spread', value: `${fingerprint.tempSpread.toFixed(1)} C`, tone: 'var(--accent)' },
          { label: 'Hashrate spread', value: `${(fingerprint.hashrateSpread / 1000).toFixed(2)} TH/s`, tone: 'var(--accent)' },
          { label: 'Total errors', value: String(fingerprint.totalErrors), tone: fingerprint.totalErrors > 0 ? 'var(--yellow)' : 'var(--green)' },
          { label: 'Top clock', value: `${fingerprint.maxFreq} MHz`, tone: 'var(--text)' },
          { label: 'Autotuner grades', value: autotunerStatus?.silicon_grades ? `${autotunerStatus.silicon_grades.a}/${autotunerStatus.silicon_grades.b}/${autotunerStatus.silicon_grades.c}/${autotunerStatus.silicon_grades.d}` : 'n/a', tone: 'var(--accent)' },
        ].map(card => (
          <div key={card.label} className="glass-card sf-kpi">
            <span className="sf-kpi-label">{card.label}</span>
            <span className="sf-kpi-value" style={{ color: card.tone }}>{card.value}</span>
          </div>
        ))}
      </div>

      <div className="sf-chain-list">
        {fingerprint.cards.map(card => (
          <div key={card.chain.id} className="register-inspector sf-chain">
            <div className="sf-chain-head">
              <div>
                <div className="sf-chain-name">Chain {card.chain.id}</div>
                <div className="sf-chain-persona">{card.persona}</div>
              </div>
              <span className={`hacker-status-chip ${card.stress > 0.72 ? 'warning' : card.chain.errors > 0 ? 'info' : 'success'}`}>
                Stress {(card.stress * 100).toFixed(0)}%
              </span>
            </div>
            <div className="sf-metric-grid">
              <div className="glass-card sf-metric">
                <div className="sf-metric-label">Hashrate</div>
                <div className="sf-metric-value" style={{ color: 'var(--accent-orange)' }}>{(card.chain.hashrate_ghs / 1000).toFixed(2)} TH/s</div>
              </div>
              <div className="glass-card sf-metric">
                <div className="sf-metric-label">Temperature</div>
                <div className="sf-metric-value" style={{ color: card.chain.temp_c >= fingerprint.hottest ? 'var(--yellow)' : 'var(--accent)' }}>{card.chain.temp_c.toFixed(1)} C</div>
              </div>
              <div className="glass-card sf-metric">
                <div className="sf-metric-label">Clock</div>
                <div className="sf-metric-value" style={{ color: 'var(--accent)' }}>{card.chain.frequency_mhz} MHz</div>
              </div>
              <div className="glass-card sf-metric">
                <div className="sf-metric-label">Errors</div>
                <div className="sf-metric-value" style={{ color: card.chain.errors > 0 ? 'var(--yellow)' : 'var(--green)' }}>{card.chain.errors}</div>
              </div>
            </div>
          </div>
        ))}
      </div>

      <SiliconGradeReportSection />
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{fingerprint.cards.length} chains profiled</span>
          <span>spread {fingerprint.tempSpread.toFixed(1)} C</span>
          <span>{fingerprint.totalErrors} HW errors</span>
        </div>
      </footer>
    </div>
  );
}
