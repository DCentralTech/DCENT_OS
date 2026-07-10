import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';

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
  (globalThis as { localStorage: Storage }).localStorage = makeStorage();
  (globalThis as { sessionStorage: Storage }).sessionStorage = makeStorage();
  vi.resetModules();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('autotuner active mode route', () => {
  it('uses PUT /api/autotuner/active and normalizes runtime_command', async () => {
    const fetchMock = vi.fn(async (url: string, _init?: RequestInit) => {
      if (url === '/api/autotuner/active') {
        return new Response(JSON.stringify({
          status: 'ok',
          mode: { mode: 'efficiency' },
          runtime_command: {
            status: 'applied',
            applied_runtime: true,
            message: 'applied',
          },
        }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      }
      return new Response(JSON.stringify({ status: 'unexpected' }), { status: 500 });
    });
    (globalThis as { fetch: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
    const { autotunerApi } = await import('./autotuner');

    await expect(autotunerApi.setMode({ mode: 'efficiency' })).resolves.toMatchObject({
      runtime: { status: 'applied', applied_runtime: true },
    });
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual(['/api/autotuner/active']);
    expect(fetchMock.mock.calls[0][1]?.method).toBe('PUT');
    expect(JSON.parse(String(fetchMock.mock.calls[0][1]?.body))).toEqual({ mode: 'efficiency' });
  });

  it('falls back to generic config only when the strict route is absent', async () => {
    const fetchMock = vi.fn(async (url: string, _init?: RequestInit) => {
      if (url === '/api/autotuner/active') {
        return new Response('missing', { status: 404 });
      }
      if (url === '/api/config') {
        return new Response(JSON.stringify({ status: 'ok' }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      }
      return new Response(JSON.stringify({ status: 'unexpected' }), { status: 500 });
    });
    (globalThis as { fetch: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
    const { autotunerApi } = await import('./autotuner');

    await expect(autotunerApi.setMode({ mode: 'performance' })).resolves.toMatchObject({
      status: 'ok',
      mode: { mode: 'performance' },
    });
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
      '/api/autotuner/active',
      '/api/config',
    ]);
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({
      autotuner: { tuner_mode: { mode: 'performance' } },
    });
  });

  it('does not fall back when the strict route rejects the requested mode', async () => {
    const fetchMock = vi.fn(async (url: string) => {
      if (url === '/api/autotuner/active') {
        return new Response(JSON.stringify({ message: 'autotuner.tuner_mode rejected' }), {
          status: 400,
          headers: { 'content-type': 'application/json' },
        });
      }
      return new Response(JSON.stringify({ status: 'unexpected' }), { status: 500 });
    });
    (globalThis as { fetch: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
    const { autotunerApi } = await import('./autotuner');

    await expect(autotunerApi.setMode({ mode: 'power_target', watts: 0 })).rejects.toMatchObject({
      message: 'autotuner.tuner_mode rejected',
    });
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual(['/api/autotuner/active']);
  });
});
