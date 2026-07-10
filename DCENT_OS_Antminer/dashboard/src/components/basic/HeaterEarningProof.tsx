import React from 'react';
import { useMinerStore } from '../../store/miner';
import { Tooltip } from '../common/Tooltip';
import { glossaryText } from '../../utils/glossary';

/**
 * Plain-language "are you actually earning?" verdict — the antidote to the
 * universal beginner anxiety (R1 pain #1). Honest: judged by accepted shares
 * over time + share recency, NOT by the local↔pool hashrate gap. Wording
 * reuses the canonical `earning_proof` truth-contract.
 *
 * Extracted from HeaterStatus so heater-home can place it in the hero's right
 * column (next to the earnings card, where it height-balances the tall dial
 * column) while heater-history still gets it as part of the full HeaterStatus.
 */
type EarningVerdict = {
  tone: 'good' | 'warming' | 'idle';
  headline: string;
  detail: string;
};

function formatAgo(seconds: number): string {
  if (seconds < 0) return 'just now';
  if (seconds < 60) return `${Math.round(seconds)}s ago`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)}h ago`;
  return `${Math.round(seconds / 86400)}d ago`;
}

export function HeaterEarningProof() {
  const status = useMinerStore(s => s.status);
  const heater = useMinerStore(s => s.heaterStatus);

  const hashrate = status?.hashrate_ghs ?? heater?.hashrate_ghs ?? 0;
  const isMining = hashrate > 0;

  const accepted = status?.accepted ?? 0;
  const rejected = status?.rejected ?? 0;
  const totalShares = accepted + rejected;
  const acceptRate = totalShares > 0 ? (accepted / totalShares) * 100 : null;
  const lastShareS = status?.pool?.last_share_s ?? null;
  const hasRecentShare = lastShareS != null && lastShareS >= 0 && lastShareS < 600;

  const earning: EarningVerdict = (() => {
    if (!isMining) {
      return {
        tone: 'idle',
        headline: 'Heater is off',
        detail: 'Start heating to begin earning Bitcoin from accepted shares.',
      };
    }
    if (accepted > 0 && hasRecentShare) {
      return {
        tone: 'good',
        headline: 'Yes — you are earning',
        detail: `${accepted.toLocaleString()} accepted share${accepted === 1 ? '' : 's'}` +
          (lastShareS != null ? ` · last one ${formatAgo(lastShareS)}` : ''),
      };
    }
    if (accepted > 0) {
      return {
        tone: 'warming',
        headline: 'Earning — waiting on the next share',
        detail: `${accepted.toLocaleString()} accepted so far` +
          (lastShareS != null ? ` · last one ${formatAgo(lastShareS)}. Shares arrive in bursts; a gap is normal.` : '.'),
      };
    }
    return {
      tone: 'warming',
      headline: 'Warming up — first share on the way',
      detail: 'The miner is hashing and connected. The first accepted share can take a few minutes — this is normal, not a fault.',
    };
  })();

  const acceptRateTone =
    acceptRate == null ? 'neutral'
      : acceptRate >= 99 ? 'good'
        : acceptRate >= 95 ? 'warn'
          : 'bad';

  return (
    <Tooltip term="earning_proof" placement="bottom">
      <div
        className={`earning-proof earning-proof--${earning.tone} dcm-card-enter`}
        role="status"
        aria-live="polite"
        tabIndex={0}
        aria-label={`Earning status: ${earning.headline}. ${earning.detail}`}
      >
        <div className="earning-proof__pulse" aria-hidden="true">
          <span className="earning-proof__pulse-dot" />
        </div>
        <div className="earning-proof__body">
          <div className="earning-proof__headline">{earning.headline}</div>
          <div className="earning-proof__detail">{earning.detail}</div>
          {isMining && acceptRate != null && (
            <div className="earning-proof__meta">
              <span
                className={`earning-proof__rate earning-proof__rate--${acceptRateTone}`}
                data-tooltip={glossaryText('accept_rate_thresholds')}
              >
                {acceptRate.toFixed(acceptRate >= 99.95 ? 0 : 1)}% accepted
              </span>
              {rejected > 0 && (
                <span
                  className="earning-proof__rejects"
                  data-tooltip={glossaryText('share_rejected')}
                >
                  {rejected.toLocaleString()} rejected
                </span>
              )}
            </div>
          )}
        </div>
        <span className="earning-proof__hint" aria-hidden="true">i</span>
      </div>
    </Tooltip>
  );
}
