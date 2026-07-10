import React, { useState, useEffect } from 'react';
import { useMinerStore } from '../../store/miner';
import { api } from '../../api/client';
import type { AutotunerChipHealthStatus, ChipColor, ChipMapCell } from '../../api/types';
import { glossary } from '../../utils/glossary';
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

/**
 * COMP-CHIPSTRIP Level B — `ChipGrid` per-chip cell state (DCENT Design
 * Language — component-contract.md §4). This `ChipCellState` type ADVERTISES the
 * §4 closed per-chip vocabulary (shared with axe — axe's chips ARE its single
 * chain; the GRID render is `[OS-only]`) so the existing honest per-chip logic
 * is contract-legible; it is a type-level advertisement — no behavior change.
 *
 * §4 closed enum: `idle | active | warm | hot | error`. The honesty contract the
 * grid already enforces (named here per COMP-CHIPSTRIP):
 *   - `temp: number | null` — null renders the canonical empty glyph and the
 *     cell REFUSES to fabricate a temperature (colors by health GRADE instead;
 *     `chip.temp == null ? HEALTH_COLOR[chip.color]` in `chipColor`).
 *   - `present: boolean` — false ⇒ the cell is "not responding"; an all-empty
 *     `/api/chips` renders the honest "Per-chip telemetry unavailable" sentinel
 *     rather than fabricating cells.
 *   - `grade: "A"|"B"|"C"|"D"` — the shared derived-grade vocabulary (the GRID
 *     UI is `[OS-only]`).
 */
export type ChipCellState = 'idle' | 'active' | 'warm' | 'hot' | 'error';

type ColorMode = 'freq' | 'temp' | 'health' | 'errors';

export function chipLivenessPulseOrdinals(presentChips: number, pulse: number): number[] {
  if (presentChips <= 0) return [];
  const burst = Math.min(4, Math.max(1, Math.ceil(presentChips / 48)));
  const base = ((pulse % presentChips) + presentChips) % presentChips;
  const ordinals: number[] = [];

  for (let step = 0; step < burst; step++) {
    const ordinal = (base + step * 13) % presentChips;
    if (!ordinals.includes(ordinal)) ordinals.push(ordinal);
  }

  return ordinals;
}

// Per-chip cell shape used by this component, projected from the REAL
// `/api/chips` ChipMapCell. No fabrication — every value below comes from the
// honest backend snapshot (RE-010). `temp` is null until dcentrald exposes
// per-chip BM1362 die temp (the additive `die_temp_c` field, omitted from the
// wire when unavailable), so the temp mode degrades honestly rather than
// inventing a number.
interface ChipCell {
  index: number;
  freq: number;        // frequency_mhz
  temp: number | null; // die_temp_c (null = not yet exposed by firmware)
  errors: number;      // crc_errors
  health_score: number;
  grade: string;
  color: ChipColor;
  present: boolean;    // health_score > 0 / responding
  autotunerHealth: AutotunerChipHealthStatus | null;
  healthTone: ChipHealthTone;
  healthSource: ChipHealthSource;
}

// Map the honest ChipColor enum (Green/Yellow/Orange/Red/Gray) to the kit
// palette. Mirrors ChipColor::css_color() in dcentrald-diagnostics so the
// health coloring matches what the backend computed.
const HEALTH_COLOR: Record<ChipColor, string> = {
  Green: '#22C55E',
  Yellow: '#EAB308',
  Orange: '#F97316',
  Red: '#EF4444',
  Gray: '#333',
};

function chipColor(mode: ColorMode, chip: ChipCell): string {
  if (mode === 'health') {
    return CHIP_HEALTH_COLORS[chip.healthTone];
  }
  if (mode === 'freq') {
    if (chip.freq <= 0) return '#333';
    // 200=blue, 500=green, 700+=red
    const t = Math.max(0, Math.min(1, (chip.freq - 200) / 500));
    if (t < 0.5) {
      const r = Math.round(34 + t * 2 * (34));
      const g = Math.round(197 * t * 2);
      const b = Math.round(94 + (1 - t * 2) * 160);
      return `rgb(${r},${g},${b})`;
    }
    const r2 = Math.round(34 + (t - 0.5) * 2 * 205);
    const g2 = Math.round(197 - (t - 0.5) * 2 * 129);
    const b2 = Math.round(94 - (t - 0.5) * 2 * 26);
    return `rgb(${r2},${g2},${b2})`;
  }
  if (mode === 'temp') {
    // Per-chip die temp is null until firmware exposes it — color by health
    // grade instead of inventing a temperature gradient.
    if (chip.temp == null) return HEALTH_COLOR[chip.color];
    if (chip.temp <= 0) return '#333';
    // 30=blue, 55=green, 65=yellow, 75+=red
    if (chip.temp < 45) return '#22C55E';
    if (chip.temp < 60) return '#EAB308';
    if (chip.temp < 70) return '#F97316';
    return '#EF4444';
  }
  // errors
  if (chip.errors === 0) return '#22C55E';
  if (chip.errors < 3) return '#EAB308';
  return '#EF4444';
}

interface ChipHeatMapProps {
  chainIndex: number;
  chainId: number;
}

export function ChipHeatMap({ chainIndex, chainId }: ChipHeatMapProps) {
  const chains = useMinerStore(s => s.status?.chains ?? []);
  const [colorMode, setColorMode] = useState<ColorMode>('freq');
  const [hoveredChip, setHoveredChip] = useState<number | null>(null);
  const [expandedChip, setExpandedChip] = useState<number | null>(null);
  // Chain-level liveness pulse. The daemon does not expose per-chip nonce
  // deltas, so the pulse is deterministic and bounded by real present chips
  // instead of nondeterministic or presented as per-chip hashing telemetry.
  const [livenessPulse, setLivenessPulse] = useState(0);

  // REAL per-chip telemetry from GET /api/chips?chain=<id>. `null` while
  // loading, `'unavailable'` if the daemon doesn't expose it or returns no
  // cells, or the projected cells. We NEVER fabricate per-chip values.
  const [cells, setCells] = useState<ChipCell[] | null | 'unavailable'>(null);
  const [gridCols, setGridCols] = useState<number | null>(null);
  const expandedPanelId = `chip-heatmap-details-${chainId}`;

  const chain = chains[chainIndex];
  const chainHashing = !!chain && chain.hashrate_ghs > 0;

  // Fetch real per-chip data for this chain. Re-fetches when the chain
  // identity changes; a lightweight 10s refresh keeps it roughly live without
  // hammering the daemon. On error/empty → honest 'unavailable' (no fallback
  // to fabricated data).
  useEffect(() => {
    let cancelled = false;

    async function load() {
      try {
        const [snapshot, autotunerHealth] = await Promise.all([
          api.getChips(chainId),
          api.getAutotunerChipHealth().catch(() => null),
        ]);
        if (cancelled) return;
        const autotunerByChip = new Map<number, AutotunerChipHealthStatus>();
        for (const chip of autotunerHealth?.chips ?? []) {
          if (chip.chain_id === chainId) {
            autotunerByChip.set(chip.chip_index, chip);
          }
        }
        const snapChain = snapshot?.chains?.find(c => c.chain_id === chainId)
          ?? snapshot?.chains?.[0];
        const rawCells: ChipMapCell[] = snapChain?.chipmap?.cells ?? [];
        if (!snapshot || rawCells.length === 0) {
          setCells('unavailable');
          setGridCols(null);
          return;
        }
        const projected: ChipCell[] = rawCells
          .slice()
          .sort((a, b) => a.index - b.index)
          .map(c => {
            const present = c.health_score > 0;
            const autotunerChip = autotunerByChip.get(c.index) ?? null;
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
            return {
              index: c.index,
              freq: c.frequency_mhz,
              temp: c.die_temp_c ?? null,
              errors: c.crc_errors,
              health_score: c.health_score,
              grade: c.grade,
              color: c.color,
              present,
              autotunerHealth: autotunerChip,
              healthTone: useAutotuner ? autotunerTone : diagnosticsTone,
              healthSource: useAutotuner ? 'autotuner' : 'diagnostics',
            };
          });
        setCells(projected);
        setGridCols(snapChain?.chipmap?.columns && snapChain.chipmap.columns > 0
          ? snapChain.chipmap.columns
          : null);
      } catch {
        if (!cancelled) {
          setCells('unavailable');
          setGridCols(null);
        }
      }
    }

    setCells(null);
    setGridCols(null);
    load();
    const t = window.setInterval(load, 10000);
    return () => { cancelled = true; window.clearInterval(t); };
  }, [chainId]);

  const realCells = Array.isArray(cells) ? cells : [];
  const presentChips = realCells.filter(c => c.present).length;

  useEffect(() => {
    if (!chainHashing || presentChips <= 0) {
      setLivenessPulse(0);
      return;
    }
    const t = window.setInterval(() => {
      setLivenessPulse(pulse => (pulse + 1) % 4096);
    }, 480);
    return () => window.clearInterval(t);
  }, [chainHashing, presentChips]);

  if (!chain) return null;

  // ── Honest states: loading + unavailable ────────────────────────────────
  if (cells === null) {
    return (
      <div className="chipmap-shell" style={{
        background: 'var(--bg)', borderRadius: 'var(--radius-sm)',
        padding: 12, marginTop: 8,
      }}>
        <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)', fontStyle: 'italic' }}>
          Loading per-chip telemetry…
        </div>
      </div>
    );
  }

  if (cells === 'unavailable') {
    return (
      <div className="chipmap-shell" style={{
        background: 'var(--bg)', borderRadius: 'var(--radius-sm)',
        padding: 12, marginTop: 8,
      }}>
        <div style={{
          fontSize: '0.72rem', lineHeight: 1.45,
          color: 'var(--text-dim)',
          padding: '10px 12px',
          background: 'var(--card-bg)',
          borderRadius: 'var(--radius-sm)',
          border: '1px solid var(--border)',
        }}>
          <div style={{ fontWeight: 700, color: 'var(--text)', marginBottom: 4 }}>
            {glossary('telemetry_per_chip_unavailable').term}
          </div>
          Chain {chainId} did not return per-chip data from <code>/api/chips</code>.
          DCENT_OS only shows real per-chip values — it will not estimate or
          fabricate a heatmap. Per-chip detail appears when the daemon publishes
          live or saved chip-health for this chain.
        </div>
      </div>
    );
  }

  // ── Real data path ───────────────────────────────────────────────────────
  const chipData = realCells;
  const totalChips = chipData.length;
  const foundChips = presentChips;

  // Chip-count match: present/total ratio (from REAL cells, not estimated).
  const healthPct = totalChips > 0 ? Math.round((foundChips / totalChips) * 100) : 0;
  const healthColor = healthPct >= 95 ? 'var(--green)' : healthPct >= 80 ? 'var(--yellow)' : 'var(--red)';

  // Grid columns: prefer the backend's layout hint, else a 9-wide grid.
  const cols = gridCols ?? 9;

  const hoveredData = hoveredChip !== null ? chipData.find(c => c.index === hoveredChip) ?? null : null;
  const expandedData = expandedChip !== null ? chipData.find(c => c.index === expandedChip) ?? null : null;
  const livenessPulseOrdinals = new Set(chipLivenessPulseOrdinals(foundChips, livenessPulse));
  const presentOrdinalByChipIndex = new Map<number, number>();
  chipData.forEach(chip => {
    if (chip.present) presentOrdinalByChipIndex.set(chip.index, presentOrdinalByChipIndex.size);
  });

  const tempLabel = (t: number | null) => (t == null ? 'n/a' : `${t.toFixed(1)}C`);

  const legendLbl = colorMode === 'freq' ? 'MHz' : colorMode === 'temp' ? '°C' : 'errors';
  const legendMin = colorMode === 'freq' ? '200' : colorMode === 'temp' ? '45' : '0';
  const legendMax = colorMode === 'freq' ? '700+' : colorMode === 'temp' ? '75+' : '3+';

  // Per-chip die temp is null until firmware exposes it. Surface that honestly
  // when the operator selects the temperature color mode.
  const dieTempUnavailable = colorMode === 'temp' && chipData.every(c => c.temp == null);
  const visibleHealthSource: ChipHealthSource = chipData.some(c => c.healthSource === 'autotuner')
    ? 'autotuner'
    : 'diagnostics';

  return (
    <div className="chipmap-shell" style={{
      background: 'var(--bg)', borderRadius: 'var(--radius-sm)',
      padding: 12, marginTop: 8,
    }}>
      {/* Temperature mode honest note — per-chip die temp not yet on the wire */}
      {dieTempUnavailable && (
        <div style={{
          fontSize: '0.65rem', fontStyle: 'italic',
          color: '#D97706', lineHeight: 1.4,
          padding: '5px 8px', marginBottom: 8,
          background: 'rgba(217, 119, 6, 0.06)',
          borderRadius: 'var(--radius-sm)',
        }}>
          <span style={{ marginRight: 4 }}>&#9432;</span>
          Per-chip die temperature is not yet reported by this firmware. Cells are
          colored by per-chip health grade instead — no temperature is estimated.
        </div>
      )}

      {/* Header bar — kit .chipmap-bar / -lh / -rh */}
      <div className="chipmap-bar">
        <div className="chipmap-bar-lh">
          <span className="chipmap-chain-label" style={{
            color: 'var(--accent)', fontFamily: "var(--font-heading)",
          }}>
            Chain {chainId} Estimated Chip Map
          </span>
          <span className="chipmap-chain-detail">
            <span style={{ color: 'var(--text-dim)' }}>{foundChips}/{totalChips} chips</span>
            <span>·</span>
            <span style={{ color: healthColor, fontWeight: 600 }}>chip-count match {healthPct}% responding</span>
            {chainHashing && (
              <>
                <span>·</span>
                <span className="chipmap-ramp">
                  <span className="chipmap-ramp-dot" aria-hidden="true" />hashing
                </span>
              </>
            )}
          </span>
        </div>

        {/* Color mode tabs — kit .chipmap-mode-switch (dual-classed onto the
            production .time-tab hooks). */}
        <div className="chipmap-bar-rh">
          <div
            className="chipmap-mode-switch"
            role="group"
            aria-label={`Chain ${chainId} chip map color mode`}
          >
            {(['freq', 'temp', 'health', 'errors'] as ColorMode[]).map(mode => (
              <button
                key={mode}
                type="button"
                onClick={() => setColorMode(mode)}
                className={`chipmap-mode-btn time-tab ${colorMode === mode ? 'active' : ''}`}
                aria-pressed={colorMode === mode}
                aria-label={mode === 'freq'
                  ? 'Color chips by frequency'
                  : mode === 'temp'
                    ? 'Color chips by temperature'
                    : mode === 'health'
                      ? 'Color chips by health grade'
                      : 'Color chips by hardware errors'}
              >
                {mode === 'freq' ? 'Frequency' : mode === 'temp' ? 'Temperature' : mode === 'health' ? 'Health' : 'Errors'}
              </button>
            ))}
          </div>
        </div>
      </div>

      {/* Chip grid — kit .chipmap-grid + .chipmap-cell with in-cell num/val */}
      <div className="chipmap-grid" style={{
        display: 'grid',
        gridTemplateColumns: `repeat(${cols}, 1fr)`,
        gap: 2,
      }}>
        {chipData.map((chip, i) => {
          const isMissing = !chip.present;
          const bg = isMissing
            ? (colorMode === 'health' ? CHIP_HEALTH_COLORS['no-data'] : '#1a1a1a')
            : chipColor(colorMode, chip);
          // "Healthy" = present + not in error/hot state. Only healthy chips breathe.
          const isHealthy = !isMissing && (
            (colorMode === 'freq' && chip.freq > 0) ||
            (colorMode === 'temp' && (chip.temp == null ? chip.health_score >= 0.7 : (chip.temp > 0 && chip.temp < 65))) ||
            (colorMode === 'health' && chip.healthTone === 'healthy') ||
            (colorMode === 'errors' && chip.errors === 0)
          );
          const isExpanded = expandedChip === chip.index;
          const isFocused = hoveredChip === chip.index;
          const presentOrdinal = presentOrdinalByChipIndex.get(chip.index) ?? -1;
          const isFlash = !isMissing && chainHashing && livenessPulseOrdinals.has(presentOrdinal);
          const cellClass = [
            'chipmap-cell',
            'chip-heatmap-chip',
            'ds-chip-cell',
            isMissing ? 'ds-chip-missing' : 'ds-chip-active',
            isHealthy ? 'ds-chip-healthy' : '',
            isExpanded ? 'ds-chip-expanded' : '',
            isFocused ? 'focused' : '',
            isFlash ? 'flash' : '',
          ].filter(Boolean).join(' ');
          // Staggered breathing so chips don't pulse in sync; modulo keeps delay bounded.
          const delayMs = isHealthy ? (i * 53) % 2400 : 0;
          const cellTag = colorMode === 'freq'
            ? (chip.freq > 0 ? chip.freq : 'off')
            : colorMode === 'temp'
              ? (chip.temp == null ? chip.grade : (chip.temp > 0 ? chip.temp.toFixed(0) : 'off'))
              : colorMode === 'health'
                ? (chip.healthTone === 'no-data' ? 'n/d' : formatHealthPercent(chip.autotunerHealth?.health_score ?? chip.health_score))
              : String(chip.errors);
          const healthScoreLabel = formatHealthPercent(chip.autotunerHealth?.health_score ?? chip.health_score);
          const healthLabel = CHIP_HEALTH_LABELS[chip.healthTone].toLowerCase();
          const sourceLabel = chipHealthSourceLabel(chip.healthSource);
          return (
            <button
              key={chip.index}
              type="button"
              data-testid={`chip-health-cell-${chainId}-${chip.index}`}
              data-health-tone={chip.healthTone}
              data-health-source={chip.healthSource}
              className={cellClass}
              onMouseEnter={() => setHoveredChip(chip.index)}
              onMouseLeave={() => setHoveredChip(null)}
              onFocus={() => setHoveredChip(chip.index)}
              onBlur={() => setHoveredChip(current => (current === chip.index ? null : current))}
              onClick={() => setExpandedChip(expandedChip === chip.index ? null : chip.index)}
              aria-controls={expandedPanelId}
              aria-expanded={expandedChip === chip.index}
              aria-label={isMissing
                ? `Chip ${chip.index} not responding`
                : `Chip ${chip.index}, ${healthLabel} health ${healthScoreLabel} ${sourceLabel}, grade ${chip.grade}, ${chip.freq} megahertz, ${tempLabel(chip.temp)}, ${chip.errors} hardware errors`
              }
              style={{
                width: '100%',
                aspectRatio: '1',
                borderRadius: 3,
                background: bg,
                color: colorMode === 'health'
                  ? chipHealthTextColor(chip.healthTone)
                  : isMissing ? 'rgba(255,255,255,.4)' : ((chip.temp ?? 0) >= 65 || chip.errors >= 3 ? '#fff' : '#0a1a0a'),
                opacity: colorMode === 'health' ? 1 : isMissing ? 0.3 : (hoveredChip === chip.index ? 1 : 0.88),
                cursor: 'pointer',
                border: isExpanded ? '2px solid var(--accent)' : '1px solid transparent',
                minWidth: 0,
                padding: 0,
                animationDelay: `${delayMs}ms`,
                // Used by .ds-chip-healthy:hover for accent-aware glow halo.
                ['--ds-chip-color' as string]: bg,
              }}
              title={isMissing
                ? `Chip ${chip.index}: not responding`
                : `Chip ${chip.index}: ${healthLabel} health ${healthScoreLabel} ${sourceLabel}, grade ${chip.grade}, ${chip.freq} MHz, ${tempLabel(chip.temp)}, ${chip.errors} err`
              }
            >
              <span className="chipmap-cell-num">{chip.index.toString().padStart(2, '0')}</span>
              <span className="chipmap-cell-val">{isMissing ? '—' : cellTag}</span>
            </button>
          );
        })}
      </div>

      {/* Hover tooltip */}
      {hoveredData && (
        <div className="ds-chip-tooltip" style={{
          marginTop: 8, padding: '6px 10px',
          background: 'var(--card-bg)', borderRadius: 'var(--radius-sm)',
          border: '1px solid var(--border)',
          fontSize: '0.7rem',
          fontFamily: "'JetBrains Mono', monospace",
          fontVariantNumeric: 'tabular-nums',
          display: 'flex', gap: 16,
        }}>
          <span style={{ color: 'var(--text-dim)' }}>Chip {hoveredData.index}</span>
          <span>Grade: <span style={{ color: 'var(--accent)' }}>{hoveredData.grade}</span></span>
          <span>Health: <span style={{ color: CHIP_HEALTH_COLORS[hoveredData.healthTone] }}>{formatHealthPercent(hoveredData.autotunerHealth?.health_score ?? hoveredData.health_score)}</span></span>
          <span>Freq: <span style={{ color: 'var(--accent)' }}>{hoveredData.freq} MHz</span></span>
          <span>Temp: <span style={{ color: hoveredData.temp == null ? 'var(--text-dim)' : hoveredData.temp >= 65 ? 'var(--red)' : hoveredData.temp >= 55 ? 'var(--yellow)' : 'var(--green)' }}>{tempLabel(hoveredData.temp)}</span></span>
          <span>Errors: <span style={{ color: hoveredData.errors > 0 ? 'var(--red)' : 'var(--green)' }}>{hoveredData.errors}</span></span>
        </div>
      )}

      {/* Expanded chip detail */}
      {expandedData && (
        <div id={expandedPanelId} className="ds-chip-detail-panel" style={{
          marginTop: 8, padding: 12,
          background: 'var(--card-bg)', borderRadius: 'var(--radius-sm)',
          border: '1px solid var(--accent)',
          fontSize: '0.8rem',
          fontVariantNumeric: 'tabular-nums',
        }}>
          <div style={{
            fontWeight: 700, color: 'var(--accent)',
            fontFamily: "var(--font-heading)",
            marginBottom: 8,
          }}>
            Estimated Details - Chip {expandedData.index} (grade {expandedData.grade})
          </div>
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(4, minmax(0, 1fr))', gap: 8 }}>
            <div>
              <div style={{ fontSize: '0.65rem', color: 'var(--text-dim)', marginBottom: 2 }}>Frequency</div>
              <div style={{ fontFamily: "'JetBrains Mono', monospace", fontWeight: 700 }}>
                {expandedData.freq} MHz
              </div>
            </div>
            <div>
              <div style={{ fontSize: '0.65rem', color: 'var(--text-dim)', marginBottom: 2 }}>Die Temp</div>
              <div style={{
                fontFamily: "'JetBrains Mono', monospace", fontWeight: 700,
                color: expandedData.temp == null ? 'var(--text-dim)' : expandedData.temp >= 65 ? 'var(--red)' : expandedData.temp >= 55 ? 'var(--yellow)' : 'var(--green)',
              }}>
                {tempLabel(expandedData.temp)}
              </div>
            </div>
            <div>
              <div style={{ fontSize: '0.65rem', color: 'var(--text-dim)', marginBottom: 2 }}>HW Errors</div>
              <div style={{
                fontFamily: "'JetBrains Mono', monospace", fontWeight: 700,
                color: expandedData.errors > 0 ? 'var(--red)' : 'var(--green)',
              }}>
                {expandedData.errors}
              </div>
            </div>
            <div>
              <div style={{ fontSize: '0.65rem', color: 'var(--text-dim)', marginBottom: 2 }}>Health</div>
              <div style={{
                fontFamily: "'JetBrains Mono', monospace", fontWeight: 700,
                color: CHIP_HEALTH_COLORS[expandedData.healthTone],
              }}>
                {formatHealthPercent(expandedData.autotunerHealth?.health_score ?? expandedData.health_score)}
              </div>
              <div style={{ fontSize: '0.62rem', color: 'var(--text-dim)', marginTop: 2 }}>
                {chipHealthSourceLabel(expandedData.healthSource)}
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Legend — kit .chipmap-legend (gradient bar + min/max + hover detail) */}
      <div className="chipmap-legend">
        {colorMode === 'health' ? (
          <ChipHealthLegend source={visibleHealthSource} compact />
        ) : (
          <>
            <span className="chipmap-legend-lbl">{legendLbl}</span>
            <div className="chipmap-legend-bar" aria-hidden="true" />
            <span className="chipmap-legend-min">{legendMin}</span>
            <span className="chipmap-legend-max">{legendMax}</span>
          </>
        )}
        <span className="chipmap-hover-detail">
          {hoveredData ? (
            <>
              <strong>Chip {hoveredData.index.toString().padStart(2, '0')}</strong>
              <span>{formatHealthPercent(hoveredData.autotunerHealth?.health_score ?? hoveredData.health_score)}</span>
              <span>{hoveredData.freq} MHz</span>
              <span>{tempLabel(hoveredData.temp)}</span>
              <span>{hoveredData.errors} err</span>
            </>
          ) : (
            <span style={{ color: 'var(--text-dim)', fontStyle: 'italic' }}>
              hover a chip for detail{chainHashing ? ' · shimmer = chain active (decorative)' : ''}
            </span>
          )}
        </span>
      </div>
    </div>
  );
}
