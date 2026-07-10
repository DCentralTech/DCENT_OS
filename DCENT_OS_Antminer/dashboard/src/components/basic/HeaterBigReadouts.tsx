import React from 'react';
import { useMinerStore } from '../../store/miner';
import { wattsToBtu } from '../../utils/thermal';
import { formatNoise } from '../../utils/format';
import { getDisplayPowerWatts, getLiveDisplayWallWatts, getPowerTelemetryLabel } from '../../utils/power';
import { getTachBackedNoiseDb, noiseUnavailableNote } from '../../utils/noise';
import { Tooltip } from '../common/Tooltip';
import { glossaryText } from '../../utils/glossary';

/**
 * The three at-a-glance readouts the operator explicitly asked to "see very
 * evidently": Heat output (BTU/h), Wall power (W), Noise (dB). Sits directly
 * under the dial + sensor selector — the design's "BigReadout" row.
 *
 * Values are computed exactly like HeaterStatus (same utils, same honest
 * fallbacks) so the two surfaces never disagree. HeaterStatus keeps the
 * richer cost/earning economics below; this row is the glance layer.
 */
export function HeaterBigReadouts() {
  const heater = useMinerStore(s => s.heaterStatus);
  const stats = useMinerStore(s => s.stats);
  const status = useMinerStore(s => s.status);
  const settings = useMinerStore(s => s.settings);

  const hasConnection = status != null || heater != null;
  const displayPower = getDisplayPowerWatts(heater, stats?.power);
  const liveWallPower = getLiveDisplayWallWatts(heater, stats?.power);
  const powerTelemetryLabel = getPowerTelemetryLabel(heater ?? stats?.power);
  const btuIsLive = liveWallPower > 0;
  const btu = btuIsLive
    ? wattsToBtu(liveWallPower)
    : heater?.btu_h ?? (displayPower > 0 ? wattsToBtu(displayPower) : 0);

  const noise = getTachBackedNoiseDb(heater);

  const kwThermal = displayPower > 0 ? displayPower / 293 : 0; // 1 kW thermal ≈ 293 W·(BTU basis)
  const kwhDay = liveWallPower > 0 ? (liveWallPower * 24) / 1000 : 0;
  const costDay = kwhDay * (settings.electricityRate || 0);

  return (
    // Kit `nest-big-row` of `nest-big-readout`s (HeaterMode.jsx:53-61 /
    // styles.css:2781). Dual-classed with the production `heater-big*` hooks
    // the loaded skin already pins, so the coordinator skin can also address
    // the canonical kit classes.
    <div className="heater-big-row nest-big-row" role="group" aria-label="Heat output, power, and noise">
      <Tooltip term="btu_per_hour" placement="bottom">
        <div className="heater-big nest-big-readout tone-accent" tabIndex={0}>
          <span className="heater-big-eyebrow nest-big-eyebrow">
            {btuIsLive ? 'Heat output' : 'Heat output estimate'}
          </span>
          <strong className="heater-big-value nest-big-value">
            {btu > 0 ? btu.toLocaleString() : hasConnection ? '0' : '—'}
            <small>{btuIsLive ? ' BTU/h' : ' BTU/h est.'}</small>
          </strong>
          <span className="heater-big-foot nest-big-foot">
            {displayPower > 0
              ? `≈ ${kwThermal.toFixed(1)} kW thermal${btuIsLive ? '' : ' estimate'}`
              : 'Waiting for heat telemetry'}
          </span>
        </div>
      </Tooltip>

      <Tooltip term="wall_power" placement="bottom">
        <div className="heater-big nest-big-readout tone-primary" tabIndex={0}>
          <span className="heater-big-eyebrow nest-big-eyebrow">Live wall power</span>
          <strong className="heater-big-value nest-big-value">
            {liveWallPower > 0 ? Math.round(liveWallPower).toLocaleString() : '—'}
            <small> W</small>
          </strong>
          <span className="heater-big-foot nest-big-foot">
            {liveWallPower > 0
              ? `${kwhDay.toFixed(1)} kWh/day · $${costDay.toFixed(2)}`
              : powerTelemetryLabel ?? 'Waiting for live wall telemetry'}
          </span>
        </div>
      </Tooltip>

      <Tooltip content={glossaryText('cut_hash_before_noise')} placement="bottom">
        <div className="heater-big nest-big-readout tone-green" tabIndex={0}>
          <span className="heater-big-eyebrow nest-big-eyebrow">Noise</span>
          <strong className="heater-big-value nest-big-value">
            {noise != null && noise > 0 ? noise : hasConnection ? 'RPM' : '—'}
            <small>{noise != null && noise > 0 ? ' dB' : ''}</small>
          </strong>
          <span className="heater-big-foot nest-big-foot">
            {noise != null && noise > 0
              ? formatNoise(noise)
              : noiseUnavailableNote(heater)}
          </span>
        </div>
      </Tooltip>
    </div>
  );
}
