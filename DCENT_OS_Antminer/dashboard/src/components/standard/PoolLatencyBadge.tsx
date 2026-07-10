import React from 'react';
import { InfoDot } from '../common/Tooltip';

/**
 * Per-pool latency display for the Active Pool Health list (HLA-9 truthfulness).
 *
 * Only the active pool has a measured submit->response RTT in the daemon's
 * mirrored PoolState; inactive pools have no measurement yet and must render an
 * honest "—", never a fabricated 0 ms. A value whose source is "honest_default"
 * is a fresh-boot/never-observed placeholder and is likewise treated as
 * not-measured.
 *
 * The latency value rendered here is exactly what `/api/pools` already exposes —
 * this component renders nothing the API did not provide and masks nothing the
 * API already masked.
 */
export function PoolLatencyBadge({
  latencyMs,
  latencyMeasured,
  latencyMsSource,
  poolId,
}: {
  latencyMs?: number | null;
  latencyMeasured?: boolean;
  latencyMsSource?: string;
  poolId?: number;
}) {
  const measured =
    latencyMeasured === true &&
    typeof latencyMs === 'number' &&
    latencyMsSource !== 'honest_default';

  return (
    <span
      style={{ color: 'var(--text-dim)' }}
      data-testid={poolId === undefined ? 'pool-latency' : `pool-latency-${poolId}`}
    >
      Latency <InfoDot term="pool_latency_ms" size={11} />:{' '}
      <span style={{
        fontFamily: "'JetBrains Mono', monospace",
        color: measured ? 'var(--text-secondary)' : 'var(--text-dim)',
      }}>
        {measured ? `${Math.round(latencyMs as number)} ms` : '—'}
      </span>
    </span>
  );
}
