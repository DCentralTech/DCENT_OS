import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type {
  HaEntity,
  MqttConfig as MqttConfigType,
  MqttStatusResponse,
  MqttTestResponse,
} from '../../api/feature-types';
import { useTranslation } from '../../i18n/i18n';
import { useMinerStore } from '../../store/miner';
import { InfoDot } from '../common/Tooltip';

// Always-published read-only entities. This list mirrors EXACTLY what
// `build_ha_discovery_entities` emits in dcentrald-api/src/mqtt.rs (9 sensors +
// 2 binary sensors) \u2014 including the `uptime` sensor that was previously missing
// from this preview. Do not add entities the firmware doesn't actually publish.
const DEFAULT_HA_ENTITIES: HaEntity[] = [
  { entityId: 'homeassistant/sensor/dcentrald_<miner>/hashrate/config', name: 'Hashrate', type: 'sensor', unit: 'TH/s', icon: 'mdi:pickaxe' },
  { entityId: 'homeassistant/sensor/dcentrald_<miner>/temperature/config', name: 'Temperature', type: 'sensor', unit: '\u00B0C', icon: 'mdi:thermometer' },
  { entityId: 'homeassistant/sensor/dcentrald_<miner>/power/config', name: 'Power', type: 'sensor', unit: 'W', icon: 'mdi:flash' },
  { entityId: 'homeassistant/sensor/dcentrald_<miner>/btu/config', name: 'BTU/h', type: 'sensor', unit: 'BTU/h', icon: 'mdi:fire' },
  { entityId: 'homeassistant/sensor/dcentrald_<miner>/fan_rpm/config', name: 'Fan RPM', type: 'sensor', unit: 'RPM', icon: 'mdi:fan' },
  { entityId: 'homeassistant/sensor/dcentrald_<miner>/efficiency/config', name: 'Efficiency', type: 'sensor', unit: 'J/TH', icon: 'mdi:lightning-bolt' },
  { entityId: 'homeassistant/sensor/dcentrald_<miner>/accepted/config', name: 'Accepted Shares', type: 'sensor', unit: '', icon: 'mdi:check-circle' },
  { entityId: 'homeassistant/sensor/dcentrald_<miner>/rejected/config', name: 'Rejected Shares', type: 'sensor', unit: '', icon: 'mdi:close-circle' },
  { entityId: 'homeassistant/sensor/dcentrald_<miner>/uptime/config', name: 'Uptime', type: 'sensor', unit: 's', icon: 'mdi:clock-outline' },
  { entityId: 'homeassistant/binary_sensor/dcentrald_<miner>/mining/config', name: 'Mining Active', type: 'binary_sensor', unit: '', icon: 'mdi:pickaxe' },
  { entityId: 'homeassistant/binary_sensor/dcentrald_<miner>/pool/config', name: 'Pool Connected', type: 'binary_sensor', unit: '', icon: 'mdi:lan-connect' },
];

// Writable command entities. The firmware only advertises these when the
// optional discovery+commands subscribe path is active (a validated-setter
// sink is wired) \u2014 so they are listed separately and clearly marked
// conditional, to keep the preview honest. See `commands_enabled` in
// build_ha_discovery_entities (mqtt.rs).
const COMMAND_HA_ENTITIES: HaEntity[] = [
  { entityId: 'homeassistant/number/dcentrald_<miner>/fan_pwm_set/config', name: 'Fan PWM', type: 'sensor', unit: '%', icon: 'mdi:fan' },
  { entityId: 'homeassistant/number/dcentrald_<miner>/target_watts_set/config', name: 'Target Power', type: 'sensor', unit: 'W', icon: 'mdi:flash' },
  { entityId: 'homeassistant/climate/dcentrald_<miner>/heater/config', name: 'Space Heater', type: 'sensor', unit: '\u00B0C', icon: 'mdi:radiator' },
];

const DEFAULT_CONFIG: MqttConfigType = {
  enabled: false,
  broker: 'mqtt://localhost:1883',
  topicPrefix: 'dcentrald',
  discovery: true,
  username: '',
  password: '',
  publishIntervalS: 5,
  restartRequired: true,
  runtimeMessage:
    'MQTT settings are saved to dcentrald.toml immediately. The running daemon picks up broker changes after restart.',
};

type BrokerParts = {
  host: string;
  port: number;
  tls: boolean;
};

function parseBroker(broker: string): BrokerParts {
  try {
    const parsed = new URL(broker.includes('://') ? broker : `mqtt://${broker}`);
    const tls = parsed.protocol === 'mqtts:';
    return {
      host: parsed.hostname,
      port: Number(parsed.port || (tls ? 8883 : 1883)),
      tls,
    };
  } catch {
    return { host: '', port: 1883, tls: false };
  }
}

// Resolve the publisher's last-publish age in seconds from whatever the daemon
// reported: an explicit `last_publish_age_s`, or derived from a `last_publish_ms`
// epoch timestamp. Returns null when neither is present (→ honest "—").
export function resolveLastPublishAgeS(
  status: Pick<MqttStatusResponse, 'last_publish_age_s' | 'last_publish_ms'> | null | undefined,
  nowMs: number = Date.now(),
): number | null {
  if (!status) return null;
  if (typeof status.last_publish_age_s === 'number' && Number.isFinite(status.last_publish_age_s)) {
    return Math.max(0, status.last_publish_age_s);
  }
  if (typeof status.last_publish_ms === 'number' && Number.isFinite(status.last_publish_ms) && status.last_publish_ms > 0) {
    return Math.max(0, (nowMs - status.last_publish_ms) / 1000);
  }
  return null;
}

// Human-readable relative age. `null` → em-dash (daemon didn't report).
export function formatPublishAge(ageS: number | null): string {
  if (ageS == null) return '—';
  if (ageS < 1) return 'just now';
  if (ageS < 60) return `${Math.round(ageS)}s ago`;
  if (ageS < 3600) return `${Math.round(ageS / 60)}m ago`;
  return `${Math.round(ageS / 3600)}h ago`;
}

export function MqttConfig() {
  const { t } = useTranslation();
  const addToast = useMinerStore(s => s.addToast);
  const [config, setConfig] = useState<MqttConfigType>(DEFAULT_CONFIG);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<MqttTestResponse | null>(null);

  // Live publisher health (GET /api/mqtt/status). `status === null` +
  // `statusUnavailable` means the daemon build doesn't expose the route (an
  // honest "unavailable", NOT "disconnected"). `statusError` is a real fetch
  // failure. Never fabricate a connection.
  const [status, setStatus] = useState<MqttStatusResponse | null>(null);
  const [statusLoading, setStatusLoading] = useState(true);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [statusUnavailable, setStatusUnavailable] = useState(false);

  const refreshStatus = useCallback(async () => {
    setStatusLoading(true);
    setStatusError(null);
    try {
      const result = await api.getMqttStatus();
      setStatus(result);
      setStatusUnavailable(result === null);
    } catch (err) {
      setStatus(null);
      setStatusUnavailable(false);
      setStatusError(err instanceof Error ? err.message : 'Unknown error');
    } finally {
      setStatusLoading(false);
    }
  }, []);

  useEffect(() => {
    void refreshStatus();
  }, [refreshStatus]);

  useEffect(() => {
    let cancelled = false;

    api.getMqttConfig()
      .then(savedConfig => {
        if (!cancelled) {
          setConfig({ ...DEFAULT_CONFIG, ...savedConfig });
        }
      })
      .catch(() => {
        if (!cancelled) {
          addToast('Could not load MQTT configuration', 'error');
        }
      })
      .finally(() => {
        if (!cancelled) {
          setLoading(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [addToast]);

  const brokerParts = useMemo(() => parseBroker(config.broker), [config.broker]);

  const update = (partial: Partial<MqttConfigType>) => {
    setTestResult(null);
    setConfig(prev => ({ ...prev, ...partial }));
  };

  const setBroker = (partial: Partial<BrokerParts>) => {
    const next = { ...brokerParts, ...partial };
    const protocol = next.tls ? 'mqtts' : 'mqtt';
    const defaultPort = next.tls ? 8883 : 1883;
    const broker = next.host.trim()
      ? `${protocol}://${next.host.trim()}:${next.port || defaultPort}`
      : '';
    update({ broker });
  };

  const handleTest = async () => {
    setTesting(true);
    setTestResult(null);
    try {
      const result = await api.testMqttConfig(config);
      setTestResult(result);
      addToast(result.message, result.ok ? 'success' : 'warning');
    } catch {
      addToast('Failed to test MQTT connection', 'error');
    } finally {
      setTesting(false);
    }
  };

  const handleSave = async () => {
    setSaving(true);
    try {
      const response = await api.updateMqttConfig(config);
      if (response.status !== 'ok') {
        addToast(response.message || 'Failed to save MQTT configuration', 'error');
      } else {
        setConfig({ ...DEFAULT_CONFIG, ...(response.config ?? config) });
        addToast(response.message, 'success');
        // The publisher restarts within a few seconds of a config change;
        // refresh the health snapshot so the card reflects the new state.
        void refreshStatus();
      }
    } catch {
      addToast('Failed to save MQTT configuration', 'error');
    } finally {
      setSaving(false);
    }
  };

  const statusText = testResult
    ? testResult.connected
      ? t('common.connected')
      : t('common.disconnected')
    : config.enabled
      ? 'Configured'
      : 'Disabled';

  const statusColor = testResult
    ? testResult.connected
      ? 'var(--feat-green)'
      : 'var(--feat-red)'
    : config.enabled
      ? 'var(--feat-orange)'
      : 'var(--feat-red)';

  // Live publisher-health derived values (honest: never invents a connection).
  const publisherEnabled = status?.enabled ?? false;
  const publisherConnected = status?.connected ?? false;
  const publisherStatusText = !publisherEnabled
    ? 'Publisher disabled'
    : publisherConnected
      ? t('common.connected')
      : 'Not connected';
  const publisherDotColor = !publisherEnabled
    ? 'var(--feat-red)'
    : publisherConnected
      ? 'var(--feat-green)'
      : 'var(--feat-orange)';
  const lastPublishText = formatPublishAge(resolveLastPublishAgeS(status));
  const entityCountText =
    status && typeof status.entity_count === 'number' ? String(status.entity_count) : '—';
  const publisherBrokerText = status?.broker && status.broker.trim() ? status.broker : '—';

  const mqttEnabledLabelId = 'mqtt-enabled-label';
  const mqttHostId = 'mqtt-host';
  const mqttPortId = 'mqtt-port';
  const mqttUsernameId = 'mqtt-username';
  const mqttPasswordId = 'mqtt-password';
  const mqttTopicId = 'mqtt-topic-prefix';
  const mqttPublishIntervalId = 'mqtt-publish-interval';
  const mqttTlsLabelId = 'mqtt-tls-label';
  const mqttDiscoveryLabelId = 'mqtt-discovery-label';

  return (
    <div className="feat-page">
      <div className="feat-header">
        <h2 className="feat-title">
          {t('mqtt.title')}
          <InfoDot
            placement="bottom"
            label="What MQTT / Home Assistant discovery does"
            content={
              <>
                Publishes this miner's telemetry (hashrate, temp, power, shares,
                BTU/h) to your MQTT broker so Home Assistant auto-creates sensors
                without hand-rolled YAML. Power, BTU/h, and efficiency sensors
                require live wall-power telemetry before they report values.
              </>
            }
          />
        </h2>
        <p className="feat-subtitle">{t('mqtt.subtitle')}</p>
      </div>

      <div className="feat-card">
        <label className="feat-toggle-row">
          <span className="feat-toggle-label" id={mqttEnabledLabelId}>{t('common.enabled')}</span>
          <button
            type="button"
            role="switch"
            aria-checked={config.enabled}
            aria-labelledby={mqttEnabledLabelId}
            className={`feat-toggle ${config.enabled ? 'active' : ''}`}
            onClick={() => update({ enabled: !config.enabled })}
          >
            <span className="feat-toggle-knob" />
          </button>
        </label>

        {!loading && (
          <>
            <div className="feat-status-pill" style={{ marginTop: 12 }} role="status" aria-live="polite">
              <span className="feat-status-dot" style={{ background: statusColor }} />
              {statusText}
            </div>
            <div style={{ marginTop: 12, fontSize: '0.75rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
              {config.runtimeMessage}
            </div>
          </>
        )}
      </div>

      {/* Live publisher health — read-only observability of GET /api/mqtt/status.
          Distinguishes loading / error / unavailable / live so nothing is
          fabricated when the daemon doesn't report. */}
      <div className="feat-card" role="status" aria-live="polite" data-testid="mqtt-publisher-health">
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, flexWrap: 'wrap' }}>
          <h3 className="feat-card-title" style={{ margin: 0 }}>Publisher Health</h3>
          <button
            type="button"
            className="feat-btn feat-btn-secondary"
            onClick={() => void refreshStatus()}
            disabled={statusLoading}
          >
            {statusLoading ? t('common.loading') : 'Refresh'}
          </button>
        </div>

        {statusLoading ? (
          <div style={{ marginTop: 12, fontSize: '0.8rem', color: 'var(--text-dim)' }}>
            Checking publisher…
          </div>
        ) : statusError ? (
          <div style={{ marginTop: 12, fontSize: '0.8rem', color: 'var(--feat-red)' }} role="alert">
            Could not read publisher health: {statusError}
          </div>
        ) : statusUnavailable || !status ? (
          <div style={{ marginTop: 12, fontSize: '0.8rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
            Live publisher health is unavailable — this daemon build does not report it.
            Use <span className="mono">Test Connection</span> above to verify the broker.
          </div>
        ) : (
          <>
            <div className="feat-status-pill" style={{ marginTop: 12 }}>
              <span className="feat-status-dot" style={{ background: publisherDotColor }} />
              {publisherStatusText}
            </div>
            <div className="feat-form-grid" style={{ marginTop: 12 }}>
              <div className="feat-input-group">
                <span className="feat-label">Last publish</span>
                <span style={{ fontSize: '0.85rem' }}>{lastPublishText}</span>
              </div>
              <div className="feat-input-group">
                <span className="feat-label">Entities published</span>
                <span style={{ fontSize: '0.85rem' }}>{entityCountText}</span>
              </div>
              <div className="feat-input-group feat-span-2">
                <span className="feat-label">Broker</span>
                <span className="mono" style={{ fontSize: '0.8rem' }}>{publisherBrokerText}</span>
              </div>
            </div>
            {status.commands_enabled != null && (
              <div style={{ marginTop: 10, fontSize: '0.72rem', color: 'var(--text-dim)' }}>
                Writable command entities: {status.commands_enabled ? 'advertised' : 'not advertised (read-only)'}.
              </div>
            )}
            {status.error && (
              <div style={{ marginTop: 10, fontSize: '0.72rem', color: 'var(--feat-orange)' }}>
                Last publisher error: {status.error}
              </div>
            )}
          </>
        )}
      </div>

      {loading ? (
        <div className="feat-card">{t('common.loading')}</div>
      ) : (
        <>
          <div className="feat-card">
            <h3 className="feat-card-title">Broker Settings</h3>
            <div className="feat-form-grid">
              <div className="feat-input-group feat-span-2">
                <label className="feat-label" htmlFor={mqttHostId}>{t('mqtt.host')}</label>
                <input
                  id={mqttHostId}
                  type="text"
                  value={brokerParts.host}
                  onChange={e => setBroker({ host: e.target.value })}
                  placeholder="203.0.113.100 or mqtt.example.com"
                  className="feat-input"
                />
              </div>

              <div className="feat-input-group">
                <label className="feat-label" htmlFor={mqttPortId}>{t('mqtt.port')}</label>
                <input
                  id={mqttPortId}
                  type="number"
                  min="1"
                  max="65535"
                  value={brokerParts.port}
                  onChange={e => setBroker({ port: Number(e.target.value) })}
                  className="feat-input"
                />
              </div>

              <div className="feat-input-group">
                <label className="feat-label" htmlFor={mqttUsernameId}>{t('mqtt.username')}</label>
                <input
                  id={mqttUsernameId}
                  type="text"
                  value={config.username}
                  onChange={e => update({ username: e.target.value })}
                  placeholder="Optional"
                  className="feat-input"
                />
              </div>

              <div className="feat-input-group">
                <label className="feat-label" htmlFor={mqttPasswordId}>{t('mqtt.password')}</label>
                <input
                  id={mqttPasswordId}
                  type="password"
                  value={config.password}
                  onChange={e => update({ password: e.target.value })}
                  placeholder="Optional"
                  className="feat-input"
                />
              </div>

              <div className="feat-input-group">
                <label className="feat-label" htmlFor={mqttTopicId}>{t('mqtt.baseTopic')}</label>
                <input
                  id={mqttTopicId}
                  type="text"
                  value={config.topicPrefix}
                  onChange={e => update({ topicPrefix: e.target.value })}
                  placeholder="dcentrald"
                  className="feat-input"
                />
              </div>

              <div className="feat-input-group">
                <label className="feat-label" htmlFor={mqttPublishIntervalId}>Publish Interval (s)</label>
                <input
                  id={mqttPublishIntervalId}
                  type="number"
                  min="1"
                  max="3600"
                  value={config.publishIntervalS}
                  onChange={e => update({ publishIntervalS: Number(e.target.value) })}
                  className="feat-input"
                />
              </div>
            </div>

            <div className="feat-row" style={{ marginTop: 16 }}>
              <label className="feat-toggle-row" style={{ flex: 1 }}>
                <span className="feat-toggle-label" id={mqttTlsLabelId}>{t('mqtt.tls')}</span>
                <button
                  type="button"
                  role="switch"
                  aria-checked={brokerParts.tls}
                  aria-labelledby={mqttTlsLabelId}
                  className={`feat-toggle ${brokerParts.tls ? 'active' : ''}`}
                  onClick={() => setBroker({ tls: !brokerParts.tls, port: brokerParts.tls ? 1883 : 8883 })}
                >
                  <span className="feat-toggle-knob" />
                </button>
              </label>

              <button
                type="button"
                className="feat-btn feat-btn-secondary"
                onClick={handleTest}
                disabled={testing || saving}
              >
                {testing ? t('common.loading') : t('mqtt.testConnection')}
              </button>
            </div>
          </div>

          <div className="feat-card">
            <label className="feat-toggle-row" style={{ marginBottom: 16 }}>
              <span className="feat-toggle-label" id={mqttDiscoveryLabelId}>{t('mqtt.haDiscovery')}</span>
              <button
                type="button"
                role="switch"
                aria-checked={config.discovery}
                aria-labelledby={mqttDiscoveryLabelId}
                className={`feat-toggle ${config.discovery ? 'active' : ''}`}
                onClick={() => update({ discovery: !config.discovery })}
              >
                <span className="feat-toggle-knob" />
              </button>
            </label>

            {config.discovery && (
              <>
                <h4 className="feat-card-subtitle">{t('mqtt.entityPreview')}</h4>
                <div className="feat-entity-list">
                  {DEFAULT_HA_ENTITIES.map(entity => (
                    <div key={entity.entityId} className="feat-entity-row">
                      <span className="feat-entity-type">{entity.type}</span>
                      <span className="feat-entity-name">{entity.name}</span>
                      <span className="feat-entity-id mono">{entity.entityId}</span>
                      {entity.unit && <span className="feat-entity-unit">{entity.unit}</span>}
                    </div>
                  ))}
                </div>

                <h4 className="feat-card-subtitle" style={{ marginTop: 16 }}>Writable controls (conditional)</h4>
                <div style={{ marginBottom: 8, fontSize: '0.72rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
                  These command entities are only advertised when the optional discovery+commands path is enabled on
                  the daemon. Until then they do not appear in Home Assistant. Fan PWM stays clamped to the home safety cap.
                </div>
                <div className="feat-entity-list">
                  {COMMAND_HA_ENTITIES.map(entity => (
                    <div key={entity.entityId} className="feat-entity-row">
                      <span className="feat-entity-type">command</span>
                      <span className="feat-entity-name">{entity.name}</span>
                      <span className="feat-entity-id mono">{entity.entityId}</span>
                      {entity.unit && <span className="feat-entity-unit">{entity.unit}</span>}
                    </div>
                  ))}
                </div>

                <div style={{ marginTop: 12, fontSize: '0.72rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
                  Runtime publishes miner state to <span className="mono">{config.topicPrefix || 'dcentrald'}/state</span> and availability to <span className="mono">{config.topicPrefix || 'dcentrald'}/availability</span>.
                  Power, BTU/h, and Efficiency sensors stay unavailable until live wall-power telemetry is present.
                </div>
              </>
            )}
          </div>

          {testResult && (
            <div className="feat-card" style={{ fontSize: '0.78rem', lineHeight: 1.5 }} role="status" aria-live="polite">
              <div>{testResult.message}</div>
              {testResult.state_topic && <div style={{ marginTop: 8 }}>State topic: <span className="mono">{testResult.state_topic}</span></div>}
              {testResult.availability_topic && <div>Availability topic: <span className="mono">{testResult.availability_topic}</span></div>}
              {testResult.client_id && <div>Test client ID: <span className="mono">{testResult.client_id}</span></div>}
            </div>
          )}

          <div className="feat-actions">
            <button type="button" className="feat-btn feat-btn-primary" onClick={handleSave} disabled={saving || testing}>
              {saving ? 'Saving...' : t('common.save')}
            </button>
          </div>
        </>
      )}
    </div>
  );
}
