// NetworkContextCard — Bitcoin network telemetry for the Standard
// dashboard. Local-first companion to CurrentBlockCard:
//
//   • Halving Countdown — blocks remaining, ~days, subsidy + era
//   • Difficulty Retarget — epoch position bar, ~days to retarget
//   • Mempool Fees — sat/vB bands when an oracle is connected,
//     graceful placeholder otherwise
//   • Network Hashrate — placeholder until dcentrald exposes a value
//
// Pure visual; all math comes from useNetworkContext. No external HTTP
// is initiated from this file — DCENT_OS dashboards are local-first.

import React from 'react';
import { useNetworkContext } from '../../hooks/useNetworkContext';
import { MempoolFeeRadial } from './MempoolFeeRadial';
import { NetworkHashrateChart } from './NetworkHashrateChart';
import { HalvingTimelineBar } from './HalvingTimelineBar';

const DASH = '—';

function fmtInt(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return DASH;
  return Math.round(value).toLocaleString();
}

function fmtDays(days: number | null): string {
  if (days === null || !Number.isFinite(days) || days < 0) return DASH;
  if (days < 1) {
    const hours = days * 24;
    if (hours < 1) {
      const minutes = Math.max(1, Math.round(hours * 60));
      return `~${minutes} min`;
    }
    return `~${hours.toFixed(1)} h`;
  }
  if (days < 10) return `~${days.toFixed(1)} days`;
  return `~${Math.round(days)} days`;
}

function fmtSubsidy(btc: number | null): string {
  if (btc === null || !Number.isFinite(btc)) return DASH;
  if (btc >= 1) return `${btc.toFixed(3).replace(/\.?0+$/, '')} BTC`;
  // Trim trailing zeros while keeping satoshi precision.
  return `${btc.toFixed(8).replace(/\.?0+$/, '')} BTC`;
}

function fmtPct(pct: number | null): string {
  if (pct === null || !Number.isFinite(pct)) return DASH;
  return `${pct.toFixed(1)}%`;
}

function feeBandLabel(band: 'low' | 'medium' | 'high' | null): string {
  if (band === 'low') return 'Low';
  if (band === 'medium') return 'Medium';
  if (band === 'high') return 'High';
  return DASH;
}

function feeBandTone(band: 'low' | 'medium' | 'high' | null): string {
  if (band === 'low') return 'network-context-fee-low';
  if (band === 'medium') return 'network-context-fee-medium';
  if (band === 'high') return 'network-context-fee-high';
  return '';
}

export function NetworkContextCard() {
  const ctx = useNetworkContext();

  const heightKnown = ctx.blockHeight !== null;

  const halvingBig = heightKnown ? fmtInt(ctx.halvingBlocksRemaining) : DASH;
  const halvingEta = heightKnown ? fmtDays(ctx.halvingEtaDays) : 'waiting for block height';
  const subsidyLine = heightKnown
    ? `Subsidy ${fmtSubsidy(ctx.subsidyBtc)} · Era ${ctx.eraIndex}`
    : 'Subsidy unavailable';

  const epochValue = heightKnown
    ? `${fmtInt(ctx.epochPosition)} / ${(2016).toLocaleString()}`
    : DASH;
  const retargetEta = heightKnown
    ? `Adjustment in ${fmtDays(ctx.retargetEtaDays)}`
    : 'waiting for block height';
  const epochProgressPct = ctx.epochProgressPct ?? 0;

  const fastestFee = ctx.feeFastestSatVb;
  const halfHourFee = ctx.feeHalfHourSatVb;
  const hourFee = ctx.feeHourSatVb;
  const feesBandTone = feeBandTone(ctx.feeBand);

  return (
    <div
      className="network-context-card"
      data-testid="network-context-card"
      aria-label="Bitcoin network context"
    >
      <div className="network-context-head">
        <span className="network-context-kicker">
          <span className="network-context-kicker-glyph" aria-hidden="true" />
          Bitcoin Network Context
        </span>
        {heightKnown ? (
          <span className="network-context-height" data-testid="network-context-block-height">
            #{ctx.blockHeight!.toLocaleString()}
          </span>
        ) : (
          <span className="network-context-height network-context-height-fallback">
            {ctx.loading ? 'loading' : 'no tip'}
          </span>
        )}
      </div>

      <div className="network-context-grid">
        {/* ── Halving countdown ────────────────────────────────── */}
        <div
          className="network-context-cell"
          data-testid="network-context-halving"
        >
          <span className="network-context-label">Halving Countdown</span>
          <div className="network-context-value" data-testid="network-context-halving-blocks">
            {halvingBig} <span className="network-context-unit">blocks</span>
          </div>
          <div className="network-context-sub" data-testid="network-context-halving-eta">
            {halvingEta}
          </div>
          <HalvingTimelineBar currentHeight={ctx.blockHeight} />
          <div className="network-context-meta" data-testid="network-context-halving-subsidy">
            {subsidyLine}
          </div>
        </div>

        {/* ── Difficulty retarget ──────────────────────────────── */}
        <div
          className="network-context-cell"
          data-testid="network-context-retarget"
        >
          <span className="network-context-label">Difficulty Retarget</span>
          <div className="network-context-value" data-testid="network-context-retarget-position">
            {epochValue} <span className="network-context-unit">blocks</span>
          </div>
          <div
            className="network-context-progress"
            role="progressbar"
            aria-valuenow={Math.round(epochProgressPct)}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-label="Difficulty epoch progress"
            data-testid="network-context-retarget-progress"
          >
            <div
              className="network-context-progress-fill"
              style={{ width: `${Math.max(0, Math.min(100, epochProgressPct))}%` }}
            />
          </div>
          <div className="network-context-sub" data-testid="network-context-retarget-eta">
            {retargetEta}
          </div>
          <div className="network-context-meta">
            Epoch {fmtPct(ctx.epochProgressPct)} complete
          </div>
        </div>

        {/* ── Mempool fees ─────────────────────────────────────── */}
        <div
          className={`network-context-cell ${feesBandTone}`}
          data-testid="network-context-fees"
        >
          <span className="network-context-label">Mempool Fees</span>
          {ctx.feesAvailable ? (
            <>
              <MempoolFeeRadial
                fees={{
                  fastest: fastestFee,
                  halfHour: halfHourFee,
                  hour: hourFee,
                }}
              />
              <div
                className="network-context-value-hidden"
                data-testid="network-context-fees-fastest"
                aria-hidden="true"
                style={{ display: 'none' }}
              >
                {fastestFee !== null ? fastestFee : DASH}
              </div>
              <div className="network-context-sub" data-testid="network-context-fees-band">
                {feeBandLabel(ctx.feeBand)} priority
              </div>
            </>
          ) : (
            <>
              <MempoolFeeRadial fees={null} />
              <div
                className="network-context-value-hidden"
                data-testid="network-context-fees-fastest"
                aria-hidden="true"
                style={{ display: 'none' }}
              >
                {DASH}
              </div>
              <div className="network-context-sub">No fee oracle connected</div>
              <div className="network-context-hint">
                Connect a fee oracle in Settings to surface mempool sat/vB bands.
              </div>
            </>
          )}
        </div>

        {/* ── Network hashrate ─────────────────────────────────── */}
        <div
          className="network-context-cell"
          data-testid="network-context-network-hashrate"
        >
          <span className="network-context-label">Network Hashrate</span>
          {/* No oracle time series is wired yet, so feed the chart the on-device
              estimate derived from the live block difficulty (≈ difficulty ·
              2^32 / 600 s). The chart renders it as a single honest value
              explicitly labeled an estimate — never as a fake sparkline/trend.
              When difficulty is unknown it falls back to the honest empty
              state. `data={[]}` because there is no real series to chart. */}
          <NetworkHashrateChart
            data={[]}
            estimate={
              ctx.networkHashrateEhEstimate !== null
                ? { eh: ctx.networkHashrateEhEstimate }
                : null
            }
          />
        </div>
      </div>

      <div className="network-context-footer">
        <span>Local-first · Computed on-device</span>
        {ctx.error ? (
          <span className="network-context-footer-warn" data-testid="network-context-error">
            {ctx.error}
          </span>
        ) : null}
      </div>
    </div>
  );
}

export default NetworkContextCard;
