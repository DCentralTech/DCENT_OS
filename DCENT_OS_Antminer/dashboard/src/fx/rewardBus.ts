import type { WsMiningSyncMessage } from '../api/types';
import type { TransportKind } from '../store/miner';
import { useMinerStore } from '../store/miner';
import { wsBridge, type WsBridgeFlush } from '../store/wsBridge';
import { BestDifficultyStore, type BestDifficultyStorage } from './bestDifficulty';
import { readFxSettings, type FxSettingsStorage } from './fxSettings';

export type FxEventKind =
  | 'share-accepted'
  | 'share-rejected'
  | 'lucky-share'
  | 'nonce-activity'
  | 'work-fresh'
  | 'pool-transition'
  | 'first-share'
  | 'best-difficulty';

export interface FxEvent {
  kind: FxEventKind;
  at: number;
  chainId?: number;
  intensity: number;
  difficulty?: number;
  targetDifficulty?: number;
  count?: number;
}

export type FxMode = 'live' | 'polled' | 'off';

interface RewardBridge {
  subscribe(fn: (flush: WsBridgeFlush) => void): () => void;
}

interface RewardStoreState {
  transport: TransportKind;
  status: {
    accepted: number;
    uptime_s: number;
    pool?: { status?: string | null } | null;
  } | null;
}

interface RewardStore {
  getState(): RewardStoreState;
  subscribe(fn: (state: RewardStoreState, previous: RewardStoreState) => void): () => void;
}

type TimerHandle = ReturnType<typeof setTimeout>;

interface RewardBusOptions {
  bridge?: RewardBridge;
  store?: RewardStore;
  now?: () => number;
  setTimeout?: (fn: () => void, delay: number) => TimerHandle;
  clearTimeout?: (handle: TimerHandle) => void;
  documentHidden?: () => boolean;
  settingsStorage?: FxSettingsStorage | null;
  bestDifficultyStorage?: BestDifficultyStorage | null;
  logger?: (message: string) => void;
  activeEffectDurationMs?: number;
}

type Listener = (event: FxEvent) => void;
type ModeListener = (mode: FxMode) => void;

interface PendingNonce {
  chainId?: number;
  at: number;
  intensity: number;
  count: number;
}

const STALE_FRAME_MS = 5000;
const CLOCK_SKEW_FALLBACK_MS = 5 * 60 * 1000;
const SHARE_ACCEPTED_THROTTLE_MS = 1500;
const NONCE_ACTIVITY_WINDOW_MS = 250;
const LUCKY_SHARE_THROTTLE_MS = 30_000;
const POOL_TRANSITION_DEBOUNCE_MS = 3000;
const FIRST_SHARE_UPTIME_MAX_S = 1800;
const MAX_ACTIVE_EFFECTS = 3;
const DEFAULT_ACTIVE_EFFECT_DURATION_MS = 1800;

function clampIntensity(value: number | null | undefined): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) return 0.5;
  return Math.max(0, Math.min(1, value));
}

function numberOrUndefined(value: number | null | undefined): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function chainKey(chainId: number | null | undefined): string {
  return typeof chainId === 'number' && Number.isFinite(chainId) ? String(chainId) : 'unknown';
}

function eventCount(message: WsMiningSyncMessage): number {
  return Math.max(1, Math.floor(numberOrUndefined(message.count) ?? 1));
}

function requiresLiveMode(kind: FxEventKind): boolean {
  return kind !== 'pool-transition';
}

export class RewardBus {
  private listeners = new Set<Listener>();
  private modeListeners = new Set<ModeListener>();
  private unsubscribeBridge: (() => void) | null = null;
  private unsubscribeStore: (() => void) | null = null;
  private started = false;
  private lastMode: FxMode;
  private activeEffects = 0;
  private skewLogged = false;
  private lastShareAcceptedAt = Number.NEGATIVE_INFINITY;
  private suppressedAcceptedCount = 0;
  private lastLuckyShareAt = Number.NEGATIVE_INFINITY;
  private firstShareEmitted = false;
  private baselineAccepted: number | null;
  private lastPoolStatus: string | null;
  private poolTransitionTimer: TimerHandle | null = null;
  private pendingPoolTransition: FxEvent | null = null;
  private pendingNonce = new Map<string, PendingNonce>();
  private nonceTimers = new Map<string, TimerHandle>();
  private lastNonceEmitAt = new Map<string, number>();
  private readonly bestDifficulty: BestDifficultyStore;

  constructor(private readonly options: RewardBusOptions = {}) {
    this.bestDifficulty = new BestDifficultyStore(options.bestDifficultyStorage);
    const state = this.store.getState();
    this.baselineAccepted = typeof state.status?.accepted === 'number' ? state.status.accepted : null;
    this.lastPoolStatus = state.status?.pool?.status ?? null;
    this.lastMode = this.deriveMode(state.transport);
  }

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    this.start();
    return () => {
      this.listeners.delete(fn);
    };
  }

  onModeChange(fn: ModeListener): () => void {
    this.modeListeners.add(fn);
    fn(this.lastMode);
    this.start();
    return () => {
      this.modeListeners.delete(fn);
    };
  }

  getMode(): FxMode {
    this.lastMode = this.deriveMode(this.store.getState().transport);
    return this.lastMode;
  }

  start(): void {
    if (this.started) return;
    this.started = true;
    this.unsubscribeBridge = this.bridge.subscribe(flush => this.handleFlush(flush));
    this.unsubscribeStore = this.store.subscribe((state, previous) => this.handleStoreChange(state, previous));
    this.checkModeChange();
  }

  stop(): void {
    this.unsubscribeBridge?.();
    this.unsubscribeBridge = null;
    this.unsubscribeStore?.();
    this.unsubscribeStore = null;
    if (this.poolTransitionTimer) {
      this.clearTimer(this.poolTransitionTimer);
      this.poolTransitionTimer = null;
    }
    for (const timer of this.nonceTimers.values()) {
      this.clearTimer(timer);
    }
    this.nonceTimers.clear();
    this.pendingNonce.clear();
    this.started = false;
  }

  private get bridge(): RewardBridge {
    return this.options.bridge ?? wsBridge;
  }

  private get store(): RewardStore {
    return (this.options.store ?? useMinerStore) as unknown as RewardStore;
  }

  private get now(): () => number {
    return this.options.now ?? (() => Date.now());
  }

  private get setTimer(): (fn: () => void, delay: number) => TimerHandle {
    return this.options.setTimeout ?? ((fn, delay) => setTimeout(fn, delay));
  }

  private get clearTimer(): (handle: TimerHandle) => void {
    return this.options.clearTimeout ?? ((handle) => clearTimeout(handle));
  }

  private documentHidden(): boolean {
    if (this.options.documentHidden) return this.options.documentHidden();
    return typeof document !== 'undefined' ? document.hidden : false;
  }

  private deriveMode(transport: TransportKind): FxMode {
    const settings = readFxSettings(this.options.settingsStorage);
    if (!settings.enabled) return 'off';
    return transport === 'ws-live' ? 'live' : 'polled';
  }

  private handleFlush(flush: WsBridgeFlush): void {
    this.checkModeChange();
    for (const message of flush.miningSync) {
      this.handleMiningSync(message, flush.at);
    }
  }

  private handleStoreChange(state: RewardStoreState, previous: RewardStoreState): void {
    if (this.baselineAccepted === null && typeof state.status?.accepted === 'number') {
      this.baselineAccepted = state.status.accepted;
    }

    this.checkModeChange(state.transport);

    const nextPoolStatus = state.status?.pool?.status ?? null;
    const prevPoolStatus = previous.status?.pool?.status ?? this.lastPoolStatus;
    if (nextPoolStatus && prevPoolStatus && nextPoolStatus !== prevPoolStatus) {
      this.queuePoolTransition(nextPoolStatus);
    }
    if (nextPoolStatus) {
      this.lastPoolStatus = nextPoolStatus;
    }
  }

  private checkModeChange(transport = this.store.getState().transport): void {
    const next = this.deriveMode(transport);
    if (next === this.lastMode) return;
    this.lastMode = next;
    for (const listener of [...this.modeListeners]) {
      listener(next);
    }
  }

  private resolveEventTime(message: WsMiningSyncMessage, arrivalAt: number): number | null {
    const timestamp = numberOrUndefined(message.timestamp_ms) ?? arrivalAt;
    const age = this.now() - timestamp;
    if (age > STALE_FRAME_MS && age < CLOCK_SKEW_FALLBACK_MS) {
      return null;
    }
    if (Math.abs(age) >= CLOCK_SKEW_FALLBACK_MS || age < -STALE_FRAME_MS) {
      if (!this.skewLogged) {
        this.skewLogged = true;
        this.options.logger?.('mining_sync timestamp skew detected; using arrival time for visual effects');
      }
      return arrivalAt;
    }
    return timestamp;
  }

  private handleMiningSync(message: WsMiningSyncMessage, arrivalAt: number): void {
    const at = this.resolveEventTime(message, arrivalAt);
    if (at === null) return;

    switch (message.event) {
      case 'share_accepted':
        this.handleShareAccepted(message, at);
        this.checkBestDifficulty(message, at);
        this.maybeEmitFirstShare(message, at);
        break;
      case 'share_rejected':
        this.emit({
          kind: 'share-rejected',
          at,
          intensity: clampIntensity(message.intensity),
          difficulty: numberOrUndefined(message.difficulty),
          targetDifficulty: numberOrUndefined(message.target_difficulty),
          count: eventCount(message),
        });
        break;
      case 'lucky_share':
        this.handleLuckyShare(message, at);
        this.checkBestDifficulty(message, at);
        break;
      case 'nonce_burst':
        this.queueNonceActivity(message, at);
        break;
      case 'job_received':
      case 'clean_job':
        this.emit({
          kind: 'work-fresh',
          at,
          chainId: numberOrUndefined(message.chain_id),
          intensity: clampIntensity(message.intensity),
          count: eventCount(message),
        });
        break;
      case 'dispatch_burst':
        break;
    }
  }

  private handleShareAccepted(message: WsMiningSyncMessage, at: number): void {
    const count = eventCount(message);
    if (at - this.lastShareAcceptedAt < SHARE_ACCEPTED_THROTTLE_MS) {
      this.suppressedAcceptedCount += count;
      return;
    }

    const emittedCount = count + this.suppressedAcceptedCount;
    this.suppressedAcceptedCount = 0;
    this.lastShareAcceptedAt = at;
    this.emit({
      kind: 'share-accepted',
      at,
      chainId: numberOrUndefined(message.chain_id),
      intensity: clampIntensity(message.intensity),
      difficulty: numberOrUndefined(message.difficulty),
      targetDifficulty: numberOrUndefined(message.target_difficulty),
      count: emittedCount,
    });
  }

  private handleLuckyShare(message: WsMiningSyncMessage, at: number): void {
    if (at - this.lastLuckyShareAt < LUCKY_SHARE_THROTTLE_MS) {
      this.handleShareAccepted({ ...message, event: 'share_accepted' }, at);
      return;
    }
    this.lastLuckyShareAt = at;
    this.emit({
      kind: 'lucky-share',
      at,
      chainId: numberOrUndefined(message.chain_id),
      intensity: clampIntensity(message.intensity),
      difficulty: numberOrUndefined(message.difficulty),
      targetDifficulty: numberOrUndefined(message.target_difficulty),
      count: eventCount(message),
    });
  }

  private maybeEmitFirstShare(message: WsMiningSyncMessage, at: number): void {
    if (this.firstShareEmitted) return;
    const status = this.store.getState().status;
    if (!status || status.uptime_s >= FIRST_SHARE_UPTIME_MAX_S) return;
    if (this.baselineAccepted !== 0) return;
    this.firstShareEmitted = true;
    this.emit({
      kind: 'first-share',
      at,
      chainId: numberOrUndefined(message.chain_id),
      intensity: clampIntensity(message.intensity),
      difficulty: numberOrUndefined(message.difficulty),
      targetDifficulty: numberOrUndefined(message.target_difficulty),
      count: 1,
    });
  }

  private checkBestDifficulty(message: WsMiningSyncMessage, at: number): void {
    const next = this.bestDifficulty.recordIfBest(message.difficulty, at);
    if (!next) return;
    this.emit({
      kind: 'best-difficulty',
      at,
      chainId: numberOrUndefined(message.chain_id),
      intensity: clampIntensity(message.intensity),
      difficulty: next.value,
      targetDifficulty: numberOrUndefined(message.target_difficulty),
      count: 1,
    });
  }

  private queueNonceActivity(message: WsMiningSyncMessage, at: number): void {
    if (this.documentHidden()) return;

    const key = chainKey(message.chain_id);
    const current = this.pendingNonce.get(key);
    this.pendingNonce.set(key, {
      chainId: numberOrUndefined(message.chain_id),
      at,
      intensity: Math.max(current?.intensity ?? 0, clampIntensity(message.intensity)),
      count: (current?.count ?? 0) + eventCount(message),
    });

    if (this.nonceTimers.has(key)) return;
    const last = this.lastNonceEmitAt.get(key);
    const dueAt = last === undefined ? at + NONCE_ACTIVITY_WINDOW_MS : Math.max(at, last + NONCE_ACTIVITY_WINDOW_MS);
    const delay = Math.max(0, dueAt - this.now());
    const timer = this.setTimer(() => this.flushNonceActivity(key), delay);
    this.nonceTimers.set(key, timer);
  }

  private flushNonceActivity(key: string): void {
    this.nonceTimers.delete(key);
    if (this.documentHidden()) {
      this.pendingNonce.delete(key);
      return;
    }

    const pending = this.pendingNonce.get(key);
    if (!pending) return;

    const now = this.now();
    const last = this.lastNonceEmitAt.get(key);
    if (last !== undefined && now - last < NONCE_ACTIVITY_WINDOW_MS) {
      const timer = this.setTimer(() => this.flushNonceActivity(key), NONCE_ACTIVITY_WINDOW_MS - (now - last));
      this.nonceTimers.set(key, timer);
      return;
    }

    this.pendingNonce.delete(key);
    this.lastNonceEmitAt.set(key, now);
    this.emit({
      kind: 'nonce-activity',
      at: now,
      chainId: pending.chainId,
      intensity: pending.intensity,
      count: pending.count,
    });
  }

  private queuePoolTransition(nextStatus: string): void {
    const at = this.now();
    this.pendingPoolTransition = {
      kind: 'pool-transition',
      at,
      intensity: nextStatus.toLowerCase().includes('mining') || nextStatus.toLowerCase().includes('connected') ? 0.8 : 0.45,
      count: 1,
    };
    if (this.poolTransitionTimer) {
      this.clearTimer(this.poolTransitionTimer);
    }
    this.poolTransitionTimer = this.setTimer(() => {
      this.poolTransitionTimer = null;
      const event = this.pendingPoolTransition;
      this.pendingPoolTransition = null;
      if (event) this.emit(event);
    }, POOL_TRANSITION_DEBOUNCE_MS);
  }

  private emit(event: FxEvent): void {
    const mode = this.getMode();
    if (mode === 'off') return;
    if (requiresLiveMode(event.kind) && mode !== 'live') return;

    const canActivate = event.intensity > 0 && this.activeEffects < MAX_ACTIVE_EFFECTS;
    const emitted = canActivate ? event : { ...event, intensity: 0 };

    if (canActivate) {
      this.activeEffects += 1;
      this.setTimer(() => {
        this.activeEffects = Math.max(0, this.activeEffects - 1);
      }, this.options.activeEffectDurationMs ?? DEFAULT_ACTIVE_EFFECT_DURATION_MS);
    }

    for (const listener of [...this.listeners]) {
      listener(emitted);
    }
  }
}

export const rewardBus = new RewardBus();

export function initRewardBus(): void {
  rewardBus.start();
}
