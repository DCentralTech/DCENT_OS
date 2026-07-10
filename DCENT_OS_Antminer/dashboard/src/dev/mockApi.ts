/* ─────────────────────────────────────────────────────────────────────────
   DCENT_OS dashboard — QA mock-telemetry harness.

   A reusable, opt-in mock backend for visual/UX verification WITHOUT a live
   miner. Inert unless explicitly enabled, so it never affects normal use:

     • URL flag:        http://<host>/?mock   (or #/...?mock)
     • localStorage:    localStorage.setItem('dcent_qa_mock', '1')

   When enabled (see main.tsx), it wraps window.fetch and serves realistic,
   type-shaped JSON (from ./mockFixtures) for the /api/* surface so every page
   renders with healthy "miner is mining" telemetry. Unmocked endpoints get a
   safe default ([] for list-shaped paths, {} otherwise) and a console.debug
   note so the fixture set can be extended. Non-/api requests pass through.

   This is a DEV/QA aid; it is gated and adds no behaviour to a normal session.
   ───────────────────────────────────────────────────────────────────────── */

import { FIXTURES as GENERATED } from './mockFixtures';
import { wsManager } from '../api/websocket';
import type { WsMiningSyncMessage } from '../api/types';

type DevInjectableWsManager = typeof wsManager & {
  devInject?: (msg: WsMiningSyncMessage) => void;
};

type MockSyncEmit = (
  event?: WsMiningSyncMessage['event'],
  overrides?: Partial<WsMiningSyncMessage>,
) => WsMiningSyncMessage;

interface MockSyncApi {
  emit: MockSyncEmit;
  burst: (count?: number, overrides?: Partial<WsMiningSyncMessage>) => WsMiningSyncMessage[];
}

declare global {
  interface Window {
    __dcentMockSync?: MockSyncApi;
  }
}

// Hand-added fixtures discovered during live QA (endpoints the generator
// missed, e.g. query-parameterised ones whose path collapses after ?strip).
const OVERRIDES: Record<string, unknown> = {
  '/api/diagnostics/chain': {
    schema: 'dcentos.diagnostics.chain.v1', id: 6,
    observation: { chips_detected: 76, chips_expected: 76, nonces_returning: true },
    verdict: 'ok', repair_action: 'none', break_point_chip_idx: null,
  },
  // EvidencePage aggregates these; it reads nested arrays unguarded.
  '/api/system/boot_timeline': { schema: 'dcentos.boot.timeline.v1', canonical: [], observed: [], complete: true },
  '/api/hardware/pic_info': { schema: 'dcentos.hardware.pic.v1', count: 0, variants: [], live_per_slot: null, live_per_slot_note: 'No live per-slot PIC data on this platform.' },
  '/api/re/catalog/index': { schema: 'dcentos.re.catalog.v1', read_only: true, content_collected: true, base_path: '/api/re/catalog', catalogs: [], limitations: [] },
  // SiliconGradeReportSection gates its .toFixed metrics on characterized===true;
  // the generated fixture set characterized true but lacked complete metrics.
  // Honest "not characterized yet" state (what a fresh/un-tuned miner shows).
  '/api/autotuner/silicon-report': {
    characterized: false, not_characterized_chips: 228, quality_score: 0, quality_tier: 'unmeasured',
    total_chips: 228, grade_a_count: 0, grade_b_count: 0, grade_c_count: 0, grade_d_count: 0,
    grade_a_pct: 0, grade_b_pct: 0, grade_c_pct: 0, grade_d_pct: 0, avg_max_stable_mhz: 0,
    best_chip_mhz: 0, worst_chip_mhz: 0, frequency_std_dev_mhz: 0,
    chain_reports: [], top_5_chips: [], bottom_5_chips: [],
  },
  '/api/thermal/supervisor': {
    enabled: true, uptime_secs: 18432, secs_since_last_step: 12, board_states: [],
    fan_max_pwm: 100, chip_imbalance_threshold_c: 8, worst_chip_imbalance_c: 3.4, hydro_configured: false,
  },
  '/api/metrics/rolling': {
    now_ms: Date.now(),
    total_samples: 60,
    w5s: {
      window_s: 5, sample_count: 5, avg_hashrate_ths: 96.2, avg_wall_watts: 3180,
      wall_power_sample_count: 5, wall_power_measured_sample_count: 0, wall_power_modeled_sample_count: 5,
      wall_power_unavailable_sample_count: 0, avg_max_chip_temp_c: 62, avg_error_rate: 0.001,
      avg_max_fan_pwm: 28, accepted_shares: 1, rejected_shares: 0,
    },
    w1m: {
      window_s: 60, sample_count: 60, avg_hashrate_ths: 95.4, avg_wall_watts: 3180,
      wall_power_sample_count: 60, wall_power_measured_sample_count: 0, wall_power_modeled_sample_count: 60,
      wall_power_unavailable_sample_count: 0, avg_max_chip_temp_c: 62, avg_error_rate: 0.001,
      avg_max_fan_pwm: 28, accepted_shares: 5, rejected_shares: 0,
    },
    w5m: {
      window_s: 300, sample_count: 60, avg_hashrate_ths: 94.9, avg_wall_watts: 3175,
      wall_power_sample_count: 60, wall_power_measured_sample_count: 0, wall_power_modeled_sample_count: 60,
      wall_power_unavailable_sample_count: 0, avg_max_chip_temp_c: 62, avg_error_rate: 0.001,
      avg_max_fan_pwm: 28, accepted_shares: 20, rejected_shares: 1,
    },
  },
  // MqttConfig surfaces live publisher health here. Honest disabled state that
  // matches the `/api/config/mqtt` fixture (enabled: false) — the card renders
  // its "publisher not running" empty state rather than a fabricated connection.
  '/api/mqtt/status': {
    enabled: false, connected: false, broker: '', discovery: true, commands_enabled: false,
    entity_count: 0, last_publish_ms: null, publish_count: 0, error: null,
  },
};

const FIXTURES: Record<string, unknown> = { ...GENERATED, ...OVERRIDES };

// Paths whose unmocked default should be a list, not an object.
const ARRAY_PATH = /\/(history|shares|logs|catalog|index|failure_modes|recovery_actions|silicon|local_rejects|audit|timeline|messages|patterns|presets|entries)\b/;

let installed = false;

function defaultIntensity(event: WsMiningSyncMessage['event']): number {
  if (event === 'lucky_share') return 1;
  if (event === 'share_accepted') return 0.82;
  if (event === 'share_rejected') return 0.6;
  if (event === 'nonce_burst') return 0.72;
  return 0.5;
}

function buildMiningSyncMessage(
  event: WsMiningSyncMessage['event'] = 'share_accepted',
  overrides: Partial<WsMiningSyncMessage> = {},
): WsMiningSyncMessage {
  const targetDifficulty = event === 'lucky_share' ? 512 : undefined;
  const difficulty = event === 'lucky_share' ? 12482 : undefined;
  return {
    type: 'mining_sync',
    timestamp_ms: Date.now(),
    event,
    chain_id: 0,
    count: 1,
    intensity: defaultIntensity(event),
    target_difficulty: targetDifficulty,
    difficulty,
    ...overrides,
  };
}

function installMockSyncApi(): void {
  const emit: MockSyncEmit = (event = 'share_accepted', overrides = {}) => {
    const message = buildMiningSyncMessage(event, overrides);
    (wsManager as DevInjectableWsManager).devInject?.(message);
    return message;
  };

  window.__dcentMockSync = {
    emit,
    burst(count = 20, overrides = {}) {
      const requested = Number.isFinite(count) ? count : 20;
      const total = Math.max(1, Math.min(200, Math.floor(requested)));
      const messages: WsMiningSyncMessage[] = [];
      for (let i = 0; i < total; i++) {
        messages.push(emit('nonce_burst', {
          chain_id: i % 3,
          intensity: 0.45 + (i % 5) * 0.1,
          ...overrides,
          timestamp_ms: Date.now(),
        }));
      }
      return messages;
    },
  };
}

export function isMockEnabled(): boolean {
  try {
    const url = new URL(window.location.href);
    if (url.searchParams.has('mock')) return true;
    if ((url.hash || '').includes('mock')) return true;
    return localStorage.getItem('dcent_qa_mock') === '1';
  } catch {
    return false;
  }
}

export function installMockApi(): void {
  if (installed) return;
  installed = true;
  // Persist the flag so it survives reloads during a QA session.
  try { localStorage.setItem('dcent_qa_mock', '1'); } catch { /* ignore */ }
  const orig = window.fetch.bind(window);
  // Freshen time-relative fixtures so mock telemetry reads as "live" (the
  // generated fixtures use a fixed timestamp that would otherwise show the
  // current block as hours-stale). Done once at install.
  const blk = FIXTURES['/api/network/block'] as Record<string, number> | undefined;
  if (blk && typeof blk === 'object') {
    const s = Math.floor(Date.now() / 1000);
    blk.timestamp = s - 120;
    blk.fetched_at = Date.now();
    blk.cache_age_ms = 2000;
  }
  (window as unknown as { __mockApi?: Record<string, unknown> }).__mockApi = FIXTURES;
  installMockSyncApi();
  window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : (input as Request).url || '';
    const path = url.replace(/^https?:\/\/[^/]+/, '').split('?')[0];
    if (path.startsWith('/api/')) {
      const method = (init?.method || (typeof input !== 'string' && !(input instanceof URL) ? (input as Request).method : 'GET') || 'GET').toUpperCase();
      let body: unknown = FIXTURES[path];
      if (body === undefined) {
        if (method !== 'GET') body = { ok: true, applied: true };
        else { body = ARRAY_PATH.test(path) ? [] : {}; if (typeof console !== 'undefined') console.debug('[mockApi] unmocked', method, path); }
      }
      return Promise.resolve(new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } }));
    }
    return orig(input as RequestInfo, init);
  }) as typeof window.fetch;
  if (typeof console !== 'undefined') console.info('[mockApi] QA mock telemetry ENABLED (' + Object.keys(FIXTURES).length + ' fixtures). Clear with localStorage.removeItem("dcent_qa_mock").');
}
