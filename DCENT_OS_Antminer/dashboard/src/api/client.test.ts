import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';

// DASH-STATE-1 (TEST-DASH-1): the shared API client must not hang forever on a
// wedged daemon (TCP connection alive, no response — the .25/.139-class
// bring-up states). request() wraps fetch in an AbortController timeout so a
// silent hang becomes a clean ApiTimeoutError the panels' existing catch-blocks
// already handle, instead of a permanent spinner with no error and no retry.

// vitest runs in the `node` env (no DOM Storage). Minimal in-memory stub so
// getAuthHeaders() can read settings without throwing.
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

function abortError(): Error {
  return Object.assign(new Error('Aborted'), { name: 'AbortError' });
}

/** fetch() that hangs until its abort signal fires (real fetch semantics). */
function hangingFetch() {
  return vi.fn((_url: string, init?: RequestInit) =>
    new Promise<Response>((_resolve, reject) => {
      const sig = init?.signal;
      if (sig?.aborted) { reject(abortError()); return; }
      sig?.addEventListener('abort', () => reject(abortError()));
    }),
  );
}

beforeEach(() => {
  (globalThis as { localStorage: Storage }).localStorage = makeStorage();
  (globalThis as { sessionStorage: Storage }).sessionStorage = makeStorage();
  vi.resetModules();
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('api client request timeout (DASH-STATE-1)', () => {
  it('aborts and throws ApiTimeoutError when the daemon never responds', async () => {
    const fetchMock = hangingFetch();
    (globalThis as { fetch: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
    const { apiFetch, ApiTimeoutError } = await import('./client');

    vi.useFakeTimers();
    const p = apiFetch('/api/status');
    const assertion = expect(p).rejects.toBeInstanceOf(ApiTimeoutError);
    await vi.advanceTimersByTimeAsync(15_001);
    await assertion;
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('returns the response normally when the daemon answers in time', async () => {
    const ok = { ok: true, status: 200 } as Response;
    const fetchMock = vi.fn(async () => ok);
    (globalThis as { fetch: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
    const { apiFetch } = await import('./client');

    const res = await apiFetch('/api/status');
    expect(res.status).toBe(200);
  });

  it('rethrows the original error (not ApiTimeoutError) when the CALLER aborts', async () => {
    const fetchMock = hangingFetch();
    (globalThis as { fetch: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
    const { apiFetch, ApiTimeoutError } = await import('./client');

    const ac = new AbortController();
    const p = apiFetch('/api/status', { signal: ac.signal });
    const assertion = expect(p).rejects.not.toBeInstanceOf(ApiTimeoutError);
    ac.abort();
    await assertion;
  });
});

describe('api auth session creation', () => {
  it('throws the daemon rejection body on the public createSession path', async () => {
    const fetchMock = vi.fn(async () => new Response('owner password rejected by daemon', { status: 401 }));
    (globalThis as { fetch: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
    const { api, ApiError } = await import('./client');

    await expect(api.createSession('wrong-password')).rejects.toMatchObject({
      name: 'ApiError',
      status: 401,
      message: 'owner password rejected by daemon',
    } satisfies Partial<InstanceType<typeof ApiError>>);
  });
});

describe('api error parsing', () => {
  it('carries canonical envelope fields', async () => {
    const { parseApiError } = await import('./client');

    await expect(parseApiError(new Response(JSON.stringify({
      error: 'Pool URL is invalid',
      detail: 'bad scheme',
      code: 'pool_validation',
      suggestion: 'Use stratum+tcp://host:port',
    }), {
      status: 400,
      headers: { 'content-type': 'application/json' },
    }))).resolves.toMatchObject({
      name: 'ApiError',
      status: 400,
      message: 'Pool URL is invalid',
      detail: 'bad scheme',
      code: 'pool_validation',
      suggestion: 'Use stratum+tcp://host:port',
    });
  });

  it('parses legacy JSON error objects', async () => {
    const { parseApiError } = await import('./client');

    await expect(parseApiError(new Response(JSON.stringify({ error: 'legacy rejection' }), {
      status: 403,
      headers: { 'content-type': 'application/json' },
    }))).resolves.toMatchObject({
      status: 403,
      message: 'legacy rejection',
    });
  });

  it('parses bare text bodies', async () => {
    const { parseApiError } = await import('./client');

    await expect(parseApiError(new Response('plain failure', { status: 400 })))
      .resolves.toMatchObject({ status: 400, message: 'plain failure' });
  });

  it('falls back for empty bodies', async () => {
    const { parseApiError } = await import('./client');

    await expect(parseApiError(new Response('', { status: 502 })))
      .resolves.toMatchObject({ status: 502, message: 'Request failed with status 502' });
  });
});

describe('api donation config route', () => {
  const donationConfig = {
    enabled: true,
    percent: 2,
    pool_url: 'stratum+tcp://pool.d-central.tech:3333',
    worker: 'DungeonMaster',
    password: 'x',
    fallback_enabled: true,
    fallback_pool_url: 'stratum+tcp://stratum.braiins.com:3333',
    fallback_worker: 'DungeonMaster',
    fallback_password: 'x',
    cycle_duration_s: 3600,
  };

  it('reads the dedicated route without falling back to generic config on 200', async () => {
    const fetchMock = vi.fn(async (url: string, _init?: RequestInit) => {
      if (url === '/api/config/donation') {
        return new Response(JSON.stringify({ status: 'ok', config: donationConfig }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      }
      return new Response(JSON.stringify({ status: 'unexpected' }), { status: 500 });
    });
    (globalThis as { fetch: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
    const { api } = await import('./client');

    await expect(api.getDonationConfig()).resolves.toMatchObject({ percent: 2 });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual(['/api/config/donation']);
    expect(fetchMock.mock.calls[0][1]?.method).toBeUndefined();
  });

  it('writes the dedicated route without falling back to generic config on 200', async () => {
    const fetchMock = vi.fn(async (url: string, _init?: RequestInit) => {
      if (url === '/api/config/donation') {
        return new Response(JSON.stringify({
          status: 'ok',
          config: { ...donationConfig, enabled: false },
          restart_required: true,
        }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      }
      return new Response(JSON.stringify({ status: 'unexpected' }), { status: 500 });
    });
    (globalThis as { fetch: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
    const { api } = await import('./client');

    await expect(api.updateDonationConfig({ ...donationConfig, enabled: false })).resolves.toMatchObject({
      config: { enabled: false },
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual(['/api/config/donation']);
    expect(fetchMock.mock.calls[0][1]?.method).toBe('POST');
  });
});

describe('api rolling metrics route', () => {
  it('reads daemon rolling averages from the typed metrics route', async () => {
    const body = {
      now_ms: 1_800_000,
      total_samples: 12,
      w5s: { window_s: 5, sample_count: 1, avg_hashrate_ths: 96, avg_wall_watts: 0, wall_power_sample_count: 0, wall_power_measured_sample_count: 0, wall_power_modeled_sample_count: 0, wall_power_unavailable_sample_count: 1, avg_max_chip_temp_c: 0, avg_error_rate: 0, avg_max_fan_pwm: 0, accepted_shares: 0, rejected_shares: 0 },
      w1m: { window_s: 60, sample_count: 12, avg_hashrate_ths: 95.25, avg_wall_watts: 0, wall_power_sample_count: 0, wall_power_measured_sample_count: 0, wall_power_modeled_sample_count: 0, wall_power_unavailable_sample_count: 12, avg_max_chip_temp_c: 0, avg_error_rate: 0, avg_max_fan_pwm: 0, accepted_shares: 0, rejected_shares: 0 },
      w5m: { window_s: 300, sample_count: 12, avg_hashrate_ths: 94.8, avg_wall_watts: 0, wall_power_sample_count: 0, wall_power_measured_sample_count: 0, wall_power_modeled_sample_count: 0, wall_power_unavailable_sample_count: 12, avg_max_chip_temp_c: 0, avg_error_rate: 0, avg_max_fan_pwm: 0, accepted_shares: 0, rejected_shares: 0 },
    };
    const fetchMock = vi.fn(async (url: string, _init?: RequestInit) => {
      expect(url).toBe('/api/metrics/rolling');
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    });
    (globalThis as { fetch: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
    const { api } = await import('./client');

    await expect(api.getRollingMetrics()).resolves.toMatchObject({
      w1m: { avg_hashrate_ths: 95.25 },
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('returns null when an older daemon lacks the rolling route', async () => {
    const fetchMock = vi.fn(async () => new Response('not found', { status: 404 }));
    (globalThis as { fetch: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
    const { api } = await import('./client');

    await expect(api.getRollingMetrics()).resolves.toBeNull();
  });
});
