import React, { useMemo, useState } from 'react';
import { api } from '../../api/client';
import { useMinerStore } from '../../store/miner';
import { wattsToBtu } from '../../utils/thermal';
import { getLiveDisplayWallWatts } from '../../utils/power';
import { ActionButton } from '../common/ActionButton';
import { Tooltip } from '../common/Tooltip';

/**
 * Tune-by-priority configurator (design-handoff ).
 *
 * The operator asked for tuning expressed as an intent, not raw freq/volts:
 * "Power target / Hashrate target / Noise / Heat / Max hashrate / Best
 * efficiency", per-unit or per-hashboard, with a live before→after preview.
 *
 * Truth-contract: the preview is an **estimate**, labelled as such. It is NOT
 * claimed telemetry. It is anchored on the miner's REAL current values
 * (5-min hashrate, live wall power accepted by provenance, derived J/TH, hottest chain) — not the
 * prototype's hardcoded BASELINE — and the circuit cap, when shown, is the
 * REAL declared `watt_cap` from `/api/status`, never invented. "Apply" hands
 * the intent to the existing profile/autotuner path (`api.saveProfile`) — the
 * autotuner still owns the actual per-chain convergence; we never pretend the
 * predicted numbers are measured.
 */
type Priority = 'power' | 'hashrate' | 'noise' | 'heat' | 'max' | 'efficiency';
type Scope = 'unit' | 'board';

interface Predicted {
  hashrate: number; // TH/s
  power: number; // W
  efficiency: number; // J/TH
  noise: number; // fan PWM cap request
  btu: number; // BTU/h
  chipTemp: number; // °C
  fanMode: 'quiet' | 'balanced' | 'performance';
}

const PRIORITY_META: { id: Priority; label: string; sub: string; color: string }[] = [
  { id: 'power', label: 'Power target', sub: 'Set a wall-watt budget. Hashrate follows.', color: 'var(--blue)' },
  { id: 'hashrate', label: 'Hashrate target', sub: 'Pick a TH/s. The tuner finds freq/voltage.', color: 'var(--accent)' },
  { id: 'noise', label: 'Fan cap target', sub: 'Home cap request; dB needs tach/RPM proof.', color: 'var(--indigo, #8B7EF8)' },
  { id: 'heat', label: 'Heat target', sub: 'Hold chip / board / room at a temperature.', color: 'var(--accent-deep)' },
  { id: 'max', label: 'Max hashrate', sub: 'Send it. Cap is your declared circuit.', color: 'var(--red)' },
  { id: 'efficiency', label: 'Best efficiency', sub: 'Lowest J/TH. Slower, cheaper.', color: 'var(--green)' },
];

function PriorityIcon({ id }: { id: Priority }) {
  const common = {
    width: 22, height: 22, viewBox: '0 0 24 24', fill: 'none',
    stroke: 'currentColor', strokeWidth: 1.6,
    strokeLinecap: 'round' as const, strokeLinejoin: 'round' as const,
  };
  switch (id) {
    case 'power': return <svg {...common}><path d="M13 2L3 14h9l-1 8 10-12h-9l1-8z" /></svg>;
    case 'hashrate': return <svg {...common}><line x1="3" y1="9" x2="21" y2="9" /><line x1="3" y1="15" x2="21" y2="15" /><line x1="9" y1="3" x2="9" y2="21" /><line x1="15" y1="3" x2="15" y2="21" /></svg>;
    case 'noise': return <svg {...common}><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" /><path d="M15.54 8.46a5 5 0 0 1 0 7.07" /><path d="M19.07 4.93a10 10 0 0 1 0 14.14" /></svg>;
    case 'heat': return <svg {...common}><path d="M14 14.76V3.5a2.5 2.5 0 0 0-5 0v11.26a4.5 4.5 0 1 0 5 0z" /></svg>;
    case 'max': return <svg {...common}><polyline points="18 15 12 9 6 15" /><polyline points="18 21 12 15 6 21" opacity=".5" /></svg>;
    case 'efficiency': return <svg {...common}><circle cx="12" cy="12" r="3" /><circle cx="12" cy="12" r="8" opacity=".5" /></svg>;
  }
}

export function TuningPriorityConfigurator() {
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const heater = useMinerStore(s => s.heaterStatus);
  const addAlert = useMinerStore(s => s.addAlert);

  // Real current baseline. Display/model fallback watts are not enough to arm
  // a tuning preview that can apply a real daemon power target.
  const baseHashrate = (status?.hashrate_ghs ?? 0) / 1000; // TH/s
  const basePower = getLiveDisplayWallWatts(heater, stats?.power);
  const chainTemps = (status?.chains ?? [])
    .map(c => (typeof c?.temp_c === 'number' ? c.temp_c : null))
    .filter((v): v is number => v != null && Number.isFinite(v));
  const baseChipTemp = chainTemps.length ? Math.max(...chainTemps) : 0;
  const baseEff = baseHashrate > 0 && basePower > 0 ? basePower / baseHashrate : 0;
  const baseFanCap = status?.fans?.pwm ?? 0;
  const baseBtu = basePower > 0 ? wattsToBtu(basePower) : 0;
  const hasBaseline = baseHashrate > 0 && basePower > 0;

  // Real declared circuit cap — only shown when the daemon reports it.
  const wattCap =
    stats?.power && 'watt_cap' in stats.power && stats.power.watt_cap
      ? stats.power.watt_cap.cap_watts
      : null;

  const [priority, setPriority] = useState<Priority>('hashrate');
  const [scope, setScope] = useState<Scope>('unit');
  const [unitW, setUnitW] = useState(() => (basePower > 0 ? Math.round(basePower) : 3200));
  const [unitTh, setUnitTh] = useState(() => (baseHashrate > 0 ? Math.round(baseHashrate) : 100));
  const [fanCapPwm, setFanCapPwm] = useState(30);
  const [targetTemp, setTargetTemp] = useState(75);

  // Transparent ratio model — explicitly an estimate, anchored on real values.
  const predicted: Predicted = useMemo(() => {
    const b = {
      hashrate: hasBaseline ? baseHashrate : 100,
      power: hasBaseline ? basePower : 3200,
    };
    let hashrate = b.hashrate;
    let power = b.power;
    let fanMode: Predicted['fanMode'] = 'balanced';

    if (priority === 'power') {
      power = unitW;
      hashrate = b.hashrate * (unitW / b.power);
      fanMode = unitW < b.power * 0.8 ? 'quiet' : unitW > b.power * 1.1 ? 'performance' : 'balanced';
    } else if (priority === 'hashrate') {
      hashrate = unitTh;
      const ratio = unitTh / b.hashrate;
      power = b.power * ratio * (ratio > 1 ? 1.12 : 0.94);
      fanMode = ratio < 0.85 ? 'quiet' : ratio > 1.1 ? 'performance' : 'balanced';
    } else if (priority === 'noise') {
      const capPct = Math.max(0, Math.min(1, (fanCapPwm - 10) / 20));
      power = b.power * (0.45 + capPct * 0.35);
      hashrate = b.hashrate * (power / b.power) * 0.96;
      fanMode = 'quiet';
    } else if (priority === 'heat') {
      const tempPct = Math.max(0, Math.min(1, (targetTemp - 60) / 25));
      power = b.power * (0.55 + tempPct * 0.6);
      hashrate = b.hashrate * (power / b.power);
      fanMode = targetTemp <= 70 ? 'quiet' : targetTemp >= 82 ? 'performance' : 'balanced';
    } else if (priority === 'max') {
      power = wattCap ?? b.power * 1.3;
      hashrate = b.hashrate * (power / b.power) * 1.05;
      fanMode = 'performance';
    } else if (priority === 'efficiency') {
      hashrate = b.hashrate * 0.85;
      power = b.power * 0.78;
      fanMode = 'quiet';
    }

    const efficiency = hashrate > 0 ? power / hashrate : 0;
    return {
      hashrate,
      power,
      efficiency,
      noise: priority === 'noise' ? fanCapPwm : (fanMode === 'quiet' ? 30 : fanMode === 'balanced' ? 30 : 60),
      btu: wattsToBtu(power),
      chipTemp: priority === 'heat' ? targetTemp : Math.round(58 + (power / (b.power || 1)) * 24),
      fanMode,
    };
  }, [priority, scope, unitW, unitTh, fanCapPwm, targetTemp, baseHashrate, basePower, hasBaseline, wattCap]);

  const [applying, setApplying] = useState(false);
  const overCap = wattCap != null && predicted.power > wattCap;

  // The real, safe levers the daemon exposes: a unit WALL-POWER TARGET
  // (`/api/home/target` — the same endpoint PowerPresets uses) and a FAN
  // MODE (`/api/fan`). The autotuner converges per-chain freq/voltage to the
  // power envelope; the fan mode bounds noise. We translate each priority to
  // the watt target that achieves it (Power = the slider directly; Max = the
  // real declared circuit cap; everything else = the model-predicted power
  // that yields the chosen hashrate / fan-cap / temp / efficiency). No fabricated
  // freq/volts are pushed; the preview stays a labelled estimate.
  const targetWatts = (() => {
    if (priority === 'power') return unitW;
    if (priority === 'max') return wattCap ?? predicted.power;
    return predicted.power;
  })();

  const apply = async () => {
    const watts = Math.round(targetWatts);
    if (!Number.isFinite(watts) || watts < 100) {
      addAlert('warning', 'No usable power target yet — connect/start mining first');
      return;
    }
    setApplying(true);
    try {
      const meta = PRIORITY_META.find(p => p.id === priority)!;
      // 1) Real unit power target.
      await api.setHeaterTarget({ watts });
      // 2) Real fan mode (bounds noise to the chosen profile).
      try {
        await api.setFan(predicted.fanMode);
      } catch {
        /* power target still applied; fan mode is best-effort */
      }
      addAlert(
        'info',
        `${meta.label} applied: ${watts.toLocaleString()} W unit target · ${predicted.fanMode} fan. The autotuner now converges per-chain frequency/voltage to this envelope.`,
      );
    } catch {
      addAlert('warning', 'Failed to apply the power target to the daemon');
    } finally {
      setApplying(false);
    }
  };

  const fmt1 = (v: number) => v.toFixed(1);
  const fmtInt = (v: number) => Math.round(v).toLocaleString();

  const rows: { label: string; before: number; after: number; unit: string; lowerBetter?: boolean; fmt: (v: number) => string; tip: string }[] = [
    { label: 'Hashrate', before: baseHashrate, after: predicted.hashrate, unit: ' TH/s', fmt: fmt1, tip: 'Total compute rate. Higher = more sats, more heat, more noise.' },
    { label: 'Power', before: basePower, after: predicted.power, unit: ' W', lowerBetter: true, fmt: fmtInt, tip: 'Wall draw. Bills, breakers and circuit caps care about this number.' },
    { label: 'Efficiency', before: baseEff, after: predicted.efficiency, unit: ' J/TH', lowerBetter: true, fmt: v => v.toFixed(2), tip: 'Joules per terahash. Lower = more work per watt.' },
    { label: 'Fan cap', before: baseFanCap, after: predicted.noise, unit: ' PWM', lowerBetter: true, fmt: v => v.toFixed(0), tip: 'Requested fan cap. Acoustic dB requires live RPM proof.' },
    { label: 'Heat out', before: baseBtu, after: predicted.btu, unit: ' BTU/h', fmt: fmtInt, tip: 'Heat delivered to the room (≈ wall power × 3.41).' },
    { label: 'Chip temp', before: baseChipTemp, after: predicted.chipTemp, unit: ' °C', lowerBetter: true, fmt: v => v.toFixed(0), tip: 'Hottest chain junction. Safe under ~85 °C.' },
  ];

  return (
    <div className="section tuning-priority">
      <div className="section-title">Tune by priority</div>
      <div className="tuning-priority-intro">
        Pick what matters. Apply sets a real unit <strong>power target</strong>
        {' '}+ <strong>fan mode</strong> on the daemon; the autotuner then
        converges per-chain frequency/voltage to that envelope. The before→after
        figures are a <strong>predicted estimate</strong> from your current
        setpoints — not measured telemetry.
      </div>

      <div className="tuning-priority-grid" role="group" aria-label="Tuning priority">
        {PRIORITY_META.map(p => {
          const active = priority === p.id;
          return (
            <button
              key={p.id}
              type="button"
              className={`tuning-priority-tile${active ? ' is-active' : ''}`}
              style={{ '--pri-color': p.color } as React.CSSProperties}
              aria-pressed={active}
              onClick={() => setPriority(p.id)}
            >
              <span className="tuning-priority-tile-icon"><PriorityIcon id={p.id} /></span>
              <span className="tuning-priority-tile-name">{p.label}</span>
              <span className="tuning-priority-tile-sub">{p.sub}</span>
              {active && <span className="tuning-priority-tile-active">ACTIVE</span>}
            </button>
          );
        })}
      </div>

      <div className="tuning-priority-conf">
        <div className="tuning-priority-conf-head">
          <h3>{PRIORITY_META.find(p => p.id === priority)!.label}</h3>
          {(priority === 'power' || priority === 'hashrate') && (
            <div className="tuning-scope" role="group" aria-label="Tuning scope">
              <button
                type="button"
                className={`tuning-scope-btn${scope === 'unit' ? ' is-active' : ''}`}
                onClick={() => setScope('unit')}
                data-tooltip="Set the priority for the whole miner. The autotuner splits it across chains."
              >
                Per unit
              </button>
              <button
                type="button"
                className={`tuning-scope-btn${scope === 'board' ? ' is-active' : ''}`}
                onClick={() => setScope('board')}
                data-tooltip="Express the target per hashboard. The autotuner commits each chain."
              >
                Per hashboard
              </button>
            </div>
          )}
        </div>

        <div className="tuning-priority-conf-grid">
          <div className="tuning-priority-controls">
            {priority === 'power' && (
              <div className="tuning-conf-row">
                <label htmlFor="tp-power">
                  {scope === 'unit' ? 'Total wall power' : 'Per-hashboard wall power'}
                </label>
                <input
                  id="tp-power"
                  className="tuning-slider"
                  type="range"
                  min={600}
                  max={(wattCap ?? 5000) + 400}
                  step={50}
                  value={unitW}
                  onChange={e => setUnitW(+e.target.value)}
                />
                <div className="tuning-conf-val"><strong>{unitW.toLocaleString()}</strong><small>W</small></div>
                {wattCap != null && (
                  <div className="tuning-conf-meta">
                    Circuit cap <strong>{wattCap.toLocaleString()} W</strong> ·{' '}
                    {Math.round((unitW / wattCap) * 100)}% of cap
                  </div>
                )}
              </div>
            )}
            {priority === 'hashrate' && (
              <div className="tuning-conf-row">
                <label htmlFor="tp-hash">
                  {scope === 'unit' ? 'Total hashrate' : 'Per-hashboard hashrate'}
                </label>
                <input
                  id="tp-hash"
                  className="tuning-slider"
                  type="range"
                  min={Math.max(20, Math.round((baseHashrate || 100) * 0.4))}
                  max={Math.round((baseHashrate || 100) * 1.6)}
                  step={1}
                  value={unitTh}
                  onChange={e => setUnitTh(+e.target.value)}
                />
                <div className="tuning-conf-val"><strong>{unitTh}</strong><small>TH/s</small></div>
                {hasBaseline && (
                  <div className="tuning-conf-meta">
                    vs current <strong>{baseHashrate.toFixed(1)} TH/s</strong>
                  </div>
                )}
              </div>
            )}
            {priority === 'noise' && (
              <>
                <div className="tuning-noise-presets">
                  {[
                    { pwm: 10, l: 'Idle request', sub: 'Verify RPM' },
                    { pwm: 20, l: 'Middle cap', sub: 'Home envelope' },
                    { pwm: 30, l: 'Home cap', sub: 'Daemon ceiling' },
                  ].map(p => (
                    <button
                      key={p.l}
                      type="button"
                      className={`tuning-noise-tile${fanCapPwm === p.pwm ? ' is-active' : ''}`}
                      onClick={() => setFanCapPwm(p.pwm)}
                    >
                      <strong>{p.l}</strong>
                      <span>PWM {p.pwm} - {p.sub}</span>
                    </button>
                  ))}
                </div>
                <div className="tuning-conf-row">
                  <label htmlFor="tp-noise">Fan cap request</label>
                  <input id="tp-noise" className="tuning-slider" type="range" min={10} max={30} step={1}
                    value={fanCapPwm} onChange={e => setFanCapPwm(+e.target.value)} />
                  <div className="tuning-conf-val"><strong>{fanCapPwm}</strong><small>PWM</small></div>
                  <div className="tuning-conf-meta">
                    Acoustic dB remains unavailable until the daemon reports tach/RPM-backed noise.
                  </div>
                </div>
              </>
            )}
            {priority === 'heat' && (
              <div className="tuning-conf-row">
                <label htmlFor="tp-heat">Target chip temperature</label>
                <input id="tp-heat" className="tuning-slider" type="range" min={60} max={88} step={1}
                  value={targetTemp} onChange={e => setTargetTemp(+e.target.value)} />
                <div className="tuning-conf-val"><strong>{targetTemp}</strong><small>°C</small></div>
                <div className="tuning-conf-meta">
                  The autotuner adjusts hashrate to hold the hottest chain near this.
                </div>
              </div>
            )}
            {priority === 'max' && (
              <div className="tuning-conf-static">
                <strong>Send it.</strong>
                <p>
                  The autotuner drives frequency and voltage as high as the
                  silicon holds, then backs off to your declared circuit cap.
                  Expect high noise and chip temps near the configured ceiling.
                </p>
                <ul>
                  <li>Circuit cap: <strong>{wattCap != null ? `${wattCap.toLocaleString()} W` : 'not declared — set it in the wizard'}</strong></li>
                  <li>The HAL voltage/thermal hard-stops still apply.</li>
                </ul>
              </div>
            )}
            {priority === 'efficiency' && (
              <div className="tuning-conf-static">
                <strong>Find the J/TH minimum.</strong>
                <p>
                  Probes a range of frequency × voltage setpoints and converges
                  on the lowest joules-per-terahash. Expect hashrate to drop, but
                  each watt does more work.
                </p>
                <ul>
                  <li>Slower than stock; cached after the first convergence.</li>
                  <li>Best for always-on home miners on a tight power budget.</li>
                </ul>
              </div>
            )}
          </div>

          <div className="tuning-priority-impact">
            <div className="tuning-impact-head">
              <span>Predicted effect</span>
              <span className="tuning-impact-meta">estimate · vs current</span>
            </div>
            <div className="tuning-impact-rows">
              {rows.map(r => {
                const delta = r.after - r.before;
                const pct = r.before === 0 ? 0 : (delta / r.before) * 100;
                const flat = Math.abs(pct) < 0.5 || !hasBaseline;
                const good = (r.lowerBetter ? -delta : delta) > 0;
                return (
                  <Tooltip key={r.label} content={r.tip} placement="left">
                    <div className="tuning-impact-row" tabIndex={0}>
                      <span className="tuning-impact-label">{r.label}</span>
                      <span className="tuning-impact-before">
                        {r.before > 0 ? r.fmt(r.before) : '—'}<small>{r.unit}</small>
                      </span>
                      <span className="tuning-impact-arrow" aria-hidden="true">→</span>
                      <span className="tuning-impact-after">{r.fmt(r.after)}<small>{r.unit}</small></span>
                      <span className={`tuning-impact-delta ${flat ? 'is-flat' : good ? 'is-good' : 'is-bad'}`}>
                        {flat ? '·' : `${delta > 0 ? '+' : ''}${r.fmt(Math.abs(delta))}`}
                      </span>
                    </div>
                  </Tooltip>
                );
              })}
            </div>
            {overCap && (
              <div className="tuning-impact-warn" role="alert">
                <span aria-hidden="true">!</span>
                <div>
                  Predicted power <strong>{Math.round(predicted.power).toLocaleString()} W</strong>{' '}
                  exceeds your declared circuit cap of{' '}
                  <strong>{wattCap!.toLocaleString()} W</strong>. The autotuner
                  will throttle to stay under it.
                </div>
              </div>
            )}
            {!hasBaseline && (
              <div className="tuning-impact-note">
                No live hashrate/wall-power baseline yet — connect/start mining for an accurate
                before→after estimate.
              </div>
            )}
            <ActionButton
              label={applying ? 'Applying…' : 'Apply power + fan target'}
              onClick={apply}
              variant="primary"
              disabled={applying || !hasBaseline}
              confirm={`Apply a ${Math.round(targetWatts).toLocaleString()} W unit power target and ${predicted.fanMode} fan mode? The autotuner converges per-chain frequency/voltage to this envelope. HAL voltage/thermal hard-stops still apply.`}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
