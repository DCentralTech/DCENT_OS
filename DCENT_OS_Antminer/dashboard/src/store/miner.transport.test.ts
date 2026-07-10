// @vitest-environment jsdom

import { beforeEach, describe, expect, it } from 'vitest';
import { deriveTransportKind, useMinerStore } from './miner';

describe('transport state derivation', () => {
  it('reports live only when a real WebSocket frame is recent', () => {
    expect(deriveTransportKind(10_000, true, 8_000, 0)).toBe('ws-live');
    expect(deriveTransportKind(10_000, true, 0, 0)).toBe('stale');
    expect(deriveTransportKind(10_000, true, 4_999, 0)).toBe('stale');
  });

  it('falls back to polling while a REST sample is recent', () => {
    expect(deriveTransportKind(20_000, false, 0, 10_001)).toBe('rest-polling');
    expect(deriveTransportKind(20_000, false, 0, 5_000)).toBe('stale');
  });
});

describe('transport store actions', () => {
  beforeEach(() => {
    useMinerStore.setState({
      wsConnected: false,
      transport: 'stale',
      lastWsFrameAt: 0,
      lastRestPollAt: 0,
    });
  });

  it('records a WebSocket frame as the live transport source', () => {
    useMinerStore.getState().markWsFrame(10_000);
    expect(useMinerStore.getState()).toMatchObject({
      wsConnected: true,
      transport: 'ws-live',
      lastWsFrameAt: 10_000,
    });
  });

  it('records REST polling as the fallback transport source', () => {
    useMinerStore.getState().markRestPoll(10_000);
    expect(useMinerStore.getState()).toMatchObject({
      wsConnected: false,
      transport: 'rest-polling',
      lastRestPollAt: 10_000,
    });
  });

  it('ages live telemetry into polling and then stale', () => {
    useMinerStore.getState().markWsFrame(10_000);
    useMinerStore.getState().markRestPoll(12_000);

    useMinerStore.getState().refreshTransportState(16_000);
    expect(useMinerStore.getState().transport).toBe('rest-polling');

    useMinerStore.getState().refreshTransportState(28_000);
    expect(useMinerStore.getState().transport).toBe('stale');
  });
});
