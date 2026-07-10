import React, { useState, useEffect, useCallback } from 'react';
import { api } from '../../api/client';
import { ActionButton } from '../common/ActionButton';
import { picToVoltage, formatHex } from '../../utils/format';
import { useActiveHardware } from '../../hooks/useActiveHardware';
import { useMinerStore } from '../../store/miner';
import { tierFromPlatformKey } from '../../utils/platformCapabilities';
import { useSystemHealth } from '../common/proxy/SystemHealthContext';
import { InfoDot } from '../common/Tooltip';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';

interface ChainVoltage {
  chain: number;
  picValue: number;
  livePicValue: number | null;
  liveVoltage: number | null;
}

interface VoltageHistoryPoint {
  time: number;
  chain: number;
  picValue: number;
  voltage: number;
}

// Safe voltage range for BM1387 (S9): approximately 7.5V - 9.5V
const SAFE_MIN_V = 7.5;
const SAFE_MAX_V = 9.5;
const PIC_MIN = 40;
const PIC_MAX = 200;
const SLIDER_RANGE_V = { min: 7.5, max: 9.5 };

// PIC I2C addresses per chain
const PIC_ADDRS: Record<number, number> = { 6: 0x55, 7: 0x56, 8: 0x57 };

// PIC16F1704 app-mode command register that returns the current DAC code
// (S9 / BM1387). Read explicitly — never reuse the mode-detect status byte.
const PIC_GET_VOLTAGE = 0x18;

// Voltage zone classification
function getVoltageZone(v: number): { zone: 'safe' | 'caution' | 'danger'; color: string; label: string } {
  if (v >= 8.5 && v <= 9.2) return { zone: 'safe', color: 'var(--green, #2DD4A0)', label: 'Safe' };
  if (v >= 7.5 && v <= 9.5) return { zone: 'caution', color: 'var(--yellow, #F0B429)', label: 'Caution' };
  return { zone: 'danger', color: 'var(--red, #EF4444)', label: 'Danger' };
}

// Voltage slider zone background
function VoltageZoneBar() {
  // 7.5 - 9.5V total range
  // Danger: 7.5-8.5 (left), Safe: 8.5-9.2 (middle), Caution: 9.2-9.5 (right)
  const dangerLeftPct = ((8.5 - SLIDER_RANGE_V.min) / (SLIDER_RANGE_V.max - SLIDER_RANGE_V.min)) * 100;
  const safePct = ((9.2 - 8.5) / (SLIDER_RANGE_V.max - SLIDER_RANGE_V.min)) * 100;
  const cautionRightPct = ((9.5 - 9.2) / (SLIDER_RANGE_V.max - SLIDER_RANGE_V.min)) * 100;

  return (
    <div className="adv-zone-bar">
      <div style={{ width: `${dangerLeftPct}%`, background: 'var(--yellow, #F0B429)' }} title="Caution (low)" />
      <div style={{ width: `${safePct}%`, background: 'var(--green, #2DD4A0)' }} title="Safe" />
      <div style={{ width: `${cautionRightPct}%`, background: 'var(--yellow, #F0B429)' }} title="Caution (high)" />
    </div>
  );
}

export function VoltageControl() {
  const { activeChain, setActiveChain } = useActiveHardware();
  const { isProxyMode } = useSystemHealth();
  const systemInfo = useMinerStore(s => s.systemInfo);
  // HACK-B-005: this interactive programmer hardcodes S9 (BM1387 / PIC16F1704)
  // constants — safe range 7.5–9.5V, DAC codes 40–200, PIC addresses 0x55–0x57,
  // and the picToVoltage formula. They are wrong (and Apply could mis-program)
  // on any other controller (am2 dsPIC, am3-aml TAS5782M, am3-bb dsPIC). Gate the
  // tool to S9 (am1-zynq) only; fail closed on unknown/other platforms.
  const isS9 = tierFromPlatformKey(systemInfo?.platform_key) === 'am1-zynq';
  const [chains, setChains] = useState<ChainVoltage[]>([
    { chain: 6, picValue: 100, livePicValue: null, liveVoltage: null },
    { chain: 7, picValue: 100, livePicValue: null, liveVoltage: null },
    { chain: 8, picValue: 100, livePicValue: null, liveVoltage: null },
  ]);
  const [statusMsg, setStatusMsg] = useState<Record<number, string>>({});
  const [errorMsg, setErrorMsg] = useState<Record<number, string>>({});
  const [safeLock, setSafeLock] = useState(true);
  const [voltageHistory, setVoltageHistory] = useState<VoltageHistoryPoint[]>([]);
  const [picInfo, setPicInfo] = useState<Record<number, string>>({});
  const [loading, setLoading] = useState(true);

  // Fetch current PIC mode + live voltage on mount (S9 / PIC16F1704 only).
  const fetchLiveValues = useCallback(async () => {
    // HACK-B-005: never probe the S9 PIC addresses on a non-S9 controller.
    if (!isS9) {
      setLoading(false);
      return;
    }
    const updated = [...chains];
    for (const cv of updated) {
      try {
        const picAddr = PIC_ADDRS[cv.chain];
        // (1) Mode-detect read (register-less): 0x60=APP, 0xCC=BOOTLOADER.
        // HACK-B-006: this status byte is NOT a DAC voltage code — never derive
        // the 'Actual' voltage from it.
        const modeRes = await api.readI2c(0, formatHex(picAddr, 2));
        const modeByte = (modeRes.data ?? [])[0] ?? null;
        if (modeByte === 0x60) {
          setPicInfo(prev => ({ ...prev, [cv.chain]: 'APP MODE (ready)' }));
        } else if (modeByte === 0xCC) {
          setPicInfo(prev => ({ ...prev, [cv.chain]: 'BOOTLOADER (needs JUMP)' }));
        } else if (modeByte !== null) {
          setPicInfo(prev => ({ ...prev, [cv.chain]: `Response: 0x${modeByte.toString(16)}` }));
        } else {
          setPicInfo(prev => ({ ...prev, [cv.chain]: 'No response' }));
        }

        // (2) Explicit GET_VOLTAGE (0x18) read — the only honest source of the
        // live DAC code, separate from the mode-detect read. Only meaningful in
        // APP mode; the bootloader has no operating voltage to report. With no
        // real readback the 'Actual' field stays '—' rather than a fabricated
        // number.
        if (modeByte === 0x60) {
          const voltRes = await api.readI2c(0, formatHex(picAddr, 2), formatHex(PIC_GET_VOLTAGE, 2));
          const dacCode = (voltRes.data ?? [])[0] ?? null;
          if (dacCode !== null) {
            cv.livePicValue = dacCode;
            cv.liveVoltage = picToVoltage(dacCode);
            cv.picValue = dacCode;
          }
        }
      } catch {
        setPicInfo(prev => ({ ...prev, [cv.chain]: 'Read failed' }));
      }
    }
    setChains(updated);
    setLoading(false);
  }, [isS9]);

  useEffect(() => {
    fetchLiveValues();
  }, [fetchLiveValues]);

  // HACK-B-005: on any non-S9 controller render an honest unavailable state
  // instead of the S9-only programmer (whose ranges, addresses, and DAC formula
  // would be wrong and whose Apply could mis-program the hardware).
  if (!isS9) {
    return (
      <div className="hacker-inspector">
        <header className="hacker-inspector-header">
          <div className="hacker-inspector-title-group">
            <div className="hacker-inspector-eyebrow">// voltage control</div>
            <h2 className="hacker-inspector-title">PIC Voltage Programmer</h2>
          </div>
          <div className="hacker-inspector-actions">
            <span className="hacker-inspector-status neutral">S9 ONLY</span>
          </div>
        </header>
        <div className="hacker-inspector-body">
          <div className="glass-card adv-card">
            <div className="adv-card-title">Per-chip voltage control unavailable</div>
            <div className="adv-empty-note">
              This interactive programmer speaks only the Antminer S9 (BM1387 /
              PIC16F1704) protocol — the fixed 7.5–9.5V safe range, DAC codes
              40–200, PIC addresses 0x55–0x57, and the picToVoltage DAC formula
              are S9-specific. Per-chip voltage control for this controller
              (dsPIC / TAS5782M) is not available in this tool. Use the PSU Lab /
              power tools for this platform instead.
            </div>
          </div>
        </div>
        <footer className="hacker-inspector-footer">
          <div className="hacker-inspector-stats">
            <span>S9 / BM1387 / PIC16F1704 only</span>
            <span>{systemInfo?.chip_type ?? 'unknown chip'}</span>
          </div>
        </footer>
      </div>
    );
  }

  const updateChain = (chainId: number, picValue: number) => {
    if (safeLock) {
      const voltage = picToVoltage(picValue);
      if (voltage < SAFE_MIN_V || voltage > SAFE_MAX_V) return;
    }
    setChains(prev => prev.map(c => c.chain === chainId ? { ...c, picValue } : c));
  };

  const sliderMin = safeLock ? PIC_MIN : 0;
  const sliderMax = safeLock ? PIC_MAX : 255;

  const stepValue = (chainId: number, delta: number) => {
    const chain = chains.find(c => c.chain === chainId);
    if (!chain) return;
    const newVal = Math.max(sliderMin, Math.min(sliderMax, chain.picValue + delta));
    updateChain(chainId, newVal);
  };

  const handleApply = async (chainId: number) => {
    const chain = chains.find(c => c.chain === chainId);
    if (!chain) return;

    setStatusMsg(prev => ({ ...prev, [chainId]: '' }));
    setErrorMsg(prev => ({ ...prev, [chainId]: '' }));
    if (isProxyMode) {
      setErrorMsg(prev => ({ ...prev, [chainId]: 'Blocked: bosminer owns voltage hardware in proxy/hybrid mode.' }));
      return;
    }

    try {
      echoCli(`volt set ${chainId} --pic ${chain.picValue}`, `est. ${picToVoltage(chain.picValue).toFixed(3)}V`);
      const res = await api.setChipVoltage({
        chain: chainId,
        pic_value: chain.picValue,
        confirm: true,
      });
      setStatusMsg(prev => ({
        ...prev,
        [chainId]: `Applied: PIC=${chain.picValue}, est. ${res.estimated_voltage_v.toFixed(3)}V`,
      }));

      setVoltageHistory(prev => [...prev, {
        time: Date.now(),
        chain: chainId,
        picValue: chain.picValue,
        voltage: res.estimated_voltage_v,
      }].slice(-100));

      // Update live readback after successful apply
      setChains(prev => prev.map(c =>
        c.chain === chainId
          ? { ...c, livePicValue: chain.picValue, liveVoltage: res.estimated_voltage_v }
          : c
      ));

      if (res.warning) {
        setErrorMsg(prev => ({ ...prev, [chainId]: res.warning! }));
      }
    } catch (e: unknown) {
      setErrorMsg(prev => ({
        ...prev,
        [chainId]: e instanceof Error ? e.message : 'Failed to set voltage',
      }));
    }
  };

  // Sparkline renderer
  const renderSparkline = (chainId: number) => {
    const points = voltageHistory.filter(p => p.chain === chainId).slice(-20);
    if (points.length < 2) return null;
    const min = Math.min(...points.map(p => p.voltage));
    const max = Math.max(...points.map(p => p.voltage));
    const range = max - min || 0.1;
    const chars = '\u2581\u2582\u2583\u2584\u2585\u2586\u2587\u2588';
    const spark = points.map(p => {
      const idx = Math.round(((p.voltage - min) / range) * (chars.length - 1));
      return chars[idx];
    }).join('');
    return (
      <div className="adv-spark">
        {spark}
        <span className="adv-spark-range">
          {min.toFixed(3)}-{max.toFixed(3)}V
        </span>
      </div>
    );
  };

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// voltage control</div>
          <h2 className="hacker-inspector-title">PIC Voltage Programmer</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${safeLock ? '' : 'danger'}`}>
            {safeLock ? 'SAFE LOCK' : 'UNLOCKED'}
          </span>
          <button
            className="hacker-inspector-help"
            onClick={() => setSafeLock(!safeLock)}
          >
            {safeLock ? '🔓 UNLOCK' : '🔒 LOCK'}
          </button>
          <button className="hacker-inspector-refresh" onClick={fetchLiveValues}>⟳ REFRESH</button>
        </div>
      </header>

      <div className="hacker-inspector-toolbar">
        <span id="voltage-control-danger-note" className="adv-danger-note">
          WARNING: Incorrect voltage settings can permanently damage hash board ASICs.
          {safeLock && <span className="adv-safe-em"> Safe Lock prevents values outside {SAFE_MIN_V}V-{SAFE_MAX_V}V.</span>}
        </span>
      </div>

      <div className="hacker-inspector-body">
      {isProxyMode && (
        <div className="adv-warn-strong">
          Voltage writes disabled: bosminer owns hardware in proxy/hybrid mode.
        </div>
      )}

      {loading && (
        <div className="glass-card adv-loading-card">
          <span>Reading PIC voltage controllers...</span>
        </div>
      )}

      <div className="advanced-grid-3">
        {chains.map(cv => {
          const voltage = picToVoltage(cv.picValue);
          const isUnsafe = voltage < SAFE_MIN_V || voltage > SAFE_MAX_V;
          const picAddr = PIC_ADDRS[cv.chain];
          const zone = getVoltageZone(voltage);
          const hasLiveData = cv.livePicValue !== null;
          const configuredV = voltage;
          const actualV = cv.liveVoltage;
          const voltageMatch = hasLiveData && actualV !== null && Math.abs(configuredV - actualV) < 0.05;
          const voltageDelta = hasLiveData && actualV !== null ? configuredV - actualV : null;

          return (
            <div key={cv.chain} className="glass-card adv-card">
              {/* PIC info header */}
              <div className="adv-flex-between adv-mb-8">
                <button
                  type="button"
                  className="advanced-link-button vc-chain-btn"
                  onClick={() => setActiveChain(cv.chain)}
                  aria-label={`Set chain ${cv.chain} active`}
                >
                  Chain {cv.chain}
                </button>
                <span
                  className="vc-pic-status"
                  style={{
                    color: picInfo[cv.chain]?.includes('APP MODE') ? 'var(--green)'
                      : picInfo[cv.chain]?.includes('BOOTLOADER') ? 'var(--yellow)'
                      : 'var(--text-dim)',
                  }}
                >
                  PIC {formatHex(picAddr, 2)}: {picInfo[cv.chain] || '---'}
                </span>
              </div>

              {/* Configured vs Actual comparison */}
              <div className="advanced-grid-2 adv-mb-12">
                <div className="vc-readout">
                  <div className="vc-readout-label">
                    Configured
                  </div>
                  <div className="vc-readout-value" style={{ color: zone.color }}>
                    {configuredV.toFixed(3)}V
                  </div>
                </div>
                <div className="vc-readout">
                  <div className="vc-readout-label">
                    Actual
                  </div>
                  {hasLiveData && actualV !== null ? (
                    <div className="adv-flex-center" style={{ gap: 6 }}>
                      <span className="vc-readout-value" style={{ color: getVoltageZone(actualV).color }}>
                        {actualV.toFixed(3)}V
                      </span>
                      {voltageMatch ? (
                        <span className="vc-match-ok" title="Match">&#10003;</span>
                      ) : (
                        <span className="vc-match-delta" title={`Delta: ${voltageDelta?.toFixed(3)}V`}>
                          &#9888; {voltageDelta !== null ? `${voltageDelta > 0 ? '+' : ''}${voltageDelta.toFixed(3)}V` : ''}
                        </span>
                      )}
                    </div>
                  ) : (
                    <div className="vc-readout-value is-dim">---</div>
                  )}
                </div>
              </div>

              {/* PIC value display */}
              <div className="vc-picval-row">
                <span className="adv-hint">
                  PIC Value
                  <InfoDot content="The 8-bit PIC DAC code. pic_val = round(1608.420446 − 170.423497 × voltage_V). Lower code = higher chip rail voltage. Out-of-range codes can permanently damage the hash board." />
                </span>
                <div className="adv-flex-center">
                  <button
                    className="btn btn-secondary adv-step-btn"
                    onClick={() => stepValue(cv.chain, -1)}
                    aria-label={`Decrease chain ${cv.chain} PIC value`}
                  >
                    -
                  </button>
                  <span className="vc-picval">
                    {cv.picValue}
                  </span>
                  <button
                    className="btn btn-secondary adv-step-btn"
                    onClick={() => stepValue(cv.chain, 1)}
                    aria-label={`Increase chain ${cv.chain} PIC value`}
                  >
                    +
                  </button>
                </div>
              </div>

              {/* Voltage zone bar */}
              <VoltageZoneBar />

              {/* Slider */}
              <input
                type="range"
                className="voltage-slider vc-slider"
                min={sliderMin}
                max={sliderMax}
                step={1}
                value={cv.picValue}
                onChange={e => updateChain(cv.chain, Number(e.target.value))}
                aria-label={`Chain ${cv.chain} chip voltage PIC DAC code, ${sliderMin} to ${sliderMax}`}
                aria-valuetext={`PIC ${cv.picValue}, ${voltage.toFixed(3)}V, ${zone.label} zone${isUnsafe ? ', outside safe range' : ''}`}
                aria-describedby="voltage-control-danger-note"
              />

              {/* Zone label */}
              <div className="adv-zone-label" style={{ color: zone.color }}>
                {zone.label} Zone ({voltage.toFixed(2)}V)
              </div>

              {/* Numeric input (synced with slider, step 1 mV) */}
              <div className="advanced-inline-actions adv-mb-12">
                <input
                  type="number"
                  min={sliderMin}
                  max={sliderMax}
                  step={1}
                  value={cv.picValue}
                  onChange={e => updateChain(cv.chain, Number(e.target.value))}
                  aria-label={`Chain ${cv.chain} PIC value (numeric)`}
                  className="adv-in-sm adv-in-mono"
                />
                <span className="adv-hint">
                  ({sliderMin}-{sliderMax}{!safeLock ? ' full DAC range' : ''}, step 1)
                </span>
              </div>

              {/* Voltage history sparkline */}
              {renderSparkline(cv.chain)}

              {/* Apply button */}
              <div className="adv-mt-8">
                <ActionButton
                  label="Apply Voltage"
                  onClick={() => handleApply(cv.chain)}
                  variant={isUnsafe ? 'danger' : 'primary'}
                  disabled={isProxyMode || (safeLock && isUnsafe)}
                  confirm={
                    isUnsafe
                      ? `DANGER: Setting chain ${cv.chain} to PIC=${cv.picValue} (${voltage.toFixed(3)}V) is OUTSIDE the safe range. This can permanently damage your hash board. Are you absolutely sure?`
                      : `Set chain ${cv.chain} voltage to PIC=${cv.picValue} (est. ${voltage.toFixed(3)}V)?`
                  }
                />
                <CliHint cmd={`volt set ${cv.chain} --pic ${cv.picValue}`} note={`est. ${voltage.toFixed(3)}V`} />
              </div>

              {statusMsg[cv.chain] && (
                <div className="adv-msg is-success is-sm is-mt">
                  {statusMsg[cv.chain]}
                </div>
              )}
              {errorMsg[cv.chain] && (
                <div className="adv-msg is-error is-sm is-mt">
                  {errorMsg[cv.chain]}
                </div>
              )}
            </div>
          );
        })}
      </div>

      {/* Reference table */}
      <div className="glass-card adv-card adv-card-mt">
        <div className="adv-card-title">
          PIC Value Reference (S9 / BM1387)
        </div>
        <div className="adv-mono-block">
          <div>Formula: pic_val = round(1608.420446 - 170.423497 * voltage_V)</div>
          <div>Inverse: voltage_V = (1608.42 - pic_val) / 170.42</div>
          <div className="adv-mt-8">
            {'PIC   0 ->  9.44V (max)   |  PIC   6 ->  9.40V (init)  |  PIC  57 -> ~9.10V (oper)'}
          </div>
          <div>
            {'PIC  92 ->  8.90V (low)   |  PIC 140 ->  8.62V         |  PIC 255 ->  7.94V (min)'}
          </div>
          <div className="adv-mt-8 adv-mono-block-dim">
            PIC addresses: Chain 6=0x55, Chain 7=0x56, Chain 8=0x57
          </div>
        </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>safe range {SAFE_MIN_V}V – {SAFE_MAX_V}V</span>
          <span>{safeLock ? 'locked' : 'UNLOCKED'}</span>
        </div>
      </footer>
    </div>
  );
}
