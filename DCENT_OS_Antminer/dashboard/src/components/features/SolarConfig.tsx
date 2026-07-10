import React, { useEffect, useState } from 'react';
import { api } from '../../api/client';
import type { InverterBrand, SolarConfig as SolarConfigType, SolarProviderStage, SolarStatus, SolarTestResponse } from '../../api/feature-types';
import { useTranslation } from '../../i18n/i18n';
import { useMinerStore } from '../../store/miner';
import { InfoDot } from '../common/Tooltip';

type ProviderOption = {
  value: InverterBrand;
  label: string;
  stage: SolarProviderStage;
  transport: string;
  trustBoundaryLabel: string;
  trustBoundaryDetail: string;
  failSafeExpectation: string;
  placeholder?: string;
  endpointLabel?: string;
  keyLabel?: string;
  help: string;
};

type SolarValidation = {
  fieldErrors: Partial<Record<'apiEndpoint' | 'apiKey' | 'bridgeBaseUrl' | 'bridgeApiKey' | 'teslaGatewayHost' | 'teslaPassword' | 'baseLoadWatts' | 'batteryThresholdPct' | 'batteryWakeHysteresisPct' | 'providerMaxSampleAgeMs' | 'providerFailureHysteresisSamples' | 'hybridImportDeadbandWatts' | 'manualProductionWatts' | 'manualSiteLoadWatts' | 'manualBatterySocPct', string>>;
  guidance: string[];
};

function isValidHttpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === 'http:' || url.protocol === 'https:';
  } catch {
    return false;
  }
}

function isValidMqttUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === 'mqtt:' || url.protocol === 'mqtts:' || url.protocol === 'ws:' || url.protocol === 'wss:';
  } catch {
    return false;
  }
}

function normalizeHost(value: string): string {
  return value.trim().replace(/^https?:\/\//, '').replace(/\/.*$/, '');
}

function validateSolarConfig(config: SolarConfigType, provider: ProviderOption | undefined): SolarValidation {
  const fieldErrors: SolarValidation['fieldErrors'] = {};
  const guidance: string[] = [];

  if (config.baseLoadWatts < 0) {
    fieldErrors.baseLoadWatts = 'Base load must be zero or higher.';
  }

  if (config.batteryThresholdPct < 0 || config.batteryThresholdPct > 100) {
    fieldErrors.batteryThresholdPct = 'Battery floor must stay between 0% and 100%.';
  }

  if (config.batteryWakeHysteresisPct < 0 || config.batteryWakeHysteresisPct > 50) {
    fieldErrors.batteryWakeHysteresisPct = 'Battery wake hysteresis must stay between 0% and 50%.';
  }

  if (config.batteryThresholdPct + config.batteryWakeHysteresisPct > 100) {
    fieldErrors.batteryWakeHysteresisPct = 'Battery floor plus wake hysteresis must stay at or below 100%.';
  }

  if (config.providerMaxSampleAgeMs < 0 || config.providerMaxSampleAgeMs > 300000) {
    fieldErrors.providerMaxSampleAgeMs = 'Provider sample-age timeout must stay between 0 ms and 300000 ms.';
  }

  if (config.providerFailureHysteresisSamples < 1 || config.providerFailureHysteresisSamples > 10) {
    fieldErrors.providerFailureHysteresisSamples = 'Failure hysteresis must stay between 1 and 10 samples.';
  }

  if (config.hybridImportDeadbandWatts < 0 || config.hybridImportDeadbandWatts > 5000) {
    fieldErrors.hybridImportDeadbandWatts = 'Hybrid deadband must stay between 0 W and 5000 W.';
  }

  if (config.inverterBrand === 'manual') {
    if (config.manualProductionWatts < 0) {
      fieldErrors.manualProductionWatts = 'Manual solar production cannot be negative.';
    }
    if (config.manualSiteLoadWatts < 0) {
      fieldErrors.manualSiteLoadWatts = 'Manual site load cannot be negative.';
    }
    if (config.manualBatterySocPct != null && (config.manualBatterySocPct < 0 || config.manualBatterySocPct > 100)) {
      fieldErrors.manualBatterySocPct = 'Manual battery SoC must stay between 0% and 100%.';
    }
    guidance.push('Manual mode is fine for commissioning, but enforcement confidence only comes from disciplined updates or a live adapter.');
  }

  if (config.inverterBrand === 'victron') {
    if (!config.apiEndpoint.trim()) {
      fieldErrors.apiEndpoint = 'Victron GX MQTT endpoint is required.';
    } else if (!isValidMqttUrl(config.apiEndpoint.trim())) {
      fieldErrors.apiEndpoint = 'Use an MQTT endpoint such as mqtt://venus.local:1883.';
    }
    guidance.push('Victron is the highest-trust path when retained MQTT topics are available on-LAN.');
  }

  if (config.inverterBrand === 'bridge') {
    if (!config.bridgeBaseUrl?.trim()) {
      fieldErrors.bridgeBaseUrl = 'Bridge base URL is required.';
    } else if (!isValidHttpUrl(config.bridgeBaseUrl.trim())) {
      fieldErrors.bridgeBaseUrl = 'Use an HTTP or HTTPS bridge URL.';
    }
    guidance.push('Bridge mode works best when the gateway normalizes solar, site load, grid, and battery fields on your LAN.');
  }

  if (config.inverterBrand === 'tesla') {
    if (!config.teslaGatewayHost?.trim()) {
      fieldErrors.teslaGatewayHost = 'Tesla gateway host is required.';
    } else if (!normalizeHost(config.teslaGatewayHost).trim()) {
      fieldErrors.teslaGatewayHost = 'Enter a Powerwall gateway host such as powerwall.local.';
    }
    guidance.push('Tesla local is usable today, but test auth and field freshness before you trust battery-floor behavior.');
  }

  if (['ecoflow', 'enphase', 'solaredge'].includes(config.inverterBrand)) {
    if (!config.apiEndpoint.trim()) {
      fieldErrors.apiEndpoint = `${provider?.endpointLabel || 'Endpoint'} is required.`;
    } else if (config.inverterBrand === 'ecoflow') {
      const endpoint = config.apiEndpoint.trim();
      if (!isValidHttpUrl(endpoint) && !isValidMqttUrl(endpoint)) {
        fieldErrors.apiEndpoint = 'Use an HTTP(S) or MQTT/WS endpoint for the EcoFlow bridge contract.';
      }
    } else if (!isValidHttpUrl(config.apiEndpoint.trim())) {
      fieldErrors.apiEndpoint = 'Use an HTTP or HTTPS endpoint.';
    }
  }

  if (config.inverterBrand === 'ecoflow') {
    guidance.push('EcoFlow is now a limited live path that expects the EcoFlow-specific HTTP bridge contract with normalized JSON metrics. Use Bridge or Manual if your EcoFlow data cannot meet that contract yet.');
  }

  if (config.inverterBrand === 'enphase') {
    guidance.push('Enphase is strongest when local consumption meters are present and test results show full field coverage.');
  }

  if (config.inverterBrand === 'solaredge') {
    guidance.push('SolarEdge cloud telemetry is better for coarse policy than fast closed-loop control.');
  }

  return { fieldErrors, guidance };
}

function fieldInputStyle(hasError: boolean): React.CSSProperties | undefined {
  return hasError
    ? {
        borderColor: 'rgba(239,68,68,0.45)',
        boxShadow: '0 0 0 1px rgba(239,68,68,0.18)',
      }
    : undefined;
}

function fieldErrorMessage(message: string | undefined) {
  if (!message) return null;
  return (
    <div role="alert" style={{ marginTop: 6, fontSize: '0.72rem', color: 'var(--red)', lineHeight: 1.4 }}>
      {message}
    </div>
  );
}

function formatProviderStage(stage: SolarProviderStage | undefined): string {
  if (stage === 'limited') return 'limited live';
  if (stage === 'unsupported') return 'unsupported';
  return stage ?? 'staged';
}

function formatMiningPowerSource(status: SolarStatus): string | null {
  if (!status.miningWattsSource) return null;
  if (status.miningWattsSource === 'unavailable' || status.miningWattsLive === false) {
    return 'Miner load source: unavailable';
  }

  const mode = status.miningWattsModeled ? 'live modeled' : 'live measured';
  return `Miner load source: ${mode} (${status.miningWattsSource})`;
}

const INVERTER_OPTIONS: ProviderOption[] = [
  {
    value: 'manual',
    label: 'Manual Input',
    stage: 'live',
    transport: 'manual',
    trustBoundaryLabel: 'Operator-entered snapshot',
    trustBoundaryDetail: 'DCENT_OS only sees what an operator typed. It cannot prove freshness, detect drift, or distinguish a stale guess from a real site event.',
    failSafeExpectation: 'Manual mode should shape commissioning decisions only. Unattended protection still needs a real Off-Grid voltage path, and preferably a live provider before solar policy is trusted.',
    help: 'Live now. Enter estimated production/load numbers manually and let DCENT_OS combine them with miner wall-power telemetry.',
  },
  {
    value: 'victron',
    label: 'Victron GX (LAN MQTT)',
    stage: 'live',
    transport: 'mqtt',
    trustBoundaryLabel: 'Local battery-first telemetry',
    trustBoundaryDetail: 'Provider data stays on-LAN and comes from the GX stack directly, so trust mostly depends on broker reachability and retained-topic freshness rather than cloud relays or operator memory.',
    failSafeExpectation: 'If MQTT freshness degrades, Green Mining should fail closed while Off-Grid still protects the DC bus independently.',
    placeholder: 'mqtt://venus.local:1883',
    endpointLabel: 'GX MQTT Endpoint',
    keyLabel: 'MQTT Password (optional)',
    help: 'Live now. Use Victron GX LAN MQTT with retained system/0 topics for direct Victron telemetry.',
  },
  {
    value: 'bridge',
    label: 'Bridge (HTTP JSON)',
    stage: 'live',
    transport: 'http-json',
    trustBoundaryLabel: 'Normalized bridge contract',
    trustBoundaryDetail: 'DCENT_OS trusts the bridge to translate upstream telemetry correctly. The trust boundary is the bridge itself: field names, sign conventions, timestamps, and battery meaning must all stay consistent.',
    failSafeExpectation: 'Treat bridge freshness and contract tests as mandatory. If the bridge goes stale or drifts, Green Mining should stop enforcing while Off-Grid keeps hard battery/DC protection.',
    placeholder: 'http://bridge.local/api/v1/victron',
    endpointLabel: 'Bridge Base URL',
    keyLabel: 'Bridge Token (optional)',
    help: 'Live now. Use an HTTP JSON bridge that exposes Victron D-Bus style production/load/grid/battery fields.',
  },
  {
    value: 'ecoflow',
    label: 'EcoFlow (Limited HTTP Bridge)',
    stage: 'limited',
    transport: 'ecoflow-http-bridge',
    trustBoundaryLabel: 'Narrow EcoFlow bridge contract',
    trustBoundaryDetail: 'DCENT_OS trusts only the specific EcoFlow bridge contract it already understands. Anything outside that contract is outside the trust boundary, even if the source is still EcoFlow.',
    failSafeExpectation: 'Use this only after repeated contract validation. If field coverage or freshness slips, fall back to observe-only policy and keep Off-Grid protection authoritative.',
    placeholder: 'mqtt://ecoflow-bridge.local:1883/dcentos/ecoflow',
    endpointLabel: 'EcoFlow Bridge Endpoint',
    keyLabel: 'Access Token / Secret',
    help: 'Limited live path. DCENT_OS can enforce against an EcoFlow-specific bridge contract over HTTP JSON or MQTT/WS JSON, but it does not claim broad direct EcoFlow auth/protocol coverage yet.',
  },
  {
    value: 'tesla',
    label: 'Tesla Powerwall (Local)',
    stage: 'live',
    transport: 'http-json',
    trustBoundaryLabel: 'Local gateway with auth/TLS quirks',
    trustBoundaryDetail: 'Telemetry stays local, but the trust boundary includes Tesla gateway auth and session behavior. A saved password is not proof that the local session will stay healthy.',
    failSafeExpectation: 'Use successful tests and rolling history as the gate for policy trust. Off-Grid remains the hard stop if Powerwall telemetry becomes stale or unavailable.',
    placeholder: 'powerwall.local',
    endpointLabel: 'Gateway Host',
    keyLabel: 'Local Gateway Password / Token',
    help: 'Live now as a first-pass local adapter. Uses Tesla local gateway endpoints for solar/load/grid and best-effort battery SoC.',
  },
  {
    value: 'enphase',
    label: 'Enphase Envoy / IQ Gateway',
    stage: 'live',
    transport: 'http-json',
    trustBoundaryLabel: 'Local solar/load with best-effort battery context',
    trustBoundaryDetail: 'Production and load can be strong when metering is installed, but battery context is still less authoritative than a native battery-first stack.',
    failSafeExpectation: 'Treat battery-aware policy as verified only after the history proves consistent field coverage. Off-Grid should still own direct battery/DC protection.',
    placeholder: 'http://envoy.local',
    endpointLabel: 'Gateway Base URL',
    keyLabel: 'Gateway Token (optional)',
    help: 'First-pass live local adapter. Uses Enphase production.json and best-effort battery SoC from ensemble secctrl when available.',
  },
  {
    value: 'solaredge',
    label: 'SolarEdge (Cloud currentPowerFlow)',
    stage: 'live',
    transport: 'cloud-http',
    trustBoundaryLabel: 'Cloud-polled telemetry',
    trustBoundaryDetail: 'The trust boundary extends across Internet reachability, API freshness, and provider-side sign conventions. This is useful visibility, but not the fastest or safest control loop source.',
    failSafeExpectation: 'Use for coarse policy and visibility. Keep aggressive battery-floor behavior on the Off-Grid side, because cloud delay should not be your only fail-safe.',
    placeholder: 'https://monitoringapi.solaredge.com/site/{siteId}/currentPowerFlow',
    endpointLabel: 'currentPowerFlow URL',
    keyLabel: 'Monitoring API Key',
    help: 'First-pass live cloud adapter. Best for coarse hybrid policy; battery-backed workflows should verify SoC carefully because telemetry freshness is cloud-dependent.',
  },
];

const DEFAULT_CONFIG: SolarConfigType = {
  enabled: false,
  inverterBrand: 'manual',
  apiEndpoint: '',
  apiKey: '',
  bridgeBaseUrl: '',
  bridgeApiKey: '',
  teslaGatewayHost: '',
  teslaPassword: '',
  solarOnlyMode: false,
  baseLoadWatts: 500,
  batteryThresholdPct: 20,
  batteryWakeHysteresisPct: 3,
  providerMaxSampleAgeMs: 60000,
  providerFailureHysteresisSamples: 1,
  hybridImportDeadbandWatts: 75,
  manualProductionWatts: 0,
  manualSiteLoadWatts: 0,
  manualBatterySocPct: null,
};

const DEFAULT_STATUS: SolarStatus = {
  productionWatts: 0,
  consumptionWatts: 0,
  miningWatts: 0,
  netGridWatts: 0,
  solarSurplusWatts: 0,
  batterySocPct: null,
  connected: false,
  message: 'Solar integration is disabled.',
};

export function SolarConfig() {
  const { t } = useTranslation();
  const addToast = useMinerStore(s => s.addToast);
  const [config, setConfig] = useState<SolarConfigType>(DEFAULT_CONFIG);
  const [status, setStatus] = useState<SolarStatus>(DEFAULT_STATUS);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<SolarTestResponse | null>(null);

  const selectedInverter = INVERTER_OPTIONS.find(option => option.value === config.inverterBrand);
  const providerLive = selectedInverter?.stage !== 'staged';

  const derivedTeslaEndpoint = config.teslaGatewayHost?.trim()
    ? `http://${config.teslaGatewayHost.trim().replace(/^https?:\/\//, '').replace(/\/.*$/, '')}/api/meters/aggregates`
    : '';

  const activeProviderLabel = selectedInverter?.label ?? config.inverterBrand;
  const activeStatusProvider = status.provider || config.inverterBrand;
  const liveConnectionLabel = status.connected ? 'connected' : 'not connected';
  const policyLabel = status.controlActive == null
    ? 'unknown'
    : status.controlActive
      ? (status.sourceProfile === 'hybrid' && !status.solarOnlyMode ? 'hybrid import-minimize' : 'enforcing')
      : 'observe only';
  const miningPowerSourceLabel = formatMiningPowerSource(status);
  const miningWattsDisplay = status.miningWattsLive === false && status.miningWatts === 0
    ? 'Unavailable'
    : `${status.miningWatts} W`;
  const validation = validateSolarConfig(config, selectedInverter);
  const validationErrors = Object.values(validation.fieldErrors).filter(Boolean) as string[];
  const canTestProvider = config.inverterBrand !== 'manual' && providerLive && validationErrors.length === 0;
  const canSave = !saving && !testing && validationErrors.length === 0;
  const providerMeta = {
    stage: status.providerStage ?? selectedInverter?.stage ?? 'staged',
    providerLiveBackend: status.providerLiveBackend ?? selectedInverter?.stage !== 'staged',
    trustBoundaryLabel: selectedInverter?.trustBoundaryLabel ?? 'Unknown trust boundary',
    trustBoundaryDetail: selectedInverter?.trustBoundaryDetail ?? 'Provider trust boundary details are in development.',
    failSafeExpectation: selectedInverter?.failSafeExpectation ?? 'Treat provider telemetry as advisory until site verification is complete.',
    recommendedProvider: status.recommendedProvider ?? config.recommendedProvider ?? null,
    backendScope: status.providerBackendScope ?? config.providerBackendScope ?? null,
    acceptedPayloadShapes: status.acceptedPayloadShapes?.length ? status.acceptedPayloadShapes : (config.acceptedPayloadShapes ?? []),
  };
  const runtimeAdopted = status.runtimeAdopted ?? false;
  const derivedProtectionMode = config.solarOnlyMode
    ? 'Solar surplus only'
    : config.batteryThresholdPct >= 40
      ? 'Battery protection biased'
      : 'Hybrid import-minimize';
  const commissioningVerdict = !runtimeAdopted && config.enabled
    ? 'Saved, restart pending'
    : !providerLive
    ? 'Staged only'
    : config.inverterBrand === 'manual'
      ? (runtimeAdopted ? 'Operator-driven runtime' : 'Operator-driven')
      : status.connected && !status.stale && (status.consecutiveFailures ?? 0) === 0
        ? 'Field-ready'
        : 'Verify before enforcing';

  const solarEnabledLabelId = 'solar-enabled-label';
  const solarInverterId = 'solar-inverter-brand';
  const solarBaseLoadId = 'solar-base-load';
  const solarBatteryThresholdId = 'solar-battery-threshold';
  const solarOnlyLabelId = 'solar-only-label';

  const refreshStatus = async () => {
    try {
      const nextStatus = await api.getSolarStatus();
      setStatus(nextStatus);
    } catch {
      addToast('Could not refresh solar status', 'error');
    }
  };

  useEffect(() => {
    let cancelled = false;

    const load = async () => {
      try {
        const [savedConfig, liveStatus] = await Promise.all([
          api.getSolarConfig(),
          api.getSolarStatus(),
        ]);
        if (!cancelled) {
          setConfig({ ...DEFAULT_CONFIG, ...savedConfig });
          setStatus(liveStatus);
        }
      } catch {
        if (!cancelled) {
          addToast('Could not load solar integration settings', 'error');
        }
      }
    };

    load();
    const timer = window.setInterval(() => {
      api.getSolarStatus().then((nextStatus) => {
        if (!cancelled) {
          setStatus(nextStatus);
        }
      }).catch(() => {});
    }, 5000);

    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [addToast]);

  const update = (partial: Partial<SolarConfigType>) => {
    setConfig(prev => ({ ...prev, ...partial }));
  };

  const handleSave = async () => {
    if (validationErrors.length > 0) {
      addToast(validationErrors[0], 'warning');
      return;
    }

    setSaving(true);
    try {
      const response = await api.updateSolarConfig(config);
      if (response.status !== 'ok') {
        addToast(response.message || 'Failed to save solar configuration', 'error');
      } else {
        setConfig(response.config ?? config);
        addToast(response.message, 'success');
        await refreshStatus();
      }
    } catch {
      addToast('Failed to save solar configuration', 'error');
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async () => {
    if (validationErrors.length > 0) {
      addToast(validationErrors[0], 'warning');
      return;
    }

    setTesting(true);
    setTestResult(null);
    try {
      const result = await api.testSolarConfig(config);
      setTestResult(result);
      addToast(result.message, result.ok ? 'success' : 'warning');
      await refreshStatus();
    } catch {
      addToast('Solar provider test failed', 'error');
    } finally {
      setTesting(false);
    }
  };

  return (
    <div className="feat-card">
      <h3 className="feat-card-title feat-title-green">
        {t('solar.title')}
        <InfoDot
          placement="bottom"
          label="What solar integration does"
          content={
            <>
              Connects DCENT_OS to your solar inverter or battery system so it
              can mine harder when you have surplus sun and back off to protect
              the battery when you don't. This page only shapes policy from
              provider telemetry — the Off-Grid page owns the hard
              battery/voltage fail-safe. Commission both together for a real
              solar+battery deployment.
            </>
          }
        />
      </h3>

      <label className="feat-toggle-row" style={{ marginBottom: 16 }}>
        <span className="feat-toggle-label" id={solarEnabledLabelId}>{t('common.enabled')}</span>
        <button
          type="button"
          role="switch"
          aria-checked={config.enabled}
            aria-labelledby={solarEnabledLabelId}
            className={`feat-toggle ${config.enabled ? 'active' : ''}`}
            onClick={() => {
              if (!providerLive) {
                addToast('Only live provider backends can be enabled for enforcement on this page', 'warning');
                return;
              }
              update({ enabled: !config.enabled });
           }}
        >
          <span className="feat-toggle-knob" />
        </button>
      </label>

      <div className="feat-form-grid">
        <div className="feat-input-group">
          <label className="feat-label" htmlFor={solarInverterId}>{t('solar.inverterBrand')}</label>
          <select
            id={solarInverterId}
            value={config.inverterBrand}
            onChange={e => {
              const nextBrand = e.target.value as InverterBrand;
              const nextOption = INVERTER_OPTIONS.find(option => option.value === nextBrand);
              update({ inverterBrand: nextBrand, enabled: nextOption?.stage === 'live' ? config.enabled : false });
            }}
            className="feat-input"
          >
            {INVERTER_OPTIONS.map(option => (
              <option key={option.value} value={option.value}>{option.label}</option>
            ))}
          </select>
        </div>

        {config.inverterBrand === 'victron' && (
          <>
            <div className="feat-input-group">
              <label className="feat-label">{selectedInverter?.endpointLabel || t('solar.apiEndpoint')}</label>
              <input
                type="text"
                value={config.apiEndpoint}
                onChange={e => update({ apiEndpoint: e.target.value })}
                placeholder={selectedInverter?.placeholder}
                className="feat-input"
                aria-invalid={!!validation.fieldErrors.apiEndpoint}
                style={fieldInputStyle(!!validation.fieldErrors.apiEndpoint)}
              />
              {fieldErrorMessage(validation.fieldErrors.apiEndpoint)}
            </div>
            <div className="feat-input-group">
              <label className="feat-label">{selectedInverter?.keyLabel || t('solar.apiKey')}</label>
              <input
                type="password"
                value={config.apiKey}
                onChange={e => update({ apiKey: e.target.value })}
                placeholder="API key or token"
                className="feat-input"
                aria-invalid={!!validation.fieldErrors.apiKey}
                style={fieldInputStyle(!!validation.fieldErrors.apiKey)}
              />
              {fieldErrorMessage(validation.fieldErrors.apiKey)}
            </div>
          </>
        )}

        {config.inverterBrand === 'bridge' && (
          <>
            <div className="feat-input-group">
              <label className="feat-label">{selectedInverter?.endpointLabel || 'Bridge Base URL'}</label>
              <input
                type="url"
                value={config.bridgeBaseUrl || ''}
                onChange={e => update({ bridgeBaseUrl: e.target.value, apiEndpoint: e.target.value })}
                placeholder={selectedInverter?.placeholder}
                className="feat-input"
                aria-invalid={!!validation.fieldErrors.bridgeBaseUrl}
                style={fieldInputStyle(!!validation.fieldErrors.bridgeBaseUrl)}
              />
              {fieldErrorMessage(validation.fieldErrors.bridgeBaseUrl)}
            </div>
            <div className="feat-input-group">
              <label className="feat-label">{selectedInverter?.keyLabel || 'Bridge Token (optional)'}</label>
              <input
                type="password"
                value={config.bridgeApiKey || ''}
                onChange={e => update({ bridgeApiKey: e.target.value, apiKey: e.target.value })}
                placeholder="Bearer token or X-Api-Key"
                className="feat-input"
                aria-invalid={!!validation.fieldErrors.bridgeApiKey}
                style={fieldInputStyle(!!validation.fieldErrors.bridgeApiKey)}
              />
              {fieldErrorMessage(validation.fieldErrors.bridgeApiKey)}
            </div>
          </>
        )}

        {config.inverterBrand === 'tesla' && (
          <>
            <div className="feat-input-group">
              <label className="feat-label">{selectedInverter?.endpointLabel || 'Gateway Host'}</label>
              <input
                type="text"
                value={config.teslaGatewayHost || ''}
                onChange={e => {
                  const nextHost = e.target.value;
                  update({
                    teslaGatewayHost: nextHost,
                    apiEndpoint: nextHost.trim()
                      ? `http://${normalizeHost(nextHost)}/api/meters/aggregates`
                      : '',
                  });
                }}
                placeholder={selectedInverter?.placeholder}
                className="feat-input"
                aria-invalid={!!validation.fieldErrors.teslaGatewayHost}
                style={fieldInputStyle(!!validation.fieldErrors.teslaGatewayHost)}
              />
              {fieldErrorMessage(validation.fieldErrors.teslaGatewayHost)}
            </div>
            <div className="feat-input-group">
              <label className="feat-label">{selectedInverter?.keyLabel || 'Local Gateway Password / Token'}</label>
              <input
                type="password"
                value={config.teslaPassword || ''}
                onChange={e => update({ teslaPassword: e.target.value, apiKey: e.target.value })}
                placeholder="Gateway password or local auth token"
                className="feat-input"
                aria-invalid={!!validation.fieldErrors.teslaPassword}
                style={fieldInputStyle(!!validation.fieldErrors.teslaPassword)}
              />
              {fieldErrorMessage(validation.fieldErrors.teslaPassword)}
            </div>
            <div className="feat-input-group">
              <label className="feat-label">Derived Endpoint</label>
              <input
                type="text"
                value={derivedTeslaEndpoint || config.apiEndpoint}
                onChange={e => update({ apiEndpoint: e.target.value })}
                placeholder="http://powerwall.local/api/meters/aggregates"
                className="feat-input"
                aria-invalid={!!validation.fieldErrors.apiEndpoint}
                style={fieldInputStyle(!!validation.fieldErrors.apiEndpoint)}
              />
              {fieldErrorMessage(validation.fieldErrors.apiEndpoint)}
            </div>
          </>
        )}

        {!['manual', 'victron', 'bridge', 'tesla'].includes(config.inverterBrand) && (
          <>
            <div className="feat-input-group">
              <label className="feat-label">{selectedInverter?.endpointLabel || t('solar.apiEndpoint')}</label>
              <input
                type="url"
                value={config.apiEndpoint}
                onChange={e => update({ apiEndpoint: e.target.value })}
                placeholder={selectedInverter?.placeholder}
                className="feat-input"
                aria-invalid={!!validation.fieldErrors.apiEndpoint}
                style={fieldInputStyle(!!validation.fieldErrors.apiEndpoint)}
              />
              {fieldErrorMessage(validation.fieldErrors.apiEndpoint)}
            </div>
            <div className="feat-input-group">
              <label className="feat-label">{selectedInverter?.keyLabel || t('solar.apiKey')}</label>
              <input
                type="password"
                value={config.apiKey}
                onChange={e => update({ apiKey: e.target.value })}
                placeholder="API key or token"
                className="feat-input"
                aria-invalid={!!validation.fieldErrors.apiKey}
                style={fieldInputStyle(!!validation.fieldErrors.apiKey)}
              />
              {fieldErrorMessage(validation.fieldErrors.apiKey)}
            </div>
          </>
        )}

        <div className="feat-input-group">
          <label className="feat-label" htmlFor={solarBaseLoadId}>{t('solar.baseLoad')}</label>
          <input
            id={solarBaseLoadId}
            type="number"
            min="0"
            step="50"
            value={config.baseLoadWatts}
            onChange={e => update({ baseLoadWatts: Number(e.target.value) })}
            className="feat-input"
            aria-invalid={!!validation.fieldErrors.baseLoadWatts}
            style={fieldInputStyle(!!validation.fieldErrors.baseLoadWatts)}
          />
          {fieldErrorMessage(validation.fieldErrors.baseLoadWatts)}
        </div>

        <div className="feat-input-group">
          <label className="feat-label" htmlFor={solarBatteryThresholdId}>{t('solar.batteryThreshold')}</label>
          <input
            id={solarBatteryThresholdId}
            type="number"
            min="0"
            max="100"
            value={config.batteryThresholdPct}
            onChange={e => update({ batteryThresholdPct: Number(e.target.value) })}
            className="feat-input"
            aria-invalid={!!validation.fieldErrors.batteryThresholdPct}
            style={fieldInputStyle(!!validation.fieldErrors.batteryThresholdPct)}
          />
          {fieldErrorMessage(validation.fieldErrors.batteryThresholdPct)}
        </div>

        <div className="feat-input-group">
          <label className="feat-label">Battery Wake Hysteresis (%)</label>
          <input
            type="number"
            min="0"
            max="50"
            step="1"
            value={config.batteryWakeHysteresisPct}
            onChange={e => update({ batteryWakeHysteresisPct: Number(e.target.value) })}
            className="feat-input"
            aria-invalid={!!validation.fieldErrors.batteryWakeHysteresisPct}
            style={fieldInputStyle(!!validation.fieldErrors.batteryWakeHysteresisPct)}
          />
          {fieldErrorMessage(validation.fieldErrors.batteryWakeHysteresisPct)}
        </div>

        <div className="feat-input-group">
          <label className="feat-label">Provider Sample-Age Timeout (ms)</label>
          <input
            type="number"
            min="0"
            max="300000"
            step="1000"
            value={config.providerMaxSampleAgeMs}
            onChange={e => update({ providerMaxSampleAgeMs: Number(e.target.value) })}
            className="feat-input"
            aria-invalid={!!validation.fieldErrors.providerMaxSampleAgeMs}
            style={fieldInputStyle(!!validation.fieldErrors.providerMaxSampleAgeMs)}
          />
          {fieldErrorMessage(validation.fieldErrors.providerMaxSampleAgeMs)}
        </div>

        <div className="feat-input-group">
          <label className="feat-label">Provider Failure Hysteresis (samples)</label>
          <input
            type="number"
            min="1"
            max="10"
            step="1"
            value={config.providerFailureHysteresisSamples}
            onChange={e => update({ providerFailureHysteresisSamples: Number(e.target.value) })}
            className="feat-input"
            aria-invalid={!!validation.fieldErrors.providerFailureHysteresisSamples}
            style={fieldInputStyle(!!validation.fieldErrors.providerFailureHysteresisSamples)}
          />
          {fieldErrorMessage(validation.fieldErrors.providerFailureHysteresisSamples)}
        </div>

        <div className="feat-input-group">
          <label className="feat-label">Hybrid Import Deadband (W)</label>
          <input
            type="number"
            min="0"
            max="5000"
            step="25"
            value={config.hybridImportDeadbandWatts}
            onChange={e => update({ hybridImportDeadbandWatts: Number(e.target.value) })}
            className="feat-input"
            aria-invalid={!!validation.fieldErrors.hybridImportDeadbandWatts}
            style={fieldInputStyle(!!validation.fieldErrors.hybridImportDeadbandWatts)}
          />
          {fieldErrorMessage(validation.fieldErrors.hybridImportDeadbandWatts)}
        </div>
      </div>

      <div style={{ marginTop: 10, fontSize: '0.72rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
        Freshness timeout applies when a provider reports <code>sampleAgeMs</code> or <code>timestampMs</code>. Set it to <code>0</code> to stop aging out those samples. Failure hysteresis controls how many consecutive provider errors DCENT_OS tolerates before fail-closed sleep on solar-only or battery-backed profiles.
      </div>

      <div style={{
        marginTop: 16,
        padding: 12,
        borderRadius: 'var(--radius)',
        background: 'rgba(59,130,246,0.08)',
        border: '1px solid rgba(59,130,246,0.18)',
        fontSize: '0.78rem',
        color: 'var(--text-dim)',
        lineHeight: 1.55,
      }}>
        <div style={{ color: 'var(--text)', fontWeight: 700, marginBottom: 6 }}>Combined solar+battery commissioning</div>
        This page decides how provider telemetry shapes solar policy. The Off-Grid page owns direct-DC and battery fail-safe behavior. For a real solar+battery deployment, commission both pages together: trusted provider here, trusted voltage path there.
      </div>

      <div style={{
        marginTop: 16, padding: 12, borderRadius: 'var(--radius)',
        background: providerLive ? 'rgba(34,197,94,0.06)' : 'rgba(234,179,8,0.08)',
        border: `1px solid ${providerLive ? 'rgba(34,197,94,0.18)' : 'rgba(234,179,8,0.24)'}`,
        fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55,
      }}>
        <strong style={{ color: 'var(--text)', fontWeight: 600 }}>{providerLive ? 'Live backend' : 'Staged path'}</strong>
        <div style={{ marginTop: 6 }}>{selectedInverter?.help}</div>
        <div style={{ marginTop: 8 }}><strong style={{ color: 'var(--text)', fontWeight: 600 }}>Trust boundary:</strong> {providerMeta.trustBoundaryDetail}</div>
        {(providerMeta.backendScope || providerMeta.acceptedPayloadShapes.length > 0) && (
          <div style={{ marginTop: 8 }}>
            {providerMeta.backendScope && <div>Backend scope: {providerMeta.backendScope}</div>}
            {providerMeta.acceptedPayloadShapes.length > 0 && (
              <div style={{ marginTop: 4 }}>Accepted shapes: {providerMeta.acceptedPayloadShapes.join(' | ')}</div>
            )}
          </div>
        )}
      </div>

      {validationErrors.length > 0 && (
        <div style={{
          marginTop: 12,
          padding: 12,
          borderRadius: 'var(--radius)',
          background: 'rgba(239,68,68,0.08)',
          border: '1px solid rgba(239,68,68,0.22)',
          fontSize: '0.76rem',
          color: 'var(--text-dim)',
          lineHeight: 1.55,
        }}>
          <div style={{ color: 'var(--red)', fontWeight: 700, marginBottom: 6 }}>Fix these fields before testing or saving</div>
          {validationErrors.map(message => (
            <div key={message} style={{ marginTop: 4 }}>{message}</div>
          ))}
        </div>
      )}

      <div style={{
        marginTop: 16,
        display: 'grid',
        gap: 10,
        gridTemplateColumns: 'repeat(auto-fit, minmax(200px, 1fr))',
      }}>
        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Commissioning state</div>
          <div style={{ fontSize: '0.9rem', color: commissioningVerdict === 'Field-ready' ? 'var(--feat-green)' : 'var(--yellow)', fontWeight: 700 }}>
            {commissioningVerdict}
          </div>
          <div style={{ marginTop: 6, fontSize: '0.76rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
            {providerMeta.stage === 'limited'
              ? 'Limited live provider. Treat successful tests as contract validation, not broad adapter proof.'
              : providerMeta.stage === 'live'
                ? 'Watch the rolling history and enable enforcement only after on-site samples stay fresh.'
                : providerMeta.stage === 'unsupported'
                  ? 'This selection is not currently backed by a supported live provider contract.'
                  : 'Save this provider disabled until a live backend exists.'}
          </div>
        </div>

        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Policy tuning summary</div>
          <div style={{ fontSize: '0.9rem', color: 'var(--text)', fontWeight: 700 }}>{derivedProtectionMode}</div>
          <div style={{ marginTop: 6, fontSize: '0.76rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
            Base load {config.baseLoadWatts} W with battery floor {config.batteryThresholdPct}%.
          </div>
        </div>

        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Fail-safe boundary</div>
          <div style={{ fontSize: '0.9rem', color: 'var(--text)', fontWeight: 700 }}>{providerMeta.trustBoundaryLabel}</div>
          <div style={{ marginTop: 6, fontSize: '0.76rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
            {providerMeta.failSafeExpectation}
          </div>
        </div>
      </div>

      {config.inverterBrand === 'manual' && (
        <div style={{ marginTop: 16 }}>
          <div style={{
            marginBottom: 12,
            padding: '10px 12px',
            borderRadius: 10,
            background: 'rgba(234,179,8,0.08)',
            border: '1px solid rgba(234,179,8,0.24)',
            fontSize: '0.76rem',
            color: 'var(--text-dim)',
            lineHeight: 1.55,
          }}>
            <div style={{ color: 'var(--yellow)', fontWeight: 700, marginBottom: 6 }}>Manual provider trust boundary</div>
            These values are operator assertions, not verified telemetry. They are useful for staged solar-policy bring-up, but they do not replace a real battery/DC fail-safe path or a live provider adapter for unattended control.
          </div>

          <div className="feat-form-grid">
          <div className="feat-input-group">
            <label className="feat-label">Manual Solar Production (W)</label>
            <input
              type="number"
              min="0"
              step="10"
              value={config.manualProductionWatts}
              onChange={e => update({ manualProductionWatts: Number(e.target.value) })}
              className="feat-input"
              aria-invalid={!!validation.fieldErrors.manualProductionWatts}
              style={fieldInputStyle(!!validation.fieldErrors.manualProductionWatts)}
            />
            {fieldErrorMessage(validation.fieldErrors.manualProductionWatts)}
          </div>
          <div className="feat-input-group">
            <label className="feat-label">Manual Site Load (W)</label>
            <input
              type="number"
              min="0"
              step="10"
              value={config.manualSiteLoadWatts}
              onChange={e => update({ manualSiteLoadWatts: Number(e.target.value) })}
              className="feat-input"
              aria-invalid={!!validation.fieldErrors.manualSiteLoadWatts}
              style={fieldInputStyle(!!validation.fieldErrors.manualSiteLoadWatts)}
            />
            {fieldErrorMessage(validation.fieldErrors.manualSiteLoadWatts)}
          </div>
          <div className="feat-input-group">
            <label className="feat-label">Manual Battery SoC (%)</label>
            <input
              type="number"
              min="0"
              max="100"
              step="1"
              value={config.manualBatterySocPct ?? ''}
              onChange={e => update({ manualBatterySocPct: e.target.value === '' ? null : Number(e.target.value) })}
              className="feat-input"
              aria-invalid={!!validation.fieldErrors.manualBatterySocPct}
              style={fieldInputStyle(!!validation.fieldErrors.manualBatterySocPct)}
            />
            {fieldErrorMessage(validation.fieldErrors.manualBatterySocPct)}
          </div>
          </div>
        </div>
      )}

      <label className="feat-toggle-row" style={{ marginTop: 16 }}>
        <span className="feat-toggle-label" id={solarOnlyLabelId}>{t('solar.solarOnlyMode')}</span>
        <button
          type="button"
          role="switch"
          aria-checked={config.solarOnlyMode}
          aria-labelledby={solarOnlyLabelId}
          className={`feat-toggle ${config.solarOnlyMode ? 'active' : ''}`}
          onClick={() => update({ solarOnlyMode: !config.solarOnlyMode })}
        >
          <span className="feat-toggle-knob" />
        </button>
      </label>

      <div style={{
        marginTop: 16, padding: 12, borderRadius: 'var(--radius)',
        background: 'rgba(255,255,255,0.03)', border: '1px solid var(--border)',
        fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.5,
      }}>
        <div style={{ display: 'grid', gap: 6, gridTemplateColumns: 'repeat(auto-fit, minmax(150px, 1fr))', marginBottom: 10 }}>
          <div><strong style={{ color: 'var(--text)' }}>Provider:</strong> {String(activeStatusProvider)}</div>
          <div><strong style={{ color: 'var(--text)' }}>Selected:</strong> {activeProviderLabel}</div>
          <div><strong style={{ color: 'var(--text)' }}>Stage:</strong> {formatProviderStage(providerMeta.stage)}</div>
          <div><strong style={{ color: 'var(--text)' }}>Backend:</strong> {providerMeta.providerLiveBackend ? 'live' : 'not live'}</div>
          <div><strong style={{ color: 'var(--text)' }}>Connection:</strong> {liveConnectionLabel}</div>
          <div><strong style={{ color: 'var(--text)' }}>Policy:</strong> {policyLabel}</div>
          <div><strong style={{ color: 'var(--text)' }}>Battery floor:</strong> {status.batteryFloorActive ? 'active' : 'clear'}</div>
        </div>
        {status.message || 'No solar status available.'}
        {providerMeta.backendScope && (
          <div style={{ marginTop: 6, fontSize: '0.72rem', color: 'var(--text-dim)' }}>
            Backend scope: {providerMeta.backendScope}
          </div>
        )}
        {providerMeta.acceptedPayloadShapes.length > 0 && (
          <div style={{ marginTop: 6, fontSize: '0.72rem', color: 'var(--text-dim)' }}>
            Accepted payload shapes: {providerMeta.acceptedPayloadShapes.join(' | ')}
          </div>
        )}
          {status.action && (
            <div style={{ marginTop: 6, fontSize: '0.72rem', color: 'var(--text-dim)' }}>
              Action: {status.action}
              {status.targetFreqMhz ? ` -> ${status.targetFreqMhz} MHz` : ''}
              {status.sleeping ? ' (sleeping)' : ''}
            </div>
          )}
        {status.controlActive != null && (
          <div style={{ marginTop: 6, fontSize: '0.72rem', color: 'var(--text-dim)' }}>
            Policy detail: {policyLabel}
            {status.solarOnlyMode ? ' | solar-only' : ''}
            {status.batteryFloorActive ? ' | battery floor active' : ''}
          </div>
        )}
      </div>

      {testResult && (
        <div style={{
          marginTop: 12, padding: 12, borderRadius: 'var(--radius)',
          background: testResult.ok ? 'rgba(34,197,94,0.08)' : 'rgba(234,179,8,0.08)',
          border: `1px solid ${testResult.ok ? 'rgba(34,197,94,0.2)' : 'rgba(234,179,8,0.25)'}`,
          fontSize: '0.76rem', color: 'var(--text-secondary)', lineHeight: 1.5,
        }}>
          <div style={{ display: 'grid', gap: 6, gridTemplateColumns: 'repeat(auto-fit, minmax(140px, 1fr))', marginBottom: 8 }}>
            <div><strong>Provider:</strong> {testResult.provider}</div>
            <div><strong>Connection:</strong> {testResult.connected ? 'connected' : 'not connected'}</div>
            <div><strong>Transport:</strong> {testResult.transport || 'n/a'}</div>
            <div><strong>Stage:</strong> {formatProviderStage(testResult.providerStage)}</div>
            <div><strong>Backend:</strong> {testResult.providerLiveBackend ? 'live' : 'not live'}</div>
          </div>
          {testResult.message}
          {testResult.providerBackendScope && (
            <div style={{ marginTop: 4 }}>Backend scope: {testResult.providerBackendScope}</div>
          )}
          {testResult.acceptedPayloadShapes && testResult.acceptedPayloadShapes.length > 0 && (
            <div style={{ marginTop: 4 }}>Accepted shapes: {testResult.acceptedPayloadShapes.join(' | ')}</div>
          )}
          {testResult.matched_fields && testResult.matched_fields.length > 0 && (
            <div style={{ marginTop: 4 }}>Matched: {testResult.matched_fields.join(', ')}</div>
          )}
          {(testResult.productionWatts != null || testResult.consumptionWatts != null || testResult.netGridWatts != null || testResult.batterySocPct != null) && (
            <div style={{ marginTop: 6 }}>
              Sample: {testResult.productionWatts ?? 0}W solar, {testResult.consumptionWatts ?? 0}W load, {testResult.netGridWatts ?? 0}W grid
              {testResult.batterySocPct != null ? `, ${testResult.batterySocPct}% battery` : ''}
            </div>
          )}
        </div>
      )}

      <div className="feat-solar-metrics">
        <div className="feat-solar-metric">
          <span className="feat-solar-dot" style={{ background: 'var(--feat-green)' }} />
          <span className="feat-solar-label">{t('solar.production')}</span>
          <span className="feat-solar-value">{status.productionWatts} W</span>
        </div>
        <div className="feat-solar-metric">
          <span className="feat-solar-dot" style={{ background: 'var(--feat-orange)' }} />
          <span className="feat-solar-label">Site + Miner Load</span>
          <span className="feat-solar-value">{status.consumptionWatts} W</span>
        </div>
        <div className="feat-solar-metric">
          <span className="feat-solar-dot" style={{ background: 'var(--accent)' }} />
          <span className="feat-solar-label">Miner Load</span>
          <span className="feat-solar-value">{miningWattsDisplay}</span>
        </div>
        <div className="feat-solar-metric">
          <span className="feat-solar-dot" style={{ background: status.netGridWatts > 0 ? 'var(--feat-red)' : 'var(--feat-green)' }} />
          <span className="feat-solar-label">{t('solar.netGrid')}</span>
          <span className="feat-solar-value">{status.netGridWatts} W</span>
        </div>
        <div className="feat-solar-metric">
          <span className="feat-solar-dot" style={{ background: 'var(--feat-blue)' }} />
          <span className="feat-solar-label">{t('solar.surplus')}</span>
          <span className="feat-solar-value">{status.solarSurplusWatts} W</span>
        </div>
        {status.batterySocPct !== null && (
          <div className="feat-solar-metric">
            <span className="feat-solar-dot" style={{ background: 'var(--feat-blue)' }} />
            <span className="feat-solar-label">{t('solar.batterySoc')}</span>
            <span className="feat-solar-value">{status.batterySocPct}%</span>
          </div>
        )}
      </div>

      {(miningPowerSourceLabel || status.miningWattsNote) && (
        <div style={{ marginTop: 8, fontSize: '0.72rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
          {miningPowerSourceLabel}
          {status.miningWattsNote ? ` - ${status.miningWattsNote}` : ''}
        </div>
      )}

      {config.inverterBrand !== 'manual' && (
        <div style={{ marginTop: 12, fontSize: '0.72rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
          {config.inverterBrand === 'victron' && 'Victron MQTT is live today. Positive grid watts mean import; negative watts mean export.'}
          {config.inverterBrand === 'bridge' && 'Bridge is live today through the existing Victron HTTP normalization path. Expose Victron-style production, consumption, grid, and battery fields over JSON.'}
          {config.inverterBrand === 'ecoflow' && 'EcoFlow is now a limited live path. It works against the explicit EcoFlow bridge contract over HTTP or MQTT/WS, not arbitrary direct EcoFlow APIs yet.'}
          {config.inverterBrand === 'tesla' && 'Tesla local is live as a first-pass adapter. Expect local-gateway auth/TLS quirks, and verify SoC/test results before trusting battery-floor behavior.'}
          {config.inverterBrand === 'enphase' && 'Enphase local is live as a first-pass adapter. Site load and net grid come from Envoy consumption meters when present, and battery SoC is best-effort.'}
          {config.inverterBrand === 'solaredge' && 'SolarEdge currentPowerFlow is live as a first-pass cloud adapter. Treat it as slower telemetry than local Victron/Enphase and verify its direction/sign behavior on your site.'}
        </div>
      )}

      <div className="feat-actions" style={{ marginTop: 16 }}>
        {config.inverterBrand !== 'manual' && providerLive && (
          <button type="button" className="feat-btn feat-btn-secondary" onClick={handleTest} disabled={saving || testing || !canTestProvider}>
            {testing ? 'Testing...' : 'Test Connection'}
          </button>
        )}
        <button type="button" className="feat-btn feat-btn-primary" onClick={handleSave} disabled={!canSave}>
          {saving ? 'Saving...' : t('common.save')}
        </button>
      </div>
    </div>
  );
}
