import React, { useState, useEffect } from 'react';
import { useMinerStore } from '../../store/miner';
import { api } from '../../api/client';
import { OverlayDialog } from '../common/OverlayDialog';
import { ChipRailMvPill } from '../common/ChainPresencePanel';
import { unsupportedMetricList } from '../../utils/format';
import type { PsuControlRequest, PsuOverrideResponse, PsuOverrideModel, PsuTroubleshootResponse } from '../../api/types';

/** Gear icon SVG for PSU override button. */
function GearIcon({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none">
      <path d="M6.5 1h3l.4 2.1a5.5 5.5 0 011.3.7L13.3 2.9l2.1 2.1-.9 2.1c.3.4.5.8.7 1.3L17.3 8.8v3l-2.1.4a5.5 5.5 0 01-.7 1.3l.9 2.1-2.1 2.1-2.1-.9c-.4.3-.8.5-1.3.7L9.5 19.6h-3l-.4-2.1a5.5 5.5 0 01-1.3-.7L2.7 17.7.6 15.6l.9-2.1c-.3-.4-.5-.8-.7-1.3L-1.3 11.8v-3l2.1-.4c.2-.5.4-.9.7-1.3l-.9-2.1L2.7 2.9l2.1.9c.4-.3.8-.5 1.3-.7L6.5 1z"
        transform="scale(0.75) translate(1,1)"
        stroke="currentColor" strokeWidth="1.5" fill="none" />
      <circle cx="8" cy="8" r="2.5" stroke="currentColor" strokeWidth="1.5" fill="none" />
    </svg>
  );
}

function LightningIcon({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none">
      <path
        d="M9.1 1 3.8 8h3.4L6.9 15 12.2 8H8.8L9.1 1Z"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinejoin="round"
      />
    </svg>
  );
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

function formatOptionalVoltage(voltage?: number | null): string {
  return typeof voltage === 'number' ? `${voltage.toFixed(2)} V` : '---';
}

function formatCapabilityLabel(value?: boolean): string {
  if (value === true) {
    return 'Available';
  }
  if (value === false) {
    return 'No';
  }
  return '---';
}

function formatAutotunerValue(value?: string | null): string {
  if (!value) {
    return '---';
  }

  return value
    .split('_')
    .map(part => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ');
}

/** PSU Override Modal. */
export function PsuOverrideModal({
  onClose, availableModels, currentModel, currentActive, currentVoltage,
}: {
  onClose: () => void;
  availableModels: PsuOverrideModel[];
  currentModel: string;
  currentActive: boolean;
  currentVoltage: number;
}) {
  const [enabled, setEnabled] = useState(currentActive);
  const [model, setModel] = useState(currentModel || 'APW7');
  const [voltage, setVoltage] = useState(currentVoltage || 12.0);
  const [saving, setSaving] = useState(false);
  const [result, setResult] = useState<string | null>(null);

  const handleSave = async () => {
    setSaving(true);
    setResult(null);
    try {
      const data = await api.updatePsuOverride({ enabled, model, voltage_v: voltage });
      setResult(data.message || 'Saved');
      if (data.status === 'ok') {
        setTimeout(onClose, 1500);
      }
    } catch (e) {
      setResult('Failed to save PSU override');
    } finally {
      setSaving(false);
    }
  };

  return (
    <OverlayDialog open onClose={onClose} ariaLabel="PSU override" maxWidth={440}>
      <div style={{ padding: 24 }}>
        <div style={{
          fontSize: '1.1rem', fontWeight: 700, color: 'var(--text)',
          marginBottom: 16, display: 'flex', justifyContent: 'space-between',
        }}>
          PSU Override
          <button onClick={onClose} style={{
            background: 'none', border: 'none', color: 'var(--text-dim)',
            cursor: 'pointer', fontSize: '1.2rem',
          }}>&times;</button>
        </div>

        {/* Warning box */}
        <div style={{
          background: 'rgba(247,147,26,0.1)', border: '1px solid rgba(247,147,26,0.3)',
          borderRadius: 8, padding: 12, marginBottom: 16, fontSize: '0.8rem',
          color: 'var(--text-dim)', lineHeight: 1.5,
        }}>
          For fixed-voltage or estimate-only Bitmain PSUs (APW3, APW7, APW9, APW9+, APW12).
          Bypasses I2C PSU detection. <strong style={{ color: 'var(--accent)' }}>
          No Loki device needed.</strong>
        </div>

        {/* Enable toggle */}
        <div style={{
          display: 'flex', justifyContent: 'space-between', alignItems: 'center',
          marginBottom: 16,
        }}>
          <span style={{ fontSize: '0.85rem', color: 'var(--text)' }}>Enable Override</span>
          <button
            onClick={() => setEnabled(!enabled)}
            style={{
              width: 48, height: 24, borderRadius: 12, border: 'none',
              background: enabled ? 'var(--accent)' : 'var(--border)',
              cursor: 'pointer', position: 'relative', transition: 'background 0.2s',
            }}
          >
            <div style={{
              width: 18, height: 18, borderRadius: '50%', background: 'var(--text)',
              position: 'absolute', top: 3,
              left: enabled ? 27 : 3, transition: 'left 0.2s',
            }} />
          </button>
        </div>

        {enabled && (
          <>
            {/* PSU Model dropdown */}
            <div style={{ marginBottom: 16 }}>
              <label
                htmlFor="psu-override-model"
                style={{ fontSize: '0.75rem', color: 'var(--text-dim)', display: 'block', marginBottom: 4 }}
              >
                PSU Model
              </label>
              <select
                id="psu-override-model"
                value={model}
                onChange={(e) => setModel(e.target.value)}
                style={{
                  width: '100%', padding: '8px 12px', borderRadius: 8,
                  background: 'var(--bg)', color: 'var(--text)',
                  border: '1px solid var(--border)', fontSize: '0.85rem',
                }}
              >
                {availableModels.map(m => (
                  <option key={m.id} value={m.id}>{m.name} ({m.voltage_range})</option>
                ))}
              </select>
            </div>

            {/* Voltage input */}
            <div style={{ marginBottom: 16 }}>
              <label
                htmlFor="psu-override-fixed-voltage"
                style={{ fontSize: '0.75rem', color: 'var(--text-dim)', display: 'block', marginBottom: 4 }}
              >
                Fixed Output Voltage
              </label>
              <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
                <input
                  id="psu-override-fixed-voltage"
                  type="range"
                  min="10"
                  max="20"
                  step="0.1"
                  value={voltage}
                  onChange={(e) => setVoltage(parseFloat(e.target.value))}
                  aria-valuetext={`${voltage.toFixed(1)} V PSU rail`}
                  style={{ flex: 1, accentColor: 'var(--accent)' }}
                />
                <div style={{
                  fontFamily: "'JetBrains Mono', monospace",
                  fontWeight: 700, fontSize: '1.1rem', color: 'var(--accent)',
                  minWidth: 70, textAlign: 'right',
                }}>
                  {voltage.toFixed(1)} V
                </div>
              </div>
              <div style={{
                fontSize: '0.7rem', color: 'var(--text-dim)', marginTop: 4,
              }}>
                Set this to match your PSU's physical voltage output (potentiometer setting)
              </div>
            </div>
          </>
        )}

        {/* Save button */}
        <button
          onClick={handleSave}
          disabled={saving}
          style={{
            width: '100%', padding: '10px 16px', borderRadius: 8,
            background: saving ? 'var(--border)' : 'var(--accent)',
            color: saving ? 'var(--text-dim)' : '#000',
            border: 'none', fontWeight: 700, fontSize: '0.85rem',
            cursor: saving ? 'not-allowed' : 'pointer',
          }}
        >
          {saving ? 'Saving...' : enabled ? 'Enable PSU Override' : 'Disable PSU Override'}
        </button>

        {result && (
          <div style={{
            marginTop: 8, fontSize: '0.8rem', textAlign: 'center',
            color: result.includes('error') || result.includes('Failed') ? 'var(--red)' : 'var(--green)',
          }}>
            {result}
          </div>
        )}
      </div>
    </OverlayDialog>
  );
}

export function PsuControlModal({ onClose }: { onClose: () => void }) {
  const mode = useMinerStore(s => s.mode);
  const addToast = useMinerStore(s => s.addToast);
  const [diag, setDiag] = useState<PsuTroubleshootResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [acting, setActing] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const [targetVoltage, setTargetVoltage] = useState('15.0');

  const refresh = async () => {
    setLoading(true);
    try {
      const next = await api.troubleshootPsu();
      setDiag(next);
      if (typeof next.voltage_out === 'number') {
        setTargetVoltage(next.voltage_out.toFixed(2));
      }
    } catch {
      setDiag(null);
      addToast('Could not load PSU status', 'error');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const runAction = async (action: PsuControlRequest['action']) => {
    setActing(true);
    setResult(null);
    try {
      const payload: PsuControlRequest = { action, confirm: true };
      if (action === 'set_voltage') {
        const voltage = Number(targetVoltage);
        if (!Number.isFinite(voltage) || voltage <= 0) {
          addToast('Enter a valid PSU target voltage', 'warning');
          setActing(false);
          return;
        }
        payload.voltage_v = voltage;
      }

      const response = await api.controlPsu(payload);
      setResult(response.message);
      addToast(response.message, response.status === 'ok' ? 'success' : response.status === 'not_implemented' ? 'warning' : 'error');
      await refresh();
    } catch {
      const message = 'PSU control request failed';
      setResult(message);
      addToast(message, 'error');
    } finally {
      setActing(false);
    }
  };

  const voltageRange = parseVoltageRange(diag?.voltage_range);
  const hackerLocked = mode !== 'hacker';

  return (
    <OverlayDialog open onClose={onClose} ariaLabel="PSU control" maxWidth={540} width="92%">
      <div style={{ padding: 24 }}>
        <div style={{
          fontSize: '1.1rem', fontWeight: 700, color: 'var(--text)', marginBottom: 16,
          display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 12,
        }}>
          <span>PSU Control</span>
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <button className="btn btn-secondary" onClick={refresh} disabled={loading || acting}>Refresh</button>
            <button onClick={onClose} style={{
              background: 'none', border: 'none', color: 'var(--text-dim)',
              cursor: 'pointer', fontSize: '1.2rem',
            }}>&times;</button>
          </div>
        </div>

        {hackerLocked && (
          <div style={{
            background: 'rgba(247,147,26,0.1)', border: '1px solid rgba(247,147,26,0.3)',
            borderRadius: 8, padding: 12, marginBottom: 16, fontSize: '0.8rem',
            color: 'var(--text-dim)', lineHeight: 1.5,
          }}>
            Live PSU actions are restricted to Hacker mode. Switch the dashboard to Hacker mode before enabling output,
            feeding the watchdog, or changing APW voltage.
          </div>
        )}

        <div style={{
          display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(150px, 1fr))',
          gap: 10, marginBottom: 16,
        }}>
          <div style={{ background: 'var(--bg)', border: '1px solid var(--border)', borderRadius: 8, padding: 10 }}>
            <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Model</div>
            <div style={{ fontWeight: 700, color: 'var(--text)', marginTop: 4 }}>{diag?.model || '---'}</div>
          </div>
          <div style={{ background: 'var(--bg)', border: '1px solid var(--border)', borderRadius: 8, padding: 10 }}>
            <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Control Path</div>
            <div style={{ fontWeight: 700, color: 'var(--text)', marginTop: 4 }}>{diag?.control_mode || '---'}</div>
          </div>
          <div style={{ background: 'var(--bg)', border: '1px solid var(--border)', borderRadius: 8, padding: 10 }}>
            <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Reported Output</div>
            <div style={{ fontWeight: 700, color: 'var(--text)', marginTop: 4 }}>
              {diag?.output_enabled == null ? '---' : diag.output_enabled ? 'Enabled' : 'Disabled'}
            </div>
          </div>
          <div style={{ background: 'var(--bg)', border: '1px solid var(--border)', borderRadius: 8, padding: 10 }}>
            <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Measured Voltage</div>
            <div style={{ fontWeight: 700, color: 'var(--text)', marginTop: 4 }}>{formatOptionalVoltage(diag?.voltage_out)}</div>
          </div>
        </div>

        {diag?.output_gate_enabled != null && (
          <div style={{
            marginBottom: 16, background: 'var(--bg)', border: '1px solid var(--border)',
            borderRadius: 8, padding: 12, fontSize: '0.8rem', color: 'var(--text-secondary)',
          }}>
            Output gate: <strong style={{ color: 'var(--text)' }}>{diag.output_gate_enabled ? 'Enabled' : 'Disabled'}</strong>
          </div>
        )}

        <div style={{
          background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)',
          borderRadius: 8, padding: 12, marginBottom: 16,
        }}>
          <div style={{ fontSize: '0.8rem', color: 'var(--text-secondary)', lineHeight: 1.5 }}>
            {loading ? 'Loading PSU status...' : diag?.message || 'No PSU status available.'}
          </div>
        </div>

        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8, marginBottom: 16 }}>
          {diag?.supports_output_gate && (
            <>
              <button className="btn btn-primary" disabled={acting || hackerLocked} onClick={() => runAction('enable_output')}>
                Enable Output
              </button>
              <button className="btn btn-secondary" disabled={acting || hackerLocked} onClick={() => runAction('disable_output')}>
                Disable Output
              </button>
            </>
          )}
          {diag?.supports_watchdog && (
            <>
              <button className="btn btn-primary" disabled={acting || hackerLocked} onClick={() => runAction('enable_watchdog')}>
                Enable Watchdog
              </button>
              <button className="btn btn-secondary" disabled={acting || hackerLocked} onClick={() => runAction('feed_watchdog')}>
                Feed Watchdog
              </button>
              <button className="btn btn-secondary" disabled={acting || hackerLocked} onClick={() => runAction('disable_watchdog')}>
                Disable Watchdog
              </button>
            </>
          )}
        </div>

        {diag?.supports_voltage_set && (
          <div style={{
            background: 'var(--bg)', border: '1px solid var(--border)', borderRadius: 8,
            padding: 12, marginBottom: 8,
          }}>
            <div style={{ fontSize: '0.8rem', fontWeight: 700, color: 'var(--text)', marginBottom: 8 }}>
              Smart APW Voltage
            </div>
            <div style={{ display: 'flex', gap: 10, alignItems: 'end', flexWrap: 'wrap' }}>
              <div style={{ flex: '1 1 180px' }}>
                <label
                  htmlFor="psu-control-target-voltage"
                  style={{ fontSize: '0.72rem', color: 'var(--text-dim)', display: 'block', marginBottom: 4 }}
                >
                  Target Voltage
                </label>
                <input
                  id="psu-control-target-voltage"
                  type="number"
                  min={voltageRange?.min ?? 10}
                  max={voltageRange?.max ?? 21}
                  step="0.01"
                  value={targetVoltage}
                  onChange={(e) => setTargetVoltage(e.target.value)}
                />
              </div>
              <button className="btn btn-primary" disabled={acting || hackerLocked} onClick={() => runAction('set_voltage')}>
                Set Voltage
              </button>
            </div>
            <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)', marginTop: 8, lineHeight: 1.5 }}>
              Supported range: {diag.voltage_range || 'Unknown'}.
              Only change voltage when you understand the miner and PSU combination you are driving.
            </div>
          </div>
        )}

        {result && (
          <div style={{
            marginTop: 8, fontSize: '0.8rem', textAlign: 'center',
            color: result.toLowerCase().includes('failed') || result.toLowerCase().includes('error') ? 'var(--red)' : 'var(--green)',
          }}>
            {result}
          </div>
        )}
      </div>
    </OverlayDialog>
  );
}

function PsuStatusSummary({
  diag,
  loading,
  mode,
}: {
  diag: PsuTroubleshootResponse | null;
  loading: boolean;
  mode: string;
}) {
  return (
    <div style={{ marginTop: 16 }}>
      <div style={{
        fontSize: '0.72rem', fontWeight: 700, color: 'var(--text-dim)', textTransform: 'uppercase',
        letterSpacing: '0.08em', marginBottom: 10,
      }}>
        Live PSU Status
      </div>

      <div style={{
        background: 'var(--bg)', borderRadius: 12, border: '1px solid var(--border)',
        padding: 12,
      }}>
        <div style={{
          display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(135px, 1fr))',
          gap: 10, marginBottom: 12,
        }}>
          {[
            ['Control Path', diag?.control_mode || '---'],
            ['Transport', diag?.transport || '---'],
            ['Reported Output', diag?.output_enabled == null ? '---' : diag.output_enabled ? 'Enabled' : 'Disabled'],
            ['Output Gate', diag?.output_gate_enabled == null ? '---' : diag.output_gate_enabled ? 'Enabled' : 'Disabled'],
            ['Measured Voltage', formatOptionalVoltage(diag?.voltage_out)],
            ['Voltage Range', diag?.voltage_range || '---'],
          ].map(([label, value]) => (
            <div key={label} style={{
              background: 'var(--card-bg)', borderRadius: 10, border: '1px solid var(--border)',
              padding: 10,
            }}>
              <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 4 }}>{label}</div>
              <div style={{ fontSize: '0.82rem', fontWeight: 700, color: 'var(--text)' }}>{value}</div>
            </div>
          ))}
        </div>

        <div style={{
          display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(140px, 1fr))',
          gap: 10, marginBottom: 12,
        }}>
          {[
            ['Output Gate Control', formatCapabilityLabel(diag?.supports_output_gate)],
            ['Voltage Programming', formatCapabilityLabel(diag?.supports_voltage_set)],
            ['Watchdog Control', formatCapabilityLabel(diag?.supports_watchdog)],
          ].map(([label, value]) => (
            <div key={label} style={{
              padding: 10, borderRadius: 10, background: 'rgba(255,255,255,0.02)',
              border: '1px solid var(--border)',
            }}>
              <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 4 }}>{label}</div>
              <div style={{ fontSize: '0.82rem', fontWeight: 700, color: 'var(--text)' }}>{value}</div>
            </div>
          ))}
        </div>

        <div style={{
          fontSize: '0.78rem', color: 'var(--text-secondary)', lineHeight: 1.55,
          background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)',
          borderRadius: 10, padding: 10,
        }}>
          {loading ? 'Refreshing PSU status...' : diag?.message || 'No PSU status available.'}
        </div>

        <div style={{
          fontSize: '0.72rem', color: 'var(--text-dim)', marginTop: 10, lineHeight: 1.5,
        }}>
          {mode === 'hacker'
            ? 'For live PSU actions, use Hacker mode and open PSU Lab.'
            : 'This view is read-only in Standard mode. Switch to Hacker mode to access live PSU controls.'}
        </div>
      </div>
    </div>
  );
}

/** Hardware Information table — matches BraiinsOS style with PSU override gear. */
export function HardwareInfoPanel() {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const autotunerStatus = useMinerStore(s => s.autotunerStatus);
  const mode = useMinerStore(s => s.mode);
  const hw = systemInfo?.hardware;
  // P3-8: AxeOS/pyasic-compat fields the daemon reports as 0 are not real
  // telemetry — surface them honestly so a 0 is never read as a measurement.
  const unsupportedFields = unsupportedMetricList(systemInfo?.unsupported_metrics);
  const [showOverride, setShowOverride] = useState(false);
  const [showControl, setShowControl] = useState(false);
  const [psuOverride, setPsuOverride] = useState<PsuOverrideResponse | null>(null);
  const [psuStatus, setPsuStatus] = useState<PsuTroubleshootResponse | null>(null);
  const [psuStatusLoading, setPsuStatusLoading] = useState(true);

  // Fetch PSU override state on mount
  useEffect(() => {
    let cancelled = false;

    const loadOverride = () => {
      api.getPsuOverride()
        .then(data => {
          if (!cancelled) {
            setPsuOverride(data);
          }
        })
        .catch(() => {});
    };

    const loadPsuStatus = () => {
      setPsuStatusLoading(true);
      api.troubleshootPsu()
        .then(data => {
          if (!cancelled) {
            setPsuStatus(data);
          }
        })
        .catch(() => {
          if (!cancelled) {
            setPsuStatus(null);
          }
        })
        .finally(() => {
          if (!cancelled) {
            setPsuStatusLoading(false);
          }
        });
    };

    loadOverride();
    loadPsuStatus();

    const timer = window.setInterval(loadPsuStatus, 10000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  const rows: { label: string; value: string }[] = [
    {
      label: 'Miner Serial Number',
      value: hw?.miner_serial || '---',
    },
    {
      label: 'Control Board',
      value: hw?.control_board || systemInfo?.board || '---',
    },
    {
      label: 'HB Type',
      value: hw?.hb_type || '---',
    },
    {
      label: 'Chip Type',
      value: hw?.chip_type || systemInfo?.chip_type || '---',
    },
    {
      label: 'Effective Autotuner Preset',
      value:
        autotunerStatus?.policy?.effective_preset_display_name
        || formatAutotunerValue(autotunerStatus?.policy?.effective_preset ?? hw?.autotuner?.effective_preset),
    },
    {
      label: 'Autotuner Family',
      value:
        autotunerStatus?.policy?.capabilities?.family_key
        || hw?.autotuner?.capabilities?.family_key
        || autotunerStatus?.policy?.capabilities?.profile_key
        || hw?.autotuner?.capabilities?.profile_key
        || '---',
    },
    {
      label: 'Capability Profile',
      value:
        autotunerStatus?.policy?.capabilities?.profile_key
        || hw?.autotuner?.capabilities?.profile_key
        || '---',
    },
    {
      label: 'Preset Resolution',
      value:
        formatAutotunerValue(
          autotunerStatus?.policy?.requested_preset_reason
          ?? hw?.autotuner?.requested_preset_reason,
        ) || 'native',
    },
    {
      label: 'Voltage Control',
      value:
        hw?.capabilities?.voltage_control === 'pic16'
          ? 'PIC16'
          : hw?.capabilities?.voltage_control === 'dspic'
            ? 'dsPIC'
            : hw?.capabilities?.voltage_control === 'nopic'
              ? 'NoPic'
              : '---',
    },
    {
      label: 'Fan RPM Feedback',
      value:
        hw?.capabilities?.fan_rpm_feedback === true
          ? 'Measured'
          : hw?.capabilities?.fan_rpm_feedback === false
            ? 'Estimated'
            : '---',
    },
    {
      label: 'Quiet Home Presets',
      value: formatCapabilityLabel(
        autotunerStatus?.policy?.capabilities?.quiet_home_presets
        ?? hw?.autotuner?.capabilities?.quiet_home_presets,
      ),
    },
    {
      label: 'Runtime DVFS',
      value: formatCapabilityLabel(
        autotunerStatus?.policy?.capabilities?.dvfs_runtime_supported
        ?? hw?.autotuner?.capabilities?.dvfs_runtime_supported,
      ),
    },
    {
      label: 'PSU Model name',
      value: hw?.psu_model || '---',
    },
    {
      label: 'PSU FW Version',
      value: hw?.psu_fw_version || '---',
    },
    {
      label: 'PSU Serial Number',
      value: hw?.psu_serial || '---',
    },
    {
      label: 'PSU Voltage Range',
      value: hw?.psu_voltage_range || '---',
    },
  ];

  return (
    <>
      <div className="section">
        <div className="section-title section-title-inline">
          <span>
            Hardware Information
            {hw?.psu_override_active && (
              <span style={{
                marginLeft: 8, fontSize: '0.65rem', color: 'var(--accent)',
                background: 'rgba(247,147,26,0.15)', padding: '2px 8px',
                borderRadius: 4, fontWeight: 600,
              }}>
                PSU OVERRIDE
              </span>
            )}
          </span>
          <button
            className="btn btn-secondary hardware-override-btn"
            onClick={() => setShowOverride(true)}
          >
            <GearIcon size={14} />
            PSU Override
          </button>
          <button
            className="btn btn-secondary hardware-override-btn"
            onClick={() => setShowControl(true)}
            title={mode === 'hacker' ? 'Live PSU control' : 'Switch to Hacker mode for live PSU control'}
          >
            <LightningIcon size={14} />
            PSU Control
          </button>
        </div>
        <div style={{
          background: 'var(--card-bg)', borderRadius: 'var(--radius)',
          border: '1px solid var(--border)', overflow: 'hidden',
        }}>
          {rows.map((row, i) => (
            <div key={row.label} className="hardware-info-row" style={{
              borderTop: i > 0 ? '1px solid var(--border)' : undefined,
            }}>
              <span className="hardware-info-label">
                {row.label}
              </span>
              <span className="hardware-info-value" style={{
                color: row.value === '---' ? 'var(--text-dim)' : 'var(--text)',
              }}>
                {row.value}
              </span>
            </div>
          ))}
        </div>

        {unsupportedFields.length > 0 && (
          <div style={{
            marginTop: 10, fontSize: '0.72rem', color: 'var(--text-dim)', lineHeight: 1.5,
          }}>
            Compatibility-only fields reported as 0 (n/a, not real telemetry):{' '}
            {unsupportedFields.join(', ')}.
          </div>
        )}

        <PsuStatusSummary diag={psuStatus} loading={psuStatusLoading} mode={mode} />

        {/* Wave-55a HIGH-2: chip-rail mV actual-vs-target pill. Sits next
            to PSU surfaces so the operator can see at a glance whether
            the dsPIC is regulating to target (green) or stuck in fw=0x82
            bootloader echo (red caption). Self-gates on the .25-class
            endpoint — null on every other unit. */}
        <div style={{ marginTop: 8 }}>
          <ChipRailMvPill />
        </div>
      </div>

      {showOverride && (
        <PsuOverrideModal
          onClose={() => setShowOverride(false)}
          availableModels={psuOverride?.available_models ?? [
            { id: 'APW3', name: 'APW3 / APW3++', voltage_range: '11.60 - 13.00 V' },
            { id: 'APW7', name: 'APW7', voltage_range: '11.60 - 14.50 V' },
            { id: 'APW9', name: 'APW9 / APW9+', voltage_range: '14.10 - 21.00 V' },
            { id: 'APW12', name: 'APW12 / APW121215', voltage_range: '11.96 - 15.20 V' },
            { id: 'custom', name: 'Custom / Other', voltage_range: '10.00 - 20.00 V' },
          ]}
          currentModel={psuOverride?.model ?? ''}
          currentActive={psuOverride?.active ?? false}
          currentVoltage={psuOverride?.voltage_v ?? 12.0}
        />
      )}

      {showControl && <PsuControlModal onClose={() => setShowControl(false)} />}
    </>
  );
}
