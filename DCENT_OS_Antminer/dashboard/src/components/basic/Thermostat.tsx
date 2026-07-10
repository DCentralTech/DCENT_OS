import React from 'react';
import { useMinerStore } from '../../store/miner';
import { toDisplayTemp, tempUnitSymbol } from '../../utils/thermal';
import { getDisplayPowerWatts, getLiveDisplayWallWatts } from '../../utils/power';
import { glossaryText } from '../../utils/glossary';

interface ThermostatProps {
  onToggle?: () => void;
  isMining?: boolean;
  toggling?: boolean;
  powerControlSupported?: boolean;
}

/*
 * READ-ONLY heat dial (kit `ThermoDial` LOOK — HeaterMode.jsx:119-202 +
 * styles.css:1604-1633), kept faithful to the 340px / -210°→+30° geometry,
 * dual breathing halos, and center treatment.
 *
 * HONESTY CONTRACT (HEATER-1): the daemon exposes NO room-temperature
 * setpoint endpoint. The only backed warmth target is preset/watts via
 * `api.setHeaterTarget` (driven by the preset tiles / PowerPresets). The
 * `/api/home/room-temp` endpoint is the MEASURED ambient INPUT channel (fed
 * by HeaterSensorSource so the daemon can compute heat delivery) — NOT a
 * setpoint. The prior dial wrote its "target temperature" into that measured
 * channel, which INVERTS the daemon's power factor (raising the target made
 * the heater run LESS) and also showed a fabricated hardcoded 21° default.
 *
 * So this dial no longer pretends to be a temperature thermostat: it is a
 * read-only gauge. The arc + thumb-marker reflect the display power-draw
 * fraction ("how hard it's running"), the center shows the REAL measured room
 * temperature (chip-temp fallback, both honestly labelled, or an em-dash when
 * neither is available), and warmth is set with the presets. The center block
 * stays the real, backed start/stop affordance (via `onToggle`); tapping the
 * big readout still switches the °C/°F display preference.
 */

// Kit dial geometry — verbatim from HeaterMode.jsx ThermoDial.
const DIAL_SIZE = 340;
const DIAL_CX = DIAL_SIZE / 2; // 170
const DIAL_CY = DIAL_SIZE / 2; // 170
const DIAL_R = 144;
const DIAL_STROKE = 12;
const ARC_START = -210; // degrees (SVG/math convention: 0° = +x, CW = +y)
const ARC_END = 30;
const ARC_SWEEP = ARC_END - ARC_START; // 240

// Display ceiling for the power-draw ring fill. A UI scaling constant for the
// arc only — NOT telemetry shown to the operator.
const MAX_POWER_W = 1500;

/** Point on the dial circle at a given angle (degrees, kit convention). */
function dialXY(deg: number, radius = DIAL_R): [number, number] {
  const rad = (deg * Math.PI) / 180;
  return [DIAL_CX + radius * Math.cos(rad), DIAL_CY + radius * Math.sin(rad)];
}

/** Fraction [0..1] → angle on the arc. */
function fracToAngle(frac: number): number {
  return ARC_START + ARC_SWEEP * Math.max(0, Math.min(1, frac));
}

export function Thermostat({ onToggle, isMining: isMiningProp, toggling, powerControlSupported = true }: ThermostatProps) {
  const status = useMinerStore(s => s.status);
  const heaterStatus = useMinerStore(s => s.heaterStatus);
  const settings = useMinerStore(s => s.settings);
  const stats = useMinerStore(s => s.stats);
  const updateSettings = useMinerStore(s => s.updateSettings);

  const tempUnit = settings.temperatureUnit;

  // Average chip temperature across chains (honest — real telemetry).
  const chipTemp = status?.chains && status.chains.length > 0
    ? status.chains.reduce((sum, c) => sum + c.temp_c, 0) / status.chains.length
    : 0;

  const hasRoomTemp = heaterStatus?.room_temp_c != null;
  const roomTempC = heaterStatus?.room_temp_c ?? 0;

  // Display power drives the read-only heat ring. Live wall power gets a plain
  // watt label; display/model fallback is visibly marked as an estimate.
  const displayPower = getDisplayPowerWatts(heaterStatus, stats?.power);
  const liveWallPower = getLiveDisplayWallWatts(heaterStatus, stats?.power);
  const fillFrac = Math.min(1, Math.max(0, displayPower / MAX_POWER_W));
  const powerText = liveWallPower > 0
    ? `${Math.round(liveWallPower)} W`
    : displayPower > 0
      ? `${Math.round(displayPower)} W est.`
      : '';

  const isMiningLocal = (status?.hashrate_ghs ?? 0) > 0;
  const isMining = isMiningProp ?? isMiningLocal;
  const heating = isMining;
  const canToggle = Boolean(onToggle) && powerControlSupported;

  // Heat-ring color ramp by power draw (low→hot).
  const ringColorLow = '#3B82F6';
  const ringColorMid = '#FAA500';
  const ringColorHigh = '#EF4444';
  function getRingColors(): [string, string] {
    if (fillFrac <= 0.4) return [ringColorLow, ringColorMid];
    if (fillFrac <= 0.7) return [ringColorMid, ringColorMid];
    return [ringColorMid, ringColorHigh];
  }
  const [gradStart, gradEnd] = getRingColors();

  // ── Kit arc math — driven by the display power-draw fraction ──────────
  const fillAngle = fracToAngle(fillFrac);
  const [sx, sy] = dialXY(ARC_START);
  const [ex, ey] = dialXY(ARC_END);
  const [tx, ty] = dialXY(fillAngle);
  const largeArcBg = ARC_SWEEP > 180 ? 1 : 0;
  const largeArcFg = (fillAngle - ARC_START) > 180 ? 1 : 0;

  // ── Display values — real measured room temp (chip-temp fallback) ─────
  const unitSym = tempUnitSymbol(tempUnit);
  const roomDisplay = hasRoomTemp ? toDisplayTemp(roomTempC, tempUnit) : null;
  const chipDisplay = chipTemp > 0 ? toDisplayTemp(chipTemp, tempUnit) : null;

  const centerKind: 'room' | 'chip' | 'none' =
    roomDisplay != null ? 'room' : chipDisplay != null ? 'chip' : 'none';
  const centerValue = roomDisplay != null ? roomDisplay : chipDisplay;
  const centerEyebrow =
    centerKind === 'room' ? 'Room temp' : centerKind === 'chip' ? 'Chip temp' : 'Temperature';

  const handleTempToggle = () => {
    updateSettings({ temperatureUnit: tempUnit === 'C' ? 'F' : 'C' });
  };

  // Active preset label (real backed warmth target — set via presets).
  const presetLabel = heaterStatus?.preset
    ? heaterStatus.preset.charAt(0).toUpperCase() + heaterStatus.preset.slice(1)
    : null;

  // State pill text — honest. Only claims heating when telemetry confirms.
  const stateText = heating ? 'Heating · earning sats' : 'Idle';

  return (
    <div className="thermostat-container">
      {/*
        Kit `nest-dial` (styles.css:1604) — a 340px positioned box. Production
        keeps `thermostat`/`mining-glow` (the loaded skin pins the dual halo +
        glow to those) and adds `nest-dial` so the coordinator skin can address
        the kit class. This dial is a read-only gauge: there are no pointer/drag
        handlers because there is no setpoint to set.
      */}
      <div
        className={`thermostat nest-dial${heating ? ' mining-glow' : ''}`}
        style={{ width: DIAL_SIZE, height: DIAL_SIZE }}
      >
        {/* Kit dual breathing halos (styles.css:1605-1607) — mining only. */}
        {heating && (
          <>
            <div className="nest-dial-halo nest-dial-halo-1" aria-hidden="true" />
            <div className="nest-dial-halo nest-dial-halo-2" aria-hidden="true" />
          </>
        )}

        {/* SVG dial — kit geometry verbatim. role="none" keeps the SVG off the
            AT tree; the interactive start/stop affordance is the center block. */}
        <svg
          role="none"
          className="thermostat-ring"
          width={DIAL_SIZE}
          height={DIAL_SIZE}
          viewBox={`0 0 ${DIAL_SIZE} ${DIAL_SIZE}`}
        >
          <defs>
            {/* Vibrant low→hot ramp (3-stop diagonal). gradStart/gradEnd stay
                the display power-draw ramp; the mid anchor is D-Central orange. */}
            <linearGradient id="heatGradientDynamic" x1="0%" y1="100%" x2="100%" y2="0%">
              <stop offset="0%" stopColor={gradStart} />
              <stop offset="55%" stopColor="#FAA500" />
              <stop offset="100%" stopColor={gradEnd} />
            </linearGradient>
            {/* EVEN premium halo — a blur applied to a clone of the EXACT arc
                (via <use href="#htFgArc">), so the bloom hugs the arc. */}
            <filter id="hdialArcGlow" x="-60%" y="-60%" width="220%" height="220%">
              <feGaussianBlur stdDeviation="7" />
            </filter>
          </defs>

          {/* Quiet concentric depth ring — a barely-there inner hairline. */}
          <circle
            cx={DIAL_CX}
            cy={DIAL_CY}
            r={DIAL_R - 34}
            fill="none"
            stroke="rgba(255,255,255,.05)"
            strokeWidth="1.5"
            className="hdial-inner-ring"
          />

          {/* Background track — one crisp, high-contrast rail. */}
          <path
            d={`M ${sx} ${sy} A ${DIAL_R} ${DIAL_R} 0 ${largeArcBg} 1 ${ex} ${ey}`}
            fill="none"
            strokeWidth={DIAL_STROKE}
            strokeLinecap="round"
            className="hdial-track"
          />

          {/* Power-draw arc — defined ONCE as #htFgArc, painted twice from the
              same geometry so the glow can never drift: (a) an even blurred halo
              clone while heating, (b) the crisp gradient stroke on top. Length
              tracks the display power-draw fraction. */}
          {fillFrac > 0 && (
            <>
              <path
                id="htFgArc"
                d={`M ${sx} ${sy} A ${DIAL_R} ${DIAL_R} 0 ${largeArcFg} 1 ${tx} ${ty}`}
                fill="none"
                stroke="url(#heatGradientDynamic)"
                strokeWidth={DIAL_STROKE}
                strokeLinecap="round"
              />
              {heating && (
                <use
                  href="#htFgArc"
                  className="hdial-arc-halo"
                  filter="url(#hdialArcGlow)"
                  aria-hidden="true"
                />
              )}
              {/* crisp arc paints LAST so it sits sharply on top of its halo */}
              <use href="#htFgArc" className="heat-ring-fill" />
            </>
          )}

          {/* Non-interactive power-draw marker (read-only — there is no
              setpoint to drag). Sits at the head of the real power-draw arc. */}
          <circle
            cx={tx}
            cy={ty}
            r={11}
            fill="url(#heatGradientDynamic)"
            stroke="#FFFFFF"
            strokeWidth={2}
            className="thermostat-thumb"
            style={{ transition: 'cx 0.3s, cy 0.3s' }}
            aria-hidden="true"
          />
        </svg>

        {/* Center — kit `nest-dial-inner` (styles.css:1608). The whole inner
            block is the start/stop affordance (production contract preserved);
            it shows the REAL measured room temp (chip fallback) + state pill +
            an honest "set warmth with presets" line. */}
        <div
          className="thermostat-inner nest-dial-inner"
          onClick={canToggle && !toggling ? onToggle : undefined}
          onKeyDown={(e) => {
            if ((e.key === ' ' || e.key === 'Enter') && canToggle && !toggling && onToggle) {
              e.preventDefault();
              onToggle();
            }
          }}
          role={canToggle ? 'button' : undefined}
          tabIndex={canToggle ? 0 : undefined}
          aria-label={canToggle ? (isMining ? 'Tap to stop heater' : 'Tap to start heater') : 'Heater status'}
          style={{ cursor: canToggle ? (toggling ? 'wait' : 'pointer') : undefined, opacity: toggling ? 0.6 : 1 }}
        >
          {/* Kit `nest-dial-eyebrow` (styles.css:1613) — honest source label. */}
          <div className="temp-label nest-dial-eyebrow">{centerEyebrow}</div>

          {/* Kit `nest-dial-temp` (styles.css:1614) — the big readout. Real
              measured room temp (chip fallback), or an em-dash. Tap switches
              the °C/°F display preference (production affordance preserved). */}
          <div
            className={`temp-value nest-dial-temp${heating ? ' heating' : ''}`}
            onClick={centerValue != null ? (e) => { e.stopPropagation(); handleTempToggle(); } : undefined}
            onKeyDown={centerValue != null ? (e) => {
              if (e.key === ' ' || e.key === 'Enter') {
                e.preventDefault();
                e.stopPropagation();
                handleTempToggle();
              }
            } : undefined}
            role={centerValue != null ? 'button' : undefined}
            tabIndex={centerValue != null ? 0 : undefined}
            data-tooltip={
              centerKind === 'room'
                ? 'Measured room temperature. Tap to switch between °C and °F.'
                : centerKind === 'chip'
                  ? glossaryText('temp_die_vs_board')
                  : undefined
            }
            aria-label={
              centerValue != null
                ? `${centerEyebrow} ${centerValue.toFixed(1)} degrees. Click to switch unit.`
                : 'Temperature unavailable'
            }
            style={{ cursor: centerValue != null ? 'pointer' : undefined }}
          >
            {centerValue != null ? centerValue.toFixed(0) : '—'}
            {centerValue != null && <span className="temp-unit nest-dial-unit">{unitSym}</span>}
          </div>

          {/* Kit `nest-dial-state` pill (styles.css:1623) */}
          <div className={`nest-dial-state ${heating ? 'on' : 'off'}`}>
            <span className="nest-dial-state-dot" aria-hidden="true" />
            {stateText}
          </div>

          {/* Kit `nest-dial-room` (styles.css:1633) — repurposed honestly: the
              real warmth target is the active preset, set via the presets, NOT
              this dial. */}
          <div
            className="temp-label temp-secondary nest-dial-room"
            data-tooltip={glossaryText('cut_hash_before_noise')}
          >
            {heating
              ? `${presetLabel ?? 'Custom'}${powerText ? ` · ${powerText}` : ''}`
              : 'Set warmth with presets'}
          </div>

          {/* Start/stop affordance hint — production contract preserved.
              Honest about hardware support. */}
          {canToggle ? (
            <div
              className="temp-label thermostat-start-hint"
              data-tooltip={glossaryText('quiet_boot')}
            >
              {isMining ? 'Tap to stop' : 'Tap to start heating'}
            </div>
          ) : (
            <div
              className="temp-label"
              style={{ marginTop: 6, fontSize: '0.65rem', color: 'var(--text-dim)', opacity: 0.8 }}
            >
              {isMining ? 'Start/stop unavailable in dashboard' : 'Configure pool to start'}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
