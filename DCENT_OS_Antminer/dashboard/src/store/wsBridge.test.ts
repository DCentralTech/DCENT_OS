import { describe, expect, it, vi } from 'vitest';
import type { WsMessage } from '../api/types';
import { WsBridge, WsMessageBatcher, type WsBridgeScheduler } from './wsBridge';

function manualScheduler() {
  let pending: (() => void) | null = null;
  const schedule: WsBridgeScheduler = vi.fn((cb: () => void) => {
    pending = cb;
    return () => {
      pending = null;
    };
  });
  return {
    schedule,
    flush: () => {
      const cb = pending;
      pending = null;
      cb?.();
    },
    get pending() {
      return pending;
    },
  };
}

const statsMessage = (hashrate: number): WsMessage => ({
  type: 'stats',
  timestamp: hashrate,
  hashrate_ghs: hashrate,
  hashrate_5s_ghs: hashrate,
  accepted: 0,
  rejected: 0,
  chains: [],
  fans: { pwm: 0, rpm: 0, per_fan: [] },
  pool: { status: 'mining' },
});

const heaterMessage = (watts: number): WsMessage => ({
  type: 'heater_status',
  power_watts: watts,
  wall_watts: watts,
  btu_h: watts * 3.412,
  noise_db: null,
  airflow_cfm: 0,
  preset: 'balanced',
  room_temp_c: null,
  cost_today_usd: 0,
  sats_today: 0,
  night_mode_active: false,
  night_mode_starts_in_s: null,
});

describe('WsMessageBatcher', () => {
  it('coalesces bursty latest-wins message types into one scheduled flush', () => {
    const scheduler = manualScheduler();
    const onFlush = vi.fn();
    const batcher = new WsMessageBatcher(onFlush, scheduler.schedule, () => 123);

    batcher.enqueue(statsMessage(100));
    batcher.enqueue(statsMessage(200));
    batcher.enqueue(heaterMessage(900));

    expect(scheduler.schedule).toHaveBeenCalledTimes(1);
    expect(onFlush).not.toHaveBeenCalled();

    scheduler.flush();

    expect(onFlush).toHaveBeenCalledTimes(1);
    expect(onFlush.mock.calls[0][0]).toMatchObject({
      at: 123,
      latest: {
        stats: expect.objectContaining({ hashrate_ghs: 200 }),
        heaterStatus: expect.objectContaining({ power_watts: 900 }),
      },
      logs: [],
      miningSync: [],
    });
  });

  it('keeps every log and mining-sync event in the frame batch', () => {
    const scheduler = manualScheduler();
    const onFlush = vi.fn();
    const batcher = new WsMessageBatcher(onFlush, scheduler.schedule, () => 456);

    batcher.enqueue({
      type: 'log',
      level: 'info',
      source: 'mining',
      timestamp: 1,
      message: 'first',
    });
    batcher.enqueue({
      type: 'mining_sync',
      timestamp_ms: 2,
      event: 'nonce_burst',
      chain_id: 0,
      count: 4,
      intensity: 0.5,
    });
    batcher.enqueue({
      type: 'mining_sync',
      timestamp_ms: 3,
      event: 'share_accepted',
      difficulty: 1024,
      target_difficulty: 512,
      intensity: 1,
    });

    scheduler.flush();

    expect(onFlush).toHaveBeenCalledWith(expect.objectContaining({
      at: 456,
      logs: [expect.objectContaining({ message: 'first' })],
      miningSync: [
        expect.objectContaining({ event: 'nonce_burst' }),
        expect.objectContaining({ event: 'share_accepted' }),
      ],
    }));
  });

  it('drops pending work when cancelled before the scheduled frame', () => {
    const scheduler = manualScheduler();
    const onFlush = vi.fn();
    const batcher = new WsMessageBatcher(onFlush, scheduler.schedule);

    batcher.enqueue(statsMessage(100));
    batcher.cancel();

    expect(scheduler.pending).toBeNull();
    batcher.flush();
    expect(onFlush).not.toHaveBeenCalled();
  });
});

describe('WsBridge', () => {
  it('subscribes to the upstream WebSocket once and stops on the last listener', () => {
    let upstreamListener: ((msg: WsMessage) => void) | null = null;
    const upstreamUnsubscribe = vi.fn();
    const source = {
      subscribe: vi.fn((fn: (msg: WsMessage) => void) => {
        upstreamListener = fn;
        return upstreamUnsubscribe;
      }),
    };
    const scheduler = manualScheduler();
    const bridge = new WsBridge(source, scheduler.schedule, () => 789);
    const listenerA = vi.fn();
    const listenerB = vi.fn();

    const unsubA = bridge.subscribe(listenerA);
    const unsubB = bridge.subscribe(listenerB);

    expect(source.subscribe).toHaveBeenCalledTimes(1);
    upstreamListener?.(statsMessage(300));
    scheduler.flush();
    expect(listenerA).toHaveBeenCalledTimes(1);
    expect(listenerB).toHaveBeenCalledTimes(1);

    unsubA();
    expect(upstreamUnsubscribe).not.toHaveBeenCalled();

    unsubB();
    expect(upstreamUnsubscribe).toHaveBeenCalledTimes(1);
  });
});
