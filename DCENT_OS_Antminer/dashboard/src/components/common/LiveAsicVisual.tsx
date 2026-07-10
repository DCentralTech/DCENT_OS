import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { useDashboardHealth } from '../../hooks/useDashboardHealth';
import { useRewardFx } from '../../fx/useRewardFx';
import { readFxSettings } from '../../fx/fxSettings';
import { formatHashrateShort, formatFrequency, formatVoltage } from '../../utils/format';
import { glossaryText } from '../../utils/glossary';
import { classifyBoardHealth } from '../../utils/health';
import type { ChainState } from '../../api/types';

type VisualVariant = 'standard' | 'hacker' | 'heater';

interface LiveAsicVisualProps {
  variant?: VisualVariant;
  compact?: boolean;
  title?: string;
  subtitle?: string;
  actionLabel?: string;
  onAction?: () => void;
}

type VisualChain = ChainState & {
  connector: string;
  placeholder?: boolean;
};

type ChainStyle = React.CSSProperties & {
  '--chain-load': string;
  '--chain-temp': string;
  '--chain-accent': string;
};

type ActivityStyle = React.CSSProperties & {
  '--dcfx-intensity': string;
  '--dcfx-asic-scale': string;
};

interface ActivityEnvelope {
  intensity: number;
  chainId?: number;
}

const CONNECTOR_LABELS = ['J6', 'J7', 'J8', 'J9'];
const ACTIVITY_DECAY_MS = 1500;
const POLLED_DELTA_THRESHOLD = 0.02;

function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(max, value));
}

function tempTone(tempC: number) {
  if (tempC <= 0) return { label: 'waiting', color: 'var(--text-dim, #78788a)', severity: 'standby' as const };
  if (tempC >= 80) return { label: 'critical', color: 'var(--red, #EF4444)', severity: 'critical' as const };
  if (tempC >= 72) return { label: 'hot', color: 'var(--red, #EF4444)', severity: 'hot' as const };
  if (tempC >= 65) return { label: 'warm', color: 'var(--yellow, #EAB308)', severity: 'warm' as const };
  return { label: 'nominal', color: 'var(--green, #22C55E)', severity: 'nominal' as const };
}

function statusTone(chain: VisualChain, unitIsMining: boolean) {
  if (chain.placeholder) return 'muted';
  // P0-3 truth-contract: derive the per-board tone from whether the board is
  // actually hashing while powered, NOT from the (often stale/mislabeled)
  // `status` string — the daemon reports a 0-GH/s dead board as "Active".
  const verdict = classifyBoardHealth(chain, unitIsMining);
  if (verdict === 'fault' || verdict === 'degraded') return 'danger';
  if (verdict === 'healthy') return 'live';
  return 'standby';
}

function makePlaceholderChains(): VisualChain[] {
  return [0, 1, 2].map((id) => ({
    id,
    connector: CONNECTOR_LABELS[id] ?? `J${id}`,
    chips: 0,
    frequency_mhz: 0,
    voltage_mv: 0,
    temp_c: 0,
    hashrate_ghs: 0,
    errors: 0,
    status: 'waiting',
    placeholder: true,
  }));
}

/**
 * Honest per-cell activity: caller-supplied hashrate ratio drives a fraction
 * of cells that are "hot". When the chain reports zero hashrate, NO cells
 * light up — we never fabricate hashing activity. We use a deterministic
 * pseudo-random pattern per (chain, index) so the lit cells don't visibly
 * "march" across re-renders (which would falsely suggest motion).
 */
function lit(chainId: number, idx: number, load: number): boolean {
  if (load <= 0) return false;
  // Stable hash to a [0,1) bucket.
  const h = ((chainId + 1) * 374761393 + idx * 668265263) >>> 0;
  const bucket = (h ^ (h >>> 13)) / 0xffffffff;
  return bucket < load;
}

function frameNow(): number {
  if (typeof performance !== 'undefined' && typeof performance.now === 'function') {
    return performance.now();
  }
  return Date.now();
}

function scheduleFrame(callback: FrameRequestCallback): number | null {
  if (typeof window === 'undefined') return null;
  if (typeof window.requestAnimationFrame === 'function') {
    return window.requestAnimationFrame(callback);
  }
  return window.setTimeout(() => callback(frameNow()), 16);
}

function cancelFrame(id: number | null): void {
  if (id === null || typeof window === 'undefined') return;
  if (typeof window.cancelAnimationFrame === 'function') {
    window.cancelAnimationFrame(id);
  }
  window.clearTimeout(id);
}

function useActivityEnvelope(): readonly [ActivityEnvelope, (intensity: number, chainId?: number) => void] {
  const [activity, setActivity] = useState<ActivityEnvelope>({ intensity: 0 });
  const frameRef = useRef<number | null>(null);

  const cancel = useCallback(() => {
    cancelFrame(frameRef.current);
    frameRef.current = null;
  }, []);

  useEffect(() => () => cancel(), [cancel]);

  const trigger = useCallback((nextIntensity: number, chainId?: number) => {
    const peak = clamp(nextIntensity, 0, 1);
    if (peak <= 0) return;
    if (typeof document !== 'undefined' && document.hidden) return;

    cancel();
    const startedAt = frameNow();
    setActivity({ intensity: peak, chainId });

    const tick: FrameRequestCallback = (now) => {
      const elapsed = Math.max(0, now - startedAt);
      if (elapsed >= ACTIVITY_DECAY_MS) {
        frameRef.current = null;
        setActivity({ intensity: 0 });
        return;
      }

      const remaining = 1 - elapsed / ACTIVITY_DECAY_MS;
      const intensity = peak * remaining * remaining;
      setActivity({ intensity: intensity < 0.015 ? 0 : intensity, chainId });
      frameRef.current = scheduleFrame(tick);
    };

    frameRef.current = scheduleFrame(tick);
  }, [cancel]);

  return [activity, trigger] as const;
}

export function LiveAsicVisual({
  variant = 'standard',
  compact = false,
  title,
  subtitle,
  actionLabel,
  onAction,
}: LiveAsicVisualProps) {
  const status = useMinerStore(s => s.status);
  const systemInfo = useMinerStore(s => s.systemInfo);
  const transport = useMinerStore(s => s.transport);
  const health = useDashboardHealth();
  const [activity, triggerActivity] = useActivityEnvelope();
  const previousPolledHashrateRef = useRef<number | null>(null);

  const chains = useMemo<VisualChain[]>(() => {
    const source = status?.chains ?? [];
    if (source.length === 0) return makePlaceholderChains();
    return source.map((chain, index) => ({
      ...chain,
      connector: CONNECTOR_LABELS[index] ?? `J${chain.id}`,
    }));
  }, [status?.chains]);

  const realChains = chains.filter(chain => !chain.placeholder);
  const chainCount = realChains.length;
  // Whole-unit "is any hashrate flowing" context for per-board health (P0-3).
  const unitIsMining = (status?.hashrate_ghs ?? 0) > 0
    || realChains.some(chain => (chain.hashrate_ghs ?? 0) > 0);
  const activeChains = realChains.filter(chain => statusTone(chain, unitIsMining) === 'live').length;
  const maxChainHashrate = Math.max(1, ...chains.map(chain => chain.hashrate_ghs));
  const totalHashrate = realChains.reduce((sum, chain) => sum + chain.hashrate_ghs, 0)
    || status?.hashrate_ghs
    || 0;
  const formattedHashrate = formatHashrateShort(totalHashrate);
  const totalChips = realChains.reduce((sum, chain) => sum + Math.max(0, chain.chips), 0);
  const maxTemp = realChains.reduce((max, chain) => Math.max(max, chain.temp_c || 0), 0);
  const temp = tempTone(maxTemp);
  const minerTone = health.minerChip.tone;
  const visualTitle = title ?? (variant === 'heater' ? 'Heat Core' : variant === 'hacker' ? 'ASIC Backplane' : 'ASIC Core');
  const visualSubtitle = subtitle ?? (
    chainCount > 0
      ? `${systemInfo?.chip_type ?? 'ASIC'} telemetry across ${chainCount} chain${chainCount === 1 ? '' : 's'}`
      : 'Waiting for first hashboard telemetry sample'
  );
  const live = activeChains > 0 || totalHashrate > 0;
  const fxSettings = readFxSettings();
  const allowCellPulse = fxSettings.enabled && fxSettings.vitality !== 'calm';
  const activityIntensity = live ? activity.intensity : 0;
  const activityStyle: ActivityStyle = {
    '--dcfx-intensity': activityIntensity.toFixed(3),
    '--dcfx-asic-scale': (1 + activityIntensity * 0.18).toFixed(3),
  };

  useRewardFx(useCallback((event) => {
    if (event.kind !== 'nonce-activity') return;
    if (event.intensity <= 0 || !live) return;
    triggerActivity(event.intensity, event.chainId);
  }, [live, triggerActivity]));

  useEffect(() => {
    const current = status?.hashrate_5s_ghs ?? totalHashrate;
    const previous = previousPolledHashrateRef.current;
    previousPolledHashrateRef.current = current;

    if (transport !== 'rest-polling') return;
    if (!live || current <= 0 || previous === null || previous <= 0) return;

    const delta = Math.abs(current - previous) / Math.max(current, previous, 1);
    if (delta < POLLED_DELTA_THRESHOLD) return;
    triggerActivity(clamp(delta * 1.5, 0.08, 0.6));
  }, [live, status?.hashrate_5s_ghs, totalHashrate, transport, triggerActivity]);

  // Any chain hotter than the warm threshold (>=65C) draws a yellow tint —
  // honest signal, not decoration.
  const showThrottleTint = temp.severity === 'warm' || temp.severity === 'hot' || temp.severity === 'critical';

  return (
    <section
      className={`live-asic-visual live-asic-visual--${variant}${compact ? ' live-asic-visual--compact' : ''}`}
      aria-label={`${visualTitle}: ${visualSubtitle}`}
      data-state={live ? 'live' : 'standby'}
      data-thermal={temp.severity}
    >
      <header className="live-asic-header">
        <div className="live-asic-heading">
          <span className="live-asic-eyebrow">{variant === 'hacker' ? 'silicon://live' : 'Silicon telemetry'}</span>
          <h3>{visualTitle}</h3>
          <p>{visualSubtitle}</p>
        </div>
        <div className="live-asic-actions">
          <span className={`live-asic-status ${minerTone}`}>
            <span className={live ? 'live-asic-pulse is-live' : 'live-asic-pulse'} aria-hidden="true" />
            {health.minerChip.label}
          </span>
          {onAction && actionLabel && (
            <button type="button" className="live-asic-action" onClick={onAction}>
              {actionLabel}
            </button>
          )}
        </div>
      </header>

      {/* F1 owns the `.dcm-asic-glow` keyframe (motion.css §18). We only WIRE
          it: `.is-mining` is applied ONLY when there is real mining activity,
          so the warm halo breathe is a truthful "this silicon is earning"
          signal — never decoration on an idle/standby unit. */}
      <div
        className={`live-asic-stack dcm-asic-glow${live ? ' is-mining' : ''}${activityIntensity > 0 ? ' dcfx-asic-active' : ''}`}
        style={activityStyle}
        aria-hidden="true"
      >
        {chains.map((chain) => {
          const load = chain.hashrate_ghs > 0
            ? clamp(chain.hashrate_ghs / maxChainHashrate, 0.08, 1)
            : statusTone(chain, unitIsMining) === 'live'
              ? 0.12
              : 0;
          const dotCount = chain.placeholder
            ? 18
            : clamp(Math.round((chain.chips || 63) / 4), 10, 32);
          // The chip grid is a REPRESENTATIVE visualization, not 1:1 per chip:
          // a 126-chip chain still renders ≤32 cells. Flag that honestly (the
          // footer carries the real chip count) so a cell is never mistaken
          // for a physical chip. `chips per cell` is surfaced via title/aria.
          const isRepresentative = !chain.placeholder && chain.chips > dotCount;
          const chipsPerCell = isRepresentative
            ? Math.max(1, Math.round(chain.chips / dotCount))
            : 1;
          const chainTemp = tempTone(chain.temp_c);
          const style: ChainStyle = {
            '--chain-load': `${Math.round(load * 100)}%`,
            '--chain-temp': `${clamp(chain.temp_c / 90, 0, 1) * 100}%`,
            '--chain-accent': chainTemp.color,
          };
          const sTone = statusTone(chain, unitIsMining);
          const isLive = sTone === 'live';

          return (
            <div
              key={chain.id}
              className={`live-asic-chain ${sTone}`}
              style={style}
              data-thermal={chainTemp.severity}
            >
              <div className="live-asic-chain-meta">
                <span>{chain.connector}</span>
                <strong>CH{chain.id}</strong>
                <span>{chain.placeholder ? 'awaiting link' : chain.status}</span>
              </div>
              <div
                className="live-asic-chip-grid"
                title={isRepresentative
                  ? `Representative grid — each cell ≈ ${chipsPerCell} chips (${chain.chips} total on this chain)`
                  : undefined}
                aria-label={isRepresentative
                  ? `Representative chip activity, each cell about ${chipsPerCell} chips`
                  : undefined}
              >
                {Array.from({ length: dotCount }).map((_, index) => {
                  const on = lit(chain.id, index, load);
                  const fxActive = on
                    && activityIntensity > 0
                    && (activity.chainId === undefined || activity.chainId === chain.id);
                  const phaseDelay = isLive && on
                    ? `${(index * 173 + chain.id * 71) % 900}ms`
                    : undefined;
                  return (
                    <span
                      key={index}
                      className={on ? `is-active${fxActive ? ' dcfx-asic-cell-active' : ''}` : undefined}
                      data-pulse={allowCellPulse && isLive && on ? 'on' : undefined}
                      style={phaseDelay ? ({
                        animationDelay: phaseDelay,
                      } as React.CSSProperties) : undefined}
                    />
                  );
                })}
              </div>
              <div className="live-asic-chain-readout">
                <span>{chain.hashrate_ghs > 0 ? `${formatHashrateShort(chain.hashrate_ghs).value} ${formatHashrateShort(chain.hashrate_ghs).unit}` : 'standby'}</span>
                <span>{chain.temp_c > 0 ? `${chain.temp_c.toFixed(0)}C ${chainTemp.label}` : 'temp --'}</span>
                <span>{chain.chips > 0 ? `${chain.chips} chips` : 'chips --'}</span>
              </div>
              <div className="live-asic-chain-rail">
                <span />
              </div>
            </div>
          );
        })}

        {showThrottleTint && (
          <div
            className={`live-asic-thermal-tint live-asic-thermal-tint--${temp.severity}`}
            aria-hidden="true"
          />
        )}
      </div>

      <footer className="live-asic-footer">
        <span><strong>{chainCount > 0 ? `${activeChains}/${chainCount}` : '--'}</strong> chains</span>
        <span><strong>{totalChips > 0 ? totalChips.toLocaleString() : '--'}</strong> chips</span>
        <span
          data-tooltip={glossaryText('hashrate_local_vs_pool')}
          data-tooltip-pos="top"
        ><strong>{totalHashrate > 0 ? `${formattedHashrate.value} ${formattedHashrate.unit}` : '--'}</strong> hashrate</span>
        <span
          data-tooltip={glossaryText('temp_die_vs_board')}
          data-tooltip-pos="top"
        ><strong>{maxTemp > 0 ? `${maxTemp.toFixed(0)}C` : '--'}</strong> max temp</span>
        {variant === 'hacker' && realChains[0] && (
          <span>
            <strong>{formatFrequency(realChains[0].frequency_mhz)} / {formatVoltage(realChains[0].voltage_mv)}</strong> ch0 setpoint
          </span>
        )}
      </footer>

    </section>
  );
}
