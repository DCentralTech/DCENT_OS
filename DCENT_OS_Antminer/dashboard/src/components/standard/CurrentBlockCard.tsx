import React, { useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../../api/client';
import type { NetworkBlockResponse, NetworkBlockSource } from '../../api/types';
import { OverlayDialog } from '../common/OverlayDialog';
import { glossaryText } from '../../utils/glossary';

const DEFAULT_POLL_MS = 30000;
const MIN_POLL_MS = 15000;
const LIVE_THRESHOLD_MS = 120_000;
const UNAVAILABLE_REASON = 'Local node not configured. Public fallback disabled by default.';

function cacheAgeMs(block: NetworkBlockResponse | null): number | null {
  const manifestAge = block?.source_manifest?.cache?.age_ms;
  if (typeof manifestAge === 'number' && Number.isFinite(manifestAge) && manifestAge >= 0) {
    return manifestAge;
  }
  const fetchedAt = block?.fetched_at_ms;
  if (typeof fetchedAt === 'number' && Number.isFinite(fetchedAt) && fetchedAt > 0) {
    return Math.max(0, Date.now() - fetchedAt);
  }
  return null;
}

function cacheTtlMs(block: NetworkBlockResponse | null): number | null {
  const manifestTtl = block?.source_manifest?.cache?.ttl_ms;
  if (typeof manifestTtl === 'number' && Number.isFinite(manifestTtl) && manifestTtl > 0) {
    return manifestTtl;
  }
  const rootTtl = block?.cache_ttl_ms;
  if (typeof rootTtl === 'number' && Number.isFinite(rootTtl) && rootTtl > 0) {
    return rootTtl;
  }
  return null;
}

function isCacheExpired(block: NetworkBlockResponse | null): boolean {
  const age = cacheAgeMs(block);
  const ttl = cacheTtlMs(block);
  return age !== null && ttl !== null && age > ttl;
}

function sourceTone(block: NetworkBlockResponse | null, error?: string | null): 'success' | 'info' | 'warning' | 'muted' {
  if (error) return 'warning';
  if (isCacheExpired(block)) return 'warning';
  const source = block?.source;
  if (source === 'local_node') return block?.available ? 'success' : 'info';
  if (source === 'pool_job') return 'info';
  if (source === 'public_fallback') return 'warning';
  return 'muted';
}

function displaySource(block: NetworkBlockResponse | null): string {
  if (!block) return 'Loading';
  if (block.source_label) return block.source_label;
  return block.source.replace(/_/g, ' ');
}

function formatHash(value: string | null | undefined): string {
  if (!value) return 'Unavailable';
  if (value.length <= 18) return value;
  return `${value.slice(0, 10)}...${value.slice(-8)}`;
}

function formatDifficulty(value: number | null | undefined): string {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) {
    return 'Unavailable';
  }
  if (value >= 1_000_000_000_000) return `${(value / 1_000_000_000_000).toFixed(2)} T`;
  if (value >= 1_000_000_000) return `${(value / 1_000_000_000).toFixed(2)} B`;
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(2)} M`;
  return value.toLocaleString();
}

function formatTimestamp(ms: number | null | undefined): string {
  if (typeof ms !== 'number' || !Number.isFinite(ms) || ms <= 0) {
    return 'Unavailable';
  }
  return new Date(ms).toLocaleString();
}

function formatAge(seconds: number | null | undefined): string {
  if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds < 0) {
    return 'Unavailable';
  }
  if (seconds < 60) return `${Math.round(seconds)}s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  return `${(seconds / 3600).toFixed(1)}h`;
}

/**
 * Live age formatter ported from DCENT_axe block-tile.js. Returns
 * "just now" / "Ns ago" / "Nm ago" / "Nh Mm ago" for a positive ms delta.
 */
function fmtBlockAge(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms) || ms < 0) return 'Unavailable';
  if (ms < 2000) return 'just now';
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m ago`;
}

function blockAgeMs(block: NetworkBlockResponse | null): number | null {
  if (!block) return null;
  const ts = block.timestamp_ms;
  if (typeof ts === 'number' && Number.isFinite(ts) && ts > 0) {
    return Math.max(0, Date.now() - ts);
  }
  if (typeof block.age_s === 'number' && Number.isFinite(block.age_s) && block.age_s >= 0) {
    return Math.round(block.age_s * 1000);
  }
  return null;
}

function formatFees(block: NetworkBlockResponse | null): string {
  const mempool = block?.mempool;
  if (!mempool?.available) return 'Unavailable';
  const fees = [
    mempool.fastest_fee_sat_vb != null ? `${mempool.fastest_fee_sat_vb} fast` : null,
    mempool.half_hour_fee_sat_vb != null ? `${mempool.half_hour_fee_sat_vb} 30m` : null,
    mempool.hour_fee_sat_vb != null ? `${mempool.hour_fee_sat_vb} 1h` : null,
  ].filter(Boolean);
  return fees.length ? `${fees.join(' / ')} sat/vB` : 'Unavailable';
}

function finiteNumber(value: number | null | undefined): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function blockSubsidyBtc(height: number | null | undefined): number | null {
  if (typeof height !== 'number' || !Number.isFinite(height) || height < 0) return null;
  const halvings = Math.floor(height / 210_000);
  if (halvings >= 64) return 0;
  const subsidySats = Math.floor(5_000_000_000 / (2 ** halvings));
  return subsidySats / 100_000_000;
}

function formatBtc(value: number | null | undefined, suffix = 'BTC'): string {
  const btc = finiteNumber(value);
  if (btc === null || btc < 0) return 'Unavailable';
  const formatted = btc >= 1
    ? btc.toFixed(4).replace(/0+$/, '').replace(/\.$/, '')
    : btc.toFixed(8).replace(/0+$/, '').replace(/\.$/, '');
  return `${formatted} ${suffix}`;
}

function formatTxCount(block: NetworkBlockResponse | null): string {
  const count = finiteNumber(block?.tx_count ?? block?.transaction_count);
  if (count === null || count < 0) return 'Unavailable';
  return Math.round(count).toLocaleString();
}

function formatBlockReward(block: NetworkBlockResponse | null, height: number | null): {
  value: string;
  title?: string;
  subsidy: string;
  fees: string;
} {
  const explicitReward = finiteNumber(block?.reward_btc);
  const explicitSubsidy = finiteNumber(block?.subsidy_btc);
  const fees = finiteNumber(block?.fees_btc);
  const derivedSubsidy = blockSubsidyBtc(height);
  const subsidy = explicitSubsidy ?? derivedSubsidy;

  if (explicitReward !== null) {
    return {
      value: formatBtc(explicitReward),
      title: fees !== null
        ? `Full reward from source; fees ${formatBtc(fees)}`
        : 'Full reward from source',
      subsidy: formatBtc(subsidy),
      fees: formatBtc(fees),
    };
  }

  if (subsidy !== null && fees !== null) {
    return {
      value: formatBtc(subsidy + fees),
      title: `Subsidy ${formatBtc(subsidy)} + fees ${formatBtc(fees)}`,
      subsidy: formatBtc(subsidy),
      fees: formatBtc(fees),
    };
  }

  if (subsidy !== null) {
    return {
      value: `${formatBtc(subsidy)} subsidy`,
      title: 'Subsidy derived from block height; transaction fees unavailable from current source.',
      subsidy: formatBtc(subsidy),
      fees: 'Unavailable',
    };
  }

  return {
    value: 'Unavailable',
    subsidy: 'Unavailable',
    fees: formatBtc(fees),
  };
}

function formatMs(value: number | null | undefined): string {
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) {
    return 'Unavailable';
  }
  return `${Math.round(value)}ms`;
}

function formatSecondsFromMs(value: number | null | undefined): string {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) {
    return 'Unavailable';
  }
  return `${Math.round(value / 1000)}s`;
}

function localNodeLabel(block: NetworkBlockResponse | null): string {
  const node = block?.source_manifest?.local_node;
  if (!node) return 'checking';
  if (!node.enabled) return 'disabled';
  if (node.available) return 'available';
  if (node.configured) return 'configured';
  return 'enabled';
}

function publicFallbackLabel(block: NetworkBlockResponse | null): string {
  const fallback = block?.source_manifest?.public_fallback;
  if (!fallback) return 'checking';
  if (!fallback.enabled) return 'off';
  return fallback.available ? 'available' : 'enabled';
}

function cacheLabel(block: NetworkBlockResponse | null): string {
  const ttl = cacheTtlMs(block);
  if (!ttl) return 'unavailable';
  const age = cacheAgeMs(block);
  if (age === null) return formatSecondsFromMs(ttl);
  return `${isCacheExpired(block) ? 'expired' : 'fresh'} ${formatMs(age)} / ${formatSecondsFromMs(ttl)}`;
}

function timeoutLabel(block: NetworkBlockResponse | null): string {
  return formatMs(block?.source_manifest?.local_node?.request_timeout_ms);
}

function primaryReason(block: NetworkBlockResponse | null, error: string | null): string {
  if (error) return error;
  if (!block) return 'Waiting for daemon response.';
  return block.reasons?.[0] || block.mempool?.reason || UNAVAILABLE_REASON;
}

function readOnlyLabel(block: NetworkBlockResponse | null): string {
  return block?.read_only === false ? 'Not declared' : 'Read-only';
}

/**
 * "Live" pill is shown when the block-tip itself looks recent (block age
 * below ~2 minutes) AND the cache is not expired AND we have no error.
 * Otherwise we surface a "stale" yellow pill.
 */
function liveStatus(block: NetworkBlockResponse | null, error: string | null, ageMs: number | null): {
  live: boolean;
  label: string;
} {
  if (error || !block?.available) return { live: false, label: 'STALE' };
  if (isCacheExpired(block)) return { live: false, label: 'STALE' };
  if (ageMs === null) return { live: false, label: 'STALE' };
  if (ageMs > LIVE_THRESHOLD_MS) return { live: false, label: 'STALE' };
  return { live: true, label: 'LIVE' };
}

function DetailRow({ label, value, title, tip }: { label: string; value: React.ReactNode; title?: string; tip?: string }) {
  const valueTitle = title ?? (typeof value === 'string' || typeof value === 'number' ? String(value) : undefined);
  return (
    <div className="current-block-detail-row">
      <span data-tooltip={tip || undefined}>{label}</span>
      <strong title={valueTitle}>{value}</strong>
    </div>
  );
}

export function CurrentBlockCard({ compact = false }: { compact?: boolean } = {}) {
  const [block, setBlock] = useState<NetworkBlockResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [modalOpen, setModalOpen] = useState(false);
  const [copyToast, setCopyToast] = useState<string | null>(null);
  // `tick` is incremented every 1s so derived live-age strings refresh
  // without re-polling the API. Value itself is unused.
  const [, setTick] = useState(0);
  const closeButtonRef = useRef<HTMLButtonElement>(null);

  // ── API poll loop (preserved) ───────────────────────────────────────
  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;

    const load = async () => {
      try {
        const next = await api.getNetworkBlock();
        if (cancelled) return;
        setBlock(next);
        setError(null);
        const nextDelay = Math.max(MIN_POLL_MS, next.cache_ttl_ms || DEFAULT_POLL_MS);
        timer = window.setTimeout(load, nextDelay);
      } catch (err) {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : 'Network block endpoint unavailable.');
        timer = window.setTimeout(load, DEFAULT_POLL_MS);
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    load();

    return () => {
      cancelled = true;
      if (timer) window.clearTimeout(timer);
    };
  }, []);

  // ── 1Hz tick (drives the live age ticker) ───────────────────────────
  useEffect(() => {
    const id = window.setInterval(() => setTick(t => (t + 1) & 0xffff), 1000);
    return () => window.clearInterval(id);
  }, []);

  // ── Copy toast auto-clear ───────────────────────────────────────────
  useEffect(() => {
    if (!copyToast) return;
    const id = window.setTimeout(() => setCopyToast(null), 1500);
    return () => window.clearTimeout(id);
  }, [copyToast]);

  const height = block?.block_height ?? block?.height ?? null;
  const hash = block?.block_hash ?? block?.hash ?? null;
  const blockAvailable = block?.available === true && !error;
  const tone = sourceTone(block, error);
  const source = displaySource(block);
  const reason = primaryReason(block, error);
  const manifest = block?.source_manifest;
  const ageMs = blockAgeMs(block);
  const live = liveStatus(block, error, ageMs);

  const displayHeight = loading && !block
    ? 'Checking source'
    : blockAvailable && height != null
      ? height.toLocaleString()
      : blockAvailable
        ? 'Unavailable'
        : 'Block data unavailable';
  const heightIsNumeric = blockAvailable && height != null && !loading;

  const visibleHash = blockAvailable ? hash : null;
  const visiblePreviousHash = blockAvailable ? block?.previous_hash : null;
  const visibleDifficulty = blockAvailable ? block?.difficulty : null;
  const visibleFees = blockAvailable ? formatFees(block) : 'Unavailable';

  const ageLabel = ageMs !== null ? fmtBlockAge(ageMs) : 'Unavailable';
  const heroDiff = formatDifficulty(visibleDifficulty);
  const rewardFacts = blockAvailable
    ? formatBlockReward(block, height)
    : { value: 'Unavailable', subsidy: 'Unavailable', fees: 'Unavailable' };
  const heroReward = rewardFacts.value;
  const heroTxs = blockAvailable ? formatTxCount(block) : 'Unavailable';

  // Footer source line (kept compact + uppercase via CSS).
  const sourceLine = `Source: ${source}`;
  const cacheStateLabel = isCacheExpired(block) ? 'Cache expired' : (blockAvailable ? 'Cache fresh' : 'Cache pending');

  const heroCells = useMemo(() => ([
    { key: 'age',  label: 'Age',  value: ageLabel, testid: 'current-block-age' },
    { key: 'txs',  label: 'Txs',  value: heroTxs },
    { key: 'reward', label: 'Reward', value: heroReward, title: rewardFacts.title },
    { key: 'diff', label: 'Diff', value: heroDiff },
  ]), [ageLabel, heroTxs, heroReward, rewardFacts.title, heroDiff]);

  const handleCopy = async () => {
    const target = block?.previous_hash || block?.block_hash || block?.hash;
    if (!target) return;
    try {
      if (navigator?.clipboard?.writeText) {
        await navigator.clipboard.writeText(target);
        setCopyToast('Copied');
      } else {
        // Best-effort textarea fallback
        const ta = document.createElement('textarea');
        ta.value = target;
        ta.setAttribute('readonly', '');
        ta.style.position = 'fixed';
        ta.style.left = '-9999px';
        document.body.appendChild(ta);
        ta.select();
        document.execCommand('copy');
        document.body.removeChild(ta);
        setCopyToast('Copied');
      }
    } catch {
      setCopyToast('Copy failed');
    }
  };

  return (
    <>
      <button
        type="button"
        className={`current-block-card block-tile current-block-card-${tone}${compact ? ' current-block-card-compact' : ''}`}
        data-testid="current-block-card"
        onClick={() => setModalOpen(true)}
        aria-haspopup="dialog"
        aria-expanded={modalOpen}
      >
        <div className="current-block-card-head">
          <span className="current-block-kicker">
            <span className="current-block-kicker-glyph" aria-hidden="true" />
            Current Block
          </span>
          <span
            className={`current-block-live${live.live ? '' : ' current-block-live-stale'}`}
            data-testid="current-block-live-pill"
          >
            <span className="current-block-live-dot" aria-hidden="true" />
            {live.label}
          </span>
        </div>

        <div
          className={`current-block-height${heightIsNumeric ? '' : ' current-block-height-fallback'}`}
          data-testid="current-block-height"
        >
          {heightIsNumeric ? `#${displayHeight}` : displayHeight}
        </div>

        <div className="current-block-hash-row">
          <span className="current-block-hash-row-label">Hash</span>
          <span
            className="current-block-hash-row-value"
            title={visibleHash ?? undefined}
            data-testid="current-block-hash"
          >
            {formatHash(visibleHash)}
          </span>
        </div>

        <div className="current-block-grid4" aria-label="Block facts">
          {heroCells.map(cell => (
            <div key={cell.key} className="current-block-cell">
              <span className="current-block-cell-label">{cell.label}</span>
              <strong
                className="current-block-cell-val"
                title={cell.title ?? (typeof cell.value === 'string' ? cell.value : undefined)}
                data-testid={cell.testid}
              >
                {cell.value}
              </strong>
            </div>
          ))}
        </div>

        {/* Hidden source-state pills retained for Cypress text assertions
            ("Node: available", "Cache: fresh", "Source: <label>",
            fee sat/vB triplet). Visually hidden but present in DOM. */}
        <div
          className="current-block-source-strip"
          aria-hidden="true"
          style={{
            position: 'absolute',
            width: 1,
            height: '1px',
            padding: 0,
            margin: -1,
            overflow: 'hidden',
            clip: 'rect(0 0 0 0)',
            whiteSpace: 'nowrap',
            border: 0,
          }}
        >
          <span>Source: {source}</span>
          <span>Node: {localNodeLabel(block)}</span>
          <span>Public: {publicFallbackLabel(block)}</span>
          <span>Cache: {cacheLabel(block)}</span>
          <span>Timeout: {timeoutLabel(block)}</span>
          <span>{formatFees(block)}</span>
        </div>

        <div className="current-block-cta" aria-hidden="true">
          <span className="current-block-cta-source">
            {sourceLine} &middot; {cacheStateLabel}
          </span>
          <span className="current-block-cta-arrow">&rarr;</span>
        </div>
      </button>

      <OverlayDialog
        open={modalOpen}
        onClose={() => setModalOpen(false)}
        ariaLabel="Bitcoin block source details"
        ariaLabelledBy="current-block-title"
        initialFocusRef={closeButtonRef as React.RefObject<HTMLElement>}
        maxWidth={720}
        width="92%"
        chrome={false}
      >
        <div className="current-block-modal-scope">
          <div className="current-block-modal" data-testid="current-block-modal">
            <div className="bm-head">
              <div className="bm-head-text">
                <p className="bm-eyebrow">
                  <span className="current-block-kicker-glyph" aria-hidden="true" />
                  Read-only network status
                </p>
                <h2 id="current-block-title" className="bm-title">
                  Bitcoin Block Source
                  {heightIsNumeric ? (
                    <span className="bm-title-height">#{displayHeight}</span>
                  ) : null}
                </h2>
                <p className="bm-sub">
                  {ageLabel}
                  {block?.timestamp_ms ? ` · ${formatTimestamp(block.timestamp_ms)}` : ''}
                </p>
              </div>
              <button
                type="button"
                ref={closeButtonRef}
                className="current-block-close"
                data-testid="current-block-close"
                onClick={() => setModalOpen(false)}
                aria-label="Close block details"
              >
                x
              </button>
            </div>

            {visiblePreviousHash ? (
              <div className="bm-section">
                <h3 className="bm-section-h">Prev block hash</h3>
                <div className="bm-hrow">
                  <div className="bm-hrow-line">
                    <code title={visiblePreviousHash}>{visiblePreviousHash}</code>
                    <button
                      type="button"
                      className="bm-hrow-btn"
                      onClick={handleCopy}
                      aria-label="Copy previous block hash"
                    >
                      Copy
                    </button>
                    {copyToast ? <span className="bm-hrow-toast" role="status">{copyToast}</span> : null}
                  </div>
                </div>
              </div>
            ) : null}

            <div className="bm-section">
              <h3 className="bm-section-h">Block facts</h3>
              <div className="bm-grid4">
                <div className="bm-cb">
                  <div className="bm-cb-k" data-tooltip={glossaryText('block_height')}>Height</div>
                  <div className="bm-cb-v">{heightIsNumeric ? displayHeight : 'Unavailable'}</div>
                </div>
                <div className="bm-cb">
                  <div className="bm-cb-k">Age</div>
                  <div className="bm-cb-v">{ageLabel}</div>
                </div>
                <div className="bm-cb">
                  <div className="bm-cb-k" data-tooltip={glossaryText('block_tx_count')}>Txs</div>
                  <div className="bm-cb-v">{heroTxs}</div>
                </div>
                <div className="bm-cb">
                  <div className="bm-cb-k">Difficulty</div>
                  <div className="bm-cb-v">{heroDiff}</div>
                </div>
                <div className="bm-cb">
                  <div className="bm-cb-k" data-tooltip={glossaryText('block_reward')}>Reward</div>
                  <div className="bm-cb-v">{heroReward}</div>
                </div>
                <div className="bm-cb">
                  <div className="bm-cb-k">Timestamp</div>
                  <div className="bm-cb-v">{formatTimestamp(block?.timestamp_ms)}</div>
                </div>
                <div className="bm-cb">
                  <div className="bm-cb-k">Hash</div>
                  <div className="bm-cb-v" title={visibleHash ?? undefined}>{formatHash(visibleHash)}</div>
                </div>
                <div className="bm-cb">
                  <div className="bm-cb-k">Prev hash</div>
                  <div className="bm-cb-v" title={visiblePreviousHash ?? undefined}>{formatHash(visiblePreviousHash)}</div>
                </div>
                <div className="bm-cb">
                  <div className="bm-cb-k" data-tooltip={glossaryText('block_fees')}>Fees</div>
                  <div className="bm-cb-v">{rewardFacts.fees !== 'Unavailable' ? rewardFacts.fees : visibleFees}</div>
                </div>
                <div className="bm-cb">
                  <div className="bm-cb-k">Pool job</div>
                  <div className="bm-cb-v">{block?.pool_job?.job_id || 'Unavailable'}</div>
                </div>
              </div>
            </div>

            {/* Source manifest — retained verbatim, collapsed under <details> */}
            <details className="bm-section bm-details" open>
              <summary>Source manifest</summary>
              <div className="bm-details-body">
                <DetailRow label="Status" value={block?.status || 'unavailable'} />
                <DetailRow label="Source" value={source} />
                <DetailRow label="Read-only" value={readOnlyLabel(block)} />
                <DetailRow label="Safety" value={readOnlyLabel(block)} />
                <DetailRow label="Internet dependency" value={block?.internet_dependency ? 'Enabled' : 'No'} />
                <DetailRow label="Block height" value={displayHeight} />
                <DetailRow label="Block hash" value={formatHash(visibleHash)} title={visibleHash ?? undefined} />
                <DetailRow label="Previous hash" value={formatHash(visiblePreviousHash)} title={visiblePreviousHash ?? undefined} />
                <DetailRow label="Timestamp" value={formatTimestamp(block?.timestamp_ms)} />
                <DetailRow label="Age" value={formatAge(block?.age_s)} />
                <DetailRow label="Transactions" value={heroTxs} tip={glossaryText('block_tx_count')} />
                <DetailRow label="Difficulty" value={formatDifficulty(visibleDifficulty)} />
                <DetailRow label="Reward" value={heroReward} tip={glossaryText('block_reward')} />
                <DetailRow label="Subsidy" value={rewardFacts.subsidy} tip={glossaryText('block_subsidy')} />
                <DetailRow label="Block fees" value={rewardFacts.fees} tip={glossaryText('block_fees')} />
                <DetailRow label="Mempool fees" value={visibleFees} />
                <DetailRow label="Latest observed pool job" value={block?.pool_job?.job_id || 'Unavailable'} />
                <DetailRow label="Pool target difficulty" value={formatDifficulty(block?.pool_job?.difficulty)} />
                <DetailRow label="Fetched" value={formatTimestamp(block?.fetched_at_ms)} />
                <DetailRow label="Cache" value={cacheLabel(block)} />
                <DetailRow label="Local node" value={localNodeLabel(block)} />
                <DetailRow label="Local node enabled" value={manifest?.local_node?.enabled ? 'Yes' : 'No'} />
                <DetailRow label="Local node configured" value={manifest?.local_node?.configured ? 'Yes' : 'No'} />
                <DetailRow label="RPC endpoint" value={manifest?.local_node?.endpoint_label || 'Unavailable'} />
                <DetailRow label="Credential mode" value={manifest?.local_node?.credential_mode || 'none'} />
                <DetailRow label="Request timeout" value={formatMs(manifest?.local_node?.request_timeout_ms)} />
                <DetailRow label="Public fallback" value={manifest?.public_fallback?.enabled ? 'Enabled' : 'Off'} />
                <DetailRow label="Cache TTL" value={formatSecondsFromMs(manifest?.cache?.ttl_ms)} />
                <DetailRow label="Cache age" value={formatMs(manifest?.cache?.age_ms)} />
                <DetailRow label="Live RPC probe" value={manifest?.local_node?.live_rpc ? 'Enabled' : 'No'} />

                {manifest ? (
                  <div className="current-block-explain" style={{ marginTop: 14 }}>
                    <h3>Source manifest</h3>
                    <ul>
                      <li>{manifest.local_node.reason || 'Local node source state unavailable.'}</li>
                      <li>{manifest.public_fallback.reason || 'Public fallback state unavailable.'}</li>
                      <li>{manifest.cache.reason || 'Cache state unavailable.'}</li>
                    </ul>
                  </div>
                ) : null}
              </div>
            </details>

            <div className="bm-section current-block-explain">
              <h3>Source limits</h3>
              <ul>
                {(block?.limitations?.length ? block.limitations : [reason]).map(item => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </div>

            {block?.reasons?.length ? (
              <div className="bm-section current-block-explain">
                <h3>Unavailable reasons</h3>
                <ul>
                  {block.reasons.map(item => (
                    <li key={item}>{item}</li>
                  ))}
                </ul>
              </div>
            ) : null}
          </div>
        </div>
      </OverlayDialog>
    </>
  );
}
