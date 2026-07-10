// DCENTos dashboard — auth credential storage (P1-6 / F-8 frontend-security).
//
// SECURITY CONTRACT
//   1. The plaintext owner password is NEVER persisted at rest. It lives only
//      in an in-memory module variable for the lifetime of the page, used to
//      re-mint a revocable session token on a 401. A reload deliberately drops
//      it — losing nothing a valid session token can't restore.
//   2. The revocable session token is persisted in sessionStorage (per-tab,
//      cleared when the tab/window closes), NOT in the long-lived, broadly-read
//      `dcentos-settings` localStorage blob. That keeps the bearer token out of
//      the durable UI-settings object and shrinks the XSS exfiltration window to
//      the current tab session.
//   3. Auth credentials are owned ONLY by this module. The `dcentos-settings`
//      blob (written by both store/miner.ts AND api/client.ts) must never carry
//      an `apiToken` or `password` field again — co-locating them is exactly
//      what let the two writers race-clobber each other and what leaked the
//      plaintext password to disk.
//
// The intentional DEV no-auth posture is preserved: when the daemon runs open
// (no owner password, no token) every getter returns null and the dashboard
// behaves exactly as before — no auth headers are forced.

const SETTINGS_KEY = 'dcentos-settings';
const SESSION_TOKEN_KEY = 'dcentos-session-token';

// In-memory ONLY. Intentionally never written to any Storage.
let volatilePassword: string | null = null;

function safeSessionStorage(): Storage | null {
  try {
    return typeof sessionStorage !== 'undefined' ? sessionStorage : null;
  } catch {
    return null;
  }
}

function safeLocalStorage(): Storage | null {
  try {
    return typeof localStorage !== 'undefined' ? localStorage : null;
  } catch {
    return null;
  }
}

/** Read the persisted revocable session (bearer) token. */
export function getSessionToken(): string | null {
  return safeSessionStorage()?.getItem(SESSION_TOKEN_KEY) || null;
}

/** Persist (token) or clear (null) the revocable session token in sessionStorage. */
export function setSessionToken(token: string | null): void {
  const ss = safeSessionStorage();
  if (!ss) return;
  if (token) {
    ss.setItem(SESSION_TOKEN_KEY, token);
  } else {
    ss.removeItem(SESSION_TOKEN_KEY);
  }
}

/** The in-memory owner password (never persisted at rest). */
export function getVolatilePassword(): string | null {
  return volatilePassword;
}

/** Hold the owner password in memory for this page session only. */
export function setVolatilePassword(password: string | null): void {
  volatilePassword = password || null;
}

/** Clear every auth credential (explicit logout / revoke). */
export function clearCredentials(): void {
  setSessionToken(null);
  volatilePassword = null;
}

/**
 * Return a shallow copy of `obj` with the credential keys removed. Used by the
 * settings store and the setup wizard so the persisted UI-settings blob can
 * never carry an `apiToken` or a plaintext `password` at rest.
 */
export function stripCredentialFields<T extends object>(obj: T): Omit<T, 'apiToken' | 'password'> {
  const clone = { ...obj };
  delete (clone as { apiToken?: unknown }).apiToken;
  delete (clone as { password?: unknown }).password;
  return clone;
}

/**
 * One-time, idempotent migration off the legacy single-blob layout. Earlier
 * builds stored the bearer token AND the plaintext password inside
 * `dcentos-settings`. On load we move any token into sessionStorage, keep any
 * password in memory for this session only, then REWRITE `dcentos-settings`
 * with both fields stripped — purging the plaintext password from disk. Safe
 * to call repeatedly and a no-op when Storage is unavailable.
 */
export function migrateLegacyCredentials(): void {
  const ls = safeLocalStorage();
  if (!ls) return;
  let raw: string | null;
  try {
    raw = ls.getItem(SETTINGS_KEY);
  } catch {
    return;
  }
  if (!raw) return;
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(raw) as Record<string, unknown>;
  } catch {
    return;
  }
  if (parsed === null || typeof parsed !== 'object') return;
  if (!('apiToken' in parsed) && !('password' in parsed)) return;

  const legacyToken = typeof parsed.apiToken === 'string' ? parsed.apiToken : '';
  const legacyPassword = typeof parsed.password === 'string' ? parsed.password : '';
  if (legacyToken && !getSessionToken()) setSessionToken(legacyToken);
  if (legacyPassword && volatilePassword === null) volatilePassword = legacyPassword;

  try {
    ls.setItem(SETTINGS_KEY, JSON.stringify(stripCredentialFields(parsed)));
  } catch {
    /* ignore write failures — the credentials are already relocated */
  }
}
