// useNetworkContext — Bitcoin network telemetry derived from the live
// block-tip height. Pure-compute on-device for halving / retarget /
// subsidy. Mempool fee bands are passed through from whichever local
// source dcentrald exposes (local node, pool job, configured oracle).
// No external HTTP is initiated by this hook — it polls dcentrald's
// /api/network/block, the same endpoint CurrentBlockCard already uses.
//
// DCENT_OS is local-first: this hook never reaches mempool.space or any
// public fee oracle from the dashboard runtime. When dcentrald has no
// fee source configured, `feesAvailable` is false and the consumer
// renders a graceful placeholder.

import { useEffect, useRef, useState } from 'react';
import { api } from '../api/client';
import type { NetworkBlockMempoolStatus, NetworkBlockResponse } from '../api/types';

/** Bitcoin halving epoch length (blocks). Sacrosanct constant. */
export const HALVING_INTERVAL = 210_000;

/** Difficulty retarget interval (blocks). Bitcoin core spec. */
export const RETARGET_INTERVAL = 2016;

/** Average block interval in seconds. 10 minutes. */
export const BLOCK_INTERVAL_SECONDS = 600;

const MS_PER_DAY = 86_400_000;
const DEFAULT_POLL_MS = 30_000;
const MIN_POLL_MS = 15_000;
const TICK_MS = 1000;

export interface NetworkContext {
  /** Best known block-tip height. Null when no source has reported yet. */
  blockHeight: number | null;
  /** Age of the current block in milliseconds (null when unknown). */
  ageMs: number | null;

  // ── Halving ─────────────────────────────────────────────────────
  /** Blocks remaining until the next halving (1..HALVING_INTERVAL). */
  halvingBlocksRemaining: number | null;
  /** Height of the next halving boundary. */
  nextHalvingBlock: number | null;
  /** Estimated days until the next halving at 10 min/block. */
  halvingEtaDays: number | null;
  /** Current era index (0 = 50 BTC subsidy, 1 = 25, 2 = 12.5, …). */
  eraIndex: number | null;
  /** Current block subsidy in BTC: 50 / 2^eraIndex. */
  subsidyBtc: number | null;

  // ── Network difficulty / hashrate (on-device estimate) ──────────
  /** Current network difficulty as reported by the block source (null when unknown). */
  networkDifficulty: number | null;
  /**
   * On-device network-hashrate estimate in EH/s, derived purely from the
   * reported difficulty: hashrate ≈ difficulty · 2^32 / 600 s. Null when no
   * difficulty is available. This is an ESTIMATE (constant 10-min interval),
   * not a measured oracle value — consumers must label it as such.
   */
  networkHashrateEhEstimate: number | null;

  // ── Difficulty retarget ─────────────────────────────────────────
  /** Position within the current 2016-block epoch (0..2015). */
  epochPosition: number | null;
  /** Epoch progress as a percentage (0..100). */
  epochProgressPct: number | null;
  /** Blocks remaining until the next difficulty retarget. */
  blocksUntilRetarget: number | null;
  /** Estimated days until the next retarget at 10 min/block. */
  retargetEtaDays: number | null;

  // ── Mempool fees (optional, oracle-provided) ────────────────────
  /** Fastest-confirm sat/vB (null when no fee oracle is connected). */
  feeFastestSatVb: number | null;
  /** Half-hour confirm sat/vB. */
  feeHalfHourSatVb: number | null;
  /** Hour confirm sat/vB. */
  feeHourSatVb: number | null;
  /** True when at least one fee band is available. */
  feesAvailable: boolean;
  /** Coarse fee posture derived from fastest_fee. */
  feeBand: 'low' | 'medium' | 'high' | null;

  /** True while the first fetch is in flight (no data yet). */
  loading: boolean;
  /** Last fetch error, if any. Null on success. */
  error: string | null;
}

const EMPTY: NetworkContext = {
  blockHeight: null,
  ageMs: null,
  halvingBlocksRemaining: null,
  nextHalvingBlock: null,
  halvingEtaDays: null,
  eraIndex: null,
  subsidyBtc: null,
  networkDifficulty: null,
  networkHashrateEhEstimate: null,
  epochPosition: null,
  epochProgressPct: null,
  blocksUntilRetarget: null,
  retargetEtaDays: null,
  feeFastestSatVb: null,
  feeHalfHourSatVb: null,
  feeHourSatVb: null,
  feesAvailable: false,
  feeBand: null,
  loading: true,
  error: null,
};

function pickHeight(raw: unknown): number | null {
  if (typeof raw !== 'number' || !Number.isFinite(raw) || raw <= 0) return null;
  return Math.floor(raw);
}

function pickDifficulty(raw: unknown): number | null {
  if (typeof raw !== 'number' || !Number.isFinite(raw) || raw <= 0) return null;
  return raw;
}

/**
 * On-device network-hashrate estimate in EH/s from difficulty.
 *   hashes/s ≈ difficulty · 2^32 / block_interval_seconds
 *   EH/s     = hashes/s / 1e18
 * Pure compute; null when difficulty is unavailable. This is an estimate
 * (assumes the nominal 600 s interval), not a measured value.
 */
function estimateNetworkHashrateEh(difficulty: number | null): number | null {
  if (difficulty === null) return null;
  const hashesPerSecond = (difficulty * 2 ** 32) / BLOCK_INTERVAL_SECONDS;
  const eh = hashesPerSecond / 1e18;
  return Number.isFinite(eh) && eh > 0 ? eh : null;
}

function feeBandFor(fastest: number | null): 'low' | 'medium' | 'high' | null {
  if (fastest === null) return null;
  if (fastest < 10) return 'low';
  if (fastest < 50) return 'medium';
  return 'high';
}

function readMempoolFees(mempool: NetworkBlockMempoolStatus | undefined | null): {
  fastest: number | null;
  halfHour: number | null;
  hour: number | null;
  any: boolean;
} {
  if (!mempool || !mempool.available) {
    return { fastest: null, halfHour: null, hour: null, any: false };
  }
  const fastest = typeof mempool.fastest_fee_sat_vb === 'number' && Number.isFinite(mempool.fastest_fee_sat_vb)
    ? mempool.fastest_fee_sat_vb
    : null;
  const halfHour = typeof mempool.half_hour_fee_sat_vb === 'number' && Number.isFinite(mempool.half_hour_fee_sat_vb)
    ? mempool.half_hour_fee_sat_vb
    : null;
  const hour = typeof mempool.hour_fee_sat_vb === 'number' && Number.isFinite(mempool.hour_fee_sat_vb)
    ? mempool.hour_fee_sat_vb
    : null;
  return {
    fastest,
    halfHour,
    hour,
    any: fastest !== null || halfHour !== null || hour !== null,
  };
}

/**
 * Hook: reactive Bitcoin network context.
 *
 * Polls /api/network/block on the same cadence as CurrentBlockCard.
 * Re-renders once per second so derived ETAs stay current without
 * thrashing the network.
 */
export function useNetworkContext(): NetworkContext {
  const [block, setBlock] = useState<NetworkBlockResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 1Hz ticker — drives ETA refresh between polls without re-fetching.
  const [, setTick] = useState(0);
  const cancelledRef = useRef(false);

  useEffect(() => {
    cancelledRef.current = false;
    let timer: number | undefined;

    const load = async () => {
      try {
        const next = await api.getNetworkBlock();
        if (cancelledRef.current) return;
        setBlock(next);
        setError(null);
        const nextDelay = Math.max(MIN_POLL_MS, next.cache_ttl_ms || DEFAULT_POLL_MS);
        timer = window.setTimeout(load, nextDelay);
      } catch (err) {
        if (cancelledRef.current) return;
        setError(err instanceof Error ? err.message : 'Network block endpoint unavailable.');
        timer = window.setTimeout(load, DEFAULT_POLL_MS);
      } finally {
        if (!cancelledRef.current) setLoading(false);
      }
    };

    load();

    return () => {
      cancelledRef.current = true;
      if (timer) window.clearTimeout(timer);
    };
  }, []);

  useEffect(() => {
    const id = window.setInterval(() => setTick(t => (t + 1) & 0xffff), TICK_MS);
    return () => window.clearInterval(id);
  }, []);

  const blockHeight = pickHeight(block?.block_height ?? block?.height ?? null);
  const fees = readMempoolFees(block?.mempool);
  const feeBand = feeBandFor(fees.fastest);
  // Difficulty is reported independently of height — surface it (and the
  // derived estimate) even before a height arrives.
  const networkDifficulty = pickDifficulty(block?.difficulty ?? null);
  const networkHashrateEhEstimate = estimateNetworkHashrateEh(networkDifficulty);

  if (blockHeight === null) {
    return {
      ...EMPTY,
      networkDifficulty,
      networkHashrateEhEstimate,
      feeFastestSatVb: fees.fastest,
      feeHalfHourSatVb: fees.halfHour,
      feeHourSatVb: fees.hour,
      feesAvailable: fees.any,
      feeBand,
      loading,
      error,
    };
  }

  // ── Age ───────────────────────────────────────────────────────────
  let ageMs: number | null = null;
  const ts = block?.timestamp_ms;
  if (typeof ts === 'number' && Number.isFinite(ts) && ts > 0) {
    ageMs = Math.max(0, Date.now() - ts);
  } else if (typeof block?.age_s === 'number' && Number.isFinite(block.age_s) && block.age_s >= 0) {
    ageMs = Math.round(block.age_s * 1000);
  }

  // ── Halving math ──────────────────────────────────────────────────
  const eraIndex = Math.floor(blockHeight / HALVING_INTERVAL);
  const subsidyBtc = 50 / Math.pow(2, eraIndex);
  const blocksIntoEra = blockHeight % HALVING_INTERVAL;
  // Exactly on a halving boundary → next halving is a full interval away.
  const halvingBlocksRemaining = blocksIntoEra === 0 ? HALVING_INTERVAL : HALVING_INTERVAL - blocksIntoEra;
  const nextHalvingBlock = (eraIndex + 1) * HALVING_INTERVAL;
  const halvingEtaDays = (halvingBlocksRemaining * BLOCK_INTERVAL_SECONDS * 1000) / MS_PER_DAY;

  // ── Retarget math ─────────────────────────────────────────────────
  const epochPosition = blockHeight % RETARGET_INTERVAL;
  const epochProgressPct = (epochPosition / RETARGET_INTERVAL) * 100;
  const blocksUntilRetarget = epochPosition === 0 ? RETARGET_INTERVAL : RETARGET_INTERVAL - epochPosition;
  const retargetEtaDays = (blocksUntilRetarget * BLOCK_INTERVAL_SECONDS * 1000) / MS_PER_DAY;

  return {
    blockHeight,
    ageMs,
    halvingBlocksRemaining,
    nextHalvingBlock,
    halvingEtaDays,
    eraIndex,
    subsidyBtc,
    networkDifficulty,
    networkHashrateEhEstimate,
    epochPosition,
    epochProgressPct,
    blocksUntilRetarget,
    retargetEtaDays,
    feeFastestSatVb: fees.fastest,
    feeHalfHourSatVb: fees.halfHour,
    feeHourSatVb: fees.hour,
    feesAvailable: fees.any,
    feeBand,
    loading,
    error,
  };
}

export default useNetworkContext;
