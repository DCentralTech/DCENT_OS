import React, { useState, useEffect, useRef, useCallback } from 'react';
import { api } from '../../api/client';
import { OverlayDialog } from '../common/OverlayDialog';
import type {
  AutotunerChipHealthResponse,
  AutotunerChipHealthStatus,
  ChipColor,
  ChipHealthSnapshotResponse,
  ChipMapCell,
} from '../../api/types';
import { useActiveHardware } from '../../hooks/useActiveHardware';
import { glossary } from '../../utils/glossary';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';
import {
  CHIP_HEALTH_COLORS,
  CHIP_HEALTH_LABELS,
  ChipHealthLegend,
  chipHealthSourceLabel,
  chipHealthTextColor,
  chipHealthToneFromAutotuner,
  chipHealthToneFromDiagnostics,
  formatHealthPercent,
  type ChipHealthSource,
  type ChipHealthTone,
} from '../common/ChipHealthLegend';

// Per-chip cell shape used by this view. Every value is projected from the REAL
// GET /api/chips ChipMapCell (the honest per-chip source that standard/
// ChipHeatMap also consumes). NO fabrication: frequency/nonce_count/crc_errors/
// grade/color come straight off the wire; `temperature` is die_temp_c which is
// null until firmware exposes it (we color by health grade then, never invent a
// number). There is no per-chip hashrate or nonce-rate field on the wire, so
// those previously-synthesized columns were removed rather than faked.
interface ChipData {
  chain: number;
  chipIndex: number;
  frequency: number;      // frequency_mhz
  temperature: number | null; // die_temp_c (null = firmware omits value)
  errors: number;         // crc_errors
  nonce_count: number;    // real cumulative nonce count
  grade: string;          // backend-computed health grade
  color: ChipColor;       // backend-computed health color
  health_score: number;
  present: boolean;       // health_score > 0 / responding
  status: 'ok' | 'error' | 'dead';
  autotunerHealth: AutotunerChipHealthStatus | null;
  healthTone: ChipHealthTone;
  healthSource: ChipHealthSource;
}

type ColorMode = 'frequency' | 'temperature' | 'health' | 'errors' | 'nonce_count';

// Map the honest ChipColor enum (Green/Yellow/Orange/Red/Gray) to the kit
// palette — mirrors ChipColor::css_color() in dcentrald-diagnostics so the
// health coloring matches what the backend computed.
const HEALTH_COLOR: Record<ChipColor, string> = {
  Green: '#22c55e',
  Yellow: '#EAB308',
  Orange: '#F97316',
  Red: '#ef4444',
  Gray: '#333',
};

const freqToColor = (freq: number, maxFreq: number): string => {
  if (freq <= 0) return '#333';
  const ratio = freq / maxFreq;
  if (ratio < 0.5) return '#3b82f6';    // Blue (low)
  if (ratio < 0.7) return '#22c55e';    // Green (normal)
  if (ratio < 0.85) return '#F7931A';   // Orange (high)
  return '#ef4444';                      // Red (very high)
};

function getTemperatureColor(temp: number | null, color: ChipColor, status: string): string {
  if (status === 'dead') return '#333';
  // Per-chip die temp is null until firmware exposes it — color by health
  // grade instead of inventing a temperature gradient (mirrors ChipHeatMap).
  if (temp == null) return HEALTH_COLOR[color];
  const ratio = Math.max(0, Math.min(1, (temp - 40) / 30));
  const r = Math.floor(ratio * 255);
  const b = Math.floor((1 - ratio) * 255);
  return `rgb(${r}, 40, ${b})`;
}

function getErrorColor(errors: number, status: string): string {
  if (status === 'dead') return '#333';
  if (errors === 0) return '#166534';
  if (errors < 3) return '#22c55e';
  if (errors < 10) return '#EAB308';
  return '#FF4444';
}

function getNonceCountColor(count: number, status: string): string {
  if (status === 'dead') return '#333';
  if (count <= 0) return '#1a1a1a';
  if (count < 100) return '#1a4731';
  if (count < 1000) return '#166534';
  if (count < 10000) return '#22c55e';
  return '#86efac';
}

const GRADE_COLORS: Record<string, string> = {
  A: '#22c55e',
  B: '#86efac',
  C: '#EAB308',
  D: '#FF4444',
  F: '#FF4444',
  X: '#555',
};

function cellsToChipData(
  snapshot: ChipHealthSnapshotResponse | null,
  autotuner: AutotunerChipHealthResponse | null,
): ChipData[] {
  const data: ChipData[] = [];
  const autotunerByChip = new Map<string, AutotunerChipHealthStatus>();
  for (const chip of autotuner?.chips ?? []) {
    autotunerByChip.set(`${chip.chain_id}:${chip.chip_index}`, chip);
  }
  for (const chain of snapshot?.chains ?? []) {
    const cells: ChipMapCell[] = chain.chipmap?.cells ?? [];
    for (const c of cells) {
      const present = c.health_score > 0;
      const autotunerChip = autotunerByChip.get(`${chain.chain_id}:${c.index}`) ?? null;
      const autotunerTone = autotunerChip
        ? chipHealthToneFromAutotuner({
          status: autotunerChip.status,
          healthScore: autotunerChip.health_score,
        })
        : null;
      const diagnosticsTone = chipHealthToneFromDiagnostics({
        present,
        color: c.color,
        grade: c.grade,
        healthScore: c.health_score,
      });
      const useAutotuner = autotunerChip !== null && autotunerTone !== null && autotunerTone !== 'no-data';
      const status: 'ok' | 'error' | 'dead' = !present
        ? 'dead'
        : c.crc_errors > 10
          ? 'error'
          : 'ok';
      data.push({
        chain: chain.chain_id,
        chipIndex: c.index,
        frequency: c.frequency_mhz,
        temperature: c.die_temp_c ?? null,
        errors: c.crc_errors,
        nonce_count: c.nonce_count,
        grade: c.grade,
        color: c.color,
        health_score: c.health_score,
        present,
        status,
        autotunerHealth: autotunerChip,
        healthTone: useAutotuner ? autotunerTone : diagnosticsTone,
        healthSource: useAutotuner ? 'autotuner' : 'diagnostics',
      });
    }
  }
  return data;
}

export function ChipFreqMap() {
  const { activeChain, activeChip, setActiveChain, setActiveChip } = useActiveHardware();
  const [colorMode, setColorMode] = useState<ColorMode>('frequency');
  const [selectedChip, setSelectedChip] = useState<ChipData | null>(null);
  const [hoveredChip, setHoveredChip] = useState<ChipData | null>(null);
  const [tooltipPos, setTooltipPos] = useState({ x: 0, y: 0 });
  const [chipData, setChipData] = useState<ChipData[]>([]);
  const [source, setSource] = useState<string>('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [autoRefresh, setAutoRefresh] = useState(false);
  const [lastRefresh, setLastRefresh] = useState<number>(0);
  const [selectedChainTab, setSelectedChainTab] = useState<number>(activeChain);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const closeChipDialogRef = useRef<HTMLButtonElement>(null);

  const fetchData = useCallback(async () => {
    try {
      echoCli('chip map');
      const [snapshot, autotunerHealth] = await Promise.all([
        api.getChips(),
        api.getAutotunerChipHealth().catch(() => null),
      ]);
      setChipData(cellsToChipData(snapshot, autotunerHealth));
      setSource(snapshot?.source ?? '');
      setLastRefresh(Date.now());
      setError('');
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : 'Failed to fetch per-chip telemetry');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchData();
  }, [fetchData]);

  useEffect(() => {
    if (autoRefresh) {
      intervalRef.current = setInterval(fetchData, 5000);
    }
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [autoRefresh, fetchData]);

  // Available chains come from the REAL snapshot (chain_ids that returned
  // cells), not a hardcoded S9 [6,7,8] assumption.
  const availableChains = Array.from(new Set(chipData.map(c => c.chain))).sort((a, b) => a - b);

  // Keep the selected tab valid: prefer the active chain, else the first chain
  // that actually returned per-chip data.
  useEffect(() => {
    if (availableChains.length === 0) return;
    setSelectedChainTab(prev => (availableChains.includes(prev) ? prev : (availableChains.includes(activeChain) ? activeChain : availableChains[0])));
  }, [activeChain, availableChains]);

  const hasChips = chipData.length > 0;
  const perChipUnavailable = !loading && !error && !hasChips;
  const chainChips = chipData.filter(c => c.chain === selectedChainTab);
  const chipCount = chainChips.length || 63;
  // Auto-size grid: 9 columns for 63 chips (9x7), auto-calculate otherwise
  const gridCols = chipCount <= 63 ? 9 : Math.ceil(Math.sqrt(chipCount));
  const maxFreq = Math.max(...chipData.map(c => c.frequency), 700);
  const visibleHealthSource: ChipHealthSource = chipData.some(c => c.healthSource === 'autotuner')
    ? 'autotuner'
    : 'diagnostics';

  const getCellColor = (chip: ChipData): string => {
    if (chip.status === 'dead') return colorMode === 'health' ? CHIP_HEALTH_COLORS['no-data'] : '#333';
    switch (colorMode) {
      case 'frequency':
        return freqToColor(chip.frequency, maxFreq);
      case 'temperature':
        return getTemperatureColor(chip.temperature, chip.color, chip.status);
      case 'health':
        return CHIP_HEALTH_COLORS[chip.healthTone];
      case 'errors':
        return getErrorColor(chip.errors, chip.status);
      case 'nonce_count':
        return getNonceCountColor(chip.nonce_count, chip.status);
    }
  };

  const handleMouseEnter = (chip: ChipData, e: React.MouseEvent) => {
    setHoveredChip(chip);
    setTooltipPos({ x: e.clientX + 12, y: e.clientY - 40 });
  };

  const handleMouseMove = (e: React.MouseEvent) => {
    setTooltipPos({ x: e.clientX + 12, y: e.clientY - 40 });
  };

  const handleChipClick = (chip: ChipData) => {
    setSelectedChip(chip);
    setActiveChain(chip.chain);
    setActiveChip(chip.chipIndex);
  };

  const formatRefreshTime = (ts: number) => {
    if (!ts) return 'never';
    return new Date(ts).toTimeString().split(' ')[0];
  };

  const tempLabel = (t: number | null) => (t == null ? 'n/a' : `${t.toFixed(1)}C`);

  // Chain summary stats — all from REAL cells. Per-chip hashrate is not on the
  // wire, so there is no totalHash row to show.
  const chainSummary = (chips: ChipData[]) => {
    if (chips.length === 0) return null;
    const alive = chips.filter(c => c.status !== 'dead');
    const dead = chips.filter(c => c.status === 'dead').length;
    const avgFreq = alive.length > 0 ? alive.reduce((s, c) => s + c.frequency, 0) / alive.length : 0;
    const tempChips = alive.filter(c => c.temperature != null);
    const avgTemp = tempChips.length > 0
      ? tempChips.reduce((s, c) => s + (c.temperature as number), 0) / tempChips.length
      : null;
    const totalErrors = chips.reduce((s, c) => s + c.errors, 0);
    const totalNonces = chips.reduce((s, c) => s + c.nonce_count, 0);
    const grades: Record<string, number> = {};
    chips.forEach(c => { grades[c.grade] = (grades[c.grade] ?? 0) + 1; });
    return { alive: alive.length, dead, avgFreq, avgTemp, totalErrors, totalNonces, grades };
  };

  const summary = chainSummary(chainChips);
  const gradeOrder = ['A', 'B', 'C', 'D', 'F', 'X'];

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// chip freq map</div>
          <h2 className="hacker-inspector-title">Per-Chip Frequency Map</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className="hacker-inspector-status neutral">last {formatRefreshTime(lastRefresh)}</span>
          <button
            className="hacker-inspector-help"
            onClick={() => setAutoRefresh(!autoRefresh)}
          >
            {autoRefresh ? '⏸ AUTO' : '▶ AUTO'}
          </button>
          <button className="hacker-inspector-refresh" onClick={fetchData}>⟳ REFRESH</button>
          <CliHint cmd={`chip map --chain ${selectedChainTab}`} />
        </div>
      </header>

      <div className="hacker-inspector-toolbar">
        {/* Chain selector tabs — derived from the chains that actually
            returned per-chip data. */}
        <div className="tab-bar cfm-tabbar">
        {availableChains.map(ch => {
          const count = chipData.filter(c => c.chain === ch).length;
          return (
            <button
              type="button"
              key={ch}
              className={`tab ${selectedChainTab === ch ? 'active' : ''}`}
              onClick={() => { setSelectedChainTab(ch); setActiveChain(ch); }}
              aria-pressed={selectedChainTab === ch}
            >
              Chain {ch}
              {count > 0 && (
                <span className="cfm-tab-count">
                  ({count})
                </span>
              )}
            </button>
          );
        })}
        </div>
      </div>

      <div className="hacker-inspector-body">
      {/* Color mode toggle */}
      <div className="cfm-mode-row">
        {(['frequency', 'temperature', 'health', 'errors', 'nonce_count'] as ColorMode[]).map(mode => (
          <button
            key={mode}
            className={`btn ${colorMode === mode ? 'btn-primary' : 'btn-secondary'} cfm-mode-btn`}
            onClick={() => setColorMode(mode)}
          >
            {mode === 'nonce_count' ? 'Nonce Count' : mode}
          </button>
        ))}
      </div>

      {/* Legend */}
      <div className="cfm-legend">
        {colorMode === 'frequency' && (
          <>
            <span><span className="cfm-sw" style={{ background: '#3b82f6' }} /> Low (&lt;50%)</span>
            <span><span className="cfm-sw" style={{ background: '#22c55e' }} /> Normal (50-70%)</span>
            <span><span className="cfm-sw" style={{ background: '#F7931A' }} /> High (70-85%)</span>
            <span><span className="cfm-sw" style={{ background: '#ef4444' }} /> Very High (&gt;85%)</span>
            <span><span className="cfm-sw" style={{ background: '#333' }} /> Dead</span>
          </>
        )}
        {colorMode === 'temperature' && (
          <>
            <span><span className="cfm-sw" style={{ background: 'rgb(0,40,255)' }} /> 40C</span>
            <span><span className="cfm-sw" style={{ background: 'rgb(128,40,128)' }} /> 55C</span>
            <span><span className="cfm-sw" style={{ background: 'rgb(255,40,0)' }} /> 70C+</span>
            <span><span className="cfm-sw" style={{ background: '#EAB308' }} /> graded (no die temp)</span>
          </>
        )}
        {colorMode === 'health' && (
          <ChipHealthLegend source={visibleHealthSource} compact />
        )}
        {colorMode === 'errors' && (
          <>
            <span><span className="cfm-sw" style={{ background: '#166534' }} /> 0 errors</span>
            <span><span className="cfm-sw" style={{ background: '#22c55e' }} /> 1-2</span>
            <span><span className="cfm-sw" style={{ background: '#EAB308' }} /> 3-9</span>
            <span><span className="cfm-sw" style={{ background: '#FF4444' }} /> 10+</span>
          </>
        )}
        {colorMode === 'nonce_count' && (
          <>
            <span><span className="cfm-sw" style={{ background: '#1a1a1a' }} /> 0</span>
            <span><span className="cfm-sw" style={{ background: '#1a4731' }} /> Low</span>
            <span><span className="cfm-sw" style={{ background: '#166534' }} /> Med</span>
            <span><span className="cfm-sw" style={{ background: '#22c55e' }} /> High</span>
            <span><span className="cfm-sw" style={{ background: '#86efac' }} /> Max</span>
          </>
        )}
      </div>

      {/* Per-chip die temp is null until firmware exposes it — surface honestly
          when the operator selects the temperature color mode. */}
      {colorMode === 'temperature' && hasChips && chainChips.every(c => c.temperature == null) && (
        <div className="cfm-source-note">
          Per-chip die temperature is not reported by this firmware. Cells are colored
          by per-chip health grade instead — no temperature is estimated.
        </div>
      )}

      {/* Loading / Error / Unavailable states */}
      {loading && (
        <div className="glass-card cfm-state-card">
          <div className="adv-state is-loading is-inline">Loading per-chip telemetry...</div>
        </div>
      )}

      {error && (
        <div className="glass-card cfm-state-card">
          <div className="cfm-state-msg cfm-state-err adv-mb-8">
            {error}
          </div>
          <button className="btn btn-secondary cfm-retry-btn" onClick={fetchData}>
            Retry
          </button>
        </div>
      )}

      {/* Honest sentinel — same contract as standard/ChipHeatMap. No fabricated
          grid is ever rendered when /api/chips returns nothing. */}
      {perChipUnavailable && (
        <div className="glass-card cfm-state-card">
          <div className="cfm-state-msg adv-mb-4" style={{ fontWeight: 700, color: 'var(--text)' }}>
            {glossary('telemetry_per_chip_unavailable').term}
          </div>
          <div className="adv-hint is-xs">
            The daemon did not return per-chip data from <code>/api/chips</code>. DCENT_OS
            only shows real per-chip values — it will not estimate or fabricate a map.
            Per-chip detail appears when the daemon publishes live or saved chip-health.
          </div>
        </div>
      )}

      {/* Main chip grid */}
      {hasChips && (
        <div className="glass-card adv-card-pad-20">
          {/* Chain summary bar */}
          {summary && (
            <div className="cfm-summary">
              <span className="cfm-summary-chain">
                Chain {selectedChainTab}
              </span>
              <span className="cfm-summary-stats">
                {summary.alive} chips | avg {summary.avgFreq.toFixed(0)} MHz | {summary.avgTemp != null ? `avg ${summary.avgTemp.toFixed(1)}C | ` : ''}{summary.totalNonces.toLocaleString()} nonces | {summary.totalErrors} err
              </span>
              {summary.dead > 0 && (
                <span className="cfm-summary-dead">{summary.dead} dead</span>
              )}
              {/* Grade distribution */}
              <span className="cfm-grade-dist">
                {gradeOrder.map(g => (summary.grades[g] ?? 0) > 0 && (
                  <span key={g} style={{ color: GRADE_COLORS[g] ?? '#888' }}>
                    {g}:{summary.grades[g]}
                  </span>
                ))}
              </span>
            </div>
          )}

          {chainChips.length === 0 ? (
            <div className="cfm-no-chips">
              No chips detected on Chain {selectedChainTab}
            </div>
          ) : (
            <div
              style={{
                display: 'grid',
                gridTemplateColumns: `repeat(${gridCols}, 1fr)`,
                gap: 3,
                maxWidth: gridCols * 44,
              }}
            >
              {chainChips.map(chip => (
                <button
                  type="button"
                  key={`${chip.chain}-${chip.chipIndex}`}
                  data-testid={`hacker-chip-health-cell-${chip.chain}-${chip.chipIndex}`}
                  data-health-tone={chip.healthTone}
                  data-health-source={chip.healthSource}
                  className={`advanced-chip-button cfm-cell ${activeChip === chip.chipIndex && activeChain === chip.chain ? 'is-active' : ''}`}
                  style={{
                    background: getCellColor(chip),
                    color: colorMode === 'health'
                      ? chipHealthTextColor(chip.healthTone)
                      : chip.status === 'dead' ? '#666' : 'rgba(255,255,255,0.85)',
                  }}
                  onMouseEnter={e => handleMouseEnter(chip, e)}
                  onMouseMove={handleMouseMove}
                  onMouseLeave={() => setHoveredChip(null)}
                  onClick={() => handleChipClick(chip)}
                  aria-label={`Chain ${chip.chain} chip ${chip.chipIndex}, ${CHIP_HEALTH_LABELS[chip.healthTone].toLowerCase()} health ${formatHealthPercent(chip.autotunerHealth?.health_score ?? chip.health_score)} ${chipHealthSourceLabel(chip.healthSource)}, ${chip.status}, ${chip.frequency} megahertz, ${chip.errors} errors`}
                >
                  {chip.status === 'dead' ? 'X' : chip.chipIndex}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Hover tooltip */}
      {hoveredChip && (
        <div
          className="cfm-tip"
          style={{ left: tooltipPos.x, top: tooltipPos.y }}
        >
          <div className="cfm-tip-title">
            Chain {hoveredChip.chain}, Chip {hoveredChip.chipIndex}
          </div>
          <div className="cfm-tip-grid">
            <span className="cfm-tip-k">Frequency:</span>
            <span>{hoveredChip.frequency} MHz</span>
            <span className="cfm-tip-k">Nonce Count:</span>
            <span>{hoveredChip.nonce_count.toLocaleString()}</span>
            <span className="cfm-tip-k">HW Errors:</span>
            <span style={{ color: hoveredChip.errors > 5 ? 'var(--red)' : 'var(--green)' }}>{hoveredChip.errors}</span>
            <span className="cfm-tip-k">Grade:</span>
            <span style={{ color: GRADE_COLORS[hoveredChip.grade] ?? '#888', fontWeight: 700 }}>{hoveredChip.grade}</span>
            <span className="cfm-tip-k">Health:</span>
            <span style={{ color: CHIP_HEALTH_COLORS[hoveredChip.healthTone], fontWeight: 700 }}>
              {formatHealthPercent(hoveredChip.autotunerHealth?.health_score ?? hoveredChip.health_score)}
            </span>
            <span className="cfm-tip-k">Source:</span>
            <span>{chipHealthSourceLabel(hoveredChip.healthSource)}</span>
            <span className="cfm-tip-k">Temp:</span>
            <span style={{ color: hoveredChip.temperature != null && hoveredChip.temperature > 65 ? 'var(--red)' : 'var(--text)' }}>{tempLabel(hoveredChip.temperature)}</span>
          </div>
        </div>
      )}

      {/* Selected chip detail popup */}
      {selectedChip && (
        <OverlayDialog
          open
          onClose={() => setSelectedChip(null)}
          ariaLabel="Selected chip details"
          initialFocusRef={closeChipDialogRef as React.RefObject<HTMLElement>}
          maxWidth={440}
        >
          <div className="glass-card cfm-detail">
            <div className="cfm-detail-head">
              <div className="cfm-detail-title">
                Chain {selectedChip.chain} - Chip {selectedChip.chipIndex}
              </div>
              <span
                className="cfm-detail-grade"
                style={{
                  color: GRADE_COLORS[selectedChip.grade] ?? '#888',
                  border: `1px solid ${GRADE_COLORS[selectedChip.grade] ?? '#888'}`,
                }}
              >
                Grade {selectedChip.grade}
              </span>
            </div>
            <div className="cfm-detail-body">
              <div>Frequency:   <span style={{ color: 'var(--accent)' }}>{selectedChip.frequency} MHz</span></div>
              <div>Die Temp:    <span style={{ color: selectedChip.temperature != null && selectedChip.temperature > 65 ? 'var(--red)' : 'var(--accent)' }}>{tempLabel(selectedChip.temperature)}</span></div>
              <div>Nonce Count: <span style={{ color: 'var(--accent)' }}>{selectedChip.nonce_count.toLocaleString()}</span></div>
              <div>HW Errors:   <span style={{ color: selectedChip.errors > 5 ? 'var(--red)' : 'var(--green)' }}>{selectedChip.errors}</span></div>
              <div>Health:      <span style={{ color: CHIP_HEALTH_COLORS[selectedChip.healthTone] }}>{formatHealthPercent(selectedChip.autotunerHealth?.health_score ?? selectedChip.health_score)}</span></div>
              <div>Source:      <span style={{ color: 'var(--accent)' }}>{chipHealthSourceLabel(selectedChip.healthSource)}</span></div>
              <div>Status:      <span style={{
                color: selectedChip.status === 'ok' ? 'var(--green)' : selectedChip.status === 'error' ? 'var(--red)' : 'var(--text-dim)',
              }}>{selectedChip.status.toUpperCase()}</span></div>
            </div>
            <div className="cfm-detail-foot">
              <button className="btn btn-secondary" onClick={() => {
                setActiveChain(selectedChip.chain);
                setActiveChip(selectedChip.chipIndex);
                setSelectedChip(null);
              }}>
                Set Active
              </button>
              <button ref={closeChipDialogRef} className="btn btn-secondary" onClick={() => setSelectedChip(null)}>Close</button>
            </div>
          </div>
        </OverlayDialog>
      )}
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>chain {selectedChainTab}</span>
          <span>{chipData.filter(c => c.chain === selectedChainTab).length} chips</span>
          <span>color: {colorMode === 'nonce_count' ? 'nonce count' : colorMode}</span>
          {colorMode === 'health' && <span>{chipHealthSourceLabel(visibleHealthSource)}</span>}
          <span>{perChipUnavailable ? 'unavailable' : `per-chip · ${source || 'live'}`}</span>
        </div>
      </footer>
    </div>
  );
}
