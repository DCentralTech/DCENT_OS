import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { autotunerApi } from '../../api/autotuner';
import type {
  AutotunerStatusResponse,
  AutotunerTelemetryResponse,
  AutotunerTelemetrySample,
  AutotunerTuningRun,
} from '../../api/types';
import { useWindowedList } from '../../hooks/useWindowedList';
import { EmptyState } from '../common/EmptyState';
import { StatePanel } from '../common/StatePanel';
import { TransportChip } from '../common/TransportChip';

const TELEMETRY_POLL_MS = 10000;
const ROW_HEIGHT = 44;

export interface ConvergenceStep {
  id: string;
  step: number;
  elapsedS: number;
  chainId: number;
  chipCount: number;
  avgFreqMhz: number | null;
  totalNonces: number;
  totalErrors: number;
  boardTempC: number | null;
  state: string;
  difficulty: number;
  decisions: string[];
}

function fmtDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '0s';
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  const rest = Math.round(seconds % 60);
  return rest > 0 ? `${minutes}m ${rest}s` : `${minutes}m`;
}

function fmtMaybeNumber(value: number | null, suffix = ''): string {
  return value == null || !Number.isFinite(value) ? '-' : `${value.toFixed(0)}${suffix}`;
}

function avgFreq(sample: AutotunerTelemetrySample): number | null {
  const freqs = sample.chips
    .map(chip => chip.freq_mhz)
    .filter(freq => Number.isFinite(freq) && freq > 0);
  if (freqs.length === 0) return null;
  return freqs.reduce((sum, freq) => sum + freq, 0) / freqs.length;
}

export function buildConvergenceRows(run: AutotunerTuningRun | null): ConvergenceStep[] {
  if (!run) return [];
  return run.samples.map((sample, index) => {
    const decisions = Array.from(new Set(
      sample.chips
        .map(chip => chip.decision)
        .filter((decision): decision is string => typeof decision === 'string' && decision.length > 0),
    ));
    return {
      id: `${run.started_at}-${index}-${sample.chain_id}-${sample.elapsed_s}`,
      step: index + 1,
      elapsedS: sample.elapsed_s,
      chainId: sample.chain_id,
      chipCount: sample.chips.length,
      avgFreqMhz: avgFreq(sample),
      totalNonces: sample.chips.reduce((sum, chip) => sum + chip.nonces, 0),
      totalErrors: sample.chips.reduce((sum, chip) => sum + chip.errors, 0),
      boardTempC: typeof sample.board_temp_c === 'number' ? sample.board_temp_c : null,
      state: sample.tuner_state,
      difficulty: sample.difficulty,
      decisions,
    };
  });
}

export function latestTelemetryRun(telemetry: AutotunerTelemetryResponse | null): AutotunerTuningRun | null {
  const runs = telemetry?.runs ?? [];
  return runs.length > 0 ? runs[runs.length - 1] : null;
}

export function convergenceProgressText(status: AutotunerStatusResponse | null, rows: ConvergenceStep[]): string {
  if (!status) return 'Loading tuner status.';
  if (typeof status.estimated_remaining_s === 'number' && status.estimated_remaining_s > 0) {
    return `Estimated remaining ${fmtDuration(status.estimated_remaining_s)} from daemon status.`;
  }
  if (typeof status.percent_complete === 'number' && status.percent_complete > 0 && status.percent_complete < 100) {
    return `${Math.round(status.percent_complete)}% complete from daemon status.`;
  }
  if (rows.length > 0 && status.phase !== 'tuned' && status.phase !== 'partially_tuned') {
    return `Step ${rows.length}, target not yet reached.`;
  }
  if (status.phase === 'tuned' || status.phase === 'partially_tuned') return 'Tuned phase reported by daemon.';
  return 'Target progress not reported yet.';
}

function ConvergenceStepRow({ row }: { row: ConvergenceStep }) {
  const decisions = row.decisions.length > 0 ? row.decisions.join(', ') : '-';
  return (
    <div
      className="autotuner-convergence-row"
      data-testid={`autotuner-convergence-row-${row.step}`}
      style={{
        display: 'grid',
        gridTemplateColumns: '64px 70px 70px 90px 80px 70px minmax(160px, 1fr)',
        gap: 10,
        alignItems: 'center',
        minHeight: ROW_HEIGHT,
        padding: '6px 10px',
        borderBottom: '1px solid var(--border-subtle, rgba(255,255,255,0.06))',
        fontSize: '0.76rem',
      }}
    >
      <span style={{ fontWeight: 700, color: 'var(--accent)' }}>Step {row.step}</span>
      <span>{fmtDuration(row.elapsedS)}</span>
      <span>Chain {row.chainId}</span>
      <span>{fmtMaybeNumber(row.avgFreqMhz, ' MHz')}</span>
      <span>{row.totalNonces.toLocaleString()} n</span>
      <span style={{ color: row.totalErrors > 0 ? 'var(--yellow)' : 'var(--green)' }}>
        {row.totalErrors} err
      </span>
      <span style={{ color: 'var(--text-dim)', minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
        {row.state} {row.boardTempC != null ? `| ${row.boardTempC.toFixed(1)} C` : ''} | diff {row.difficulty} | {decisions}
      </span>
    </div>
  );
}

export function ConvergenceTimeline({ status }: { status: AutotunerStatusResponse | null }) {
  const [telemetry, setTelemetry] = useState<AutotunerTelemetryResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const next = await autotunerApi.getTelemetry();
      setTelemetry(next);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Autotuner telemetry fetch failed');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      if (cancelled) return;
      await load();
    };
    void poll();
    const id = window.setInterval(poll, TELEMETRY_POLL_MS);
    return () => { cancelled = true; window.clearInterval(id); };
  }, [load]);

  const latestRun = latestTelemetryRun(telemetry);
  const rows = useMemo(() => buildConvergenceRows(latestRun), [latestRun]);
  const windowed = useWindowedList<HTMLDivElement>({
    count: rows.length,
    itemHeight: ROW_HEIGHT,
    overscan: 8,
    disabled: rows.length <= 200,
  });
  const visibleRows = rows.slice(windowed.start, windowed.end);
  const progress = convergenceProgressText(status, rows);
  const off = status?.enabled === false;
  const runLabel = telemetry?.recording
    ? 'recording'
    : latestRun
      ? latestRun.completed ? 'last run complete' : 'last run incomplete'
      : 'no runs';
  const noRunsHint = telemetry?.message
    ? `${telemetry.message} CSV export will also report no runs until the autotuner records a characterization window.`
    : 'The JSON telemetry endpoint returned no runs. CSV export will also report no runs until the autotuner records a characterization window.';

  return (
    <section
      className="section ds-glass-card autotuner-panel-section"
      data-testid="autotuner-convergence-timeline"
      aria-label="Autotuner convergence timeline"
    >
      <div style={{ display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap', marginBottom: 12 }}>
        <div className="autotuner-panel-section-title" style={{ marginBottom: 0 }}>
          Convergence timeline
        </div>
        <span className="small-tag muted">{runLabel}</span>
        <span className="small-tag muted">{progress}</span>
        <TransportChip className="ds-chip" showDot={false} />
      </div>

      {off && (
        <EmptyState
          data-testid="autotuner-convergence-off"
          title="Autotuner OFF"
          hint="No convergence timeline is shown while the daemon reports the autotuner disabled. Enable it to record real tuning steps."
          live={false}
        />
      )}

      {!off && loading && telemetry === null && (
        <div className="cp-empty-note" data-testid="autotuner-convergence-loading">
          Loading autotuner telemetry...
        </div>
      )}

      {!off && error && telemetry === null && (
        <StatePanel
          tone="danger"
          title="Autotuner telemetry unavailable"
          message={error}
          action={<button type="button" className="ds-btn ds-btn--secondary ds-btn--sm" onClick={() => { void load(); }}>Retry</button>}
        />
      )}

      {!off && !loading && !error && rows.length === 0 && (
        <EmptyState
          data-testid="autotuner-convergence-empty"
          title="No tuning runs recorded yet"
          hint={noRunsHint}
          live={false}
        />
      )}

      {!off && rows.length > 0 && (
        <div
          aria-label="Autotuner tuning telemetry samples"
          data-testid="autotuner-convergence-table"
          data-row-count={rows.length}
          style={{ overflowX: 'auto' }}
        >
          <div
            style={{
              display: 'grid',
              gridTemplateColumns: '64px 70px 70px 90px 80px 70px minmax(160px, 1fr)',
              gap: 10,
              padding: '6px 10px',
              fontSize: '0.68rem',
              textTransform: 'uppercase',
              color: 'var(--text-dim)',
              borderBottom: '1px solid var(--border-subtle, rgba(255,255,255,0.06))',
              minWidth: 700,
            }}
          >
            <span>Step</span>
            <span>Elapsed</span>
            <span>Chain</span>
            <span>Avg freq</span>
            <span>Nonces</span>
            <span>Errors</span>
            <span>State</span>
          </div>
          <div
            ref={windowed.containerRef}
            onScroll={windowed.onScroll}
            style={{
              maxHeight: 360,
              overflowY: rows.length > 8 ? 'auto' : 'visible',
              minWidth: 700,
            }}
          >
            {windowed.padTop > 0 && <div style={{ height: windowed.padTop }} aria-hidden="true" />}
            {visibleRows.map(row => <ConvergenceStepRow key={row.id} row={row} />)}
            {windowed.padBottom > 0 && <div style={{ height: windowed.padBottom }} aria-hidden="true" />}
          </div>
        </div>
      )}
    </section>
  );
}
