/**
 *  HIGH-2 (2026-05-24) — chain presence + chip-rail mV panel.
 *
 * Two operator-truth surfaces that replace the lying
 * `mining_enabled = hashrate_ghs > 0` derived state on partial-chain
 *  runs:
 *
 * 1. `ChainPresencePanel` — per-chain "chips_responding / chips_expected"
 *    pill (green ≥90%, yellow 50-89%, red <50%) with `chip_enum_handoff_caveat`
 *    InfoDot. Honest about partial-chain reality ( sometimes shows
 *    34/126 on .25 and STILL produces accepted shares — operators must not
 *    interpret that as "broken").
 *
 * 2. `ChipRailMvPill` — per-chain mV actual-vs-target pill (green within
 *    ±200 mV of target, yellow 50-99%, red <50% with a load-bearing
 *    caption pointing the operator at "dsPIC may be in fw=0x82 —
 *    recipe broken?"). Surfaces the dsPIC fw=0x82-vs-fw=0x89 dual-state
 *    model from .
 *
 * Both poll `/api/mining/chain/presence` (Part B). On older daemons /
 * non-`a lab unit` units the endpoint returns null → both components render
 * gracefully empty (no regression).
 */

import { useEffect, useRef, useState } from 'react';
import { api } from '../../api/client';
import type { ChainPresenceResponse } from '../../api/types';
import { InfoDot } from './Tooltip';
import { chainOrdinal } from '../../utils/format';

const POLL_INTERVAL_MS = 10_000;
// Data older than ~2 poll intervals (or a tick that just failed) is surfaced as
// stale so the chip-presence / chip-rail pills can never silently freeze at a
// last-good value and read as live (the panel's anti-"lying-state" purpose).
const STALE_AFTER_MS = POLL_INTERVAL_MS * 2;

function pillColor(ratio: number): { fg: string; bg: string; border: string } {
  if (ratio >= 0.9) {
    return {
      fg: 'var(--green, #2DD4A0)',
      bg: 'rgba(45, 212, 160, 0.10)',
      border: 'rgba(45, 212, 160, 0.32)',
    };
  }
  if (ratio >= 0.5) {
    return {
      fg: 'var(--amber, #F59E0B)',
      bg: 'rgba(245, 158, 11, 0.10)',
      border: 'rgba(245, 158, 11, 0.36)',
    };
  }
  return {
    fg: 'var(--red, #EF4444)',
    bg: 'rgba(239, 68, 68, 0.12)',
    border: 'rgba(239, 68, 68, 0.45)',
  };
}

interface ChainPresenceState {
  /** Last-known presence snapshot, or null until the first success. */
  data: ChainPresenceResponse | null;
  /** Epoch ms of the most recent successful fetch, or null if none yet. */
  lastSuccessTs: number | null;
  /** True when the latest tick failed OR data is older than ~2 poll intervals. */
  stale: boolean;
  /** Seconds since the last successful fetch (rounded), or null if none yet. */
  staleForSec: number | null;
}

/**
 * DASH-STATE-3: poll `/api/mining/chain/presence` and expose staleness.
 *
 * `api.getChainPresence` already maps 404/501 → null (old daemon / non-`a lab unit`
 * units), so ANY error reaching this hook's catch is a REAL failure. Previously
 * the catch silently kept the last-known data with no visible difference from
 * live — undercutting the panel's anti-"lying-state" purpose. Now a failed tick
 * (or data older than ~2 poll intervals) is surfaced via `stale`, and the
 * components render a small "stale · updated Ns ago" chip mirroring SvgChart's
 * STALE-badge affordance.
 */
function useChainPresence(): ChainPresenceState {
  const [data, setData] = useState<ChainPresenceResponse | null>(null);
  const [lastSuccessTs, setLastSuccessTs] = useState<number | null>(null);
  // True only when the most recent tick threw (a real failure). Separate from
  // the age-based check below so a long-lived but never-refreshed value still
  // reads stale.
  const [lastTickFailed, setLastTickFailed] = useState(false);
  // Re-render roughly each poll interval so the age-based stale check (and the
  // "updated Ns ago" caption) stay current even when no new data arrives.
  const [, setNowTick] = useState(0);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    const tick = async () => {
      try {
        const next = await api.getChainPresence();
        if (!mountedRef.current) return;
        setData(next);
        setLastSuccessTs(Date.now());
        setLastTickFailed(false);
      } catch {
        // Reached only on a real failure (404/501 are mapped to null upstream).
        // Keep the last-known data but flag the tick as failed so the UI can
        // show staleness instead of pretending the frozen value is live.
        if (mountedRef.current) setLastTickFailed(true);
      }
    };
    void tick();
    const timer = setInterval(tick, POLL_INTERVAL_MS);
    // Independent age ticker so staleness becomes visible even if the poll
    // itself stops resolving entirely.
    const ager = setInterval(() => {
      if (mountedRef.current) setNowTick(n => n + 1);
    }, POLL_INTERVAL_MS);
    return () => {
      mountedRef.current = false;
      clearInterval(timer);
      clearInterval(ager);
    };
  }, []);

  const ageMs = lastSuccessTs == null ? null : Date.now() - lastSuccessTs;
  const stale = lastTickFailed || ageMs == null || ageMs > STALE_AFTER_MS;
  const staleForSec = ageMs == null ? null : Math.round(ageMs / 1000);

  return { data, lastSuccessTs, stale, staleForSec };
}

/** Compact stale indicator mirroring the SvgChart STALE-badge affordance. */
function StalePill({ staleForSec }: { staleForSec: number | null }) {
  const updated = staleForSec == null
    ? 'no successful update yet'
    : `updated ${staleForSec}s ago`;
  return (
    <span
      data-testid="chain-presence-stale"
      role="status"
      title={`Chain presence telemetry is stale — last values shown are the last known, not live (${updated}).`}
      style={{
        display: 'inline-flex',
        gap: 5,
        alignItems: 'center',
        padding: '2px 8px',
        borderRadius: 999,
        background: 'rgba(245, 158, 11, 0.12)',
        border: '1px solid rgba(245, 158, 11, 0.36)',
        color: 'var(--amber, #F59E0B)',
        fontFamily: "'JetBrains Mono', monospace",
        fontSize: '0.68rem',
        fontWeight: 700,
        letterSpacing: '0.04em',
        textTransform: 'uppercase',
      }}
    >
      stale
      <span style={{ opacity: 0.8, fontWeight: 500, textTransform: 'none', letterSpacing: 0 }}>
        · {updated}
      </span>
    </span>
  );
}

export function ChainPresencePanel() {
  const { data: presence, stale, staleForSec } = useChainPresence();
  if (!presence || presence.chains.length === 0) return null;

  return (
    <section
      aria-label="Chain presence (chips responding vs expected)"
      data-testid="chain-presence-panel"
      data-stale={stale ? 'true' : 'false'}
      style={{
        margin: '8px 0',
        padding: '10px 12px',
        border: '1px solid rgba(255,255,255,0.08)',
        borderRadius: 8,
        background: 'rgba(0,0,0,0.18)',
        fontFamily: "'Inter', sans-serif",
      }}
    >
      <div style={{ display: 'flex', gap: 6, alignItems: 'center', marginBottom: 8 }}>
        <strong style={{ fontSize: '0.82rem', color: 'var(--text, #E8E8E8)' }}>
          Chain presence
        </strong>
        <InfoDot term="chip_enum_handoff_caveat" />
        {stale && (
          <span style={{ marginLeft: 'auto' }}>
            <StalePill staleForSec={staleForSec} />
          </span>
        )}
      </div>
      <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
        {presence.chains.map(chain => {
          const ratio = chain.chips_expected > 0
            ? chain.chips_responding / chain.chips_expected
            : 0;
          const tone = pillColor(ratio);
          const pct = chain.chips_expected > 0
            ? Math.round(ratio * 100)
            : 0;
          return (
            <div
              key={chain.idx}
              data-chain-idx={chain.idx}
              data-presence-ratio={ratio.toFixed(2)}
              style={{
                display: 'inline-flex',
                gap: 8,
                alignItems: 'center',
                padding: '4px 10px',
                borderRadius: 999,
                background: tone.bg,
                border: `1px solid ${tone.border}`,
                color: tone.fg,
                fontFamily: "'JetBrains Mono', monospace",
                fontSize: '0.76rem',
                fontWeight: 700,
              }}
              title={`Chain ${chainOrdinal(chain.idx)} (array idx ${chain.idx}): ${chain.chips_responding}/${chain.chips_expected} (${pct}%)`}
            >
              <span style={{ color: 'var(--text-dim, #7C7C86)', fontWeight: 500 }}>
                chain {chainOrdinal(chain.idx)}
              </span>
              <span>
                {chain.chips_responding}/{chain.chips_expected}
              </span>
              <span style={{ opacity: 0.7 }}>{pct}%</span>
            </div>
          );
        })}
      </div>
    </section>
  );
}

/**
 * Chip-rail mV pill — designed to sit inline next to the PSU row in
 * `HardwareInfoPanel`. Compact single-pill rendering (no panel chrome)
 * so it fits cleanly into the existing PSU strip.
 */
export function ChipRailMvPill() {
  const { data: presence, stale, staleForSec } = useChainPresence();
  if (!presence || presence.chains.length === 0) return null;

  // Aggregate across chains: worst pill wins (operator sees the worst-case
  // by design). If any chain has mv_actual==null, that's a yellow signal
  // (we have no telemetry — usually dsPIC bootloader echo state).
  let worstChain = presence.chains[0];
  for (const chain of presence.chains) {
    const worstActual = worstChain.mv_actual;
    const chainActual = chain.mv_actual;
    if (worstActual == null) continue;
    if (chainActual == null) {
      worstChain = chain;
      continue;
    }
    const worstTarget = worstChain.mv_target ?? 13700;
    const chainTarget = chain.mv_target ?? 13700;
    const worstRatio = worstActual / worstTarget;
    const chainRatio = chainActual / chainTarget;
    if (chainRatio < worstRatio) worstChain = chain;
  }

  const target = worstChain.mv_target ?? 13700;
  const actual = worstChain.mv_actual;
  let tone: { fg: string; bg: string; border: string };
  let caption: string | null = null;
  let pctLabel: string;

  if (actual == null) {
    tone = pillColor(0); // red
    pctLabel = '— mV';
    caption = 'dsPIC may be in fw=0x82 — handoff recipe broken?';
  } else {
    const ratio = target > 0 ? actual / target : 0;
    const within200 = target > 0 && Math.abs(actual - target) <= 200;
    if (within200) {
      tone = pillColor(0.95);
    } else if (ratio >= 0.5) {
      tone = pillColor(0.6);
    } else {
      tone = pillColor(0);
      caption = 'dsPIC may be in fw=0x82 — handoff recipe broken?';
    }
    pctLabel = `${actual} mV / ${target} mV`;
  }

  return (
    <div
      data-testid="chip-rail-mv-pill"
      data-chain-idx={worstChain.idx}
      data-stale={stale ? 'true' : 'false'}
      style={{
        display: 'inline-flex',
        flexDirection: 'column',
        gap: 3,
        alignItems: 'flex-start',
      }}
    >
      <div
        style={{
          display: 'inline-flex',
          gap: 6,
          alignItems: 'center',
          padding: '3px 8px',
          borderRadius: 999,
          background: tone.bg,
          border: `1px solid ${tone.border}`,
          color: tone.fg,
          fontFamily: "'JetBrains Mono', monospace",
          fontSize: '0.74rem',
          fontWeight: 700,
        }}
        title={`Chain ${worstChain.idx} chip rail: ${pctLabel}`}
      >
        <span style={{ color: 'var(--text-dim, #7C7C86)', fontWeight: 500 }}>
          chip rail
        </span>
        <span>{pctLabel}</span>
        <InfoDot term="chain_rail_mv_xil25" />
      </div>
      {stale && <StalePill staleForSec={staleForSec} />}
      {caption && (
        <span
          style={{
            fontSize: '0.7rem',
            color: 'var(--red, #EF4444)',
            fontFamily: "'Inter', sans-serif",
          }}
        >
          {caption}
        </span>
      )}
    </div>
  );
}
