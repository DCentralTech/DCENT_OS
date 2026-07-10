//  W5.7: live model-profile fetch with embedded last-known-good fallback.
//
//: hardcoded firmware lists
// in the dashboard drift from the firmware source-of-truth (history: 14/24
// ACH names were wrong because the Rust crate was the truth and the dashboard
// hadn't been updated). The fix is to fetch the source-of-truth at runtime
// from the daemon and use the embedded constants only as a fallback for
// network-down / pre-route-bind / offline-rendering cases.
//
// Source of truth: `dcentrald-silicon-profiles` registry, served at
// `GET /api/profiles/silicon` (returns `SiliconProfileSummary[]` keyed by
// chip + hashboard + miner_model). This hook fetches once per session and
// caches in-memory. Components that need a per-model rendering profile
// continue to use `getModelProfile()` from `utils/modelProfiles`; the hook
// exposes the LIVE chip-id set (from the API) which a build-time conformance
// test asserts matches whatever the silicon-profiles crate emits.

import { useEffect, useState } from 'react';
import { siliconProfilesApi, type SiliconProfileSummary } from '../api/profiles-silicon';
import {
  MODEL_PROFILES,
  type ModelProfile,
} from '../utils/modelProfiles';

// Session-scoped cache — module-level so re-mounts of consumers don't re-fetch.
// Cleared by reload (browser session boundary). Single in-flight promise so
// 5+ components mounting in the same render don't fan out to parallel fetches.
let liveCache: SiliconProfileSummary[] | null = null;
let liveCacheError: Error | null = null;
let inFlight: Promise<SiliconProfileSummary[]> | null = null;

export interface UseModelProfilesResult {
  /** Static last-known-good profiles, keyed by firmware `model_key`. */
  profiles: Readonly<Record<string, ModelProfile>>;
  /** Live chip-id set from `/api/profiles/silicon` — null while loading or on fetch failure. */
  liveChips: ReadonlySet<string> | null;
  /** Live silicon-profile summaries from the API, or null if unavailable. */
  liveSummaries: ReadonlyArray<SiliconProfileSummary> | null;
  /** True until the first fetch attempt resolves (success or failure). */
  loading: boolean;
  /** True if the fetch failed AND we are using the embedded snapshot. */
  fellBackToSnapshot: boolean;
}

function dedupeChipIds(rows: SiliconProfileSummary[]): Set<string> {
  const out = new Set<string>();
  for (const row of rows) {
    if (row && typeof row.chip === 'string' && row.chip.length > 0) {
      out.add(row.chip);
    }
  }
  return out;
}

async function fetchSiliconProfilesOnce(): Promise<SiliconProfileSummary[]> {
  if (liveCache !== null) return liveCache;
  if (inFlight) return inFlight;
  inFlight = siliconProfilesApi
    .list()
    .then(rows => {
      liveCache = Array.isArray(rows) ? rows : [];
      liveCacheError = null;
      return liveCache;
    })
    .catch(err => {
      liveCacheError = err instanceof Error ? err : new Error(String(err));
      // Mark the cache as "tried but empty" so we don't retry every render.
      // Operators can hard-refresh to re-attempt. This is the explicit
      // last-known-good fallback path — render the embedded snapshot.
      liveCache = [];
      return liveCache;
    })
    .finally(() => {
      inFlight = null;
    });
  return inFlight;
}

/**
 * Fetch silicon profiles from `/api/profiles/silicon` once per session and
 * expose them alongside the embedded last-known-good snapshot. Components
 * that just want a per-model rendering profile should keep calling
 * `getModelProfile(model)` from `utils/modelProfiles`; this hook is for
 * code that needs the live chip-id set or wants to surface a "live" badge
 * when API fetch succeeds.
 */
export function useModelProfiles(): UseModelProfilesResult {
  const [liveSummaries, setLiveSummaries] = useState<SiliconProfileSummary[] | null>(
    liveCache,
  );
  const [loading, setLoading] = useState<boolean>(liveCache === null);
  const [fellBackToSnapshot, setFellBackToSnapshot] = useState<boolean>(
    liveCache !== null && liveCacheError !== null,
  );

  useEffect(() => {
    let cancelled = false;
    if (liveCache !== null) {
      // Fast-path — module cache already populated by an earlier mount.
      setLiveSummaries(liveCache);
      setLoading(false);
      setFellBackToSnapshot(liveCacheError !== null);
      return () => {
        cancelled = true;
      };
    }

    setLoading(true);
    fetchSiliconProfilesOnce().then(rows => {
      if (cancelled) return;
      setLiveSummaries(rows);
      setLoading(false);
      setFellBackToSnapshot(liveCacheError !== null);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const liveChips = liveSummaries ? dedupeChipIds(liveSummaries) : null;

  return {
    profiles: MODEL_PROFILES,
    liveChips,
    liveSummaries,
    loading,
    fellBackToSnapshot,
  };
}

/**
 * Test seam — used by Cypress-style fixtures and unit tests to clear the
 * module-level cache between runs. Not for production code.
 */
export function __resetModelProfilesCacheForTests(): void {
  liveCache = null;
  liveCacheError = null;
  inFlight = null;
}
