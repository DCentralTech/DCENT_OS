import React, { useEffect, useRef, useState } from 'react';
import { api } from '../../api/client';
import type { SolarStatus, SolarVerificationSample } from '../../api/feature-types';
import type {
  OffGridAdcConfig,
  OffGridConfigPayload,
  OffGridProbeResponse,
  OffGridConfigResponse,
  OffGridPreset,
  OffGridStatusResponse,
} from '../../api/types';
import { useMinerStore } from '../../store/miner';
import { BatteryGauge } from './BatteryGauge';
import { VoltageZoneBar } from './VoltageZoneBar';
import { SectionSkeleton } from '../common/skeletons';
import { EmptyState } from '../common/EmptyState';
import { NoLogsIllustration } from '../common/illustrations';
import { InfoDot } from '../common/Tooltip';
import { wattsToBtu } from '../../utils/thermal';

const DEFAULT_CONFIG: OffGridConfigPayload = {
  source_profile: 'direct_dc',
  enabled: false,
  battery_preset: 'lifepo4_48v',
  adc: null,
  freq_step_mhz: 25,
  min_frequency_mhz: 200,
  loop_interval_ms: 2000,
  custom_critical_v: null,
  custom_low_v: null,
  custom_high_v: null,
  custom_full_v: null,
  custom_recovery_v: null,
};

const STATUS_STALE_AFTER_MS = 12_000;

function defaultAdcConfig(type: OffGridAdcConfig['type']): OffGridAdcConfig {
  switch (type) {
    case 'ina226':
      return { type: 'ina226', i2c_bus: 0, i2c_addr: 0x40, shunt_mohm: 10, voltage_divider: 1 };
    case 'sysfs':
      return {
        type: 'sysfs',
        voltage_path: '/sys/bus/iio/devices/iio:device0/in_voltage0_raw',
        vref: 1.8,
        bits: 12,
        voltage_divider: 1,
      };
    case 'simulated':
      return { type: 'simulated', voltage_v: 52, current_a: 0 };
  }
}

function formatUptime(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return h > 0 ? `${h}h ${m}m` : `${m}m`;
}

function formatAge(ms: number | null | undefined): string {
  if (ms == null) return 'n/a';
  if (ms < 1000) return `${ms} ms`;
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remSeconds = seconds % 60;
  return remSeconds > 0 ? `${minutes}m ${remSeconds}s` : `${minutes}m`;
}

function formatTimestamp(timestampMs: number | null | undefined): string {
  if (!timestampMs) return 'n/a';
  return new Date(timestampMs).toLocaleString();
}

function MetricCard({ label, value, unit, color, hint }: {
  label: string; value: string; unit?: string; color?: string; hint?: string;
}) {
  return (
    <div style={{
      background: 'var(--card-bg)', borderRadius: 'var(--radius)',
      border: '1px solid var(--border)', padding: '12px 16px', textAlign: 'center',
    }}>
      <div style={{ fontSize: '0.7rem', color: 'var(--text-dim)', marginBottom: 4 }}>{label}</div>
      <div style={{
        fontFamily: "'JetBrains Mono', monospace", fontWeight: 700,
        fontSize: '1.1rem', color: color || 'var(--text)',
      }}>
        {value}{unit && <span style={{ fontSize: '0.7rem', fontWeight: 400 }}> {unit}</span>}
      </div>
      {hint && <div style={{ fontSize: '0.6rem', color: 'var(--text-dim)', marginTop: 4 }}>{hint}</div>}
    </div>
  );
}

function InsightCard({ title, children, color }: {
  title: string; children: React.ReactNode; color?: string;
}) {
  return (
    <div style={{
      // : was `${color}10` / `${color}30` — appending hex-alpha to a
      // var() (the default `var(--accent)`) is invalid CSS and silently
      // dropped the background + border. color-mix tints correctly for both
      // var() and hex inputs.
      background: `color-mix(in srgb, ${color || 'var(--accent)'} 8%, transparent)`,
      border: `1px solid color-mix(in srgb, ${color || 'var(--accent)'} 22%, transparent)`,
      borderRadius: 'var(--radius)', padding: '12px 16px', marginBottom: 12,
    }}>
      <div style={{ fontSize: '0.75rem', fontWeight: 700, color: color || 'var(--accent)', marginBottom: 4 }}>
        {title}
      </div>
      <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
        {children}
      </div>
    </div>
  );
}

function SolarBatteryProviderSummary({
  solarStatus,
  verificationHistory,
}: {
  solarStatus: SolarStatus | null;
  verificationHistory: SolarVerificationSample[];
}) {
  const passRate = verificationHistory.length > 0
    ? Math.round((verificationHistory.filter(entry => entry.connected && !entry.stale).length / verificationHistory.length) * 100)
    : 0;
  const lastHealthy = [...verificationHistory].reverse().find(entry => entry.connected && !entry.stale);
  const stage = solarStatus?.providerStage ?? 'unknown';
  const telemetryBacked = solarStatus?.providerTelemetryBacked ?? (solarStatus?.provider !== 'manual');
  const runtimeAdopted = solarStatus?.runtimeAdopted ?? false;
  const connectionValue = !runtimeAdopted
    ? 'Pending restart'
    : telemetryBacked
      ? (solarStatus?.connected ? 'Connected' : 'Offline')
      : 'Manual input';
  const verdict = !runtimeAdopted
    ? 'Saved config only'
    : !solarStatus?.providerLiveBackend
    ? 'Backend unavailable'
    : !telemetryBacked
      ? 'Manual runtime only'
      : solarStatus.connected && !solarStatus.stale && (solarStatus.consecutiveFailures ?? 0) === 0
      ? 'Healthy for field validation'
      : solarStatus?.connected
        ? 'Connected but needs observation'
        : 'Needs provider attention';

  return (
    <div style={{
      background: 'var(--card-bg)', borderRadius: 'var(--radius)', border: '1px solid var(--border)',
      padding: 16, marginBottom: 16,
    }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, flexWrap: 'wrap', marginBottom: 12 }}>
        <div>
          <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)', fontWeight: 700, letterSpacing: '0.04em', textTransform: 'uppercase' }}>
            Solar Provider Status
          </div>
          <div style={{ fontSize: '0.95rem', color: 'var(--text)', fontWeight: 700, marginTop: 4 }}>
            Keep this healthy before trusting solar+battery automation
          </div>
        </div>
        <div style={{ fontSize: '0.74rem', color: 'var(--text-dim)', maxWidth: 340 }}>
          Off-Grid owns hard battery/DC protection. The Solar page owns provider-backed policy. Both must look healthy for a real solar+battery deployment.
        </div>
      </div>

      <div style={{ display: 'grid', gap: 10, gridTemplateColumns: 'repeat(auto-fit, minmax(160px, 1fr))', marginBottom: 12 }}>
        <MetricCard label="Provider" value={solarStatus?.provider || '---'} color="var(--accent)" hint={solarStatus?.transport || 'no transport'} />
        <MetricCard label="Stage" value={stage} color={stage === 'live' ? 'var(--green)' : stage === 'limited' ? 'var(--yellow)' : 'var(--text-dim)'} hint={solarStatus?.providerStageReason || 'backend maturity'} />
        <MetricCard label="Connection" value={connectionValue} color={!runtimeAdopted ? 'var(--yellow)' : telemetryBacked ? (solarStatus?.connected ? 'var(--green)' : 'var(--yellow)') : 'var(--accent)'} hint={verdict} />
        <MetricCard label="Sample Age" value={formatAge(solarStatus?.sampleAgeMs)} color={solarStatus?.stale ? 'var(--yellow)' : 'var(--text)'} hint={solarStatus?.stale ? 'stale' : 'fresh'} />
        <MetricCard label="Failures" value={String(solarStatus?.consecutiveFailures ?? 0)} color={(solarStatus?.consecutiveFailures ?? 0) > 0 ? 'var(--yellow)' : 'var(--green)'} hint={`Pass rate ${passRate}%`} />
        <MetricCard label="Last Healthy" value={lastHealthy ? formatTimestamp(lastHealthy.timestampMs) : 'n/a'} hint="Verification history" />
      </div>

      <div style={{ display: 'grid', gap: 10, gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))', marginBottom: 12 }}>
        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Live policy state</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            {(solarStatus?.action || 'observe').toUpperCase()}
            {solarStatus?.targetFreqMhz ? ` -> ${solarStatus.targetFreqMhz} MHz` : ''}
            {solarStatus?.sleeping ? ' | sleeping' : ''}
            {solarStatus?.batteryFloorActive ? ' | battery floor active' : ''}
          </div>
        </div>
        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Matched fields</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            {solarStatus?.matchedFields?.length || solarStatus?.matched_fields?.length
              ? (solarStatus.matchedFields ?? solarStatus.matched_fields ?? []).join(', ')
              : 'No matched fields yet'}
          </div>
        </div>
      </div>

      <div style={{
        fontSize: '0.78rem', color: 'var(--text-secondary)', lineHeight: 1.55,
        background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: 10,
      }}>
        {solarStatus?.message || 'No provider status yet.'}
      </div>

      <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)', marginTop: 10, lineHeight: 1.5 }}>
        Configure, test, and inspect detailed provider history in Green Mining. Use this card to verify that the solar side is healthy enough to pair with battery protection on this page.
      </div>
    </div>
  );
}

export function getThresholdWarnings(config: OffGridConfigPayload, preset: OffGridPreset | undefined): string[] {
  const critical = config.custom_critical_v ?? preset?.critical_v;
  const low = config.custom_low_v ?? preset?.low_v;
  const high = config.custom_high_v ?? preset?.high_v;
  const full = config.custom_full_v ?? preset?.full_v;
  const recovery = config.custom_recovery_v ?? preset?.recovery_v;
  const warnings: string[] = [];

  if (critical == null || low == null || high == null || full == null || recovery == null) {
    warnings.push('Thresholds are incomplete. Pick a preset or fill all override values before field use.');
    return warnings;
  }

  if (!(critical < low && low < high && high < full)) {
    warnings.push('Threshold order should rise cleanly: critical < low < high < full.');
  }
  if (recovery <= critical) {
    warnings.push('Recovery voltage must stay above critical to prevent permanent sleep.');
  }
  if (config.adc?.type === 'simulated') {
    warnings.push('Simulated ADC is lab-only. Do not leave it enabled on a live battery-backed install.');
  }

  return warnings;
}

function OffGridCommissioningSummary({
  config,
  preset,
  status,
}: {
  config: OffGridConfigPayload;
  preset: OffGridPreset | undefined;
  status: OffGridStatusResponse | null;
}) {
  const warnings = getThresholdWarnings(config, preset);

  return (
    <div style={{
      background: 'var(--card-bg)',
      borderRadius: 'var(--radius)',
      border: '1px solid var(--border)',
      padding: 16,
      marginBottom: 16,
    }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, flexWrap: 'wrap', marginBottom: 12 }}>
        <div>
          <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)', fontWeight: 700, letterSpacing: '0.04em', textTransform: 'uppercase' }}>
            Off-Grid Commissioning
          </div>
          <div style={{ fontSize: '0.95rem', color: 'var(--text)', fontWeight: 700, marginTop: 4 }}>
            Sensor, threshold, and fail-safe summary
          </div>
        </div>
        <div style={{ fontSize: '0.74rem', color: 'var(--text-dim)', maxWidth: 340 }}>
          Verify these values before unattended direct-DC or solar-battery operation. This controller is the hard battery/DC guardrail, so the goal is safe curtailment and fail-closed sleep, not optimistic estimates.
        </div>
      </div>

      <div style={{ display: 'grid', gap: 10, gridTemplateColumns: 'repeat(auto-fit, minmax(170px, 1fr))', marginBottom: 12 }}>
        <MetricCard label="Source profile" value={config.source_profile === 'solar_battery' ? 'Solar + Battery' : 'Direct DC'} color="var(--green)" />
        <MetricCard label="ADC backend" value={config.adc?.type ?? 'unset'} color={config.adc ? 'var(--accent)' : 'var(--yellow)'} hint={status?.sensor_source || 'Pick a real sensor path'} />
        <MetricCard label="Loop interval" value={String(config.loop_interval_ms)} unit="ms" hint="Control cadence" />
        <MetricCard label="Min frequency" value={String(config.min_frequency_mhz)} unit="MHz" hint="Sleep floor guard" />
        <MetricCard label="Frequency step" value={String(config.freq_step_mhz)} unit="MHz" hint="Ramp aggressiveness" />
      </div>

      <div style={{ display: 'grid', gap: 10, gridTemplateColumns: 'repeat(auto-fit, minmax(220px, 1fr))' }}>
        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Threshold plan</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            Critical {(config.custom_critical_v ?? preset?.critical_v ?? 0).toFixed(1)} V, low {(config.custom_low_v ?? preset?.low_v ?? 0).toFixed(1)} V, high {(config.custom_high_v ?? preset?.high_v ?? 0).toFixed(1)} V, full {(config.custom_full_v ?? preset?.full_v ?? 0).toFixed(1)} V, recovery {(config.custom_recovery_v ?? preset?.recovery_v ?? 0).toFixed(1)} V.
          </div>
        </div>

        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Live controller state</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            {status
              ? `${status.zone} zone, ${status.state}. ${status.message || 'No extra status message.'}`
              : 'Status not loaded yet.'}
          </div>
        </div>

        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Fail-safe expectation</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            If the ADC path is wrong, stale, or unhealthy, this workflow should stop trusting the source and move toward a safe low-power or sleep state. Solar providers do not replace this voltage guard.
          </div>
        </div>
      </div>

      {warnings.length > 0 && (
        <div style={{
          marginTop: 12,
          padding: '10px 12px',
          borderRadius: 10,
          background: 'rgba(234,179,8,0.08)',
          border: '1px solid rgba(234,179,8,0.24)',
          fontSize: '0.76rem',
          color: 'var(--text-dim)',
          lineHeight: 1.55,
        }}>
          <div style={{ color: 'var(--yellow)', fontWeight: 700, marginBottom: 6 }}>Commissioning warnings</div>
          {warnings.map(warning => (
            <div key={warning} style={{ marginTop: 4 }}>{warning}</div>
          ))}
        </div>
      )}

      <div style={{
        marginTop: 12,
        padding: '10px 12px',
        borderRadius: 10,
        background: config.source_profile === 'solar_battery' ? 'rgba(59,130,246,0.08)' : 'rgba(34,197,94,0.06)',
        border: `1px solid ${config.source_profile === 'solar_battery' ? 'rgba(59,130,246,0.18)' : 'rgba(34,197,94,0.16)'}`,
        fontSize: '0.76rem',
        color: 'var(--text-dim)',
        lineHeight: 1.55,
      }}>
        <div style={{ color: 'var(--text)', fontWeight: 700, marginBottom: 6 }}>Trust split</div>
        {config.source_profile === 'solar_battery'
          ? 'Off-Grid is authoritative for bus voltage, battery-floor sleep, and direct-DC fail-safe behavior. Green Mining is authoritative only for provider-fed solar policy once that provider has proven fresh and trustworthy.'
          : 'In Direct DC mode, this page is the primary authority. Protection quality depends almost entirely on the ADC path and threshold plan you commission here.'}
      </div>
    </div>
  );
}

function PowerSourceGuide() {
  const sources = [
    {
      icon: '[AC]', title: 'Grid AC + Smart PSU',
      desc: 'Standard Bitmain behavior with PSU control/telemetry when supported.',
      who: 'Most miners on standard APW power',
      config: 'Wizard: Grid AC',
    },
    {
      icon: '[FIXED]', title: 'Grid AC + Fixed PSU',
      desc: 'APW3/APW7 or other fixed-voltage supply. Use PSU Override after boot.',
      who: 'DIY and budget setups',
      config: 'Hardware Info > PSU Override',
    },
    {
      icon: '[DC]', title: 'Direct DC',
      desc: 'Battery, bench supply, car battery, or generator feeding the miner directly. Firmware uses voltage-aware curtailment instead of AC circuit assumptions.',
      who: 'Off-grid experiments and DC-native installs',
      config: 'Commission below on this page',
    },
    {
      icon: '[SOLAR]', title: 'Solar + Battery',
      desc: 'Battery-backed solar where DCENT_OS ramps mining up and down against bus voltage to protect the bank.',
      who: 'Cabins, sovereignty, remote power',
      config: 'Commission below on this page',
    },
  ];

  return (
    <div>
      <div style={{ fontSize: '0.85rem', color: 'var(--text-dim)', marginBottom: 16, lineHeight: 1.6 }}>
        DCENT_OS supports multiple real-world power paths. Direct DC and solar-battery installs now commission from this page instead of requiring hand-edited TOML.
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(260px, 1fr))', gap: 12 }}>
        {sources.map(source => (
          <div key={source.title} style={{
            background: 'var(--card-bg)', borderRadius: 'var(--radius)',
            border: '1px solid var(--border)', padding: 16,
          }}>
            <div style={{ fontSize: '1rem', marginBottom: 6 }}>{source.icon}</div>
            <div style={{ fontWeight: 700, fontSize: '0.85rem', color: 'var(--text)', marginBottom: 6 }}>{source.title}</div>
            <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.5, marginBottom: 8 }}>{source.desc}</div>
            <div style={{ fontSize: '0.7rem', color: 'var(--text-dim)' }}><span style={{ fontWeight: 600 }}>Best for:</span> {source.who}</div>
            <div style={{ fontSize: '0.68rem', color: 'var(--accent)', fontFamily: "'JetBrains Mono', monospace", marginTop: 4 }}>{source.config}</div>
          </div>
        ))}
      </div>
    </div>
  );
}

function OffGridConfigCard({
  config,
  presets,
  onChange,
  onTest,
  onSave,
  testing,
  saving,
  testResult,
  response,
}: {
  config: OffGridConfigPayload;
  presets: OffGridPreset[];
  onChange: (next: OffGridConfigPayload) => void;
  onTest: () => void;
  onSave: () => void;
  testing: boolean;
  saving: boolean;
  testResult: OffGridProbeResponse | null;
  response: OffGridConfigResponse | null;
}) {
  const selectedPreset = presets.find(preset => preset.id === config.battery_preset);
  const adcType = config.adc?.type ?? 'none';

  const updateConfig = (patch: Partial<OffGridConfigPayload>) => {
    onChange({ ...config, ...patch });
  };

  const updateThreshold = (key: keyof OffGridConfigPayload, value: string) => {
    updateConfig({ [key]: value.trim() === '' ? null : Number(value) } as Partial<OffGridConfigPayload>);
  };

  const saveHintId = 'off-grid-save-hint';
  const testHintId = 'off-grid-test-hint';
  const isSolarBattery = config.source_profile === 'solar_battery';
  const controlsDisabled = testing || saving;
  const simulatedAdcSaveBlocked = config.adc?.type === 'simulated';
  // D7-4: block Save while the config is showing its OWN dangerous/incomplete
  // VOLTAGE-threshold warnings (e.g. recovery <= critical -> "permanent sleep",
  // or a non-rising critical<low<high<full order). The daemon rejects these too,
  // but the operator must not be able to hit Save on a set the UI itself flagged.
  // The simulated-ADC warning is excluded here — it has its own dedicated block
  // (`simulatedAdcSaveBlocked`) and must not double-render.
  const thresholdWarnings = getThresholdWarnings(config, selectedPreset).filter(
    (w) => !w.startsWith('Simulated ADC'),
  );
  const thresholdSaveBlocked = thresholdWarnings.length > 0;
  const probeTone = !testResult
    ? null
    : !testResult.ok
      ? {
        border: 'rgba(239,68,68,0.25)',
        background: 'rgba(239,68,68,0.08)',
        color: 'var(--red)',
        title: 'Probe failed',
      }
      : testResult.plausible
        ? {
          border: 'rgba(34,197,94,0.25)',
          background: 'rgba(34,197,94,0.08)',
          color: 'var(--green)',
          title: 'Probe looks usable',
        }
        : {
          border: 'rgba(234,179,8,0.25)',
          background: 'rgba(234,179,8,0.08)',
          color: 'var(--yellow)',
          title: 'Probe needs review',
        };

  return (
    <div style={{
      background: 'var(--card-bg)', borderRadius: 'var(--radius)', border: '1px solid var(--border)',
      padding: 16, marginBottom: 16,
    }}>
      <div className="section-title" style={{ color: 'var(--green)', marginBottom: 12 }}>
        {isSolarBattery ? 'Solar + Battery Fail-Safe' : 'Direct DC Commissioning'}
      </div>

      <div style={{
        marginBottom: 14,
        padding: '10px 12px',
        borderRadius: 10,
        background: isSolarBattery ? 'rgba(59,130,246,0.08)' : 'rgba(34,197,94,0.06)',
        border: `1px solid ${isSolarBattery ? 'rgba(59,130,246,0.18)' : 'rgba(34,197,94,0.16)'}`,
        fontSize: '0.76rem',
        color: 'var(--text-dim)',
        lineHeight: 1.55,
      }}>
        <div style={{ color: 'var(--text)', fontWeight: 700, marginBottom: 6 }}>
          {isSolarBattery ? 'What this page controls' : 'What this page must prove'}
        </div>
        {isSolarBattery
          ? 'This page is the hard safety side of a solar+battery deployment: voltage sensing, threshold ordering, ramp-down, and sleep/recovery. Use the Solar page for provider policy, but do not duplicate the battery fail-safe there.'
          : 'This page should prove the miner can trust the DC bus measurement it will use to curtail or sleep. If the sensor path is wrong, the fail-safe is wrong.'}
      </div>

      <fieldset disabled={controlsDisabled} style={{ border: 0, padding: 0, margin: 0, minInlineSize: 0 }}>
        <div style={{
        display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(220px, 1fr))', gap: 12,
        marginBottom: 14,
        }}>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
            <span style={{ fontSize: '0.75rem', color: 'var(--text-dim)' }}>Deployment Style</span>
            <select value={config.source_profile} onChange={e => updateConfig({ source_profile: e.target.value as OffGridConfigPayload['source_profile'] })}>
            <option value="direct_dc">Direct DC</option>
            <option value="solar_battery">Solar + Battery</option>
          </select>
        </label>

        <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
          <span style={{ fontSize: '0.75rem', color: 'var(--text-dim)' }}>Battery Preset</span>
          <select value={config.battery_preset} onChange={e => updateConfig({ battery_preset: e.target.value })}>
            {presets.map(preset => (
              <option key={preset.id} value={preset.id}>{preset.label}</option>
            ))}
            <option value="custom">Custom</option>
          </select>
        </label>

        <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
          <span style={{ fontSize: '0.75rem', color: 'var(--text-dim)' }}>Controller Enabled</span>
          <select value={config.enabled ? 'enabled' : 'disabled'} onChange={e => updateConfig({ enabled: e.target.value === 'enabled' })}>
            <option value="disabled">Disabled</option>
            <option value="enabled">Enabled</option>
          </select>
        </label>

        <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
          <span style={{ fontSize: '0.75rem', color: 'var(--text-dim)' }}>ADC Backend</span>
          <select value={adcType} onChange={e => updateConfig({ adc: e.target.value === 'none' ? null : defaultAdcConfig(e.target.value as OffGridAdcConfig['type']) })}>
            <option value="none">Select backend</option>
            <option value="ina226">INA226</option>
            <option value="sysfs">Sysfs ADC</option>
            <option value="simulated">Simulated (lab only)</option>
          </select>
        </label>
        </div>

        {config.adc?.type === 'ina226' && (
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(160px, 1fr))', gap: 10, marginBottom: 14 }}>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>I2C Bus</span><input type="number" value={config.adc.i2c_bus} onChange={e => updateConfig({ adc: { ...config.adc!, i2c_bus: Number(e.target.value) } as OffGridAdcConfig })} /></label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>I2C Address</span><input type="number" value={config.adc.i2c_addr} onChange={e => updateConfig({ adc: { ...config.adc!, i2c_addr: Number(e.target.value) } as OffGridAdcConfig })} /></label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Shunt (mOhm)</span><input type="number" value={config.adc.shunt_mohm} onChange={e => updateConfig({ adc: { ...config.adc!, shunt_mohm: Number(e.target.value) } as OffGridAdcConfig })} /></label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Voltage Divider</span><input type="number" step="0.1" value={config.adc.voltage_divider} onChange={e => updateConfig({ adc: { ...config.adc!, voltage_divider: Number(e.target.value) } as OffGridAdcConfig })} /></label>
        </div>
        )}

        {config.adc?.type === 'sysfs' && (
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))', gap: 10, marginBottom: 14 }}>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6, gridColumn: '1 / -1' }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Voltage Path</span><input value={config.adc.voltage_path} onChange={e => updateConfig({ adc: { ...config.adc!, voltage_path: e.target.value } as OffGridAdcConfig })} /></label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Vref</span><input type="number" step="0.1" value={config.adc.vref} onChange={e => updateConfig({ adc: { ...config.adc!, vref: Number(e.target.value) } as OffGridAdcConfig })} /></label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Bits</span><input type="number" value={config.adc.bits} onChange={e => updateConfig({ adc: { ...config.adc!, bits: Number(e.target.value) } as OffGridAdcConfig })} /></label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Voltage Divider</span><input type="number" step="0.1" value={config.adc.voltage_divider} onChange={e => updateConfig({ adc: { ...config.adc!, voltage_divider: Number(e.target.value) } as OffGridAdcConfig })} /></label>
        </div>
        )}

        {config.adc?.type === 'simulated' && (
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(160px, 1fr))', gap: 10, marginBottom: 14 }}>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Simulated Voltage</span><input type="number" step="0.1" value={config.adc.voltage_v} onChange={e => updateConfig({ adc: { ...config.adc!, voltage_v: Number(e.target.value) } as OffGridAdcConfig })} /></label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Simulated Current</span><input type="number" step="0.1" value={config.adc.current_a} onChange={e => updateConfig({ adc: { ...config.adc!, current_a: Number(e.target.value) } as OffGridAdcConfig })} /></label>
        </div>
        )}

        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(160px, 1fr))', gap: 10, marginBottom: 14 }}>
        <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Frequency Step</span><input type="number" value={config.freq_step_mhz} onChange={e => updateConfig({ freq_step_mhz: Number(e.target.value) })} /></label>
        <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Minimum Frequency</span><input type="number" value={config.min_frequency_mhz} onChange={e => updateConfig({ min_frequency_mhz: Number(e.target.value) })} /></label>
        <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Loop Interval (ms)</span><input type="number" step="100" value={config.loop_interval_ms} onChange={e => updateConfig({ loop_interval_ms: Number(e.target.value) })} /></label>
        </div>

        <div style={{ marginBottom: 12 }}>
        <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginBottom: 8 }}>Optional Threshold Overrides</div>
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(130px, 1fr))', gap: 10 }}>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Critical</span><input type="number" step="0.1" placeholder={selectedPreset ? `${selectedPreset.critical_v}` : ''} value={config.custom_critical_v ?? ''} onChange={e => updateThreshold('custom_critical_v', e.target.value)} /></label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Low</span><input type="number" step="0.1" placeholder={selectedPreset ? `${selectedPreset.low_v}` : ''} value={config.custom_low_v ?? ''} onChange={e => updateThreshold('custom_low_v', e.target.value)} /></label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>High</span><input type="number" step="0.1" placeholder={selectedPreset ? `${selectedPreset.high_v}` : ''} value={config.custom_high_v ?? ''} onChange={e => updateThreshold('custom_high_v', e.target.value)} /></label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Full</span><input type="number" step="0.1" placeholder={selectedPreset ? `${selectedPreset.full_v}` : ''} value={config.custom_full_v ?? ''} onChange={e => updateThreshold('custom_full_v', e.target.value)} /></label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 6 }}><span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>Recovery</span><input type="number" step="0.1" placeholder={selectedPreset ? `${selectedPreset.recovery_v}` : ''} value={config.custom_recovery_v ?? ''} onChange={e => updateThreshold('custom_recovery_v', e.target.value)} /></label>
        </div>
        </div>
      </fieldset>

      {response && (
        <div style={{
          background: response.ready ? 'rgba(34,197,94,0.08)' : 'rgba(234,179,8,0.08)',
          border: `1px solid ${response.ready ? 'rgba(34,197,94,0.25)' : 'rgba(234,179,8,0.25)'}`,
          borderRadius: 10,
          padding: 10,
          fontSize: '0.78rem',
          color: 'var(--text-secondary)',
          lineHeight: 1.5,
          marginBottom: 12,
        }}>
          {response.readiness_message}
        </div>
      )}

      {testResult && probeTone && (
        <div style={{
          background: probeTone.background,
          border: `1px solid ${probeTone.border}`,
          borderRadius: 10,
          padding: 12,
          marginBottom: 12,
        }}>
          <div style={{ fontSize: '0.76rem', fontWeight: 700, color: probeTone.color, marginBottom: 6 }}>
            {probeTone.title}
          </div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary)', lineHeight: 1.5 }}>
            {testResult.message}
          </div>
          <div style={{
            display: 'grid',
            gridTemplateColumns: 'repeat(auto-fit, minmax(120px, 1fr))',
            gap: 8,
            marginTop: 10,
            fontSize: '0.74rem',
            color: 'var(--text-dim)',
          }}>
            <div><strong style={{ color: 'var(--text)' }}>Backend:</strong> {testResult.backend}</div>
            <div><strong style={{ color: 'var(--text)' }}>Source:</strong> {testResult.sensorSource}</div>
            <div><strong style={{ color: 'var(--text)' }}>Voltage:</strong> {testResult.voltageV != null ? `${testResult.voltageV.toFixed(2)} V` : 'n/a'}</div>
            <div><strong style={{ color: 'var(--text)' }}>Current:</strong> {testResult.hasCurrent && testResult.currentA != null ? `${testResult.currentA.toFixed(2)} A` : 'voltage only'}</div>
            <div><strong style={{ color: 'var(--text)' }}>Power:</strong> {testResult.powerW != null ? `${Math.round(testResult.powerW)} W` : 'n/a'}</div>
            <div><strong style={{ color: 'var(--text)' }}>Verdict:</strong> {testResult.ok ? (testResult.plausible ? 'plausible live reading' : 'connected but implausible') : 'probe failed'}</div>
          </div>
        </div>
      )}

      {simulatedAdcSaveBlocked && (
        <div
          role="alert"
          style={{
            background: 'rgba(234,179,8,0.08)',
            border: '1px solid rgba(234,179,8,0.25)',
            borderRadius: 10,
            padding: 10,
            fontSize: '0.78rem',
            color: 'var(--text-secondary)',
            lineHeight: 1.5,
            marginBottom: 12,
          }}
        >
          Simulated ADC is lab-only and cannot be saved as off-grid protection config. Use Test ADC Path for lab probes, then select INA226 or Sysfs ADC before saving commissioning config.
        </div>
      )}

      {thresholdSaveBlocked && (
        <div
          role="alert"
          style={{
            background: 'rgba(234,179,8,0.08)',
            border: '1px solid rgba(234,179,8,0.25)',
            borderRadius: 10,
            padding: 10,
            fontSize: '0.78rem',
            color: 'var(--text-secondary)',
            lineHeight: 1.5,
            marginBottom: 12,
          }}
        >
          Battery thresholds are unsafe or incomplete and cannot be saved as off-grid protection config: {thresholdWarnings.join(' ')}
        </div>
      )}

      <div style={{ display: 'flex', gap: 10, alignItems: 'center', flexWrap: 'wrap' }}>
        <button type="button" className="btn btn-secondary" onClick={onTest} disabled={testing || saving || !config.adc} aria-describedby={testHintId}>{testing ? 'Testing...' : 'Test ADC Path'}</button>
        <button type="button" className="btn btn-primary" onClick={onSave} disabled={saving || testing || simulatedAdcSaveBlocked || thresholdSaveBlocked} aria-describedby={saveHintId}>{saving ? 'Saving...' : 'Save Off-Grid Config'}</button>
        <div id={testHintId} style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>
          Probes the configured sensor path immediately without restarting `dcentrald`.
        </div>
        <div id={saveHintId} style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>
          {thresholdSaveBlocked
            ? 'Fix the flagged battery thresholds before saving — an inverted or incomplete set could sleep the miner permanently.'
            : simulatedAdcSaveBlocked
            ? 'Simulated ADC can be probed from this page, but saved protection config must use INA226 or Sysfs ADC.'
            : 'Saving updates `/data/dcentrald.toml`. Restart `dcentrald` after commissioning changes.'}
        </div>
      </div>
    </div>
  );
}

export function OffGridPage() {
  const addToast = useMinerStore(s => s.addToast);
  const [status, setStatus] = useState<OffGridStatusResponse | null>(null);
  const [statusPollFailures, setStatusPollFailures] = useState(0);
  const [lastStatusRefreshMs, setLastStatusRefreshMs] = useState<number | null>(null);
  const [presets, setPresets] = useState<OffGridPreset[]>([]);
  const [config, setConfig] = useState<OffGridConfigPayload>(DEFAULT_CONFIG);
  const [configResponse, setConfigResponse] = useState<OffGridConfigResponse | null>(null);
  const [testResult, setTestResult] = useState<OffGridProbeResponse | null>(null);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const probeRequestIdRef = useRef(0);
  const configRevisionRef = useRef(0);

  const refreshStatus = async () => {
    try {
      const nextStatus = await api.getOffGridStatus();
      setStatus(nextStatus);
      setLastStatusRefreshMs(Date.now());
      setStatusPollFailures(0);
    } catch {
      // Background poll failure — the page already surfaces this inline via
      // statusPollFailures ("Off-grid telemetry unavailable" empty state).
      // Don't also fire a toast; that's redundant noise for a passive poll.
      setStatusPollFailures(prev => prev + 1);
    }
  };

  useEffect(() => {
    let cancelled = false;

    const load = async () => {
      try {
        const [nextStatus, nextPresets, nextConfig] = await Promise.all([
          api.getOffGridStatus(),
          api.getOffGridPresets(),
          api.getOffGridConfig(),
        ]);
        if (cancelled) {
          return;
        }
        setStatus(nextStatus);
        setLastStatusRefreshMs(Date.now());
        setStatusPollFailures(0);
        setPresets(nextPresets.presets || []);
        setConfig(nextConfig);
        setConfigResponse(nextConfig);
      } catch {
        if (!cancelled) {
          // Initial load failure — surfaced inline via the
          // "Off-grid telemetry unavailable" empty state; no toast.
          setStatusPollFailures(prev => prev + 1);
        }
      }
    };

    load();
    const interval = window.setInterval(() => {
      api.getOffGridStatus().then((nextStatus) => {
        if (!cancelled) {
          setStatus(nextStatus);
          setLastStatusRefreshMs(Date.now());
          setStatusPollFailures(0);
        }
      }).catch(() => {
        if (!cancelled) {
          setStatusPollFailures(prev => prev + 1);
        }
      });
    }, 3000);

    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [addToast]);

  const saveConfig = async () => {
    if (config.adc?.type === 'simulated') {
      addToast(
        'Simulated ADC is lab-only; select INA226 or Sysfs ADC before saving off-grid protection.',
        'error',
      );
      return;
    }

    setSaving(true);
    setTestResult(null);
    try {
      const response = await api.updateOffGridConfig(config);
      if (response.status !== 'ok' || !response.config) {
        addToast(response.message || 'Failed to save off-grid config', 'error');
      } else {
        setConfig(response.config);
        setConfigResponse(response.config);
        addToast(response.message, 'success');
        await refreshStatus();
      }
    } catch {
      addToast('Failed to save off-grid config', 'error');
    } finally {
      setSaving(false);
    }
  };

  const handleConfigChange = (next: OffGridConfigPayload) => {
    configRevisionRef.current += 1;
    setConfig(next);
    setTestResult(null);
  };

  const testConfig = async () => {
    const requestId = probeRequestIdRef.current + 1;
    probeRequestIdRef.current = requestId;
    const configRevision = configRevisionRef.current;
    setTesting(true);
    setTestResult(null);
    try {
      const response = await api.testOffGridConfig(config);
      if (probeRequestIdRef.current !== requestId || configRevisionRef.current !== configRevision) {
        return;
      }
      setTestResult(response);
      addToast(
        response.message,
        !response.ok ? 'error' : response.plausible ? 'success' : 'warning',
      );
    } catch {
      if (probeRequestIdRef.current === requestId) {
        setTestResult(null);
      }
      addToast('Failed to probe the off-grid sensor path', 'error');
    } finally {
      if (probeRequestIdRef.current === requestId) {
        setTesting(false);
      }
    }
  };

  const statusStale = lastStatusRefreshMs != null
    && Date.now() - lastStatusRefreshMs > STATUS_STALE_AFTER_MS;

  const telemetryStaleBanner = status && (statusStale || statusPollFailures > 0) ? (
    <div style={{
      marginBottom: 16,
      padding: '10px 12px',
      borderRadius: 10,
      background: 'rgba(234,179,8,0.08)',
      border: '1px solid rgba(234,179,8,0.25)',
      color: 'var(--text-secondary)',
      fontSize: '0.76rem',
      lineHeight: 1.5,
    }}>
      Off-grid telemetry may be stale. Last successful refresh was {formatAge(lastStatusRefreshMs == null ? null : Date.now() - lastStatusRefreshMs)} ago ({formatTimestamp(lastStatusRefreshMs)}).
      Showing the last known UI state until polling recovers.
    </div>
  ) : null;

  if (!status) {
    return (
      <div className="section">
        <div className="section-title" style={{ color: 'var(--green)' }}>Power Source Guide</div>
        {statusPollFailures > 0 ? (
          <EmptyState
            illustration={<NoLogsIllustration />}
            title="Off-grid telemetry unavailable"
            hint="Still retrying the off-grid commissioning API. The page has not received a usable status response yet."
            data-testid="offgrid-status-unavailable"
          />
        ) : (
          <SectionSkeleton rows={4} data-testid="offgrid-status-loading" />
        )}
      </div>
    );
  }

  if (!status.enabled) {
    return (
      <div className="section">
        <div className="section-title" style={{ color: 'var(--green)' }}>Off-Grid Commissioning</div>

        <div style={{ marginTop: 16 }}>{telemetryStaleBanner}</div>

        <div style={{ marginTop: 24 }}>
          <OffGridConfigCard
            config={config}
            presets={presets}
            onChange={handleConfigChange}
            onTest={testConfig}
            onSave={saveConfig}
            testing={testing}
            saving={saving}
            testResult={testResult}
            response={configResponse}
          />
        </div>
      </div>
    );
  }

  const selectedPreset = presets.find(preset => preset.id === config.battery_preset);
  const criticalV = status.critical_v ?? selectedPreset?.critical_v ?? 40;
  const lowV = status.low_v ?? selectedPreset?.low_v ?? 47;
  const highV = status.high_v ?? selectedPreset?.high_v ?? 53.6;
  const fullV = status.full_v ?? selectedPreset?.full_v ?? 54.4;
  const freqColor = status.freq_pct >= 75 ? 'var(--green)' : status.freq_pct >= 50 ? 'var(--yellow)' : 'var(--red)';
  const charging = status.current_a < -0.1;
  const minerPowerWatts = Math.max(0, status.power_w);
  const powerIsMeasured = status.has_current === true;
  const offGridBtuH = wattsToBtu(minerPowerWatts);
  const offGridBtuHint = powerIsMeasured
    ? 'Heat output from measured ADC current'
    : 'Estimated from source telemetry';

  const zoneExplanation: Record<string, string> = {
    critical: 'Battery critically low. Mining paused to prevent deep discharge damage. Waiting for recovery voltage.',
    low: 'Battery low. Reducing hashrate to conserve energy. Frequency ramping down.',
    normal: 'Battery healthy. Mining at stable frequency. Supply and demand balanced.',
    high: 'Voltage is elevated. Surplus energy is available, so DCENT_OS is allowed to ramp up.',
    full: 'Battery is full or the source is comfortably above target. Mining at maximum allowed frequency.',
    pending_restart: 'Configuration is saved, but the running daemon has not restarted into the new off-grid workflow yet.',
    sensor_fault: status.message || 'The voltage sensor path is unhealthy. DCENT_OS has moved to a safe sensor-fault state.',
  };

  return (
    <div className="section">
      {telemetryStaleBanner}

      <div className="section-title" style={{ color: 'var(--green)' }}>
        Off-Grid / Direct DC
        <InfoDot
          placement="bottom"
          label="What off-grid / direct-DC mode does"
          content={
            <>
              For battery, solar, bench-supply, or generator power instead of a
              wall outlet. DCENT_OS watches your DC bus voltage and ramps mining
              down (or sleeps it) as the source weakens, then wakes back up when
              it recovers — so it protects your battery bank instead of draining
              it flat. Protection is only as good as the voltage sensor you wire.
            </>
          }
        />
        <span style={{
          marginLeft: 8, fontSize: '0.65rem',
          background: status.zone === 'critical' || status.zone === 'sensor_fault' ? 'rgba(239,68,68,0.2)' :
            status.zone === 'low' || status.zone === 'pending_restart' ? 'rgba(234,179,8,0.2)' : 'rgba(34,197,94,0.2)',
          color: status.zone === 'critical' || status.zone === 'sensor_fault' ? 'var(--red)' :
            status.zone === 'low' || status.zone === 'pending_restart' ? 'var(--yellow)' : 'var(--green)',
          padding: '2px 8px', borderRadius: 4, fontWeight: 600,
        }}>
          {status.zone.toUpperCase()}
        </span>
      </div>

      <div style={{
        fontSize: '0.78rem', color: 'var(--text-dim)', marginBottom: 16,
        padding: '8px 12px', borderRadius: 8,
        background: status.zone === 'critical' || status.zone === 'sensor_fault' ? 'rgba(239,68,68,0.08)' :
          status.zone === 'low' || status.zone === 'pending_restart' ? 'rgba(234,179,8,0.08)' : 'rgba(34,197,94,0.08)',
        borderLeft: `3px solid ${status.zone === 'critical' || status.zone === 'sensor_fault' ? 'var(--red)' :
          status.zone === 'low' || status.zone === 'pending_restart' ? 'var(--yellow)' : 'var(--green)'}`,
      }}>
        {zoneExplanation[status.zone] || status.message || 'Monitoring source voltage.'}
      </div>

      <div style={{
        background: 'var(--card-bg)', borderRadius: 'var(--radius)',
        border: '1px solid var(--border)', padding: 16, marginBottom: 16,
        display: 'grid', gridTemplateColumns: '80px 1fr', gap: 16, alignItems: 'center',
      }}>
        <BatteryGauge
          soc_pct={status.battery_soc_pct}
          voltage_v={status.bus_voltage_v}
          current_a={status.current_a}
          zone={status.zone}
        />
        <div>
          <VoltageZoneBar
            voltage_v={status.bus_voltage_v}
            critical_v={criticalV}
            low_v={lowV}
            high_v={highV}
            full_v={fullV}
          />
          <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginTop: 8, fontFamily: "'JetBrains Mono', monospace" }}>
            Rate: {status.voltage_rate_vps >= 0 ? '+' : ''}{(status.voltage_rate_vps * 60).toFixed(2)} V/min
            {charging ? ' (charging)' : status.current_a > 0.1 ? ' (discharging)' : ''}
          </div>
        </div>
      </div>

      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(130px, 1fr))', gap: 8, marginBottom: 16 }}>
        <MetricCard label="Frequency" value={`${status.freq_pct.toFixed(0)}%`} unit={`${status.target_freq_mhz} MHz`} color={freqColor} hint="Auto-scaled by source voltage" />
        <MetricCard label="Power" value={`${Math.round(status.power_w)}`} unit="W" hint={powerIsMeasured ? 'Measured from ADC backend' : 'Estimated from source telemetry'} />
        <MetricCard label={powerIsMeasured ? 'BTU/h' : 'BTU/h est.'} value={`${offGridBtuH}`} color="var(--accent)" hint={offGridBtuHint} />
        <MetricCard label="Battery Uptime" value={formatUptime(status.uptime_battery_s)} hint="Time on DC workflow" />
        <MetricCard label="Energy Used" value={status.energy_consumed_wh > 1000 ? `${(status.energy_consumed_wh / 1000).toFixed(1)}` : `${Math.round(status.energy_consumed_wh)}`} unit={status.energy_consumed_wh > 1000 ? 'kWh' : 'Wh'} hint="This session" />
        <MetricCard label="Sensor" value={status.sensor_source || '---'} hint={status.sensor_ok ? 'Healthy input path' : status.message || 'Sensor not healthy'} color={status.sensor_ok ? 'var(--green)' : 'var(--yellow)'} />
        <MetricCard label="State" value={status.state} color="var(--text-dim)" hint="Controller state" />
      </div>

      <OffGridConfigCard
        config={config}
        presets={presets}
        onChange={handleConfigChange}
        onTest={testConfig}
        onSave={saveConfig}
        testing={testing}
        saving={saving}
        testResult={testResult}
        response={configResponse}
      />
    </div>
  );
}

function HowItWorks() {
  const [open, setOpen] = useState(false);
  const contentId = 'off-grid-how-it-works-content';
  return (
    <div style={{ background: 'var(--card-bg)', borderRadius: 'var(--radius)', border: '1px solid var(--border)', overflow: 'hidden' }}>
      <button type="button" aria-expanded={open} aria-controls={contentId} onClick={() => setOpen(!open)} style={{
        width: '100%', padding: '12px 16px', background: 'none', border: 'none',
        color: 'var(--text)', cursor: 'pointer', display: 'flex', justifyContent: 'space-between', alignItems: 'center',
      }}>
        <span style={{ fontSize: '0.8rem', fontWeight: 600 }}>How Off-Grid Mode Works</span>
        <span style={{ fontSize: '0.8rem', color: 'var(--text-dim)' }}>{open ? '[up]' : '[down]'}</span>
      </button>
      {open && (
        <div id={contentId} style={{ padding: '0 16px 16px', fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.7 }}>
          <p style={{ marginBottom: 12 }}>
            <strong style={{ color: 'var(--text)' }}>The idea is simple:</strong> DCENT_OS watches your DC bus voltage every few seconds. When voltage is healthy, it mines at full speed. When voltage drops, it ramps frequency down. When voltage is critical, it sleeps the miner. When voltage recovers, it wakes conservatively and ramps back up.
          </p>
          <p style={{ marginBottom: 12 }}>
            <strong style={{ color: 'var(--text)' }}>The key deployment rule:</strong> battery protection is only as good as the voltage source you configure. That is why DCENT_OS now requires an explicit ADC backend instead of quietly pretending the bus is healthy.
          </p>
          <p style={{ marginBottom: 12 }}>
            <strong style={{ color: 'var(--text)' }}>Five voltage zones</strong> drive behavior: Critical (sleep), Low (ramp down), Normal (hold), High (ramp up), and Full (max mining).
          </p>
          <p>
            <strong style={{ color: 'var(--text)' }}>No judgment:</strong> whether you are running a bench supply, a battery bank, a cabin solar rig, or a supercapacitor experiment, DCENT_OS should tell the truth about what it can measure and what it is only estimating.
          </p>
        </div>
      )}
    </div>
  );
}
