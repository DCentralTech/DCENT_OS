import React from 'react';
import { useMinerStore } from '../../store/miner';
import {
  wattsToBtu, estimateDailyCost, estimateDailySats,
} from '../../utils/thermal';
import { getDisplayPowerWatts, getLiveDisplayWallWatts } from '../../utils/power';
import { glossaryText } from '../../utils/glossary';

/**
 * "Earning sats while heating" card — emits the kit `nest-earning` grammar
 * (styled by handoff-skin-heater.css) to match HeaterMode.jsx's
 * `EarningCard`, but every number is REAL store data computed with the
 * EXACT same helpers HeaterStatus.tsx already uses:
 *
 *   - sats today        → `heaterStatus.sats_today` (server-reported; the
 *                          same field SatsCounter/HeaterStatus read). When
 *                          0 but mining, an explicit "~projected" estimate
 *                          via `estimateDailySats` (clearly labelled).
 *   - BTC value (USD)   → sats / 1e8 * settings.btcPrice (HeaterStatus calc).
 *   - heating cost      → `estimateDailyCost(livePower, settings.electricityRate)`
 *                          where livePower = `getLiveDisplayWallWatts(heater, stats)`.
 *                          Display/model fallback watts can show heat estimates
 *                          but cannot drive billing or net-offset math.
 *   - net heat cost     → cost − btcValue (HeaterStatus `netCost`).
 *
 * Truth-contract copy: no mandatory-fee/devfee framing; honest empty/standby
 * states; reported vs projected stays explicitly labelled; net offset is the
 * canonical `net_value_offset` glossary contract.
 */
export function HeaterEarningCard() {
  const heater = useMinerStore(s => s.heaterStatus);
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const settings = useMinerStore(s => s.settings);

  const hasConnection = status != null || heater != null;

  // Display heat estimates can use modeled/display power, but electricity cost
  // and net-offset math require live wall-power provenance.
  const statsPower = stats?.power;
  const displayPower = getDisplayPowerWatts(heater, statsPower);
  const liveWallPower = getLiveDisplayWallWatts(heater, statsPower);

  const hashrate = status?.hashrate_ghs ?? heater?.hashrate_ghs ?? 0;
  const isMining = hashrate > 0;

  // Reported sats (server) — the same field SatsCounter/HeaterStatus read.
  const reportedSats = heater?.sats_today ?? 0;
  // Honest projection when nothing is reported yet but the miner is hashing.
  // P0-4: anchored to the backend-reported network difficulty (canonical); 0
  // when difficulty is unknown so we never show a fabricated figure.
  const projectedSats = isMining ? estimateDailySats(hashrate, heater?.network_difficulty) : 0;
  const usingProjection = reportedSats <= 0 && projectedSats > 0;
  const visibleSats = reportedSats > 0 ? reportedSats : projectedSats;
  const satsUsd = (visibleSats / 100_000_000) * settings.btcPrice;

  // Heating electricity cost (canonical HeaterStatus calc).
  const dailyCost = liveWallPower > 0 ? estimateDailyCost(liveWallPower, settings.electricityRate) : 0;
  // HEATER-5: flag cost/net as an uncalibrated estimate until the operator
  // confirms an electricity rate (the default is a daemon guess).
  const rateUncalibrated = settings.electricityRateCalibrated === false;

  // Net heat cost (HeaterStatus `netCost`): electricity − BTC value.
  const net = dailyCost - satsUsd;

  // Offset bar: how much of today's heating bill the BTC earnings cover.
  const offsetPct = dailyCost > 0
    ? Math.min(100, Math.max(0, Math.round((satsUsd / dailyCost) * 100)))
    : 0;

  const btuIsLive = liveWallPower > 0;
  const btu = btuIsLive
    ? wattsToBtu(liveWallPower)
    : heater?.btu_h ?? (displayPower > 0 ? wattsToBtu(displayPower) : 0);
  const btuUnit = btuIsLive ? 'BTU/h' : 'BTU/h est.';

  // ── Honest empty state — no power telemetry yet ───────────────────────
  if (displayPower <= 0 && visibleSats <= 0) {
    return (
      <div
        className="nest-card nest-earning"
        data-tooltip={glossaryText('net_value_offset')}
      >
        <div className="nest-earning-eyebrow">
          <SatsGlyph /> Earning sats while heating
        </div>
        <div className="nest-earning-amount">
          <span className="nest-earning-usd">$0.00</span>
          <span className="nest-earning-sats">
            {hasConnection ? 'No earnings yet today' : 'Heater telemetry unavailable'}
          </span>
        </div>
        <div className="nest-earning-rows">
          <div>
            <span>How this works</span>
            <strong>—</strong>
          </div>
          <div className="nest-earning-net">
            <span>
              {hasConnection
                ? 'Start heating to offset your electricity with Bitcoin earnings.'
                : 'Connect to the miner to see live earnings.'}
            </span>
            <strong>—</strong>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div
      className="nest-card nest-earning"
      data-tooltip={glossaryText('net_value_offset')}
    >
      <div className="nest-earning-eyebrow">
        <SatsGlyph /> Earning sats while heating
      </div>
      <div className="nest-earning-amount">
        <span className="nest-earning-usd">${satsUsd.toFixed(2)}</span>
        <span className="nest-earning-sats">
          {visibleSats.toLocaleString()} sats {usingProjection ? '/day projected' : 'today'}
        </span>
      </div>
      <div
        className="nest-earning-bar"
        role="img"
        aria-label={
          dailyCost > 0
            ? `Bitcoin earnings offset ${offsetPct}% of today's heating cost`
            : 'Heating cost pending live wall-power data'
        }
      >
        <div className="nest-earning-bar-fill" style={{ width: `${offsetPct}%` }} />
        <span className="nest-earning-bar-label">
          {dailyCost > 0
            ? `Offsets ${offsetPct}% of today's heating bill`
            : `${btu > 0 ? `${btu.toLocaleString()} ${btuUnit}` : 'Heating'} — cost pending live wall power`}
        </span>
      </div>
      <div className="nest-earning-rows">
        <div>
          <span>Bitcoin {usingProjection ? 'projected' : 'earned'}</span>
          <strong style={{ color: 'var(--green)' }}>+${satsUsd.toFixed(2)}</strong>
        </div>
        <div>
          <span>
            {dailyCost > 0
              ? `Heating electricity${rateUncalibrated ? ' (uncalibrated estimate)' : ''}`
              : 'Heating electricity (live power unavailable)'}
          </span>
          <strong>{dailyCost > 0 ? `−$${dailyCost.toFixed(2)}` : '—'}</strong>
        </div>
        <div className="nest-earning-net">
          <span>Net heat cost</span>
          {dailyCost > 0 ? (
            <strong style={{ color: net < 0 ? 'var(--green)' : 'var(--accent)' }}>
              ${Math.abs(net).toFixed(2)} {net < 0 ? 'saved' : 'net'}
            </strong>
          ) : (
            <strong>{'—'}</strong>
          )}
        </div>
      </div>
    </div>
  );
}

// Kit HeaterMode.jsx `HeaterIcon.sats` (18x18, stroke 1.6).
function SatsGlyph() {
  return (
    <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M12 1v22M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6" />
    </svg>
  );
}
