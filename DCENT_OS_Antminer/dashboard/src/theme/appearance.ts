/* ─────────────────────────────────────────────────────────────────────────
   DCENT_OS — light/dark appearance pre-paint applier (UINAV-7)

   Mirrors theme/accent.ts: a synchronous module side-effect (imported in
   main.tsx ahead of ReactDOM.createRoot().render) so the stored light/dark
   preference is on <html> BEFORE the first paint — no flash-of-dark when a
   light-preference operator reloads.

   The preference is read from the SAME persisted blob the Zustand store uses
   (`dcentos-settings`.appearance), so the store and this pre-paint applier
   never disagree. App.tsx keeps the attribute in sync for runtime toggles.

   ── Backward-compat invariant ──
   When appearance is absent / 'dark' (existing users + existing persisted
   blobs), we still stamp `data-appearance="dark"`; styles/light-theme.css only
   matches `[data-appearance="light"]`, so dark renders byte-identically to the
   pre-UINAV-7 build. Only an explicit 'light' opt-in changes anything.
   ───────────────────────────────────────────────────────────────────────── */

export type Appearance = 'dark' | 'light';

const SETTINGS_KEY = 'dcentos-settings';
const ATTR = 'data-appearance';

/** Read the persisted appearance, defaulting to 'dark' (current behaviour). */
export function readStoredAppearance(): Appearance {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as { appearance?: unknown };
      if (parsed && parsed.appearance === 'light') return 'light';
    }
  } catch {
    /* localStorage unavailable / malformed — fall back to dark */
  }
  return 'dark';
}

/** Stamp the appearance attribute on <html>. Idempotent. */
export function applyAppearance(appearance: Appearance): void {
  try {
    document.documentElement.setAttribute(ATTR, appearance === 'light' ? 'light' : 'dark');
  } catch {
    /* document unavailable (non-DOM env) — no-op */
  }
}

// Run before React mounts so the very first paint is correct.
applyAppearance(readStoredAppearance());
