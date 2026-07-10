import React, { useCallback, useEffect, useState } from 'react';
import api from '../../api/client';
import type { WebhookConfig, WebhookFormat } from '../../api/types';
import { useMinerStore } from '../../store/miner';

interface FutureChannelConfig {
  enabled: boolean;
}

// Per-format UI metadata. The `value` matches the firmware `[webhook].format`
// contract: 'generic' | 'discord' | 'slack' | 'telegram'.
const WEBHOOK_FORMATS: ReadonlyArray<{
  value: WebhookFormat;
  label: string;
  urlLabel: string;
  urlPlaceholder: string;
  help: string;
}> = [
  {
    value: 'generic',
    label: 'Generic webhook',
    urlLabel: 'Webhook URL',
    urlPlaceholder: 'https://example.com/webhook',
    help: 'POSTs the raw JSON alert envelope to any endpoint (ntfy.sh, PagerDuty, n8n, your own server).',
  },
  {
    value: 'discord',
    label: 'Discord',
    urlLabel: 'Discord webhook URL',
    urlPlaceholder: 'https://discord.com/api/webhooks/...',
    help: 'In your Discord channel: Settings → Integrations → Webhooks → New Webhook, then paste the Webhook URL.',
  },
  {
    value: 'slack',
    label: 'Slack',
    urlLabel: 'Slack webhook URL',
    urlPlaceholder: 'https://hooks.slack.com/services/...',
    help: 'Create a Slack incoming webhook (api.slack.com/messaging/webhooks) and paste its URL.',
  },
  {
    value: 'telegram',
    label: 'Telegram',
    urlLabel: '',
    urlPlaceholder: '',
    help: 'Message @BotFather to create a bot and copy its token, add the bot to your chat/channel, then paste the numeric chat id.',
  },
];

function webhookFormatMeta(format: WebhookFormat) {
  return WEBHOOK_FORMATS.find(f => f.value === format) ?? WEBHOOK_FORMATS[0];
}

interface AlertThresholds {
  tempHigh: number;
  tempCritical: number;
  hashrateDropPct: number;
  fanFailure: boolean;
  poolDisconnect: boolean;
  hwErrorRate: number;
}

interface NotificationStorage {
  channels: {
    telegram: FutureChannelConfig;
    discord: FutureChannelConfig;
    email: FutureChannelConfig;
    webhook: {
      enabled: boolean;
      webhookUrl?: string;
    };
  };
  thresholds: AlertThresholds;
}

const DEFAULT_THRESHOLDS: AlertThresholds = {
  tempHigh: 65,
  tempCritical: 75,
  hashrateDropPct: 20,
  fanFailure: true,
  poolDisconnect: true,
  hwErrorRate: 2,
};

const DEFAULT_WEBHOOK_EVENTS = [
  'emergency_shutdown',
  'fan_failure',
  'pool_disconnected',
  'mining_stopped',
  'hashboard_offline',
  'thermal_restart',
];

const EVENT_LABELS: Record<string, string> = {
  emergency_shutdown: 'Emergency shutdown',
  fan_failure: 'Fan failure',
  pool_disconnected: 'Pool disconnected',
  mining_stopped: 'Mining stopped',
  thermal_restart: 'Thermal restart',
  hashboard_offline: 'Hash board offline',
};

function formatEventLabel(event: string): string {
  return EVENT_LABELS[event] ?? event.replace(/_/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
}

function loadNotificationStorage(): NotificationStorage {
  try {
    const raw = localStorage.getItem('dcentos-notifications');
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<NotificationStorage>;
      return {
        channels: {
          telegram: { enabled: parsed.channels?.telegram?.enabled ?? false },
          discord: { enabled: parsed.channels?.discord?.enabled ?? false },
          email: { enabled: parsed.channels?.email?.enabled ?? false },
          webhook: {
            enabled: parsed.channels?.webhook?.enabled ?? false,
            webhookUrl: parsed.channels?.webhook?.webhookUrl ?? '',
          },
        },
        thresholds: { ...DEFAULT_THRESHOLDS, ...parsed.thresholds },
      };
    }
  } catch {
    // Ignore corrupted local settings and fall back to defaults.
  }

  return {
    channels: {
      telegram: { enabled: false },
      discord: { enabled: false },
      email: { enabled: false },
      webhook: { enabled: false, webhookUrl: '' },
    },
    thresholds: DEFAULT_THRESHOLDS,
  };
}

function saveNotificationStorage(config: NotificationStorage) {
  localStorage.setItem('dcentos-notifications', JSON.stringify(config));
}

function normalizeWebhookConfig(config: Partial<WebhookConfig> | null | undefined): WebhookConfig {
  const supportedEvents = config?.supported_events?.length
    ? config.supported_events
    : DEFAULT_WEBHOOK_EVENTS;

  return {
    enabled: config?.enabled ?? false,
    url: config?.url ?? '',
    events: config?.events?.length ? config.events : supportedEvents,
    supported_events: supportedEvents,
    restart_required: config?.restart_required ?? true,
    // Default to the generic URL path so older daemons (no `format` field)
    // keep working exactly as before. Telegram fields default empty; the
    // daemon masks the bot token to "<redacted>" and treats it as
    // keep-existing on POST (same contract as the URL).
    format: config?.format ?? 'generic',
    telegram_bot_token: config?.telegram_bot_token ?? '',
    telegram_chat_id: config?.telegram_chat_id ?? '',
  };
}

export function NotificationSettings() {
  const addAlert = useMinerStore(s => s.addAlert);

  const [localConfig, setLocalConfig] = useState<NotificationStorage>(() => loadNotificationStorage());
  const [webhookConfig, setWebhookConfig] = useState<WebhookConfig>(() => {
    const stored = loadNotificationStorage();
    return normalizeWebhookConfig({
      enabled: stored.channels.webhook.enabled,
      url: stored.channels.webhook.webhookUrl ?? '',
      events: DEFAULT_WEBHOOK_EVENTS,
      supported_events: DEFAULT_WEBHOOK_EVENTS,
      restart_required: true,
    });
  });
  const [loadingWebhook, setLoadingWebhook] = useState(true);
  const [savingWebhook, setSavingWebhook] = useState(false);
  const [testingWebhook, setTestingWebhook] = useState(false);
  const [testResult, setTestResult] = useState<'ok' | 'fail' | null>(null);

  const syncWebhookToLocalStorage = useCallback((config: WebhookConfig) => {
    setLocalConfig(prev => {
      const next = {
        ...prev,
        channels: {
          ...prev.channels,
          webhook: {
            enabled: config.enabled,
            webhookUrl: config.url,
          },
        },
      };
      saveNotificationStorage(next);
      return next;
    });
  }, []);

  useEffect(() => {
    let cancelled = false;

    async function loadWebhook() {
      try {
        const config = normalizeWebhookConfig(await api.getWebhookConfig());
        if (cancelled) return;
        setWebhookConfig(config);
        syncWebhookToLocalStorage(config);
      } catch (error) {
        if (cancelled) return;
        addAlert('warning', `Failed to load webhook settings: ${error instanceof Error ? error.message : 'Unknown error'}`);
      } finally {
        if (!cancelled) {
          setLoadingWebhook(false);
        }
      }
    }

    void loadWebhook();

    return () => {
      cancelled = true;
    };
  }, [addAlert, syncWebhookToLocalStorage]);

  const updateThreshold = useCallback((updates: Partial<AlertThresholds>) => {
    setLocalConfig(prev => {
      const next = {
        ...prev,
        thresholds: { ...prev.thresholds, ...updates },
      };
      saveNotificationStorage(next);
      return next;
    });
  }, []);

  const updateWebhook = useCallback((updates: Partial<WebhookConfig>) => {
    setWebhookConfig(prev => normalizeWebhookConfig({ ...prev, ...updates }));
    setTestResult(null);
  }, []);

  const toggleWebhookEvent = useCallback((event: string, checked: boolean) => {
    setWebhookConfig(prev => {
      const nextEvents = checked
        ? Array.from(new Set([...prev.events, event]))
        : prev.events.filter(item => item !== event);
      return normalizeWebhookConfig({ ...prev, events: nextEvents });
    });
    setTestResult(null);
  }, []);

  const saveWebhook = useCallback(async () => {
    setSavingWebhook(true);
    try {
      const response = await api.updateWebhookConfig({
        enabled: webhookConfig.enabled,
        url: webhookConfig.url,
        events: webhookConfig.events,
        format: webhookConfig.format,
        telegram_bot_token: webhookConfig.telegram_bot_token,
        telegram_chat_id: webhookConfig.telegram_chat_id,
      });
      const normalized = normalizeWebhookConfig(response.config);
      setWebhookConfig(normalized);
      syncWebhookToLocalStorage(normalized);
      addAlert('info', response.message);
    } catch (error) {
      addAlert('warning', `Failed to save webhook settings: ${error instanceof Error ? error.message : 'Unknown error'}`);
    } finally {
      setSavingWebhook(false);
    }
  }, [addAlert, syncWebhookToLocalStorage, webhookConfig]);

  const testWebhook = useCallback(async () => {
    setTestingWebhook(true);
    setTestResult(null);

    try {
      const response = await api.testWebhookConfig({
        enabled: webhookConfig.enabled,
        url: webhookConfig.url,
        events: webhookConfig.events,
        format: webhookConfig.format,
        telegram_bot_token: webhookConfig.telegram_bot_token,
        telegram_chat_id: webhookConfig.telegram_chat_id,
      });
      setTestResult('ok');
      addAlert('info', response.message);
    } catch (error) {
      setTestResult('fail');
      addAlert('warning', `Webhook test failed: ${error instanceof Error ? error.message : 'Unknown error'}`);
    } finally {
      setTestingWebhook(false);
    }
  }, [addAlert, webhookConfig]);

  const activeFormat: WebhookFormat = webhookConfig.format ?? 'generic';
  const formatMeta = webhookFormatMeta(activeFormat);
  const isTelegram = activeFormat === 'telegram';
  const canTestWebhook = isTelegram
    ? !!((webhookConfig.telegram_bot_token ?? '').trim() && (webhookConfig.telegram_chat_id ?? '').trim())
    : !!webhookConfig.url.trim();

  return (
    <div>
      <div className="section-title">Notification Channels</div>

      <div className="page-surface" style={{ marginBottom: 12 }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 12, gap: 12, flexWrap: 'wrap' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
            <label className="control-option">
              <input
                type="checkbox"
                checked={webhookConfig.enabled}
                onChange={e => updateWebhook({ enabled: e.target.checked })}
                disabled={loadingWebhook}
              />
              <span style={{ fontWeight: 600 }}>Alert notifications</span>
            </label>
            <span className="control-option-copy">Webhook, Discord, Slack, or Telegram delivery</span>
          </div>
          <div className="standard-inline-actions">
            {testResult && (
              <span className={`status-inline ${testResult === 'ok' ? 'success' : 'danger'}`} role="status">
                {testResult === 'ok' ? 'OK' : 'Failed'}
              </span>
            )}
            <button
              className="btn btn-secondary btn-compact"
              disabled={loadingWebhook || savingWebhook || testingWebhook}
              onClick={() => void saveWebhook()}
            >
              {savingWebhook ? 'Saving...' : 'Save'}
            </button>
            <button
              className="btn btn-secondary btn-compact"
              disabled={loadingWebhook || testingWebhook || !canTestWebhook}
              onClick={() => void testWebhook()}
            >
              {testingWebhook ? 'Testing...' : 'Test'}
            </button>
          </div>
        </div>

        <div>
          <label className="field-label" htmlFor="webhook-format">Channel</label>
          <select
            id="webhook-format"
            value={activeFormat}
            onChange={e => updateWebhook({ format: e.target.value as WebhookFormat })}
            disabled={loadingWebhook}
          >
            {WEBHOOK_FORMATS.map(f => (
              <option key={f.value} value={f.value}>{f.label}</option>
            ))}
          </select>
        </div>

        {isTelegram ? (
          <>
            <div style={{ marginTop: 12 }}>
              <label className="field-label" htmlFor="webhook-telegram-token">Telegram bot token</label>
              <input
                id="webhook-telegram-token"
                type="password"
                autoComplete="off"
                value={webhookConfig.telegram_bot_token ?? ''}
                onChange={e => updateWebhook({ telegram_bot_token: e.target.value })}
                placeholder="123456789:ABCdefGhIJKlmNoPQRsTUVwxyz"
                disabled={loadingWebhook}
              />
              <div style={{ marginTop: 4, fontSize: '0.72rem', color: 'var(--text-dim)' }}>
                Stored on the daemon and masked in this form. Leave the masked value as-is to keep the existing token.
              </div>
            </div>
            <div style={{ marginTop: 12 }}>
              <label className="field-label" htmlFor="webhook-telegram-chat">Telegram chat id</label>
              <input
                id="webhook-telegram-chat"
                type="text"
                value={webhookConfig.telegram_chat_id ?? ''}
                onChange={e => updateWebhook({ telegram_chat_id: e.target.value })}
                placeholder="-1001234567890"
                disabled={loadingWebhook}
              />
            </div>
          </>
        ) : (
          <div style={{ marginTop: 12 }}>
            <label className="field-label" htmlFor="webhook-url">{formatMeta.urlLabel}</label>
            <input
              id="webhook-url"
              type="url"
              value={webhookConfig.url}
              onChange={e => updateWebhook({ url: e.target.value })}
              placeholder={formatMeta.urlPlaceholder}
              disabled={loadingWebhook}
            />
          </div>
        )}

        <div style={{ marginTop: 8, fontSize: '0.78rem', color: 'var(--text-dim)' }} role="note">
          {formatMeta.help}
        </div>

        <div style={{ marginTop: 12 }}>
          <div className="field-label" style={{ marginBottom: 8 }}>Events</div>
          <fieldset className="control-option-grid" style={{ border: 0, margin: 0, padding: 0 }}>
            <legend className="sr-only">Webhook events</legend>
            {webhookConfig.supported_events.map(event => (
              <label key={event} className="control-option">
                <input
                  type="checkbox"
                  checked={webhookConfig.events.includes(event)}
                  onChange={e => toggleWebhookEvent(event, e.target.checked)}
                  disabled={loadingWebhook}
                />
                {formatEventLabel(event)}
              </label>
            ))}
          </fieldset>
        </div>

        <div style={{ marginTop: 12, fontSize: '0.8rem', color: 'var(--text-dim)' }}>
          Saves to `dcentrald.toml`. Test delivery happens immediately. Live alert delivery uses the saved config after a daemon restart.
        </div>
      </div>

      <div className="page-surface" style={{ marginBottom: 12 }}>
        <div style={{ fontWeight: 600, marginBottom: 6 }}>Email</div>
        <div style={{ fontSize: '0.85rem', color: 'var(--text-dim)' }}>
          Discord, Slack, and Telegram are now live channels — select them in the Channel dropdown above.
          Direct email (SMTP) delivery is in development; to get alerts by email today, point the generic
          webhook at an email-relay service (for example a Zapier/IFTTT/n8n webhook-to-email step).
        </div>
      </div>

      <div className="section-title" style={{ marginTop: 20 }}>Alert Thresholds</div>
      <div className="page-surface">
        {/* STD-B-01 honest-state: these thresholds persist to localStorage only
            and are not yet consumed by the daemon or any client-side alert
            engine. Do NOT imply they change what triggers alerts today. */}
        <div style={{ marginBottom: 14 }}>
          <div style={{ fontWeight: 600, marginBottom: 4 }}>
            Browser-stored preferences — not yet enforced by the daemon
          </div>
          <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)' }}>
            These thresholds are saved in this browser only. They do not change what triggers
            alerts today — daemon-side enforcement is planned for a later release. For live
            alert delivery now, use the webhook channel above.
          </div>
        </div>
        <div className="surface-grid-2">
          <div>
            <label className="field-label" htmlFor="notification-temp-high">
              High Temperature Warning (C)
            </label>
            <input
              id="notification-temp-high"
              type="number"
              value={localConfig.thresholds.tempHigh}
              onChange={e => updateThreshold({ tempHigh: Number(e.target.value) })}
              min={40}
              max={80}
            />
          </div>
          <div>
            <label className="field-label" htmlFor="notification-temp-critical">
              Critical Temperature (C)
            </label>
            <input
              id="notification-temp-critical"
              type="number"
              value={localConfig.thresholds.tempCritical}
              onChange={e => updateThreshold({ tempCritical: Number(e.target.value) })}
              min={50}
              max={90}
            />
          </div>
          <div>
            <label className="field-label" htmlFor="notification-hashrate-drop">
              Hashrate Drop Alert (%)
            </label>
            <input
              id="notification-hashrate-drop"
              type="number"
              value={localConfig.thresholds.hashrateDropPct}
              onChange={e => updateThreshold({ hashrateDropPct: Number(e.target.value) })}
              min={5}
              max={80}
              step={5}
            />
          </div>
          <div>
            <label className="field-label" htmlFor="notification-hw-error-rate">
              HW Error Rate Alert (%)
            </label>
            <input
              id="notification-hw-error-rate"
              type="number"
              value={localConfig.thresholds.hwErrorRate}
              onChange={e => updateThreshold({ hwErrorRate: Number(e.target.value) })}
              min={0.5}
              max={10}
              step={0.5}
            />
          </div>
        </div>
        <div style={{ display: 'flex', gap: 20, marginTop: 12, paddingTop: 12, borderTop: '1px solid var(--border)', flexWrap: 'wrap' }}>
          <label className="control-option">
            <input
              type="checkbox"
              checked={localConfig.thresholds.fanFailure}
              onChange={e => updateThreshold({ fanFailure: e.target.checked })}
            />
            Fan failure alert
          </label>
          <label className="control-option">
            <input
              type="checkbox"
              checked={localConfig.thresholds.poolDisconnect}
              onChange={e => updateThreshold({ poolDisconnect: e.target.checked })}
            />
            Pool disconnect alert
          </label>
        </div>
      </div>
    </div>
  );
}
