import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';

// DASH-STATE-4: the WebSocket manager must not trust a half-open socket. A
// socket can stay readyState OPEN while the server silently stops delivering
// frames; without an idle deadline `connected` stays true forever and
// useMinerData suppresses its REST poll fallback.

const PING_INTERVAL_MS = 25_000;

interface MockSocket {
  url: string;
  readyState: number;
  onopen: (() => void) | null;
  onmessage: ((ev: { data: string }) => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
  send: ReturnType<typeof vi.fn>;
  close: ReturnType<typeof vi.fn>;
  open: () => void;
  deliver: (data: string) => void;
}

let lastSocket: MockSocket | null = null;
let sockets: MockSocket[] = [];

class MockWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  readyState = MockWebSocket.CONNECTING;
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  send = vi.fn();
  close = vi.fn(() => {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.();
  });

  constructor(public url: string) {
    lastSocket = this as unknown as MockSocket;
    sockets.push(lastSocket);
  }

  open() {
    this.readyState = MockWebSocket.OPEN;
    this.onopen?.();
  }

  deliver(data: string) {
    this.onmessage?.({ data });
  }
}

function makeStorage(): Storage {
  const m = new Map<string, string>();
  return {
    get length() { return m.size; },
    clear: () => m.clear(),
    getItem: (k: string) => (m.has(k) ? m.get(k)! : null),
    key: (i: number) => Array.from(m.keys())[i] ?? null,
    removeItem: (k: string) => { m.delete(k); },
    setItem: (k: string, v: string) => { m.set(k, String(v)); },
  } as Storage;
}

beforeEach(() => {
  lastSocket = null;
  sockets = [];
  (globalThis as { localStorage: Storage }).localStorage = makeStorage();
  (globalThis as { sessionStorage: Storage }).sessionStorage = makeStorage();
  (globalThis as { location: { protocol: string; host: string; hostname: string } }).location = {
    protocol: 'http:',
    host: 'miner.local',
    hostname: 'miner.local',
  };
  (globalThis as { WebSocket: unknown }).WebSocket = MockWebSocket;
  vi.resetModules();
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('WebSocketManager candidates', () => {
  it('tries same-origin first, then the production daemon port', async () => {
    const { WebSocketManager } = await import('./websocket');
    const manager = new WebSocketManager({ dev: false });
    expect(manager.candidateUrls).toEqual([
      'ws://miner.local/ws',
      'ws://miner.local:8080/ws',
    ]);
  });

  it('uses only the same-origin candidate in dev so the Vite proxy remains authoritative', async () => {
    const { WebSocketManager } = await import('./websocket');
    const manager = new WebSocketManager({ dev: true });
    expect(manager.candidateUrls).toEqual(['ws://miner.local/ws']);
  });

  it('can be imported outside a browser-like runtime', async () => {
    delete (globalThis as { location?: Location }).location;
    const { WebSocketManager } = await import('./websocket');
    const manager = new WebSocketManager({ dev: false });
    expect(manager.candidateUrls).toEqual([
      'ws://localhost/ws',
      'ws://localhost:8080/ws',
    ]);
  });

  it('rotates to the direct daemon candidate when the first socket closes before any frame', async () => {
    const { WebSocketManager } = await import('./websocket');
    const manager = new WebSocketManager({ dev: false });

    manager.connect();
    expect(sockets[0].url).toBe('ws://miner.local/ws');
    sockets[0].open();
    expect(manager.connected).toBe(false);
    sockets[0].close();

    await vi.advanceTimersByTimeAsync(0);
    expect(sockets[1].url).toBe('ws://miner.local:8080/ws');

    manager.disconnect();
  });

  it('sticks to the candidate that delivered a frame before reconnecting', async () => {
    const { WebSocketManager } = await import('./websocket');
    const manager = new WebSocketManager({ dev: false });

    manager.connect();
    sockets[0].open();
    sockets[0].deliver('{"type":"log","level":"info","source":"system","message":"ready"}');
    expect(manager.connected).toBe(true);
    sockets[0].close();

    await vi.advanceTimersByTimeAsync(1000);
    expect(sockets[1].url).toBe('ws://miner.local/ws');

    manager.disconnect();
  });

  it('fans out dev-injected frames through the normal listener path', async () => {
    const { WebSocketManager } = await import('./websocket');
    const manager = new WebSocketManager({ dev: true });
    const received: unknown[] = [];
    const snapshots: unknown[] = [];
    manager.subscribe(msg => received.push(msg));
    manager.onConnectionChange(snapshot => snapshots.push(snapshot));

    const message = {
      type: 'mining_sync',
      timestamp_ms: Date.now(),
      event: 'share_accepted',
      intensity: 0.8,
      count: 1,
    } as const;
    (manager as typeof manager & { devInject?: (msg: typeof message) => void }).devInject?.(message);

    expect(received).toEqual([message]);
    expect(manager.connected).toBe(true);
    expect(snapshots.at(-1)).toMatchObject({ connected: true });
  });
});

describe('WebSocketManager idle watchdog (DASH-STATE-4)', () => {
  it('force-closes a half-open socket so connected flips to false', async () => {
    const { wsManager } = await import('./websocket');
    wsManager.connect();
    const sock = lastSocket!;
    sock.open();
    sock.deliver('{"type":"log","level":"info","source":"system","message":"ready"}');
    expect(wsManager.connected).toBe(true);

    vi.advanceTimersByTime(PING_INTERVAL_MS);
    expect(sock.send).toHaveBeenCalledTimes(1);
    expect(sock.close).not.toHaveBeenCalled();
    expect(wsManager.connected).toBe(true);

    vi.advanceTimersByTime(PING_INTERVAL_MS * 2);
    expect(sock.close).toHaveBeenCalled();
    expect(wsManager.connected).toBe(false);

    wsManager.disconnect();
  });

  it('keeps the socket open while frames keep arriving', async () => {
    const { wsManager } = await import('./websocket');
    wsManager.connect();
    const sock = lastSocket!;
    sock.open();

    for (let i = 0; i < 4; i++) {
      vi.advanceTimersByTime(PING_INTERVAL_MS - 1);
      sock.deliver('{"type":"stats"}');
      vi.advanceTimersByTime(1);
    }

    expect(sock.close).not.toHaveBeenCalled();
    expect(wsManager.connected).toBe(true);
    expect(sock.send.mock.calls.length).toBeGreaterThanOrEqual(4);

    wsManager.disconnect();
  });
});
