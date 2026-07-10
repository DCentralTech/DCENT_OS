import { wsManager } from '../api/websocket';
import type {
  WsAutotunerChipHealthMessage,
  WsAutotunerEfficiencyMessage,
  WsAutotunerStatusMessage,
  WsDiagnosticMessage,
  WsHeaterMessage,
  WsLogMessage,
  WsMessage,
  WsMiningSyncMessage,
  WsStatsMessage,
} from '../api/types';

export interface WsBridgeLatest {
  stats?: WsStatsMessage;
  heaterStatus?: WsHeaterMessage;
  diagnosticProgress?: WsDiagnosticMessage;
  autotunerStatus?: WsAutotunerStatusMessage;
  autotunerEfficiency?: WsAutotunerEfficiencyMessage;
  autotunerChipHealth?: WsAutotunerChipHealthMessage;
}

export interface WsBridgeFlush {
  at: number;
  latest: WsBridgeLatest;
  logs: WsLogMessage[];
  miningSync: WsMiningSyncMessage[];
}

type WsBridgeListener = (flush: WsBridgeFlush) => void;
type WsSource = { subscribe: (fn: (msg: WsMessage) => void) => () => void };
export type WsBridgeScheduler = (cb: () => void) => () => void;

function defaultScheduler(cb: () => void): () => void {
  if (typeof window !== 'undefined' && typeof window.requestAnimationFrame === 'function') {
    const id = window.requestAnimationFrame(() => cb());
    return () => window.cancelAnimationFrame(id);
  }
  const id = setTimeout(cb, 16);
  return () => clearTimeout(id);
}

function hasLatest(latest: WsBridgeLatest): boolean {
  return Object.values(latest).some(Boolean);
}

export class WsMessageBatcher {
  private latest: WsBridgeLatest = {};
  private logs: WsLogMessage[] = [];
  private miningSync: WsMiningSyncMessage[] = [];
  private cancelScheduled: (() => void) | null = null;
  private lastMessageAt = 0;

  constructor(
    private readonly onFlush: WsBridgeListener,
    private readonly schedule: WsBridgeScheduler = defaultScheduler,
    private readonly now: () => number = () => Date.now(),
  ) {}

  enqueue(message: WsMessage): void {
    this.lastMessageAt = this.now();

    switch (message.type) {
      case 'stats':
        this.latest.stats = message;
        break;
      case 'heater_status':
        this.latest.heaterStatus = message;
        break;
      case 'diagnostic_progress':
        this.latest.diagnosticProgress = message;
        break;
      case 'autotuner_status':
        this.latest.autotunerStatus = message;
        break;
      case 'autotuner_efficiency':
        this.latest.autotunerEfficiency = message;
        break;
      case 'autotuner_chip_health':
        this.latest.autotunerChipHealth = message;
        break;
      case 'log':
        this.logs.push(message);
        break;
      case 'mining_sync':
        this.miningSync.push(message);
        break;
    }

    if (!this.cancelScheduled) {
      this.cancelScheduled = this.schedule(() => this.flush());
    }
  }

  flush(): void {
    this.cancelScheduled = null;
    if (!hasLatest(this.latest) && this.logs.length === 0 && this.miningSync.length === 0) {
      return;
    }

    const batch: WsBridgeFlush = {
      at: this.lastMessageAt || this.now(),
      latest: this.latest,
      logs: this.logs,
      miningSync: this.miningSync,
    };

    this.latest = {};
    this.logs = [];
    this.miningSync = [];
    this.onFlush(batch);
  }

  cancel(): void {
    if (this.cancelScheduled) {
      this.cancelScheduled();
      this.cancelScheduled = null;
    }
    this.latest = {};
    this.logs = [];
    this.miningSync = [];
    this.lastMessageAt = 0;
  }
}

export class WsBridge {
  private listeners = new Set<WsBridgeListener>();
  private unsubscribeSource: (() => void) | null = null;
  private readonly batcher: WsMessageBatcher;

  constructor(
    private readonly source: WsSource = wsManager,
    schedule?: WsBridgeScheduler,
    now?: () => number,
  ) {
    this.batcher = new WsMessageBatcher(flush => this.emit(flush), schedule, now);
  }

  subscribe(fn: WsBridgeListener): () => void {
    this.listeners.add(fn);
    this.start();
    return () => {
      this.listeners.delete(fn);
      if (this.listeners.size === 0) {
        this.stop();
      }
    };
  }

  private start(): void {
    if (this.unsubscribeSource) {
      return;
    }
    this.unsubscribeSource = this.source.subscribe(message => this.batcher.enqueue(message));
  }

  private stop(): void {
    if (this.unsubscribeSource) {
      this.unsubscribeSource();
      this.unsubscribeSource = null;
    }
    this.batcher.cancel();
  }

  private emit(flush: WsBridgeFlush): void {
    for (const listener of [...this.listeners]) {
      listener(flush);
    }
  }
}

export const wsBridge = new WsBridge();
