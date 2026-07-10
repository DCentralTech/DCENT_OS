// WebSocket connection manager with auto-reconnect and REST fallback support.

import type { WsMessage } from './types';
import { getSessionToken } from './credentials';

type WsListener = (msg: WsMessage) => void;

export interface WsConnectionSnapshot {
  connected: boolean;
  lastMessageTs: number;
  url: string | null;
  candidates: string[];
}

type WsConnectionListener = (state: WsConnectionSnapshot) => void;

const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 30000;
const PING_INTERVAL_MS = 25000;
// DASH-STATE-4: a half-open socket can stay OPEN after frames stop arriving.
// Close it after two missed ping windows so REST polling can resume.
const IDLE_DEADLINE_MS = PING_INTERVAL_MS * 2;

function appendToken(url: string): string {
  const token = getSessionToken();
  if (!token) return url;
  return `${url}?token=${encodeURIComponent(token)}`;
}

function getRuntimeLocation(): Pick<Location, 'protocol' | 'host' | 'hostname'> {
  const runtimeLocation = globalThis.location;
  return {
    protocol: runtimeLocation?.protocol ?? 'http:',
    host: runtimeLocation?.host ?? 'localhost',
    hostname: runtimeLocation?.hostname ?? runtimeLocation?.host?.split(':')[0] ?? 'localhost',
  };
}

function buildCandidates(dev: boolean): string[] {
  const runtimeLocation = getRuntimeLocation();
  const proto = runtimeLocation.protocol === 'https:' ? 'wss:' : 'ws:';
  const sameOrigin = `${proto}//${runtimeLocation.host}/ws`;
  if (dev) return [sameOrigin];

  const hostname = runtimeLocation.hostname || runtimeLocation.host.split(':')[0];
  const direct = `${proto}//${hostname}:8080/ws`;
  return Array.from(new Set([sameOrigin, direct]));
}

export class WebSocketManager {
  private ws: WebSocket | null = null;
  private listeners = new Set<WsListener>();
  private connectionListeners = new Set<WsConnectionListener>();
  private reconnectDelay = RECONNECT_BASE_MS;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private pingTimer: ReturnType<typeof setInterval> | null = null;
  private candidates: string[];
  private candidateIndex = 0;
  private currentUrl: string | null = null;
  private failedCandidatesThisRound = 0;
  private frameReceivedOnSocket = false;
  private _connected = false;
  private lastMessageTs = 0;

  constructor(options: { dev?: boolean } = {}) {
    this.candidates = buildCandidates(options.dev ?? import.meta.env.DEV);
  }

  get connected() { return this._connected; }
  get lastFrameAt() { return this.lastMessageTs; }
  get candidateUrls() { return [...this.candidates]; }

  connect() {
    if (
      this.ws?.readyState === WebSocket.OPEN ||
      this.ws?.readyState === WebSocket.CONNECTING
    ) return;
    this.cleanup();

    try {
      this.currentUrl = this.candidates[this.candidateIndex] ?? this.candidates[0];
      this.frameReceivedOnSocket = false;
      this.ws = new WebSocket(appendToken(this.currentUrl));
    } catch {
      this.scheduleReconnect();
      return;
    }

    this.ws.onopen = () => {
      this.reconnectDelay = RECONNECT_BASE_MS;
      this.lastMessageTs = Date.now();
      this.startPing();
      this.emitConnectionChange();
    };

    this.ws.onmessage = (event) => {
      this.lastMessageTs = Date.now();
      this.frameReceivedOnSocket = true;
      this.failedCandidatesThisRound = 0;
      this._connected = true;
      this.emitConnectionChange();
      try {
        const msg = JSON.parse(event.data) as WsMessage;
        this.listeners.forEach(fn => fn(msg));
      } catch {
        // Ignore malformed frames. The idle watchdog still saw a live frame.
      }
    };

    this.ws.onclose = () => {
      const hadFrame = this.frameReceivedOnSocket;
      this._connected = false;
      this.stopPing();
      this.emitConnectionChange();

      if (!hadFrame && this.failedCandidatesThisRound < this.candidates.length - 1) {
        this.failedCandidatesThisRound += 1;
        this.candidateIndex = (this.candidateIndex + 1) % this.candidates.length;
        this.scheduleReconnect(0);
        return;
      }

      if (!hadFrame) {
        this.failedCandidatesThisRound = 0;
        this.candidateIndex = 0;
      }
      this.scheduleReconnect();
    };

    this.ws.onerror = () => {
      this._connected = false;
      this.emitConnectionChange();
      this.ws?.close();
    };
  }

  disconnect() {
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    this.cleanup();
    this.emitConnectionChange();
  }

  subscribe(fn: WsListener): () => void {
    this.listeners.add(fn);
    return () => { this.listeners.delete(fn); };
  }

  onConnectionChange(fn: WsConnectionListener): () => void {
    this.connectionListeners.add(fn);
    fn(this.connectionSnapshot());
    return () => { this.connectionListeners.delete(fn); };
  }

  private cleanup() {
    this.stopPing();
    if (this.ws) {
      this.ws.onopen = null;
      this.ws.onmessage = null;
      this.ws.onclose = null;
      this.ws.onerror = null;
      if (this.ws.readyState === WebSocket.OPEN) this.ws.close();
      this.ws = null;
    }
    this._connected = false;
    this.currentUrl = null;
    this.frameReceivedOnSocket = false;
  }

  private scheduleReconnect(delay = this.reconnectDelay) {
    if (this.reconnectTimer) return;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
    if (delay > 0) {
      this.reconnectDelay = Math.min(this.reconnectDelay * 2, RECONNECT_MAX_MS);
    }
  }

  private startPing() {
    this.pingTimer = setInterval(() => {
      if (this.ws?.readyState !== WebSocket.OPEN) return;
      if (Date.now() - this.lastMessageTs > IDLE_DEADLINE_MS) {
        this.ws.close();
        return;
      }
      this.ws.send('ping');
    }, PING_INTERVAL_MS);
  }

  private stopPing() {
    if (this.pingTimer) {
      clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
  }

  private connectionSnapshot(): WsConnectionSnapshot {
    return {
      connected: this._connected,
      lastMessageTs: this.lastMessageTs,
      url: this.currentUrl,
      candidates: [...this.candidates],
    };
  }

  private emitConnectionChange() {
    const snapshot = this.connectionSnapshot();
    this.connectionListeners.forEach(fn => fn(snapshot));
  }
}

type WebSocketManagerDevApi = WebSocketManager & {
  devInject?: (msg: WsMessage) => void;
};

if (import.meta.env.DEV) {
  (WebSocketManager.prototype as WebSocketManagerDevApi).devInject = function devInject(
    this: WebSocketManager,
    msg: WsMessage,
  ): void {
    const manager = this as unknown as {
      listeners: Set<WsListener>;
      connectionListeners: Set<WsConnectionListener>;
      lastMessageTs: number;
      frameReceivedOnSocket: boolean;
      failedCandidatesThisRound: number;
      _connected: boolean;
    };

    manager.lastMessageTs = Date.now();
    manager.frameReceivedOnSocket = true;
    manager.failedCandidatesThisRound = 0;
    manager._connected = true;
    const snapshot: WsConnectionSnapshot = {
      connected: manager._connected,
      lastMessageTs: manager.lastMessageTs,
      url: null,
      candidates: this.candidateUrls,
    };
    manager.connectionListeners.forEach(fn => fn(snapshot));
    manager.listeners.forEach(fn => fn(msg));
  };
}

export const wsManager = new WebSocketManager();
export default wsManager;
