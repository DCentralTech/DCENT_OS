// @vitest-environment jsdom
//
// Honest-observability regression: the MQTT page surfaces LIVE publisher health
// from GET /api/mqtt/status. It must (a) show an honest "unavailable" state when
// the daemon build doesn't expose the route (null), never a fabricated
// connection; (b) render real publisher metrics (last-publish age + entity
// count) when the daemon reports them. Also covers the pure age helpers and the
// reconciled HA entity preview (the previously-missing Uptime sensor).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';

const getMqttConfig = vi.fn();
const updateMqttConfig = vi.fn();
const testMqttConfig = vi.fn();
const getMqttStatus = vi.fn();

vi.mock('../../api/client', () => {
  const api = {
    getMqttConfig: (...args: unknown[]) => getMqttConfig(...args),
    updateMqttConfig: (...args: unknown[]) => updateMqttConfig(...args),
    testMqttConfig: (...args: unknown[]) => testMqttConfig(...args),
    getMqttStatus: (...args: unknown[]) => getMqttStatus(...args),
  };
  return { api, default: api };
});

// Stable across renders — the config-load effect depends on `addToast`, so a
// fresh fn each render would re-fire the effect in a loop.
vi.mock('../../store/miner', () => {
  const addToast = vi.fn();
  return {
    useMinerStore: (selector: (s: { addToast: () => void }) => unknown) =>
      selector({ addToast }),
  };
});

vi.mock('../../i18n/i18n', () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

vi.mock('../common/Tooltip', () => ({
  InfoDot: () => null,
}));

import { MqttConfig, resolveLastPublishAgeS, formatPublishAge } from './MqttConfig';

const baseConfig = {
  enabled: true,
  broker: 'mqtt://broker.local:1883',
  topicPrefix: 'dcentrald',
  discovery: true,
  username: '',
  password: '',
  publishIntervalS: 5,
  restartRequired: false,
  runtimeMessage: 'runtime',
};

beforeEach(() => {
  vi.clearAllMocks();
  getMqttConfig.mockResolvedValue({ ...baseConfig });
});

afterEach(() => cleanup());

describe('MqttConfig — live publisher health card', () => {
  it('shows an honest "unavailable" state when the daemon does not report status (null)', async () => {
    getMqttStatus.mockResolvedValue(null);
    render(<MqttConfig />);

    await waitFor(() => expect(getMqttStatus).toHaveBeenCalled());
    expect(await screen.findByText(/Live publisher health is unavailable/i)).toBeTruthy();
    // Must NOT fabricate a connected publisher.
    expect(screen.queryByText('Last publish')).toBeNull();
  });

  it('renders real publisher metrics (last-publish age + entity count) when reported', async () => {
    getMqttStatus.mockResolvedValue({
      enabled: true,
      connected: true,
      broker: 'broker.local:1883',
      discovery: true,
      commands_enabled: false,
      entity_count: 11,
      last_publish_age_s: 5,
      publish_count: 42,
      error: null,
    });
    render(<MqttConfig />);

    await waitFor(() => expect(getMqttStatus).toHaveBeenCalled());
    expect(await screen.findByText('Last publish')).toBeTruthy();
    expect(screen.getByText('5s ago')).toBeTruthy();
    expect(screen.getByText('Entities published')).toBeTruthy();
    expect(screen.getByText('11')).toBeTruthy();
    expect(screen.getByText('broker.local:1883')).toBeTruthy();
  });

  it('surfaces a real fetch error instead of pretending the publisher is fine', async () => {
    getMqttStatus.mockRejectedValue(new Error('boom'));
    render(<MqttConfig />);

    await waitFor(() => expect(getMqttStatus).toHaveBeenCalled());
    const alert = await screen.findByRole('alert');
    expect(alert.textContent ?? '').toMatch(/could not read publisher health/i);
  });

  it('lists the Uptime sensor in the HA discovery entity preview', async () => {
    getMqttStatus.mockResolvedValue(null);
    render(<MqttConfig />);

    await waitFor(() => expect(getMqttConfig).toHaveBeenCalled());
    // Reconciled with build_ha_discovery_entities — Uptime was previously missing.
    expect(await screen.findByText('Uptime')).toBeTruthy();
    expect(screen.getByText(/Power, BTU\/h, and Efficiency sensors stay unavailable until live wall-power telemetry is present/i)).toBeTruthy();
    // The 3 writable command entities are clearly marked conditional.
    expect(screen.getByText(/only advertised when the optional discovery\+commands path is enabled/i)).toBeTruthy();
  });
});

describe('MqttConfig — publish-age helpers', () => {
  it('resolves age from last_publish_age_s when present', () => {
    expect(resolveLastPublishAgeS({ last_publish_age_s: 30 })).toBe(30);
  });

  it('derives age from last_publish_ms when no explicit age is given', () => {
    const now = 1_000_000_000_000;
    expect(resolveLastPublishAgeS({ last_publish_ms: now - 12_000 }, now)).toBe(12);
  });

  it('returns null when neither field is present (→ honest em-dash)', () => {
    expect(resolveLastPublishAgeS({})).toBeNull();
    expect(resolveLastPublishAgeS(null)).toBeNull();
    expect(formatPublishAge(null)).toBe('—');
  });

  it('formats ages into human-readable relative buckets', () => {
    expect(formatPublishAge(0.4)).toBe('just now');
    expect(formatPublishAge(8)).toBe('8s ago');
    expect(formatPublishAge(120)).toBe('2m ago');
    expect(formatPublishAge(7200)).toBe('2h ago');
  });
});
