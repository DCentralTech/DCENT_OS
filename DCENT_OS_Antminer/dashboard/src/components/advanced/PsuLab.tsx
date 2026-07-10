import React, { useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type { PsuControlRequest, PsuControlResponse, PsuTroubleshootResponse } from '../../api/types';
import { useMinerStore } from '../../store/miner';
import { selectIsMining } from '../../utils/miningStatus';
import { useSystemHealth } from '../common/proxy/SystemHealthContext';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';

// Map a PSU control action to its `dcent …` command equivalent (design vocab).
function psuCliCmd(action: PsuControlRequest['action'], voltageV?: number): string {
  switch (action) {
    case 'set_voltage':
      return `psu set vout ${typeof voltageV === 'number' ? voltageV.toFixed(2) : voltageV}`;
    case 'enable_output':
      return 'psu output on';
    case 'disable_output':
      return 'psu output off';
    case 'enable_watchdog':
      return 'psu watchdog enable';
    case 'feed_watchdog':
      return 'psu watchdog feed';
    case 'disable_watchdog':
      return 'psu watchdog disable';
    default:
      return `psu ${action}`;
  }
}

type HistoryEntry = {
  id: string;
  timestamp: number;
  action: string;
  status: PsuControlResponse['status'];
  message: string;
  measuredVoltageV?: number | null;
  outputEnabled?: boolean | null;
  outputGateEnabled?: boolean | null;
};

const HISTORY_STORAGE_KEY = 'dcentos-psu-history';
const MAX_HISTORY = 12;

function loadHistory(): HistoryEntry[] {
  try {
    const raw = localStorage.getItem(HISTORY_STORAGE_KEY);
    if (!raw) {
      return [];
    }
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed.slice(0, MAX_HISTORY);
  } catch {
    return [];
  }
}

function saveHistory(entries: HistoryEntry[]) {
  localStorage.setItem(HISTORY_STORAGE_KEY, JSON.stringify(entries.slice(0, MAX_HISTORY)));
}

function parseVoltageRange(range?: string | null): { min: number; max: number } | null {
  if (!range) {
    return null;
  }

  const match = range.match(/([0-9]+(?:\.[0-9]+)?)\s*V\s*[-–]\s*([0-9]+(?:\.[0-9]+)?)\s*V/i);
  if (!match) {
    return null;
  }

  return {
    min: parseFloat(match[1]),
    max: parseFloat(match[2]),
  };
}

function formatMaybeVoltage(value?: number | null): string {
  return typeof value === 'number' ? `${value.toFixed(2)} V` : '---';
}

function toneColor(status: PsuControlResponse['status'] | 'idle'): string {
  switch (status) {
    case 'ok':
      return 'var(--green)';
    case 'error':
      return 'var(--red)';
    case 'not_implemented':
      return 'var(--yellow)';
    default:
      return 'var(--text-dim)';
  }
}

export function PsuLab() {
  const addToast = useMinerStore(s => s.addToast);
  const status = useMinerStore(s => s.status);
  const { isProxyMode } = useSystemHealth();
  const [diag, setDiag] = useState<PsuTroubleshootResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [acting, setActing] = useState(false);
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [armed, setArmed] = useState(false);
  const [allowOutputGate, setAllowOutputGate] = useState(false);
  const [allowVoltageSet, setAllowVoltageSet] = useState(false);
  const [ackLivePower, setAckLivePower] = useState(false);
  const [targetVoltage, setTargetVoltage] = useState('15.00');
  const [lastResult, setLastResult] = useState<{ status: PsuControlResponse['status'] | 'idle'; message: string } | null>(null);
  const [history, setHistory] = useState<HistoryEntry[]>(() => loadHistory());

  // Canonical whole-miner mining state (Omega P0-7 / C-8) — gates the live-power
  // acknowledgement for output/voltage actions. Sharing the selector keeps this
  // safety gate consistent with the rest of the dashboard (and slightly more
  // conservative: it also trips on 5 s-only or single-chain hashing).
  const isMining = selectIsMining(status);
  const voltageRange = useMemo(() => parseVoltageRange(diag?.voltage_range), [diag?.voltage_range]);

  const refresh = async (showToastOnError = false) => {
    setLoading(true);
    try {
      const next = await api.troubleshootPsu();
      setDiag(next);
      if (typeof next.voltage_out === 'number') {
        setTargetVoltage(next.voltage_out.toFixed(2));
      }
    } catch {
      if (showToastOnError) {
        addToast('Could not refresh PSU status', 'error');
      }
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  useEffect(() => {
    if (!autoRefresh) {
      return;
    }

    const timer = window.setInterval(() => {
      refresh();
    }, 5000);

    return () => window.clearInterval(timer);
  }, [autoRefresh]);

  const pushHistory = (entry: HistoryEntry) => {
    setHistory(prev => {
      const next = [entry, ...prev].slice(0, MAX_HISTORY);
      saveHistory(next);
      return next;
    });
  };

  const resetInterlocks = () => {
    setArmed(false);
    setAllowOutputGate(false);
    setAllowVoltageSet(false);
    setAckLivePower(false);
  };

  const runAction = async (action: PsuControlRequest['action']) => {
    const isOutputAction = action === 'enable_output' || action === 'disable_output';
    const isVoltageAction = action === 'set_voltage';
    const isDangerous = isOutputAction || isVoltageAction || action === 'disable_watchdog';

    if (isProxyMode) {
      const message = 'Blocked: bosminer owns PSU hardware in proxy/hybrid mode.';
      setLastResult({ status: 'error', message });
      addToast(message, 'warning');
      return;
    }
    if (!armed) {
      addToast('Arm the PSU controls before sending live power actions', 'warning');
      return;
    }
    if (isOutputAction && !allowOutputGate) {
      addToast('Acknowledge output gating before enabling or disabling PSU output', 'warning');
      return;
    }
    if (isVoltageAction && !allowVoltageSet) {
      addToast('Acknowledge voltage programming before changing APW output voltage', 'warning');
      return;
    }
    if ((isOutputAction || isVoltageAction) && isMining && !ackLivePower) {
      addToast('Acknowledge that the miner is energized before changing live PSU state', 'warning');
      return;
    }

    const request: PsuControlRequest = { action, confirm: true };
    if (isVoltageAction) {
      const voltage = Number(targetVoltage);
      if (!Number.isFinite(voltage)) {
        addToast('Enter a valid target voltage', 'warning');
        return;
      }
      if (voltageRange && (voltage < voltageRange.min || voltage > voltageRange.max)) {
        addToast(`Voltage must stay within ${voltageRange.min.toFixed(2)} V - ${voltageRange.max.toFixed(2)} V`, 'warning');
        return;
      }
      request.voltage_v = voltage;
    }

    setActing(true);
    try {
      echoCli(psuCliCmd(action, request.voltage_v));
      const response = await api.controlPsu(request);
      setLastResult({ status: response.status, message: response.message });
      addToast(response.message, response.status === 'ok' ? 'success' : response.status === 'not_implemented' ? 'warning' : 'error');

      pushHistory({
        id: `${Date.now()}-${action}`,
        timestamp: Date.now(),
        action,
        status: response.status,
        message: response.message,
        measuredVoltageV: response.measured_voltage_v ?? response.voltage_out ?? null,
        outputEnabled: response.output_enabled ?? null,
        outputGateEnabled: response.output_gate_enabled ?? null,
      });

      await refresh();
    } catch {
      const message = 'PSU action failed';
      setLastResult({ status: 'error', message });
      addToast(message, 'error');
      pushHistory({
        id: `${Date.now()}-${action}`,
        timestamp: Date.now(),
        action,
        status: 'error',
        message,
      });
    } finally {
      setActing(false);
      if (isDangerous) {
        resetInterlocks();
      }
    }
  };

  // : OUTPUT-OFF and NO-DATA previously both rendered 'neutral', so the
  // badge looked identical for "PSU off" vs "no telemetry". Give OUTPUT-OFF a
  // distinct 'danger' tone; only genuine no-data stays 'neutral'.
  const psuStatusTone = isProxyMode
    ? 'warning'
    : diag?.output_enabled
      ? ''
      : diag?.output_enabled === false
        ? 'danger'
        : 'neutral';
  const psuStatusLabel = isProxyMode
    ? 'BOSMINER OWNS'
    : diag?.output_enabled
      ? 'OUTPUT ON'
      : diag?.output_enabled === false
        ? 'OUTPUT OFF'
        : 'NO DATA';

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// psu lab</div>
          <h2 className="hacker-inspector-title">APW Power Control</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${psuStatusTone}`}>{psuStatusLabel}</span>
          <label className="pl-auto-label">
            <input type="checkbox" checked={autoRefresh} onChange={e => setAutoRefresh(e.target.checked)} />
            auto
          </label>
          <button className="hacker-inspector-refresh" onClick={() => refresh(true)} disabled={loading || acting}>⟳ REFRESH</button>
        </div>
      </header>

      <div className="hacker-inspector-body">
      <div className="pl-outer">
      <div className="psu-lab-grid">
        <div className="register-inspector ds-card-hover pl-card">
          <div className="pl-head">
            <div>
              <div className="pl-head-title">PSU Control Lab</div>
              <div className="pl-head-desc">
                Live APW and output-gate controls with guard rails. Use this page for repeatable power actions instead of one-off modal commands.
              </div>
            </div>
          </div>

          <div className="pl-stat-grid">
            {[
              ['Model', diag?.model || '---'],
              ['Transport', diag?.transport || '---'],
              ['Control Path', diag?.control_mode || '---'],
              ['Output', diag?.output_enabled == null ? '---' : diag.output_enabled ? 'Enabled' : 'Disabled'],
              ['Gate', diag?.output_gate_enabled == null ? '---' : diag.output_gate_enabled ? 'Enabled' : 'Disabled'],
              ['Voltage', formatMaybeVoltage(diag?.voltage_out)],
            ].map(([label, value]) => (
              <div key={label} className="pl-stat-cell">
                <div className="pl-stat-k">{label}</div>
                <div className="pl-stat-v">{value}</div>
              </div>
            ))}
          </div>

          <div className="pl-telemetry">
            {loading ? 'Refreshing PSU status...' : diag?.message || 'No PSU telemetry available.'}
          </div>

          {lastResult && (
            <div className="pl-result" style={{
              borderColor: toneColor(lastResult.status),
              color: toneColor(lastResult.status),
            }}>
              {lastResult.message}
            </div>
          )}

          <div className="pl-controls-stack">
            {isProxyMode && (
              <div className="pl-proxy-warn">
                PSU control actions disabled: bosminer owns hardware in proxy/hybrid mode.
              </div>
            )}
            <div className="pl-panel">
              <div className="pl-panel-title">Safety Interlocks</div>
              <div className="pl-interlocks">
                <label className="pl-interlock">
                  <input type="checkbox" checked={armed} onChange={e => setArmed(e.target.checked)} />
                  <span>I am on the correct miner and I want to send live PSU commands.</span>
                </label>
                {diag?.supports_output_gate && (
                  <label className="pl-interlock">
                    <input type="checkbox" checked={allowOutputGate} onChange={e => setAllowOutputGate(e.target.checked)} />
                    <span>I understand output gating can instantly remove or restore hash-board power.</span>
                  </label>
                )}
                {diag?.supports_voltage_set && (
                  <label className="pl-interlock">
                    <input type="checkbox" checked={allowVoltageSet} onChange={e => setAllowVoltageSet(e.target.checked)} />
                    <span>I understand wrong APW voltage values can damage attached hardware.</span>
                  </label>
                )}
                {isMining && (
                  <label className="pl-interlock">
                    <input type="checkbox" checked={ackLivePower} onChange={e => setAckLivePower(e.target.checked)} />
                    <span>The miner is currently energized and possibly hashing. I accept the risk of changing power live.</span>
                  </label>
                )}
              </div>
            </div>

            <div className="pl-panel">
              <div className="pl-panel-title is-mb10">Output Controls</div>
              <div className="pl-btn-wrap">
                <button className="btn btn-primary" onClick={() => runAction('enable_output')} disabled={isProxyMode || acting || !diag?.supports_output_gate}>Enable Output</button>
                <button className="btn btn-secondary" onClick={() => runAction('disable_output')} disabled={isProxyMode || acting || !diag?.supports_output_gate}>Disable Output</button>
                <button className="btn btn-secondary" onClick={() => runAction('enable_watchdog')} disabled={isProxyMode || acting || !diag?.supports_watchdog}>Enable Watchdog</button>
                <button className="btn btn-secondary" onClick={() => runAction('feed_watchdog')} disabled={isProxyMode || acting || !diag?.supports_watchdog}>Feed Watchdog</button>
                <button className="btn btn-secondary" onClick={() => runAction('disable_watchdog')} disabled={isProxyMode || acting || !diag?.supports_watchdog}>Disable Watchdog</button>
              </div>
              <CliHint cmd="psu output on" note="psu output off · psu watchdog enable|feed|disable" />
            </div>

            {diag?.supports_voltage_set && (
              <div className="pl-panel">
                <div className="pl-panel-title is-mb10">APW Voltage Programming</div>
                <div className="pl-voltage-row">
                  <div className="pl-voltage-field">
                    <label className="pl-voltage-label" htmlFor="psu-target-voltage">Target Voltage</label>
                    <input
                      id="psu-target-voltage"
                      type="number"
                      min={voltageRange?.min ?? 10}
                      max={voltageRange?.max ?? 21}
                      step="0.01"
                      value={targetVoltage}
                      onChange={e => setTargetVoltage(e.target.value)}
                      aria-label="Target PSU output voltage (volts)"
                      aria-describedby="psu-target-voltage-hint"
                    />
                  </div>
                  <button className="btn btn-primary" onClick={() => runAction('set_voltage')} disabled={isProxyMode || acting}>Set Voltage</button>
                </div>
                <CliHint cmd={`psu set vout ${Number.isFinite(Number(targetVoltage)) ? Number(targetVoltage).toFixed(2) : targetVoltage}`} />
                <div className="pl-voltage-hint" id="psu-target-voltage-hint">
                  Supported range: {diag.voltage_range || 'Unknown'}.
                </div>
              </div>
            )}
          </div>
        </div>

        <div className="register-inspector ds-card-hover pl-card">
          <div className="pl-hist-head">
            <div>
              <div className="pl-hist-title">Recent Action History</div>
              <div className="pl-hist-desc">Stored in this browser for quick operator recall.</div>
            </div>
            <button
              className="btn btn-secondary"
              onClick={() => {
                setHistory([]);
                saveHistory([]);
              }}
              disabled={history.length === 0}
            >
              Clear
            </button>
          </div>

          <div className="pl-hist-list">
            {history.length === 0 && (
              <div className="adv-empty-note">No PSU actions recorded yet.</div>
            )}
            {history.map(entry => (
              <div key={entry.id} className="pl-hist-entry" style={{ borderColor: toneColor(entry.status) }}>
                <div className="pl-hist-entry-head">
                  <div className="pl-hist-action">{entry.action}</div>
                  <div className="pl-hist-ts">{new Date(entry.timestamp).toLocaleString()}</div>
                </div>
                <div className="pl-hist-msg" style={{ color: toneColor(entry.status) }}>{entry.message}</div>
                <div className="pl-hist-meta">
                  <span>Measured: {formatMaybeVoltage(entry.measuredVoltageV)}</span>
                  {entry.outputEnabled != null && <span>Output: {entry.outputEnabled ? 'On' : 'Off'}</span>}
                  {entry.outputGateEnabled != null && <span>Gate: {entry.outputGateEnabled ? 'On' : 'Off'}</span>}
                </div>
              </div>
            ))}
          </div>
        </div>
      </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{diag?.model || 'no PSU'}</span>
          <span>{history.length} actions logged</span>
          {diag?.voltage_out != null && <span>{diag.voltage_out.toFixed(2)} V</span>}
        </div>
      </footer>
    </div>
  );
}
