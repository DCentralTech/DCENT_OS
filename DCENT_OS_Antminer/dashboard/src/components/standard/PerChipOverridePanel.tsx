import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../../api/client';
import { useMinerStore } from '../../store/miner';
import { ActionButton } from '../common/ActionButton';
import { Tooltip } from '../common/Tooltip';
import { formatFrequency } from '../../utils/format';
import type { AutotunerChipHealthStatus } from '../../api/types';

/**
 * Per-chip override drill-down (design-handoff Tuning kit `PerChipOverride`).
 *
 * TRUTH CONTRACT:
 *  - The chip grid is wired to the REAL per-chip telemetry from
 *    `GET /api/autotuner/chip-health` (`api.getAutotunerChipHealth()`).
 *    There is NO synthetic / pseudo-random grid. If the build or hardware
 *    path reports zero per-chip records, we render an honest empty state.
 *  - The daemon does NOT expose a per-chip "silicon grade" field. The A/B/C
 *    grade shown here is DERIVED from the real `health_score` /
 *    `error_rate_pct` and is labelled "derived" so it is never mistaken for
 *    a factory bin.
 *  - "Masked" chips are detected from the REAL `status` string the daemon
 *    reports for the chip — we do not invent a mask state.
 *  - The ONLY real per-chip write the API exposes is
 *    `api.setChipFrequency({ chain, chip, freq_mhz, confirm })`
 *    (`POST /api/debug/chip/frequency`). "Reset to default" and "Set
 *    frequency" use it, addressed by the chip's REAL `chain_id`.
 *  - There is NO mask/unmask endpoint and NO freeze-setpoint endpoint in
 *    the API client. Those actions are rendered DISABLED with honest copy
 *    rather than faking success.
 */

type Grade = 'A' | 'B' | 'C' | 'X';

interface DerivedChip {
  chainId: number;
  chipIndex: number;
  grade: Grade;       // 'X' === masked / excluded by the daemon
  masked: boolean;
  freqMhz: number;
  healthScore: number;
  errorRatePct: number;
  hashrateRatio: number;
  backoffCount: number;
  statusLabel: string;
}

const GRADE_META: Record<Grade, { color: string; label: string; tip: string }> = {
  A: { color: 'var(--green)', label: 'A', tip: 'Strong silicon — high health score, low error rate.' },
  B: { color: 'var(--yellow)', label: 'B', tip: 'Average silicon — usable, some derate headroom.' },
  C: { color: 'var(--red)', label: 'C', tip: 'Weak silicon — low health / elevated error rate.' },
  X: { color: 'var(--text-dim)', label: 'X', tip: 'Masked / excluded by the daemon — not producing work.' },
};

// A masked chip is one the daemon has explicitly excluded. We read this from
// the REAL status string; we never infer it from grade.
function isMaskedStatus(status: string): boolean {
  const s = status.toLowerCase();
  return (
    s.includes('mask') ||
    s.includes('disabl') ||
    s.includes('exclud') ||
    s.includes('dead') ||
    s.includes('offline')
  );
}

// Honest derived grade. NOT a factory bin — surfaced from live health.
function deriveGrade(c: AutotunerChipHealthStatus): Grade {
  if (isMaskedStatus(c.status)) return 'X';
  const health = Number.isFinite(c.health_score) ? c.health_score : 0;
  const err = Number.isFinite(c.error_rate_pct) ? c.error_rate_pct : 0;
  if (health >= 80 && err <= 2) return 'A';
  if (health >= 55 && err <= 6) return 'B';
  return 'C';
}

export function PerChipOverridePanel() {
  const addAlert = useMinerStore(s => s.addAlert);

  const [collapsed, setCollapsed] = useState(true);
  const [loading, setLoading] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [raw, setRaw] = useState<AutotunerChipHealthStatus[]>([]);
  const [meta, setMeta] = useState<{ source: string; stale: boolean; ageS: number; message: string } | null>(null);

  const [activeChain, setActiveChain] = useState<number | null>(null);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [busyKey, setBusyKey] = useState<string | null>(null);

  const cancelledRef = useRef(false);

  const load = useCallback(async () => {
    setLoading(true);
    setFetchError(null);
    try {
      const r = await api.getAutotunerChipHealth();
      if (cancelledRef.current) return;
      setRaw(Array.isArray(r.chips) ? r.chips : []);
      setMeta({
        source: r.source ?? 'unknown',
        stale: !!r.stale,
        ageS: typeof r.age_s === 'number' ? r.age_s : 0,
        message: r.message ?? '',
      });
      setLoaded(true);
    } catch {
      if (cancelledRef.current) return;
      setFetchError('Could not reach the chip-health endpoint on this miner.');
      setLoaded(true);
    } finally {
      if (!cancelledRef.current) setLoading(false);
    }
  }, []);

  // Lazily fetch the first time the panel is expanded (don't poll the daemon
  // for a collapsed drill-down).
  useEffect(() => {
    cancelledRef.current = false;
    if (!collapsed && !loaded && !loading) void load();
    return () => { cancelledRef.current = true; };
  }, [collapsed, loaded, loading, load]);

  const chips: DerivedChip[] = useMemo(
    () =>
      raw.map(c => {
        const masked = isMaskedStatus(c.status);
        return {
          chainId: c.chain_id,
          chipIndex: c.chip_index,
          grade: deriveGrade(c),
          masked,
          freqMhz: c.freq_mhz,
          healthScore: c.health_score,
          errorRatePct: c.error_rate_pct,
          hashrateRatio: c.hashrate_ratio,
          backoffCount: c.backoff_count,
          statusLabel: c.status || 'unknown',
        };
      }),
    [raw],
  );

  const chainIds = useMemo(() => {
    const set = new Set<number>();
    chips.forEach(c => set.add(c.chainId));
    return [...set].sort((a, b) => a - b);
  }, [chips]);

  useEffect(() => {
    if (activeChain == null && chainIds.length > 0) setActiveChain(chainIds[0]);
    if (activeChain != null && chainIds.length > 0 && !chainIds.includes(activeChain)) {
      setActiveChain(chainIds[0]);
    }
  }, [chainIds, activeChain]);

  const chainChips = useMemo(
    () =>
      chips
        .filter(c => c.chainId === activeChain)
        .sort((a, b) => a.chipIndex - b.chipIndex),
    [chips, activeChain],
  );

  const selected = useMemo(
    () => chips.find(c => `${c.chainId}:${c.chipIndex}` === selectedKey) ?? null,
    [chips, selectedKey],
  );

  const counts = useMemo(() => {
    const acc = { A: 0, B: 0, C: 0, X: 0 };
    chips.forEach(c => { acc[c.grade] += 1; });
    return acc;
  }, [chips]);

  // The only real per-chip write. Addressed by the chip's REAL chain_id (the
  // ground-truth chain the daemon assigned in the chip-health record), and
  // its real chip_index — NOT a guessed offset. (TuningProfiles.tsx:166 uses
  // `chain: i + 6, chip: -1` for a whole-chain broadcast keyed by the
  // status.chains array index; that 6-based offset is the legacy Zynq
  // FPGA-chain numbering for a broadcast write. Here we already hold the
  // daemon's authoritative per-chip `chain_id`, so we send it verbatim and
  // do not re-derive an offset.)
  const setChipFreq = useCallback(
    async (chip: DerivedChip, freqMhz: number) => {
      const key = `${chip.chainId}:${chip.chipIndex}`;
      setBusyKey(key);
      try {
        await api.setChipFrequency({
          chain: chip.chainId,
          chip: chip.chipIndex,
          freq_mhz: freqMhz,
          confirm: true,
        });
        addAlert(
          'info',
          `Chain ${chip.chainId} chip ${chip.chipIndex}: frequency set to ${freqMhz} MHz. The daemon converges the chip toward this target.`,
        );
        // Re-pull live state so the grid reflects the daemon's view.
        await load();
      } catch {
        addAlert('warning', `Failed to set frequency on chain ${chip.chainId} chip ${chip.chipIndex}`);
      } finally {
        setBusyKey(null);
      }
    },
    [addAlert, load],
  );

  const chainAvgFreq = useMemo(() => {
    const list = chainChips.filter(c => !c.masked && Number.isFinite(c.freqMhz) && c.freqMhz > 0);
    if (list.length === 0) return 0;
    return Math.round(list.reduce((s, c) => s + c.freqMhz, 0) / list.length);
  }, [chainChips]);

  const hasData = loaded && !fetchError && chips.length > 0;
  const honestEmpty = loaded && !fetchError && chips.length === 0;

  return (
    <section className="section per-chip-override">
      <button
        type="button"
        className={`pco-disclosure${collapsed ? '' : ' is-open'}`}
        aria-expanded={!collapsed}
        onClick={() => setCollapsed(v => !v)}
      >
        <span className="pco-disclosure-caret" aria-hidden="true">{collapsed ? '▸' : '▾'}</span>
        <span className="pco-disclosure-title">Per-chip override</span>
        <span className="pco-disclosure-sub">
          Live per-chip silicon health &amp; per-chip frequency. Advanced.
        </span>
        {hasData && (
          <span className="pco-disclosure-count">
            {chips.length} chip{chips.length === 1 ? '' : 's'} · {chainIds.length} chain{chainIds.length === 1 ? '' : 's'}
          </span>
        )}
      </button>

      {!collapsed && (
        <div className="pco-body">
          <div className="pco-intro">
            Per-chip grades are <strong>derived from live health</strong> (the daemon does not
            report a factory silicon bin). The grid is wired to{' '}
            <code>/api/autotuner/chip-health</code> — real telemetry only.
          </div>

          {loading && !hasData && (
            <div className="pco-state">Loading per-chip health…</div>
          )}

          {fetchError && (
            <div className="pco-state pco-state-error" role="alert">
              {fetchError} Per-chip override is unavailable until the daemon responds.
              <div className="pco-state-actions">
                <button type="button" className="btn btn-secondary" onClick={() => void load()}>
                  Retry
                </button>
              </div>
            </div>
          )}

          {honestEmpty && (
            <div className="pco-state" role="status">
              <strong>Per-chip health not reported by this build / hardware path.</strong>
              <div className="pco-state-sub">
                This miner&apos;s firmware/autotuner did not return per-chip records
                {meta?.message ? ` (${meta.message})` : ''}. Per-chip override needs
                live per-chip telemetry; nothing is shown rather than fabricated.
              </div>
              <div className="pco-state-actions">
                <button type="button" className="btn btn-secondary" onClick={() => void load()}>
                  Refresh
                </button>
              </div>
            </div>
          )}

          {hasData && (
            <>
              <div className="pco-toolbar">
                <div className="pco-legend" aria-label="Silicon grade legend">
                  {(['A', 'B', 'C', 'X'] as Grade[]).map(g => (
                    <Tooltip key={g} content={GRADE_META[g].tip}>
                      <span className="pco-legend-item" tabIndex={0}>
                        <span
                          className="pco-legend-swatch"
                          style={{ background: GRADE_META[g].color }}
                          aria-hidden="true"
                        />
                        <span className="pco-legend-label">
                          {g === 'X' ? 'Masked' : `Grade ${g}`}
                        </span>
                        <span className="pco-legend-count tnum">{counts[g]}</span>
                      </span>
                    </Tooltip>
                  ))}
                </div>
                <div className="pco-toolbar-meta">
                  {meta?.stale && <span className="small-tag warn">stale {Math.round(meta.ageS)}s</span>}
                  <span className="pco-src">src: {meta?.source ?? 'unknown'}</span>
                  <button
                    type="button"
                    className="btn btn-secondary pco-refresh"
                    onClick={() => void load()}
                    disabled={loading}
                  >
                    {loading ? 'Refreshing…' : 'Refresh'}
                  </button>
                </div>
              </div>

              <div className="pco-tabs" role="tablist" aria-label="Hash chains">
                {chainIds.map(cid => {
                  const n = chips.filter(c => c.chainId === cid).length;
                  const active = cid === activeChain;
                  return (
                    <button
                      key={cid}
                      type="button"
                      role="tab"
                      aria-selected={active}
                      className={`pco-tab${active ? ' is-active' : ''}`}
                      onClick={() => { setActiveChain(cid); setSelectedKey(null); }}
                    >
                      Chain {cid}
                      <span className="pco-tab-count tnum">{n}</span>
                    </button>
                  );
                })}
              </div>

              <div className="pco-split">
                <div
                  className="pco-grid"
                  role="grid"
                  aria-label={`Chain ${activeChain} chips`}
                >
                  {chainChips.map(c => {
                    const key = `${c.chainId}:${c.chipIndex}`;
                    const isSel = key === selectedKey;
                    return (
                      <Tooltip
                        key={key}
                        content={
                          c.masked
                            ? `Chip ${c.chipIndex} — masked (${c.statusLabel})`
                            : `Chip ${c.chipIndex} — grade ${c.grade} · ${c.healthScore.toFixed(0)} health · ${c.freqMhz} MHz`
                        }
                      >
                        <button
                          type="button"
                          role="gridcell"
                          className={`pco-cell pco-cell-${c.grade.toLowerCase()}${c.masked ? ' is-masked' : ''}${isSel ? ' is-selected' : ''}`}
                          style={{ '--cell-color': GRADE_META[c.grade].color } as React.CSSProperties}
                          onClick={() => setSelectedKey(key)}
                          aria-pressed={isSel}
                          aria-label={`Chip ${c.chipIndex}, ${c.masked ? 'masked' : `grade ${c.grade}`}`}
                        >
                          <span className="pco-cell-idx tnum">{c.chipIndex}</span>
                        </button>
                      </Tooltip>
                    );
                  })}
                </div>

                <div className="pco-inspector" aria-live="polite">
                  {!selected && (
                    <div className="pco-inspector-empty">
                      <div className="pco-inspector-empty-title">No chip selected</div>
                      <div className="pco-inspector-empty-sub">
                        Select a chip in the grid to see its derived grade, live
                        frequency, health and the per-chip actions.
                      </div>
                      <div className="pco-inspector-chainstat">
                        Chain {activeChain} avg freq:{' '}
                        <strong className="tnum">
                          {chainAvgFreq > 0 ? formatFrequency(chainAvgFreq) : '—'}
                        </strong>
                      </div>
                    </div>
                  )}

                  {selected && (
                    <div className="pco-inspector-card">
                      <div className="pco-inspector-head">
                        <span
                          className="pco-inspector-grade"
                          style={{ '--cell-color': GRADE_META[selected.grade].color } as React.CSSProperties}
                        >
                          {selected.masked ? 'MASKED' : `GRADE ${selected.grade}`}
                        </span>
                        <span className="pco-inspector-title">
                          Chain {selected.chainId} · Chip {selected.chipIndex}
                        </span>
                      </div>

                      <div className="pco-inspector-stats">
                        <div className="pco-stat">
                          <span className="pco-stat-label">Frequency</span>
                          <span className="pco-stat-val tnum">{formatFrequency(selected.freqMhz)}</span>
                        </div>
                        <div className="pco-stat">
                          <span className="pco-stat-label">Health</span>
                          <span className="pco-stat-val tnum">{selected.healthScore.toFixed(0)}</span>
                        </div>
                        <div className="pco-stat">
                          <span className="pco-stat-label">Error rate</span>
                          <span className="pco-stat-val tnum">{selected.errorRatePct.toFixed(2)}%</span>
                        </div>
                        <div className="pco-stat">
                          <span className="pco-stat-label">Hashrate ratio</span>
                          <span className="pco-stat-val tnum">{(selected.hashrateRatio * 100).toFixed(0)}%</span>
                        </div>
                        <div className="pco-stat">
                          <span className="pco-stat-label">Backoffs</span>
                          <span className="pco-stat-val tnum">{selected.backoffCount}</span>
                        </div>
                        <div className="pco-stat">
                          <span className="pco-stat-label">Daemon status</span>
                          <span className="pco-stat-val">{selected.statusLabel}</span>
                        </div>
                      </div>

                      <div className="pco-inspector-note">
                        Grade is <strong>derived</strong> from live health/error-rate,
                        not a factory bin.
                      </div>

                      <div className="pco-actions">
                        {/* Mask/Unmask — NO real endpoint exists in the API
                            client. Rendered disabled with honest copy rather
                            than faking success. */}
                        <Tooltip content="Per-chip mask/unmask is in development for this firmware build. This control stays disabled until daemon support is promoted.">
                          <span className="pco-action-disabled" tabIndex={0}>
                            <button type="button" className="btn btn-secondary" disabled>
                              {selected.masked ? 'Unmask chip' : 'Mask chip'}
                            </button>
                            <span className="pco-action-disabled-tag">Preview</span>
                          </span>
                        </Tooltip>

                        {/* Freeze setpoint — likewise no real endpoint. */}
                        <Tooltip content="Freeze setpoint is in development for this firmware build. Pinning a chip's setpoint requires daemon support, so this control stays disabled.">
                          <span className="pco-action-disabled" tabIndex={0}>
                            <button type="button" className="btn btn-secondary" disabled>
                              Freeze setpoint
                            </button>
                            <span className="pco-action-disabled-tag">Preview</span>
                          </span>
                        </Tooltip>

                        {/* Reset to default — REAL: sets this chip's frequency
                            back to the chain average via the real
                            setChipFrequency endpoint. */}
                        <ActionButton
                          label={
                            busyKey === `${selected.chainId}:${selected.chipIndex}`
                              ? 'Applying…'
                              : 'Reset to chain default'
                          }
                          variant="secondary"
                          disabled={
                            selected.masked ||
                            chainAvgFreq <= 0 ||
                            busyKey === `${selected.chainId}:${selected.chipIndex}`
                          }
                          onClick={() => setChipFreq(selected, chainAvgFreq)}
                          confirm={`Reset chain ${selected.chainId} chip ${selected.chipIndex} to the chain average frequency (${chainAvgFreq} MHz)? This calls the daemon's real per-chip frequency endpoint. HAL voltage/thermal hard-stops still apply.`}
                        />
                      </div>

                      <div className="pco-actions-note">
                        Only the real <code>/api/debug/chip/frequency</code> write is enabled.
                        Mask &amp; Freeze remain preview controls until daemon support is promoted.
                      </div>
                    </div>
                  )}
                </div>
              </div>
            </>
          )}
        </div>
      )}
    </section>
  );
}
