// HalvingTimelineBar — visual halving countdown bar. Shows progress
// from the current epoch start (`floor(height / 210000) * 210000`) to
// the next halving (`epoch start + 210000`).
//
// Above the bar: blocks remaining + ~days ETA.
// Below the bar: era markers (era 0 / era 1 / era 2 / current / next)
// with tick marks on a faint horizontal line.
//
// Empty state when `currentHeight === null`. Pure SVG + DOM. No deps.

import React from 'react';

const HALVING_INTERVAL = 210_000;
const BLOCK_INTERVAL_SECONDS = 600;

interface Props {
  currentHeight: number | null;
}

function fmtDays(blocks: number): string {
  const days = (blocks * BLOCK_INTERVAL_SECONDS) / 86400;
  if (days < 1) {
    const hours = days * 24;
    if (hours < 1) {
      return `~${Math.max(1, Math.round(hours * 60))} min`;
    }
    return `~${hours.toFixed(1)} h`;
  }
  if (days < 10) return `~${days.toFixed(1)} days`;
  return `~${Math.round(days)} days`;
}

export function HalvingTimelineBar({ currentHeight }: Props) {
  if (currentHeight === null || !Number.isFinite(currentHeight)) {
    return (
      <div className="halving-timeline-bar empty" data-testid="halving-timeline-bar-empty">
        <div className="halving-timeline-bar-empty-msg">
          Halving timeline unavailable
        </div>
        <div className="halving-timeline-bar-empty-sub">
          Waiting for block height
        </div>
      </div>
    );
  }

  const epochIdx = Math.floor(currentHeight / HALVING_INTERVAL);
  const epochStart = epochIdx * HALVING_INTERVAL;
  const nextHalving = epochStart + HALVING_INTERVAL;
  const blocksIn = currentHeight - epochStart;
  const blocksLeft = nextHalving - currentHeight;
  const pct = Math.max(0, Math.min(100, (blocksIn / HALVING_INTERVAL) * 100));

  // Era markers — show 2 previous + current + next (= 4 ticks total on the
  // strip below the bar). When epoch index < 2, fall back to (0 .. current+1).
  const eras: number[] = [];
  const startEra = Math.max(0, epochIdx - 2);
  for (let i = startEra; i <= epochIdx + 1; i++) eras.push(i);

  return (
    <div className="halving-timeline-bar" data-testid="halving-timeline-bar">
      <div className="halving-timeline-bar-readout">
        <div className="halving-timeline-bar-blocks">
          <span className="halving-timeline-bar-blocks-num">
            {blocksLeft.toLocaleString()}
          </span>
          <span className="halving-timeline-bar-blocks-unit">blocks left</span>
        </div>
        <div className="halving-timeline-bar-eta">{fmtDays(blocksLeft)}</div>
      </div>

      <div
        className="halving-timeline-bar-track"
        role="progressbar"
        aria-valuenow={Math.round(pct)}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label="Halving epoch progress"
      >
        <div
          className="halving-timeline-bar-fill"
          style={{ width: `${pct}%` }}
        />
        <div
          className="halving-timeline-bar-marker"
          style={{ left: `${pct}%` }}
          aria-hidden="true"
        />
      </div>

      <div className="halving-timeline-bar-era-strip">
        <div className="halving-timeline-bar-era-line" aria-hidden="true" />
        {eras.map(era => {
          const isCurrent = era === epochIdx;
          const isNext = era === epochIdx + 1;
          // Position eras evenly across the strip.
          const pos = ((era - eras[0]) / Math.max(1, eras.length - 1)) * 100;
          return (
            <div
              key={era}
              className={`halving-timeline-bar-era-tick ${isCurrent ? 'current' : ''} ${isNext ? 'next' : ''}`}
              style={{ left: `${pos}%` }}
            >
              <div className="halving-timeline-bar-era-dot" />
              <div className="halving-timeline-bar-era-label">
                {isCurrent ? `Era ${era} · now` : isNext ? `Era ${era} · next` : `Era ${era}`}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export default HalvingTimelineBar;
