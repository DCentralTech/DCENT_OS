// @vitest-environment jsdom
//
// Parity-wiring regression: the notification "Channel" selector drives REAL
// per-channel webhook config. The URL-based formats (generic / discord / slack)
// show a single URL field with a format-specific label; the Telegram format
// hides the URL and shows bot-token + chat-id fields instead. Saving forwards
// the exact firmware contract fields (format / telegram_bot_token /
// telegram_chat_id) to the existing updateWebhookConfig endpoint.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';

const getWebhookConfig = vi.fn();
const updateWebhookConfig = vi.fn();
const testWebhookConfig = vi.fn();

vi.mock('../../api/client', () => {
  const api = {
    getWebhookConfig: (...args: unknown[]) => getWebhookConfig(...args),
    updateWebhookConfig: (...args: unknown[]) => updateWebhookConfig(...args),
    testWebhookConfig: (...args: unknown[]) => testWebhookConfig(...args),
  };
  return { api, default: api };
});

// NOTE: the mocked state object/fn MUST be stable across renders — the
// component's webhook-load effect depends on `addAlert`, so a fresh fn each
// render would re-fire the effect forever (infinite re-render / OOM).
vi.mock('../../store/miner', () => {
  const addAlert = vi.fn();
  return {
    useMinerStore: (selector: (s: { addAlert: () => void }) => unknown) =>
      selector({ addAlert }),
  };
});

import { NotificationSettings } from './NotificationSettings';

const baseWebhook = {
  enabled: false,
  url: '',
  events: [],
  supported_events: ['mining_started', 'pool_disconnected'],
  restart_required: true,
  format: 'generic' as const,
  telegram_bot_token: '',
  telegram_chat_id: '',
};

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  getWebhookConfig.mockResolvedValue({ ...baseWebhook });
});

afterEach(() => cleanup());

describe('NotificationSettings — per-channel webhook format selector', () => {
  it('renders the URL field for the generic format and no Telegram fields', async () => {
    render(<NotificationSettings />);
    await waitFor(() => expect(getWebhookConfig).toHaveBeenCalled());

    expect(screen.getByLabelText('Webhook URL')).toBeTruthy();
    expect(screen.queryByLabelText('Telegram bot token')).toBeNull();
    expect(screen.queryByLabelText('Telegram chat id')).toBeNull();
  });

  it('switches to Telegram bot token + chat id fields and hides the URL field', async () => {
    render(<NotificationSettings />);
    await waitFor(() => expect(getWebhookConfig).toHaveBeenCalled());

    fireEvent.change(screen.getByLabelText('Channel'), { target: { value: 'telegram' } });

    expect(screen.getByLabelText('Telegram bot token')).toBeTruthy();
    expect(screen.getByLabelText('Telegram chat id')).toBeTruthy();
    expect(screen.queryByLabelText('Webhook URL')).toBeNull();
  });

  it('shows a Discord-labelled URL field for the discord format', async () => {
    render(<NotificationSettings />);
    await waitFor(() => expect(getWebhookConfig).toHaveBeenCalled());

    fireEvent.change(screen.getByLabelText('Channel'), { target: { value: 'discord' } });

    expect(screen.getByLabelText('Discord webhook URL')).toBeTruthy();
    expect(screen.queryByLabelText('Telegram bot token')).toBeNull();
  });

  it('saves the Telegram channel with the exact firmware contract fields', async () => {
    updateWebhookConfig.mockResolvedValue({
      status: 'ok',
      message: 'saved',
      config: { ...baseWebhook, format: 'telegram' },
    });

    render(<NotificationSettings />);
    await waitFor(() => expect(getWebhookConfig).toHaveBeenCalled());

    fireEvent.change(screen.getByLabelText('Channel'), { target: { value: 'telegram' } });
    fireEvent.change(screen.getByLabelText('Telegram bot token'), { target: { value: '123456:ABCdef' } });
    fireEvent.change(screen.getByLabelText('Telegram chat id'), { target: { value: '-1001234567890' } });

    fireEvent.click(screen.getByRole('button', { name: /^save$/i }));

    await waitFor(() => expect(updateWebhookConfig).toHaveBeenCalledTimes(1));
    expect(updateWebhookConfig).toHaveBeenCalledWith(
      expect.objectContaining({
        format: 'telegram',
        telegram_bot_token: '123456:ABCdef',
        telegram_chat_id: '-1001234567890',
      }),
    );
  });

  it('keeps the alert-threshold section honestly labelled as browser-stored / not daemon-enforced', async () => {
    render(<NotificationSettings />);
    await waitFor(() => expect(getWebhookConfig).toHaveBeenCalled());

    expect(screen.getByText(/not yet enforced by the daemon/i)).toBeTruthy();
  });
});
