import React from 'react';
import { useMinerStore } from '../../store/miner';
import { formatNoise } from '../../utils/format';
import {
  wattsToBtu, noiseComparison,
  btuComparison, btuHeaterCount, estimateDailyCost, estimateDailySats,
} from '../../utils/thermal';
import { getDisplayPowerWatts, getLiveDisplayWallWatts, getPowerTargetingLabel, getPowerTelemetryLabel } from '../../utils/power';
import { getTachBackedNoiseDb, noiseUnavailableNote } from '../../utils/noise';
import { useValueFlash } from '../../hooks/useValueFlash';
import { glossaryText } from '../../utils/glossary';
import { HeaterEarningProof } from './HeaterEarningProof';

/**
 * Full heat-output + economics summary. The "are you actually earning?"
 * verdict was extracted into HeaterEarningProof so heater-home can place it in
 * the hero's right column; HeaterStatus (used on heater-history) still leads
 * with it for continuity, then shows the BTU hero + cost/sats/noise/net cards.
 * heater-home does NOT render HeaterStatus — those cards would duplicate the
 * hero's BigReadouts + EarningCard.
 */
export function HeaterStatus() {
  const heater = useMinerStore(s => s.heaterStatus);
  const settings = useMinerStore(s => s.settings);

  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const hasConnection = status != null || heater != null;

  // Use heater API values when available, fallback to standard mining data
  const statsPower = stats?.power;
  const displayPower = getDisplayPowerWatts(heater, statsPower);
  const liveWallPower = getLiveDisplayWallWatts(heater, statsPower);
  const unitPower = heater?.power_watts ?? statsPower?.watts ?? 0;
  const powerTelemetryLabel = getPowerTelemetryLabel(statsPower ?? heater);
  const powerTargetingLabel = getPowerTargetingLabel(statsPower ?? heater);
  const btuIsLive = liveWallPower > 0;
  const btu = btuIsLive
    ? wattsToBtu(liveWallPower)
    : heater?.btu_h ?? (displayPower > 0 ? wattsToBtu(displayPower) : 0);
  const hashrate = status?.hashrate_ghs ?? heater?.hashrate_ghs ?? 0;
  const isMining = hashrate > 0;

  // Power source indicator
  const powerSource = statsPower && 'source' in statsPower ? statsPower.source : 'estimated';

  const noise = getTachBackedNoiseDb(heater);

  const sats = heater?.sats_today ?? 0;
  const satsUsd = (sats / 100_000_000) * settings.btcPrice;

  // Daily cost estimate requires live wall-power telemetry. Display/model
  // fallback watts can support heat estimates, but not billing or net math.
  const dailyCost = liveWallPower > 0 ? estimateDailyCost(liveWallPower, settings.electricityRate) : null;
  // HEATER-5: until the operator confirms an electricity rate, the rate is the
  // daemon default — label cost/net figures as an uncalibrated estimate.
  const rateUncalibrated = settings.electricityRateCalibrated === false;

  // Daily BTC estimate from current hashrate.
  // P0-4: anchored to the backend-reported network difficulty (canonical); 0
  // when difficulty is unknown so we never show a fabricated figure.
  const dailySats = hashrate > 0 ? estimateDailySats(hashrate, heater?.network_difficulty) : 0;
  const dailyBtcUsd = (dailySats / 100_000_000) * settings.btcPrice;

  // Net cost: electricity cost minus reported-or-estimated BTC value.
  const netCost = dailyCost != null ? dailyCost - dailyBtcUsd : null;

  // Standby: connected but not mining and no real power draw
  const isStandby = hasConnection && !isMining && displayPower === 0;
  const netIsSavings = netCost != null && netCost <= 0;

  // Noise display value: only show backend RPM-backed estimates.
  const noiseDisplay = noise != null && noise > 0 ? noise : 0;
  const noiseLabel = noiseDisplay > 0 ? formatNoise(noiseDisplay) : '';

  // Comparisons
  const btuCompare = btu > 0 ? btuComparison(btu) : '';
  const noiseCompare = noiseDisplay > 0 ? noiseComparison(noiseDisplay) : '';

  // Flash classes for live KPI updates
  const btuFlashClass = useValueFlash(btu > 0 ? btu : null);
  const satsFlashClass = useValueFlash(sats > 0 ? sats : null);

  return (
    <div className="heater-status-section" role="region" aria-label="Heat output and cost summary">
      {/* "Are you actually earning?" reassurance — extracted to its own
          component (heater-home renders it in the hero right column). */}
      <HeaterEarningProof />

      {/* Hero BTU/h Card */}
      <div className="glass-card btu-hero-card">
        <div className="btu-hero-header">
          {isMining && (
            <div className="flame-icon-animated ds-flame-flicker">
              <svg width="28" height="28" viewBox="0 0 24 24" fill="var(--accent, #FAA500)" stroke="none">
                <path d="M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.38-.5-2-1-3-1.072-2.143-.224-4.054 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.153.433-2.294 1-3a2.5 2.5 0 0 0 2.5 2.5z" />
              </svg>
            </div>
          )}
          <span
            className="btu-hero-label"
            data-tooltip={glossaryText('btu_per_hour')}
          >
            {btuIsLive ? 'HEAT OUTPUT' : 'HEAT OUTPUT EST.'}
          </span>
          {powerSource !== 'estimated' && (
            <span
              className="btu-power-source"
              data-tooltip={glossaryText('wall_power')}
            >
              {powerSource}
            </span>
          )}
        </div>
        <div
          className={`btu-hero-value ${btuFlashClass}`.trim()}
          aria-live="polite"
          aria-label={btu > 0
            ? `${btu.toLocaleString()} ${btuIsLive ? 'BTU per hour' : 'estimated BTU per hour'}`
            : isStandby ? 'Standby, minimal heat output' : 'No heat output'}
        >
          {/* HEATER-6: no fabricated standby constant — show an em-dash and an
              honest "Standby — minimal draw" caption instead of a fixed ~85. */}
          {btu > 0
            ? btu.toLocaleString()
            : isStandby
              ? '—'
              : hasConnection
                ? '0'
                : '—'
          }
        </div>
        <div className="btu-hero-unit" aria-hidden="true">
          {btuIsLive ? 'BTU/h' : 'BTU/h est.'}
        </div>
        {btuCompare && (
          <div className="btu-hero-comparison">{btuCompare}</div>
        )}
        {/*
          No heat output yet — keep the hero shell populated with an honest
          caption instead of a bare "0"/"—" so the card never reads as broken
          or empty while telemetry is still arriving.
        */}
        {btu <= 0 && !isStandby && (
          <div className="btu-hero-comparison">
            {hasConnection ? 'Waiting for heat telemetry' : 'Heater telemetry unavailable'}
          </div>
        )}
        {isStandby && (
          <div className="btu-hero-comparison">Standby — minimal draw</div>
        )}
        <div className="btu-hero-watts">
          {/* HEATER-6: no fabricated ~25 W standby constant. Live power reads
              as wall draw; display/model fallback reads as an estimate. */}
          {liveWallPower > 0
            ? `${liveWallPower.toLocaleString()} W wall`
            : displayPower > 0
              ? `${displayPower.toLocaleString()} W estimate`
            : isStandby
              ? 'Minimal draw'
              : hasConnection
                ? '0 W'
                : '-- W'
          }
        </div>
        {displayPower > 0 && unitPower > 0 && (
          <div className="btu-hero-comparison">
            Miner unit {unitPower.toLocaleString()} W before PSU losses
          </div>
        )}
        {powerTelemetryLabel && (
          <div className="btu-hero-comparison">
            {powerTelemetryLabel}
          </div>
        )}
        {powerTargetingLabel && (
          <div className="btu-hero-comparison">
            {powerTargetingLabel}
          </div>
        )}
        {statsPower && 'watt_cap' in statsPower && statsPower.watt_cap && (
          <div className="btu-hero-comparison">
            Circuit cap {statsPower.watt_cap.cap_watts.toLocaleString()} W: {statsPower.watt_cap.utilization_pct.toFixed(0)}% used
            {statsPower.watt_cap.throttling
              ? `, throttling ${Math.round(statsPower.watt_cap.overage_watts || 0)} W over`
              : `, ${Math.round(statsPower.watt_cap.headroom_watts)} W headroom`}
          </div>
        )}
      </div>

      {/* Cost + Earnings Row */}
      <div className="status-cards">
        <div
          className="glass-card status-card border-orange"
          data-tooltip={glossaryText('daily_cost')}
        >
          <div className="value">
            {dailyCost != null ? `$${dailyCost.toFixed(2)}` : (hasConnection ? '—' : '--')}
          </div>
          <div className="label">Daily Cost</div>
          <div className="card-comparison">
            {dailyCost != null
              ? `${(liveWallPower / 1000).toFixed(2)} kWh @ $${settings.electricityRate}/kWh${rateUncalibrated ? ' · uncalibrated estimate' : ''}`
              : displayPower > 0
                ? 'Live wall power unavailable for cost'
                : ''}
          </div>
        </div>
        <div
          className="glass-card status-card border-amber"
          data-tooltip={
            sats > 0
              ? 'Sats (satoshis = 1/100,000,000 BTC) the pool has credited you today. This is reported earnings, not a projection.'
              : glossaryText('sats_estimate')
          }
        >
          <div className={`value sats-value ${satsFlashClass}`.trim()}>
            {sats > 0 ? sats.toLocaleString() : dailySats > 0 ? `~${dailySats.toLocaleString()}` : (hasConnection ? '0' : '--')}
          </div>
          <div className="unit">{sats > 0 ? 'sats today' : 'sats/day projected'}</div>
          <div className="label">{sats > 0 ? 'BTC Reported Today' : 'BTC Projection'}</div>
          {isMining && sats === 0 && dailySats === 0 && (
            <div className="card-comparison">Solo mining -- earning on block find</div>
          )}
          {(satsUsd > 0 || dailyBtcUsd > 0) && (
            <div className="card-comparison">${(satsUsd > 0 ? satsUsd : dailyBtcUsd).toFixed(2)} USD</div>
          )}
        </div>
        <div
          className="glass-card status-card border-blue"
          data-tooltip={glossaryText('cut_hash_before_noise')}
        >
          <div className="value">
            {noiseDisplay > 0 ? noiseDisplay : (hasConnection ? 'RPM' : '--')}
          </div>
          <div className="unit">
            {noiseLabel ? `dB - ${noiseLabel}` : 'unavailable'}
          </div>
          <div className="label">Noise</div>
          {noiseCompare ? (
            <div className="card-comparison">{noiseCompare}</div>
          ) : (
            <div className="card-comparison">{noiseUnavailableNote(heater)}</div>
          )}
        </div>
        <div
          className={`glass-card status-card ${netCost == null ? 'border-orange' : netIsSavings ? 'border-green' : 'border-red'}`}
          data-tooltip={glossaryText('net_value_offset')}
        >
          <div className={`value ${netCost == null ? '' : netIsSavings ? 'net-positive' : 'net-negative'}`.trim()}>
            {netCost != null
              ? `${netIsSavings ? '+' : '-'}$${Math.abs(netCost).toFixed(2)}`
              : hasConnection ? '—' : '--'
            }
          </div>
          <div className="label">{netCost == null ? 'Net Pending' : netIsSavings ? 'Net Savings' : 'Net Cost'}</div>
          <div className="card-comparison">
            {netCost != null
              ? `per day (electricity - BTC)${rateUncalibrated ? ' · uncalibrated estimate' : ''}`
              : displayPower > 0
                ? 'Live wall power unavailable for net cost'
                : ''}
          </div>
        </div>
      </div>
    </div>
  );
}
