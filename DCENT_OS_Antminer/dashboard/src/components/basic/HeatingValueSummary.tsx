import React, { useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { getLiveDisplayWallWatts } from '../../utils/power';
import { glossaryText } from '../../utils/glossary';

export function HeatingValueSummary() {
  const heater = useMinerStore(s => s.heaterStatus);
  const status = useMinerStore(s => s.status);
  const settings = useMinerStore(s => s.settings);

  const [heatingMode, setHeatingMode] = useState(true); // Are you using this for heating?

  const stats = useMinerStore(s => s.stats);
  const hashrateGhs = status?.hashrate_ghs ?? heater?.hashrate_ghs ?? 0;
  // Economic cost must be live wall-power backed; display/model fallback watts
  // are fine for warmth estimates elsewhere, but not for billing math.
  const powerWatts = getLiveDisplayWallWatts(heater, stats?.power);
  const uptimeS = status?.uptime_s ?? 0;
  const hoursRunning = uptimeS / 3600;

  // Sats earned today in USD
  const satsToday = heater?.sats_today ?? 0;
  const satsUsd = (satsToday / 100_000_000) * settings.btcPrice;

  // Electricity cost: (watts / 1000) * hoursRunning * electricityRate
  const electricityCost = powerWatts > 0
    ? (powerWatts / 1000) * hoursRunning * settings.electricityRate
    : null;

  // Net value depends on heating mode:
  // If heating: electricity cost is offset (you'd pay it for a space heater anyway), so net = sats earned
  // If not heating: net = sats earned - electricity cost
  const netValue = heatingMode || electricityCost == null ? satsUsd : satsUsd - electricityCost;

  // HEATER-5: when the operator hasn't confirmed an electricity rate, the rate
  // is the daemon default guess — label any cost/net figure as an uncalibrated
  // estimate instead of presenting it as a confident dollar amount.
  const rateUncalibrated = settings.electricityRateCalibrated === false;

  // If not mining, show zero state
  const isMining = hashrateGhs > 0;

  return (
    <div className="heating-value-summary">
      <div className="heating-value-amount">
        ${isMining ? netValue.toFixed(2) : '0.00'}
      </div>
      <div
        className="heating-value-label"
        data-tooltip={glossaryText('net_value_offset')}
      >
        Today's net value
      </div>
      {isMining && (satsUsd > 0 || electricityCost != null) && (
        <div className="hv-breakdown">
          <div className="hv-row">
            <span>Bitcoin earned</span>
            <span className="hv-amount hv-amount--earn">+${satsUsd.toFixed(2)}</span>
          </div>
          <div className="hv-row">
            <span>Electricity cost{rateUncalibrated ? ' (uncalibrated estimate)' : ''}</span>
            <span className={`hv-amount${heatingMode ? ' hv-amount--offset' : ' hv-amount--cost'}`}>
              {electricityCost != null ? `-$${electricityCost.toFixed(2)}` : 'Unavailable'}
            </span>
          </div>
          {heatingMode && electricityCost != null && electricityCost > 0 && (
            <div id="heating-value-offset-note" className="hv-offset-note">
              Offset by heating -- you'd pay this for a space heater anyway
            </div>
          )}
          {!heatingMode && electricityCost == null && (
            <div id="heating-value-offset-note" className="hv-offset-note">
              Live wall-power unavailable; net excludes electricity cost.
            </div>
          )}
          <div className={`hv-row hv-row--total${netValue >= 0 ? ' is-positive' : ' is-negative'}`}>
            <span>Net</span>
            <span>${netValue >= 0 ? '+' : '-'}${Math.abs(netValue).toFixed(2)}</span>
          </div>
        </div>
      )}

      {/* Heating mode toggle */}
      {isMining && (
        <div
          className="hv-mode-toggle"
          data-tooltip={glossaryText('net_value_offset')}
        >
          <span>Using for heat?</span>
          <button
            type="button"
            className={`hv-switch${heatingMode ? ' is-on' : ''}`}
            onClick={() => setHeatingMode(!heatingMode)}
            role="switch"
            aria-checked={heatingMode}
            aria-label="Toggle heating mode for cost calculation"
            aria-describedby={(heatingMode && electricityCost != null && electricityCost > 0) || (!heatingMode && electricityCost == null) ? 'heating-value-offset-note' : undefined}
          >
            <span className="hv-switch__thumb" aria-hidden="true" />
          </button>
          <span className={`hv-mode-state${heatingMode ? ' is-yes' : ''}`}>
            {heatingMode ? 'Yes' : 'No'}
          </span>
        </div>
      )}
    </div>
  );
}
