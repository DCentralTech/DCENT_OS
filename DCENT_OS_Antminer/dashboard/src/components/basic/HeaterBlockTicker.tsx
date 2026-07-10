import React, { useEffect, useRef, useState } from 'react';
import { api } from '../../api/client';
import type { NetworkBlockResponse } from '../../api/types';
import { glossaryText } from '../../utils/glossary';

/**
 * BTC block ticker pill for the Heater header — anchors the heater as a
 * Bitcoin miner ("heat is the proof, sats are the receipt").
 *
 * Truth-contract: the block height + age come from the SAME read-only
 * `/api/network/block` source CurrentBlockCard uses. The prototype faked an
 * incrementing "Xs ago" counter — production does NOT. Age is derived from
 * the real `age_s` / `timestamp_ms` plus locally-elapsed wall time since the
 * real `fetched_at_ms` (honest extrapolation of a real timestamp, not a
 * fabricated number). When the source is unavailable the pill says so
 * plainly instead of inventing a height.
 */
function fmtAge(totalS: number): string {
  if (totalS < 0) return 'just now';
  if (totalS < 60) return `${Math.floor(totalS)}s ago`;
  const m = Math.floor(totalS / 60);
  if (m < 60) return `${m}m ${Math.floor(totalS % 60)}s ago`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m ago`;
}

export function HeaterBlockTicker() {
  const [block, setBlock] = useState<NetworkBlockResponse | null>(null);
  const [failed, setFailed] = useState(false);
  const [, force] = useState(0);
  const fetchedAtRef = useRef<number>(Date.now());

  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const b = await api.getNetworkBlock();
        if (!alive) return;
        setBlock(b);
        setFailed(false);
        fetchedAtRef.current = Date.now();
      } catch {
        if (alive) setFailed(true);
      }
    };
    void load();
    const poll = setInterval(load, 60_000);
    const tick = setInterval(() => force(n => n + 1), 1000);
    return () => {
      alive = false;
      clearInterval(poll);
      clearInterval(tick);
    };
  }, []);

  const height = block?.block_height ?? block?.height ?? null;
  const available = block?.available === true && typeof height === 'number';

  // Real baseline age + locally-elapsed time since the real fetch.
  let ageText: string | null = null;
  if (available) {
    const elapsedSinceFetch = (Date.now() - fetchedAtRef.current) / 1000;
    if (typeof block?.age_s === 'number' && block.age_s >= 0) {
      ageText = fmtAge(block.age_s + elapsedSinceFetch);
    } else if (typeof block?.timestamp_ms === 'number' && block.timestamp_ms > 0) {
      ageText = fmtAge((Date.now() - block.timestamp_ms) / 1000);
    }
  }

  if (failed && !block) {
    return (
      <span className="heater-block-pill is-unavailable" data-tooltip={glossaryText('block_height')}>
        <span className="heater-block-pill-eyebrow">BTC block</span>
        <span className="heater-block-pill-na">unavailable</span>
      </span>
    );
  }

  return (
    <span
      className={`heater-block-pill${available ? '' : ' is-unavailable'}`}
      data-tooltip="Latest Bitcoin block your miner is competing for. Heat is the proof, sats are the receipt."
    >
      <span className="heater-block-pill-dot" aria-hidden="true" />
      <span className="heater-block-pill-eyebrow">BTC block</span>
      {available ? (
        <>
          <strong className="heater-block-pill-height">#{height!.toLocaleString()}</strong>
          {ageText && <span className="heater-block-pill-age">· {ageText}</span>}
        </>
      ) : (
        <span className="heater-block-pill-na">{block ? 'unavailable' : '…'}</span>
      )}
    </span>
  );
}
