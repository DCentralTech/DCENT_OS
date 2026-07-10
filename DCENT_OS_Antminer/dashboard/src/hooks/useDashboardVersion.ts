import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api/client';
import type { DashboardVersionResponse } from '../api/types';

const RECHECK_AFTER_MS = 60 * 60 * 1000;
const SHA256_RE = /^[a-f0-9]{64}$/;

export interface DashboardVersionCheck {
  checking: boolean;
  mismatch: boolean;
  showReloadBanner: boolean;
  localSha256: string | null;
  remote: DashboardVersionResponse | null;
  dismiss: () => void;
  reload: () => void;
  checkNow: () => Promise<void>;
}

export function normalizeDashboardSha256(value: string | null | undefined): string | null {
  if (typeof value !== 'string') return null;
  const normalized = value.trim().toLowerCase();
  return SHA256_RE.test(normalized) ? normalized : null;
}

export function shouldShowDashboardVersionMismatch(
  localSha256: string | null | undefined,
  remote: Pick<DashboardVersionResponse, 'sha256'> | null | undefined,
): boolean {
  const local = normalizeDashboardSha256(localSha256);
  const installed = normalizeDashboardSha256(remote?.sha256);
  return Boolean(local && installed && local !== installed);
}

export function shouldRecheckDashboardVersion(lastCheckAt: number, now: number): boolean {
  return lastCheckAt === 0 || now - lastCheckAt > RECHECK_AFTER_MS;
}

export async function sha256Hex(bytes: ArrayBuffer): Promise<string | null> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) return null;
  const digest = await subtle.digest('SHA-256', bytes);
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('');
}

async function readCachedDocumentBytes(): Promise<ArrayBuffer | null> {
  if (typeof window === 'undefined' || typeof fetch === 'undefined') return null;
  try {
    const res = await fetch(window.location.href, {
      cache: 'only-if-cached',
      mode: 'same-origin',
    });
    if (!res.ok) return null;
    return await res.arrayBuffer();
  } catch {
    return null;
  }
}

export async function getLoadedDashboardSha256(): Promise<string | null> {
  const bytes = await readCachedDocumentBytes();
  return bytes ? sha256Hex(bytes) : null;
}

export function useDashboardVersion(): DashboardVersionCheck {
  const [checking, setChecking] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [localSha256, setLocalSha256] = useState<string | null>(null);
  const [remote, setRemote] = useState<DashboardVersionResponse | null>(null);
  const [mismatch, setMismatch] = useState(false);
  const lastCheckAt = useRef(0);
  const checkInFlight = useRef<Promise<void> | null>(null);

  const checkNow = useCallback(async () => {
    if (checkInFlight.current) return checkInFlight.current;
    const run = (async () => {
      setChecking(true);
      lastCheckAt.current = Date.now();
      try {
        const [installed, loadedSha] = await Promise.all([
          api.getDashboardVersion(),
          getLoadedDashboardSha256(),
        ]);
        const nextMismatch = shouldShowDashboardVersionMismatch(loadedSha, installed);
        setRemote(installed);
        setLocalSha256(normalizeDashboardSha256(loadedSha));
        setMismatch(nextMismatch);
        if (!nextMismatch) setDismissed(false);
      } catch {
        setMismatch(false);
      } finally {
        setChecking(false);
        checkInFlight.current = null;
      }
    })();
    checkInFlight.current = run;
    return run;
  }, []);

  useEffect(() => {
    void checkNow();

    const maybeRecheck = () => {
      if (shouldRecheckDashboardVersion(lastCheckAt.current, Date.now())) {
        void checkNow();
      }
    };

    const onVisibility = () => {
      if (document.visibilityState === 'visible') maybeRecheck();
    };

    window.addEventListener('focus', maybeRecheck);
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      window.removeEventListener('focus', maybeRecheck);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, [checkNow]);

  return {
    checking,
    mismatch,
    showReloadBanner: mismatch && !dismissed,
    localSha256,
    remote,
    dismiss: () => setDismissed(true),
    reload: () => window.location.reload(),
    checkNow,
  };
}
