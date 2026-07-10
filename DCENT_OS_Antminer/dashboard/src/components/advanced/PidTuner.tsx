import React, { useState, useCallback, useMemo } from 'react';
import { api } from '../../api/client';
import { ActionButton } from '../common/ActionButton';
import { SvgChart, DataPoint } from '../common/SvgChart';
import { useSystemHealth } from '../common/proxy/SystemHealthContext';
import { InfoDot } from '../common/Tooltip';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';

// PID output gauge component (0-100% of fan PWM range).
function PidGauge({ value, max = 100 }: { value: number; max?: number }) {
  const pct = Math.max(0, Math.min(100, (value / max) * 100));
  const angle = (pct / 100) * 180 - 90; // -90 to 90 degrees

  // Color based on output percentage
  const color = pct < 30 ? 'var(--blue)' : pct < 60 ? 'var(--green)' : pct < 80 ? 'var(--accent)' : 'var(--red)';

  return (
    <div style={{ position: 'relative', width: '100%', maxWidth: 200, margin: '0 auto' }}>
      <svg viewBox="0 0 200 120" style={{ width: '100%' }}>
        {/* Background arc */}
        <path
          d="M 20 100 A 80 80 0 0 1 180 100"
          fill="none"
          stroke="rgba(255,255,255,0.06)"
          strokeWidth="12"
          strokeLinecap="round"
        />
        {/* Filled arc */}
        <path
          d="M 20 100 A 80 80 0 0 1 180 100"
          fill="none"
          stroke={color}
          strokeWidth="12"
          strokeLinecap="round"
          strokeDasharray={`${pct * 2.51} 251`}
          style={{ filter: `drop-shadow(0 0 6px ${color}40)` }}
        />
        {/* Needle */}
        <line
          x1="100" y1="100"
          x2={100 + 60 * Math.cos((angle * Math.PI) / 180)}
          y2={100 - 60 * Math.sin((angle * Math.PI) / 180)}
          stroke="var(--text)"
          strokeWidth="2"
          strokeLinecap="round"
        />
        <circle cx="100" cy="100" r="4" fill="var(--accent)" />
        {/* Value text */}
        <text x="100" y="90" textAnchor="middle" fill={color} fontSize="20" fontWeight="700"
          fontFamily="'JetBrains Mono', monospace">
          {pct.toFixed(0)}%
        </text>
        <text x="100" y="115" textAnchor="middle" fill="var(--text-dim)" fontSize="10"
          fontFamily="'JetBrains Mono', monospace">
          PWM {value.toFixed(0)}/{max}
        </text>
        {/* Scale labels */}
        <text x="20" y="115" textAnchor="middle" fill="var(--text-dim)" fontSize="9">0</text>
        <text x="180" y="115" textAnchor="middle" fill="var(--text-dim)" fontSize="9">100%</text>
      </svg>
    </div>
  );
}

// Horizontal bar for a PID term (positive extends right, negative extends left from center)
function PidTermBar({ label, value, color, maxRange = 50 }: {
  label: string;
  value: number;
  color: string;
  maxRange?: number;
}) {
  const clamped = Math.max(-maxRange, Math.min(maxRange, value));
  const pct = (Math.abs(clamped) / maxRange) * 50; // 0-50%
  const isPositive = clamped >= 0;

  return (
    <div className="pid-term">
      <div className="pid-term-head">
        <span style={{ color }}>{label}</span>
        <span className="pid-term-val">{value.toFixed(3)}</span>
      </div>
      <div className="pid-term-track">
        {/* Center line */}
        <div className="pid-term-center" />
        {/* Bar */}
        <div
          className="pid-term-bar"
          style={{
            left: isPositive ? '50%' : `${50 - pct}%`,
            width: `${pct}%`,
            background: color,
            boxShadow: `0 0 8px ${color}40`,
          }}
        />
      </div>
    </div>
  );
}

export function PidTuner() {
  const { isProxyMode } = useSystemHealth();
  const [kp, setKp] = useState(2.0);
  const [ki, setKi] = useState(0.1);
  const [kd, setKd] = useState(0.5);
  const [setpoint, setSetpoint] = useState(55);
  const [currentTemp, setCurrentTemp] = useState(0);
  const [output, setOutput] = useState(0);
  const [integral, setIntegral] = useState(0);
  const [lastError, setLastError] = useState(0);
  const [pTerm, setPTerm] = useState(0);
  const [iTerm, setITerm] = useState(0);
  const [dTerm, setDTerm] = useState(0);
  const [statusMsg, setStatusMsg] = useState('');
  const [errorMsg, setErrorMsg] = useState('');
  const [isLiveData, setIsLiveData] = useState(false);

  // Chart data stored in state for SvgChart
  const [setpointData, setSetpointData] = useState<DataPoint[]>([]);
  const [tempData, setTempData] = useState<DataPoint[]>([]);
  const [outputData, setOutputData] = useState<DataPoint[]>([]);

  const handleReadCurrent = useCallback(async () => {
    setErrorMsg('');
    try {
      const state = await api.getPidState();
      setKp(state.kp);
      setKi(state.ki);
      setKd(state.kd);
      setSetpoint(state.setpoint);
      setCurrentTemp(state.current_temp);
      setOutput(state.output);
      setIntegral(state.integral);
      setLastError(state.last_error);
      // Calculate terms from live data
      const err = state.setpoint - state.current_temp;
      setPTerm(state.kp * err);
      setITerm(state.ki * state.integral);
      setDTerm(state.kd * state.last_error);
      const tick = Math.floor(Date.now() / 1000);
      setSetpointData([{ time: tick, value: state.setpoint }]);
      setTempData([{ time: tick, value: state.current_temp }]);
      setOutputData([{ time: tick, value: state.output }]);
      setIsLiveData(true);
      setStatusMsg('PID state loaded from device');
    } catch (e: unknown) {
      setErrorMsg(e instanceof Error ? e.message : 'Failed to read PID state');
    }
  }, []);

  const handleApply = useCallback(async () => {
    setErrorMsg('');
    setStatusMsg('');
    if (isProxyMode) {
      setErrorMsg('Blocked: bosminer owns fan control in proxy/hybrid mode.');
      return;
    }
    if (!isLiveData) {
      setErrorMsg('Read current PID state before applying changes.');
      return;
    }
    try {
      echoCli(`pid set --kp ${kp} --ki ${ki} --kd ${kd} --setpoint ${setpoint}`);
      await api.setPidParams({ kp, ki, kd, setpoint, confirm: true });
      setStatusMsg('PID parameters applied');
    } catch (e: unknown) {
      setErrorMsg(e instanceof Error ? e.message : 'Failed to apply PID params');
    }
  }, [isLiveData, isProxyMode, kp, ki, kd, setpoint]);

  // SR summary derived from the exact series the chart renders.
  const chartSummary = useMemo(() => {
    if (!isLiveData) {
      return 'PID response chart: no live PID state loaded yet';
    }
    if (tempData.length === 0) {
      return 'PID response chart (Live), no data yet';
    }
    let sum = 0;
    let count = 0;
    for (const p of tempData) {
      if (Number.isFinite(p.value)) { sum += p.value; count++; }
    }
    if (count === 0) return 'PID response chart (Live), no data yet';
    const avgTemp = sum / count;
    return `PID response chart (Live): ${avgTemp.toFixed(1)}C average temperature toward ${setpoint}C setpoint across ${count} samples`;
  }, [tempData, isLiveData, setpoint]);

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// pid tuner</div>
          <h2 className="hacker-inspector-title">Fan Control Loop</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span
            className={`hacker-inspector-status ${isLiveData ? '' : 'warning'}`}
            title={isLiveData ? 'PID state read from the device' : 'Read Current to load live device state'}
          >
            {isLiveData ? 'LIVE DATA' : 'AWAITING LIVE STATE'}
          </span>
          <span className={`hacker-inspector-status ${isProxyMode ? 'warning' : ''}`}>
            {isProxyMode ? 'PROXY MODE' : `SP ${setpoint.toFixed(1)}C`}
          </span>
        </div>
      </header>

      <div className="hacker-inspector-body">
      <div className="advanced-split-layout">
        {/* Left column: controls + visualization */}
        <div className="pid-col">
          {/* Parameters */}
          <div className="glass-card adv-card">
            <div className="adv-card-title pid-params-title adv-flex-center" style={{ display: 'flex' }}>
              Parameters
              <InfoDot term="pid_gain" />
            </div>
            {isProxyMode && (
              <div className="adv-msg is-warn adv-mb-12" style={{ fontSize: '0.78rem' }}>
                PID writes disabled: bosminer owns fan control in proxy/hybrid mode.
              </div>
            )}

            {/* Kp */}
            <div className="pid-param">
              <div className="pid-param-head">
                <label className="adv-hint">Kp (Proportional)</label>
                <span className="pid-param-val" style={{ color: 'var(--blue, #3b82f6)' }}>
                  {kp.toFixed(2)}
                </span>
              </div>
              <input
                type="range" className="voltage-slider"
                min={0} max={10} step={0.1} value={kp}
                onChange={e => setKp(Number(e.target.value))}
                aria-label="Kp proportional gain"
              />
            </div>

            {/* Ki */}
            <div className="pid-param">
              <div className="pid-param-head">
                <label className="adv-hint">Ki (Integral)</label>
                <span className="pid-param-val" style={{ color: 'var(--green, #2DD4A0)' }}>
                  {ki.toFixed(3)}
                </span>
              </div>
              <input
                type="range" className="voltage-slider"
                min={0} max={2} step={0.01} value={ki}
                onChange={e => setKi(Number(e.target.value))}
                aria-label="Ki integral gain"
              />
            </div>

            {/* Kd */}
            <div className="pid-param">
              <div className="pid-param-head">
                <label className="adv-hint">Kd (Derivative)</label>
                <span className="pid-param-val" style={{ color: 'var(--accent, #FAA500)' }}>
                  {kd.toFixed(2)}
                </span>
              </div>
              <input
                type="range" className="voltage-slider"
                min={0} max={5} step={0.1} value={kd}
                onChange={e => setKd(Number(e.target.value))}
                aria-label="Kd derivative gain"
              />
            </div>

            {/* Setpoint */}
            <div className="pid-param">
              <div className="pid-param-head">
                <label className="adv-hint">Setpoint (target temp)</label>
                <span className="pid-param-val" style={{ color: 'var(--yellow)' }}>
                  {setpoint}C
                </span>
              </div>
              <input
                type="range" className="voltage-slider"
                min={30} max={75} step={1} value={setpoint}
                onChange={e => setSetpoint(Number(e.target.value))}
                aria-label="Setpoint target temperature"
              />
            </div>

            <div className="advanced-inline-actions">
              <ActionButton
                label="Apply"
                onClick={handleApply}
                disabled={isProxyMode || !isLiveData}
                confirm={`Apply PID params: Kp=${kp}, Ki=${ki}, Kd=${kd}, setpoint=${setpoint}C?`}
              />
              <ActionButton label="Read Current" onClick={handleReadCurrent} variant="secondary" />
            </div>
            <CliHint cmd={`pid set --kp ${kp} --ki ${ki} --kd ${kd} --setpoint ${setpoint}`} />

            {statusMsg && <div className="adv-msg is-success is-mt">{statusMsg}</div>}
            {errorMsg && <div className="adv-msg is-error is-mt">{errorMsg}</div>}
          </div>

          {/* PID Output Gauge */}
          <div className="glass-card adv-card">
            <div className="adv-card-title is-sm-tight">
              PID Output
            </div>
            <PidGauge value={output} max={100} />
          </div>

          {/* P/I/D Term Bars */}
          <div className="glass-card adv-card">
            <div className="adv-card-title is-sm">
              PID Terms
            </div>
            <PidTermBar label="P (Proportional)" value={pTerm} color="var(--blue)" maxRange={50} />
            <PidTermBar label="I (Integral)" value={iTerm} color="var(--green)" maxRange={20} />
            <PidTermBar label="D (Derivative)" value={dTerm} color="var(--accent)" maxRange={30} />
          </div>

          {/* Debug readout */}
          <div className="glass-card adv-card">
            <div className="adv-card-title is-sm-tight adv-flex-center" style={{ display: 'flex', gap: 8 }}>
              State
              <span className={`hacker-inspector-status ${isLiveData ? '' : 'warning'}`}>
                {isLiveData ? 'LIVE' : 'AWAITING LIVE STATE'}
              </span>
            </div>
            <div className="adv-mono-block pid-state">
              <div>Current Temp: <span style={{ color: 'var(--text)' }}>{isLiveData ? `${currentTemp.toFixed(1)}C` : '--'}</span></div>
              <div>PID Output:   <span style={{ color: 'var(--accent-secondary)' }}>{isLiveData ? output.toFixed(1) : '--'}</span></div>
              <div>Integral:     <span style={{ color: 'var(--text-dim)' }}>{isLiveData ? integral.toFixed(3) : '--'}</span></div>
              <div>Last Error:   <span style={{ color: 'var(--text-dim)' }}>{isLiveData ? lastError.toFixed(3) : '--'}</span></div>
            </div>
          </div>
        </div>

        {/* Right column: chart */}
        <div className="glass-card adv-card pid-chart-card">
          <div className="adv-card-title pid-chart-title">
            PID Response
          </div>
          {!isLiveData && (
            <div className="adv-hint is-xs adv-mb-8">
              Read current PID state to begin. No chart data is shown until the daemon returns live values.
            </div>
          )}
          <div className="pid-legend">
            <span><span style={{ color: '#FFD700' }}>---</span> Setpoint</span>
            <span><span style={{ color: 'var(--term-green)' }}>---</span> Temperature</span>
            <span><span style={{ color: 'var(--accent)' }}>---</span> PID Output</span>
          </div>
          <SvgChart
            series={[
              { data: setpointData, color: '#FFD700', label: 'Setpoint', dashed: true, yAxis: 'left' },
              { data: tempData, color: 'var(--term-green)', label: 'Temp', yAxis: 'left' },
              { data: outputData, color: 'var(--accent)', label: 'PID Output', yAxis: 'right' },
            ]}
            height={260}
            summaryText={chartSummary}
          />

          {/* Setpoint vs Actual mini display */}
          <div className="pid-sp-actual">
            <div>
              <span className="pid-sp-label">SETPOINT</span>
              <span className="pid-sp-value" style={{ color: '#FFD700' }}>{setpoint}C</span>
            </div>
            <div>
              <span className="pid-sp-label">ACTUAL</span>
              <span
                className="pid-sp-value"
                style={{ color: Math.abs(currentTemp - setpoint) < 2 ? 'var(--term-green)' : 'var(--accent)' }}
              >
                {isLiveData ? `${currentTemp.toFixed(1)}C` : '--'}
              </span>
            </div>
            <div>
              <span className="pid-sp-label">DELTA</span>
              <span
                className="pid-sp-value"
                style={{ color: Math.abs(currentTemp - setpoint) < 2 ? 'var(--green)' : 'var(--red)' }}
              >
                {isLiveData ? `${(currentTemp - setpoint) >= 0 ? '+' : ''}${(currentTemp - setpoint).toFixed(1)}C` : '--'}
              </span>
            </div>
          </div>
        </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>Kp {kp.toFixed(2)}</span>
          <span>Ki {ki.toFixed(2)}</span>
          <span>Kd {kd.toFixed(2)}</span>
          <span>SP {setpoint.toFixed(1)} C</span>
        </div>
      </footer>
    </div>
  );
}
