// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { StatusResponse, WsMiningSyncMessage } from '../api/types';
import type { TransportKind } from '../store/miner';
import type { WsBridgeFlush } from '../store/wsBridge';
import { RewardBus, type FxEvent } from './rewardBus';

class MemoryStorage {
  values = new Map<string, string>();
  getItem(key: string) {
    return this.values.get(key) ?? null;
  }
  setItem(key: string, value: string) {
    this.values.set(key, value);
  }
}

class FakeBridge {
  listeners = new Set<(flush: WsBridgeFlush) => void>();
  subscribe(fn: (flush: WsBridgeFlush) => void) {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }
  flush(message: WsMiningSyncMessage, at = message.timestamp_ms) {
    const batch: WsBridgeFlush = {
      at,
      latest: {},
      logs: [],
      miningSync: [message],
    };
    for (const listener of [...this.listeners]) {
      listener(batch);
    }
  }
}

class FakeStore {
  state: { transport: TransportKind; status: StatusResponse | null };
  listeners = new Set<(state: typeof this.state, previous: typeof this.state) => void>();

  constructor(state: { transport: TransportKind; status: StatusResponse | null }) {
    this.state = state;
  }

  getState() {
    return this.state;
  }

  setState(partial: Partial<typeof this.state>) {
    const previous = this.state;
    this.state = { ...this.state, ...partial };
    for (const listener of [...this.listeners]) {
      listener(this.state, previous);
    }
  }

  subscribe(fn: (state: typeof this.state, previous: typeof this.state) => void) {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }
}

function status(overrides: Partial<StatusResponse> = {}): StatusResponse {
  return {
    hashrate_ghs: 1000,
    hashrate_5s_ghs: 1000,
    accepted: 0,
    rejected: 0,
    uptime_s: 60,
    firmware_version: 'test',
    mode: 'standard',
    chains: [],
    fans: { pwm: 20, rpm: 1200, per_fan: [] },
    pool: {
      url: 'stratum+tcp://example.test:3333',
      status: 'mining',
      difficulty: 512,
      last_share_s: 0,
      donating: false,
    },
    ...overrides,
  };
}

function miningSync(event: WsMiningSyncMessage['event'], overrides: Partial<WsMiningSyncMessage> = {}): WsMiningSyncMessage {
  return {
    type: 'mining_sync',
    timestamp_ms: Date.now(),
    event,
    intensity: 0.5,
    ...overrides,
  };
}

function setup(options: { transport?: TransportKind; status?: StatusResponse | null; hidden?: () => boolean } = {}) {
  const bridge = new FakeBridge();
  const store = new FakeStore({
    transport: options.transport ?? 'ws-live',
    status: options.status === undefined ? status() : options.status,
  });
  const events: FxEvent[] = [];
  const storage = new MemoryStorage();
  const bus = new RewardBus({
    bridge,
    store,
    now: () => Date.now(),
    setTimeout,
    clearTimeout,
    documentHidden: options.hidden ?? (() => false),
    settingsStorage: storage,
    bestDifficultyStorage: storage,
    activeEffectDurationMs: 10_000,
  });
  bus.subscribe(event => events.push(event));
  return { bridge, store, bus, events, storage };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(0);
});

afterEach(() => {
  vi.useRealTimers();
});

describe('RewardBus', () => {
  it('drops stale reconnect-backlog mining sync frames', () => {
    const { bridge, events } = setup();

    vi.setSystemTime(10_000);
    bridge.flush(miningSync('share_accepted', { timestamp_ms: 4_000, difficulty: 512 }));

    expect(events).toEqual([]);
  });

  it('throttles accepted-share effects and folds suppressed counts into the next emission', () => {
    const { bridge, events } = setup();

    vi.setSystemTime(1000);
    bridge.flush(miningSync('share_accepted', { difficulty: 512, count: 1 }));
    vi.setSystemTime(1500);
    bridge.flush(miningSync('share_accepted', { difficulty: 513, count: 2 }));
    vi.setSystemTime(2600);
    bridge.flush(miningSync('share_accepted', { difficulty: 514, count: 1 }));

    const accepted = events.filter(event => event.kind === 'share-accepted');
    expect(accepted).toHaveLength(2);
    expect(accepted[0].count).toBe(1);
    expect(accepted[1].count).toBe(3);
  });

  it('coalesces nonce bursts to at most four emissions per chain per second', () => {
    const { bridge, events } = setup();

    for (let i = 0; i < 100; i += 1) {
      const at = i * 10;
      vi.setSystemTime(at);
      bridge.flush(miningSync('nonce_burst', { timestamp_ms: at, chain_id: 1, count: 1, intensity: i / 100 }));
      vi.advanceTimersByTime(10);
    }
    vi.advanceTimersByTime(1000);

    const nonceEvents = events.filter(event => event.kind === 'nonce-activity');
    expect(nonceEvents.length).toBeLessThanOrEqual(4);
    expect(nonceEvents.every(event => event.chainId === 1)).toBe(true);
    expect(nonceEvents.reduce((sum, event) => sum + (event.count ?? 0), 0)).toBe(100);
  });

  it('does not accumulate nonce activity while the page is hidden', () => {
    const { bridge, events } = setup({ hidden: () => true });

    bridge.flush(miningSync('nonce_burst', { chain_id: 2, count: 10, intensity: 1 }));
    vi.advanceTimersByTime(1000);

    expect(events).toEqual([]);
  });

  it('downgrades repeated lucky shares to accepted-share events', () => {
    const { bridge, events } = setup();

    vi.setSystemTime(1000);
    bridge.flush(miningSync('lucky_share', { difficulty: 4096, target_difficulty: 512, intensity: 1 }));
    vi.setSystemTime(2000);
    bridge.flush(miningSync('lucky_share', { difficulty: 4097, target_difficulty: 512, intensity: 1 }));

    expect(events.some(event => event.kind === 'lucky-share')).toBe(true);
    expect(events.filter(event => event.kind === 'lucky-share')).toHaveLength(1);
    expect(events.some(event => event.kind === 'share-accepted')).toBe(true);
  });

  it('persists and emits only locally achieved best difficulty improvements', () => {
    const { bridge, events } = setup();

    bridge.flush(miningSync('share_accepted', { difficulty: 1024, target_difficulty: 512 }));
    bridge.flush(miningSync('share_accepted', { difficulty: 900, target_difficulty: 2048 }));
    bridge.flush(miningSync('share_accepted', { difficulty: null, target_difficulty: 4096 }));
    bridge.flush(miningSync('lucky_share', { difficulty: 4096, target_difficulty: 512 }));

    const best = events.filter(event => event.kind === 'best-difficulty');
    expect(best.map(event => event.difficulty)).toEqual([1024, 4096]);
  });

  it('suppresses per-share and per-nonce effects in polled mode', () => {
    const { bridge, events } = setup({ transport: 'rest-polling' });

    bridge.flush(miningSync('share_accepted', { difficulty: 512 }));
    bridge.flush(miningSync('lucky_share', { difficulty: 2048 }));
    bridge.flush(miningSync('nonce_burst', { chain_id: 0, count: 5 }));
    vi.advanceTimersByTime(1000);

    expect(events).toEqual([]);
  });

  it('emits pool transitions from real store edges after a debounce', () => {
    const { store, events } = setup({ transport: 'rest-polling' });

    store.setState({ status: status({ pool: { ...status().pool, status: 'connecting' } }) });
    vi.advanceTimersByTime(2999);
    expect(events).toEqual([]);
    vi.advanceTimersByTime(1);

    expect(events).toEqual([expect.objectContaining({ kind: 'pool-transition', intensity: 0.45 })]);
  });

  it('emits first-share once only for a fresh session with zero accepted baseline', () => {
    const { bridge, events } = setup({ status: status({ accepted: 0, uptime_s: 120 }) });

    bridge.flush(miningSync('share_accepted', { difficulty: 512 }));
    vi.setSystemTime(2000);
    bridge.flush(miningSync('share_accepted', { difficulty: 600 }));

    expect(events.filter(event => event.kind === 'first-share')).toHaveLength(1);
  });

  it('does not emit first-share when uptime is already long or accepted baseline is nonzero', () => {
    const longRun = setup({ status: status({ accepted: 0, uptime_s: 3600 }) });
    longRun.bridge.flush(miningSync('share_accepted', { difficulty: 512 }));
    expect(longRun.events.some(event => event.kind === 'first-share')).toBe(false);

    const existingShares = setup({ status: status({ accepted: 4, uptime_s: 120 }) });
    existingShares.bridge.flush(miningSync('share_accepted', { difficulty: 512 }));
    expect(existingShares.events.some(event => event.kind === 'first-share')).toBe(false);
  });

  it('caps active visual effects and emits overflow as intensity zero', () => {
    const { bridge, events } = setup();

    bridge.flush(miningSync('share_rejected', { intensity: 0.8 }));
    bridge.flush(miningSync('share_rejected', { intensity: 0.8 }));
    bridge.flush(miningSync('share_rejected', { intensity: 0.8 }));
    bridge.flush(miningSync('share_rejected', { intensity: 0.8 }));

    expect(events.map(event => event.intensity)).toEqual([0.8, 0.8, 0.8, 0]);
  });
});
