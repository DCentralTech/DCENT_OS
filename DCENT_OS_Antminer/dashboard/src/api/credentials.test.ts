import { describe, it, expect, beforeEach, vi } from 'vitest';

// P1-6 (F-8) frontend security: the dashboard must NEVER persist the plaintext
// owner password at rest, and the durable `dcentos-settings` localStorage blob
// must never carry an `apiToken` or `password` field (the bearer token lives in
// sessionStorage, the password in memory only). These tests pin that contract
// against both the credentials module and the settings store's save chokepoint.

const SETTINGS_KEY = 'dcentos-settings';
const SESSION_TOKEN_KEY = 'dcentos-session-token';

// Minimal in-memory Storage stub (vitest runs in the `node` env — no DOM Storage).
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

describe('credentials storage contract', () => {
  it('stripCredentialFields removes only apiToken and password', async () => {
    const { stripCredentialFields } = await import('./credentials');
    const cleaned = stripCredentialFields({
      minerName: 'Rig', mode: 'standard', apiToken: 'tok', password: 'secret',
    });
    expect(cleaned).toEqual({ minerName: 'Rig', mode: 'standard' });
    expect(cleaned).not.toHaveProperty('apiToken');
    expect(cleaned).not.toHaveProperty('password');
  });

  it('keeps the bearer token in sessionStorage and the password in memory only', async () => {
    const credentials = await import('./credentials');
    credentials.setSessionToken('tok-abc');
    credentials.setVolatilePassword('plaintext-pw');

    // Token persisted to sessionStorage, NOT localStorage.
    expect(globalThis.sessionStorage.getItem(SESSION_TOKEN_KEY)).toBe('tok-abc');
    expect(globalThis.localStorage.getItem(SESSION_TOKEN_KEY)).toBeNull();
    expect(credentials.getSessionToken()).toBe('tok-abc');

    // Password is in memory only — no Storage key holds the plaintext.
    expect(credentials.getVolatilePassword()).toBe('plaintext-pw');
    expect(globalThis.localStorage.getItem(SETTINGS_KEY)).toBeNull();
    expect(globalThis.localStorage.length).toBe(0);            // nothing in localStorage at all
    expect(globalThis.sessionStorage.length).toBe(1);          // ONLY the token, no password entry
    expect(globalThis.sessionStorage.key(0)).toBe(SESSION_TOKEN_KEY);
  });

  it('migrateLegacyCredentials purges a legacy plaintext password+token from localStorage', async () => {
    globalThis.localStorage.setItem(SETTINGS_KEY, JSON.stringify({
      minerName: 'Rig',
      mode: 'standard',
      apiToken: 'legacy-tok',
      password: 'legacy-plaintext-pw',
    }));

    const credentials = await import('./credentials');
    credentials.migrateLegacyCredentials();

    const raw = globalThis.localStorage.getItem(SETTINGS_KEY)!;
    // The plaintext password is gone from the durable blob entirely.
    expect(raw).not.toContain('legacy-plaintext-pw');
    const blob = JSON.parse(raw);
    expect(blob).not.toHaveProperty('password');
    expect(blob).not.toHaveProperty('apiToken');
    // Non-credential settings survive the migration untouched.
    expect(blob.minerName).toBe('Rig');
    expect(blob.mode).toBe('standard');
    // The revocable token was relocated to sessionStorage.
    expect(globalThis.sessionStorage.getItem(SESSION_TOKEN_KEY)).toBe('legacy-tok');
  });
});

describe('settings store never persists the plaintext password', () => {
  it('updateSettings strips apiToken and password from the dcentos-settings blob', async () => {
    const { useMinerStore } = await import('../store/miner');

    useMinerStore.getState().updateSettings({
      password: 'super-secret-owner-pw',
      apiToken: 'session-token-xyz',
      minerName: 'My Rig',
    });

    const raw = globalThis.localStorage.getItem(SETTINGS_KEY)!;
    const blob = JSON.parse(raw);

    // THE load-bearing assertion: the persisted blob has no plaintext password.
    expect(raw).not.toContain('super-secret-owner-pw');
    expect(blob).not.toHaveProperty('password');
    expect(blob).not.toHaveProperty('apiToken');
    // Non-credential settings still persist normally.
    expect(blob.minerName).toBe('My Rig');

    // Credentials are routed to their isolated stores: token in sessionStorage,
    // password in memory only.
    expect(globalThis.sessionStorage.getItem(SESSION_TOKEN_KEY)).toBe('session-token-xyz');
    expect(useMinerStore.getState().settings.password).toBe('super-secret-owner-pw');
  });
});
