import React, { useState, useMemo } from 'react';
import { useMinerStore } from '../../store/miner';
import { FanControl, normalizeFanMode } from '../common/FanControl';
import { StatePanel } from '../common/StatePanel';
import { TaskHandoffBanner } from '../common/TaskHandoffBanner';
import { useFanControl } from '../../hooks/useFanControl';
import { useTemp } from '../../hooks/useTemp';
import { ThermalPowerPostureCard } from './ThermalPowerPostureCard';
import { TIME_RANGES } from '../../utils/constants';
import { SvgChart, ChartSeries } from '../common/SvgChart';
import { getLiveWallWatts } from '../../utils/power';
import { wattsToBtu } from '../../utils/thermal';
import { glossaryText } from '../../utils/glossary';
import { InfoDot } from '../common/Tooltip';
import { getTachBackedNoiseDb, noiseUnavailableNote } from '../../utils/noise';

// Tach-less AM2/XIL control boards spin the fans physically but report rpm===0
// (no tach wired). Only call a fan "Stopped" when it was actually commanded off
// (pwm===0); rpm===0 with a live PWM command means we have no tach signal, not a
// stopped fan — printing "Stopped" there is a false negative on those boards.
// Mirrors the FanControl / KitFanMonitor honesty contract.
function fanRpmLabel(rpm: number, pwm: number): string {
  if (rpm > 0) return rpm.toLocaleString();
  return pwm > 0 ? 'No tach' : 'Stopped';
}

function TempGauge({ value, label, min, max, unit, thresholds }: {
  value: number;
  label: string;
  min: number;
  max: number;
  unit: string;
  thresholds: { warn: number; danger: number };
}) {
  const pct = Math.max(0, Math.min(1, (value - min) / (max - min)));
  const angle = -135 + pct * 270; // -135 to +135 degrees
  const color = value >= thresholds.danger
    ? 'var(--red)'
    : value >= thresholds.warn
      ? 'var(--yellow)'
      : 'var(--green)';

  return (
    <div style={{ textAlign: 'center' }}>
      <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginBottom: 8 }}>{label}</div>
      <div style={{ position: 'relative', width: 140, height: 80, margin: '0 auto' }}>
        <svg viewBox="0 0 140 80" width="140" height="80">
          {/* Background arc */}
          <path
            d="M 15 75 A 55 55 0 0 1 125 75"
            fill="none"
            stroke="var(--border)"
            strokeWidth="8"
            strokeLinecap="round"
          />
          {/* Green zone */}
          <path
            d="M 15 75 A 55 55 0 0 1 125 75"
            fill="none"
            stroke={color}
            strokeWidth="8"
            strokeLinecap="round"
            strokeDasharray={`${pct * 175} 175`}
            style={{ transition: 'stroke-dasharray 0.3s, stroke 0.3s' }}
          />
          {/* Needle */}
          <line
            x1="70"
            y1="75"
            x2={70 + 42 * Math.cos((angle - 90) * Math.PI / 180)}
            y2={75 + 42 * Math.sin((angle - 90) * Math.PI / 180)}
            stroke={color}
            strokeWidth="2"
            strokeLinecap="round"
            style={{ transition: 'all 0.3s' }}
          />
          {/* Center dot */}
          <circle cx="70" cy="75" r="4" fill={color} style={{ transition: 'fill 0.3s' }} />
        </svg>
      </div>
      <div style={{
        fontFamily: "'JetBrains Mono', monospace",
        fontWeight: 700, fontSize: '1.4rem', color,
        marginTop: 4,
        transition: 'color 0.3s',
      }}>
        {value > 0 ? `${value.toFixed(1)}${unit}` : 'N/A'}
      </div>
      <div style={{
        display: 'flex', justifyContent: 'space-between',
        fontSize: '0.7rem', color: 'var(--text-dim)',
        width: 140, margin: '2px auto 0',
      }}>
        <span>{min}{unit}</span>
        <span>{max}{unit}</span>
      </div>
    </div>
  );
}

function RpmGauge({ rpm, maxRpm, pwm }: { rpm: number; maxRpm: number; pwm: number }) {
  const pct = Math.max(0, Math.min(1, rpm / maxRpm));
  const angle = -135 + pct * 270;

  return (
    <div style={{ textAlign: 'center' }}>
      <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginBottom: 8 }}>Fan RPM</div>
      <div style={{ position: 'relative', width: 140, height: 80, margin: '0 auto' }}>
        <svg viewBox="0 0 140 80" width="140" height="80">
          <path
            d="M 15 75 A 55 55 0 0 1 125 75"
            fill="none"
            stroke="var(--border)"
            strokeWidth="8"
            strokeLinecap="round"
          />
          <path
            d="M 15 75 A 55 55 0 0 1 125 75"
            fill="none"
            stroke="var(--accent)"
            strokeWidth="8"
            strokeLinecap="round"
            strokeDasharray={`${pct * 175} 175`}
            style={{ transition: 'stroke-dasharray 0.3s' }}
          />
          <line
            x1="70"
            y1="75"
            x2={70 + 42 * Math.cos((angle - 90) * Math.PI / 180)}
            y2={75 + 42 * Math.sin((angle - 90) * Math.PI / 180)}
            stroke="var(--accent)"
            strokeWidth="2"
            strokeLinecap="round"
            style={{ transition: 'all 0.3s' }}
          />
          <circle cx="70" cy="75" r="4" fill="var(--accent)" />
        </svg>
      </div>
      <div style={{
        fontFamily: "'JetBrains Mono', monospace",
        fontWeight: 700, fontSize: '1.4rem', color: 'var(--text)',
        marginTop: 4,
      }}>
        {fanRpmLabel(rpm, pwm)}
      </div>
      <div style={{
        display: 'flex', justifyContent: 'space-between',
        fontSize: '0.7rem', color: 'var(--text-dim)',
        width: 140, margin: '2px auto 0',
      }}>
        <span>0</span>
        <span>{maxRpm.toLocaleString()}</span>
      </div>
    </div>
  );
}

function TemperatureChart() {
  const tempHook = useTemp();
  const [timeRange, setTimeRange] = useState(0);
  const tempHistory = useMinerStore(s => s.tempHistory);
  const powerHistory = useMinerStore(s => s.powerHistory);
  const timeRangeLabel = 'Temperature history time range';

  const series = useMemo(() => {
    const rangeSeconds = TIME_RANGES[timeRange].seconds;
    const cutoff = Date.now() / 1000 - rangeSeconds;
    const result: ChartSeries[] = [];

    const filteredTemp = tempHistory.filter(p => p.time >= cutoff);
    if (filteredTemp.length > 0) {
      result.push({
        data: filteredTemp,
        color: 'var(--red, #EF4444)',
        label: `Temp (${tempHook.symbol})`,
        yAxis: 'left',
      });
    }

    const filteredPower = powerHistory.filter(p => p.time >= cutoff);
    if (filteredPower.length > 0) {
      result.push({
        data: filteredPower,
        color: 'var(--accent)',
        label: 'Power (W)',
        dashed: true,
        yAxis: 'right',
      });
    }

    return result;
  }, [tempHistory, powerHistory, timeRange, tempHook.symbol]);

  // SR summary: announce the chip-temp window + average from the exact series
  // the chart will render so screen-reader truth tracks visible truth. This is
  // the most safety-relevant Standard readout, so it must never be silent.
  const summaryText = useMemo(() => {
    const rangeLabel = TIME_RANGES[timeRange].label;
    const tempSeries = series.find(s => s.label?.startsWith('Temp'));
    if (!tempSeries || tempSeries.data.length === 0) {
      return `Chip temperature and power chart, ${rangeLabel} window, no data yet`;
    }
    let sum = 0;
    let count = 0;
    for (const p of tempSeries.data) {
      if (Number.isFinite(p.value)) { sum += p.value; count++; }
    }
    if (count === 0) return `Chip temperature and power chart, ${rangeLabel} window, no data yet`;
    const avg = sum / count;
    return `Chip temperature over last ${rangeLabel}: ${tempHook.format(avg)} average across ${count} samples`;
  }, [series, timeRange, tempHook]);

  return (
    <div>
      <div className="page-surface-header" style={{ marginBottom: 8 }}>
        <span className="section-title" style={{ margin: 0 }}>Temperature History</span>
        {/* Kit `.tab-underline` range bar (Pages.jsx) — dual-classed with the
            production `time-range-tabs`/`time-tab` hooks so the handoff skin
            renders the kit underline treatment; wiring/aria unchanged. */}
        <div className="tab-underline time-range-tabs" role="group" aria-label={timeRangeLabel}>
          {TIME_RANGES.map((range, i) => (
            <button
              key={range.label}
              type="button"
              className={`time-tab ${timeRange === i ? 'active' : ''}`}
              onClick={() => setTimeRange(i)}
              aria-pressed={timeRange === i}
              aria-label={`Show ${range.label} temperature history`}
            >
              {range.label}
            </button>
          ))}
        </div>
      </div>
      <SvgChart
        series={series}
        height={220}
        summaryText={summaryText}
        style={{ borderRadius: 'var(--radius)', overflow: 'hidden' }}
      />
      <div className="legend-row" style={{ marginTop: 8 }}>
        <span className="legend-pill"><span className="legend-dot" style={{ background: 'var(--red)' }} /> Chip Temperature ({tempHook.symbol})</span>
        <span className="legend-pill"><span className="legend-dot" style={{ background: 'var(--accent)' }} /> Power (W)</span>
      </div>
    </div>
  );
}

export function TempFansPage() {
  const temp = useTemp();
  const fans = useMinerStore(s => s.status?.fans);
  const heaterStatus = useMinerStore(s => s.heaterStatus);
  const chains = useMinerStore(s => s.status?.chains ?? []);
  const systemInfo = useMinerStore(s => s.systemInfo);
  const power = useMinerStore(s => s.stats?.power);

  const pwm = fans?.pwm ?? 0;
  const rpm = fans?.rpm ?? 0;
  const activeFanMode = normalizeFanMode(fans?.mode);
  const fanModeSource = activeFanMode ? 'daemon' : 'unknown';

  const { sending, handleModeChange, handlePwmChange } = useFanControl();

  // Temperature summary from chain data
  const chainTemps = chains.filter(c => c.temp_c > 0).map(c => c.temp_c);
  const avgTemp = chainTemps.length > 0
    ? chainTemps.reduce((s, t) => s + t, 0) / chainTemps.length
    : 0;
  const maxTemp = chainTemps.length > 0 ? Math.max(...chainTemps) : 0;

  const noise = getTachBackedNoiseDb(heaterStatus);
  const noiseColor = noise == null ? 'var(--text-dim)'
    : noise <= 45 ? 'var(--green)'
    : noise <= 60 ? 'var(--yellow)'
    : 'var(--red)';
  const noiseNote = noise == null
    ? noiseUnavailableNote(heaterStatus)
    : (heaterStatus?.noise_note ?? 'Tach-backed acoustic estimate');
  const fanEvidence = rpm > 0 ? 'RPM confirmed' : 'no tach signal';

  const tempColor = (t: number) =>
    t >= 70 ? 'var(--red)' : t >= 60 ? 'var(--yellow)' : 'var(--green)';

  const minTemp = chainTemps.length > 0 ? Math.min(...chainTemps) : 0;

  // PSU panel telemetry: only live wall-power readings drive wall power / heat,
  // so static/model fallback watts do not look like measured PSU telemetry.
  const psuWallWatts = getLiveWallWatts(power);
  const psuSource =
    power && 'source' in power ? power.source : null;
  const psuBtuH = psuWallWatts > 0 ? wattsToBtu(psuWallWatts) : 0;

  return (
    <div className="page-content">
      <div className="page-hero-strip">
        <div className="page-hero-summary">
          <div className="page-hero-eyebrow">THERMALS</div>
          <div className="page-hero-title">Thermals And Cooling</div>
          <div className="page-hero-stat" data-tooltip={glossaryText('temp_die_vs_board')}>
            {maxTemp > 0 ? temp.format(maxTemp) : '—'}
          </div>
          <div className="page-hero-substat">
            {chains.length > 0
              ? `Max across ${chains.length} chain${chains.length === 1 ? '' : 's'} · PWM ${pwm}/100`
              : 'Awaiting chain temperature data.'}
          </div>
        </div>
        <div className="hero-kpi-strip">
          <div className="hero-kpi" data-tooltip={glossaryText('temp_die_vs_board')}>
            <div className="kpi-label">Min Temp</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {minTemp > 0 ? temp.format(minTemp) : '—'}
              </span>
            </div>
          </div>
          <div className="hero-kpi" data-tooltip={glossaryText('fan_pwm')}>
            <div className="kpi-label">Fan PWM</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{pwm}%</span>
            </div>
            <div className="kpi-sub" data-tooltip={glossaryText('cut_hash_before_noise')}>{fanEvidence}</div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Fan RPM</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{fanRpmLabel(rpm, pwm)}</span>
            </div>
          </div>
        </div>
      </div>

      <section className="section">
      <TaskHandoffBanner
        expectedMode="standard"
        title="Cooling task opened from Heater mode"
        copy="Use the detailed fan and thermal controls here, then jump back to the simpler heat view when comfort and noise feel right."
      />

      <ThermalPowerPostureCard />

      {/* PSU panel — real store data (systemInfo.hardware + stats.power),
          honest "—" where the miner reports nothing. Wave-13 de-dup: the
          per-chain "Board temperatures" bar list that used to sit beside this
          was REMOVED — it duplicated the authoritative "Temperature Gauges"
          grid below (per-chain temp was being shown 2-3x on this one page). */}
      <div className="section">
        <div className="section-title">PSU</div>
        <div className="temp-psu-grid">
          {[
            ['Model', systemInfo?.hardware?.psu_model ?? '—', undefined],
            ['Live wall power', psuWallWatts > 0 ? `${psuWallWatts.toFixed(0)} W` : '—', undefined],
            ['Source', psuSource ?? '—', undefined],
            ['Fan RPM', rpm > 0 ? rpm.toLocaleString() : '—', undefined],
            ['Heat', psuBtuH > 0 ? `${psuBtuH.toFixed(0)} BTU/h` : '—', 'var(--yellow)'],
            ['FW version', systemInfo?.hardware?.psu_fw_version ?? '—', 'var(--accent)'],
          ].map(([label, value, color]) => (
            <div key={label as string}>
              <div style={{ color: 'var(--text-dim)', fontSize: '0.7rem', textTransform: 'uppercase', letterSpacing: '.05em' }}>
                {label}
              </div>
              <div style={{ fontFamily: "'JetBrains Mono', monospace", marginTop: 2, color: color as string | undefined }}>
                {value}
              </div>
            </div>
          ))}
        </div>
        {systemInfo?.hardware?.psu_override_active && (
          <div style={{ marginTop: 10, fontSize: '0.78rem', color: 'var(--text-dim)' }}>
            PSU override active{systemInfo.hardware.psu_voltage_range
              ? ` · ${systemInfo.hardware.psu_voltage_range}`
              : ''}.
          </div>
        )}
      </div>

      {/* Thermal Gauges Row */}
      <div className="section">
        <div className="section-title">Temperature Gauges</div>
        <div className="page-surface">
          {(chains.length === 0 || chainTemps.length === 0) && (
            <div style={{ marginBottom: 16 }}>
              <StatePanel
                title={chains.length === 0 ? 'Waiting for chain telemetry' : 'Boards detected, but temperatures are not ready yet'}
                message={chains.length === 0
                  ? 'DCENT_OS has not received chain temperature data yet. Control-board telemetry may be live before hash boards finish reporting.'
                  : 'Hash boards are visible, but chip temperatures are still zero. Check board power, cold-boot state, and sensor readiness.'}
                tone={chains.length === 0 ? 'info' : 'warning'}
                compact
              />
            </div>
          )}
          <div className="thermal-gauge-grid">
            {/* Average Chain Temperature */}
            <TempGauge
              value={temp.convert(avgTemp)}
              label="Average Chain Temp"
              min={temp.convert(0)}
              max={temp.convert(100)}
              unit={temp.symbol}
              thresholds={{ warn: temp.convert(60), danger: temp.convert(70) }}
            />

            {/* Per-chain temperatures */}
            {chains.map(c => (
              <TempGauge
                key={c.id}
                value={temp.convert(c.temp_c)}
                label={`Chain ${c.id}`}
                min={temp.convert(0)}
                max={temp.convert(100)}
                unit={temp.symbol}
                thresholds={{ warn: temp.convert(60), danger: temp.convert(70) }}
              />
            ))}

            {/* Fan RPM Gauge */}
            <RpmGauge rpm={rpm} maxRpm={6000} pwm={pwm} />
          </div>

          {/* Temperature summary bar */}
          <div className="thermal-summary-grid">
            <div className="thermal-summary-card">
              <div className="thermal-summary-label">Average</div>
              <div className="thermal-summary-value" style={{ color: avgTemp > 0 ? tempColor(avgTemp) : 'var(--text-dim)' }}>
                {avgTemp > 0 ? temp.format(avgTemp) : 'N/A'}
              </div>
            </div>
            <div className="thermal-summary-card">
              <div className="thermal-summary-label">Max</div>
              <div className="thermal-summary-value" style={{ color: maxTemp > 0 ? tempColor(maxTemp) : 'var(--text-dim)' }}>
                {maxTemp > 0 ? temp.format(maxTemp) : 'N/A'}
              </div>
            </div>
            <div className="thermal-summary-card">
              <div className="thermal-summary-label">Noise</div>
              <div className="thermal-summary-value" style={{ color: noiseColor }}>
                {noise != null ? `${noise.toFixed(0)} dB` : 'RPM needed'}
              </div>
              <div className="thermal-summary-note">{noiseNote}</div>
            </div>
          </div>

          {/* Thermal threshold legend — inlined into the gauges surface
              (Wave-13: was a near-empty standalone .section > .page-surface).
              The PWM summary card was removed — Fan PWM is in the hero strip. */}
          <div className="legend-row" style={{ marginTop: 14 }}>
            <div className="legend-pill"><span className="legend-dot" style={{ background: 'var(--green)' }} /> Normal {'<'} {temp.format(55)}</div>
            <div className="legend-pill"><span className="legend-dot" style={{ background: 'var(--yellow)' }} /> Ramp {temp.format(60)}</div>
            <div className="legend-pill"><span className="legend-dot" style={{ background: 'var(--red)' }} /> Hot {temp.format(65)}</div>
            <div className="legend-pill"><span className="legend-dot" style={{ background: '#FF0000' }} /> Critical {temp.format(75)}</div>
          </div>
        </div>
      </div>

      {/* Fan Control */}
      <div className="section">
        <div className="section-title">
          Fan Control
          {sending && <span style={{ fontSize: '0.7rem', color: 'var(--text-dim)', marginLeft: 8 }}>(updating...)</span>}
        </div>
        <div className="page-surface">
          <FanControl
            currentPwm={pwm}
            currentRpm={rpm}
            activeMode={activeFanMode}
            modeSource={fanModeSource}
            onModeChange={handleModeChange}
            onPwmChange={handlePwmChange}
            disabled={sending}
          />
          {/* Wave-13: the redundant "Quick PWM Adjust" slider was removed —
              FanControl above already provides the PWM slider + mode control. */}
        </div>
      </div>

      {/* Fan Curve Editor lives on the Tuning page (StandardDashboard tuning
          tab) — Wave-2 de-dup (STD-A-10): it was mounted here AND there, so the
          duplicate was removed from this Thermals page to keep one canonical
          home. */}

      {/* Temperature History Chart */}
      <div className="section">
        <div className="page-surface">
          <TemperatureChart />
        </div>
      </div>

      {/* Safety Info */}
      <div className="section">
        <div className="page-surface" style={{ fontSize: '0.8rem', color: 'var(--text-dim)' }}>
          <div className="page-surface-title" style={{ color: 'var(--text-secondary)', marginBottom: 8, display: 'inline-flex', alignItems: 'center', gap: 6 }}>
            Thermal Safety Overrides
            <InfoDot term="cut_hash_before_noise" size={12} />
          </div>
          <div style={{ lineHeight: 1.8 }}>
            Safety overrides request the maximum allowed fan output, subject to the active profile or configured ceiling, when any of these conditions are met:
          </div>
          <ul style={{ margin: '8px 0', paddingLeft: 20, lineHeight: 1.8 }}>
            <li>Temperature sensor reads fail</li>
            <li>Fan tachometer reads 0 for more than 5 seconds</li>
            <li>Any chip temperature exceeds {temp.format(65)}</li>
            <li>Mining daemon (dcentrald) crashes while hash boards are powered</li>
          </ul>
          <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', fontStyle: 'italic' }}>
            Boot default requests the home fan cap and cuts hash power before raising noise. Confirm acoustic results with RPM feedback and operator hearing.
          </div>
        </div>
      </div>
      </section>
    </div>
  );
}
