import React from 'react';
import { useMinerStore } from '../../store/miner';
import { estimateSatsPerSecond } from '../../utils/thermal';
import { useValueFlash } from '../../hooks/useValueFlash';
import { glossaryText } from '../../utils/glossary';

function OdometerDigits({ value, flashClass = '' }: { value: number; flashClass?: string }) {
  const digits = value.toLocaleString().split('');
  return (
    <span
      className={flashClass}
      style={{
        display: 'inline-flex',
        fontVariantNumeric: 'tabular-nums',
        fontFeatureSettings: "'tnum'",
      }}
    >
      {digits.map((d, i) => (
        <span key={`${i}-${d}`} style={{
          display: 'inline-block',
          transition: 'transform 0.3s ease-out, opacity 0.15s',
          animation: 'fadeIn 0.3s ease-out',
        }}>
          {d}
        </span>
      ))}
    </span>
  );
}

export function SatsCounter() {
  const heater = useMinerStore(s => s.heaterStatus);
  const status = useMinerStore(s => s.status);
  const settings = useMinerStore(s => s.settings);
  const setCurrentPage = useMinerStore(s => s.setCurrentPage);

  const baseSats = heater?.sats_today ?? 0;
  const hashrateGhs = status?.hashrate_ghs ?? heater?.hashrate_ghs ?? 0;
  // P0-4: ticker rate anchored to the backend-reported network difficulty
  // (canonical); 0 when difficulty is unknown so the projection isn't faked.
  const satsPerSec = estimateSatsPerSecond(hashrateGhs, heater?.network_difficulty);
  const isMining = hashrateGhs > 0;

  const reportedSats = Math.floor(baseSats);
  const projectedDailySats = Math.max(0, Math.floor(satsPerSec * 86400));
  const visibleSats = reportedSats > 0 ? reportedSats : projectedDailySats;
  const usdValue = (visibleSats / 100_000_000) * settings.btcPrice;
  // FE-2: the USD figure converts sats with a MANUAL BTC price. DCENT_OS is
  // local-first and never fetches a live price, so when the operator has never
  // set one (`btcPriceLastUpdated == null`) it is a built-in fallback default.
  // Surface the USD as an estimate and name the fallback so a hardcoded price
  // is never presented as an authoritative quote.
  const btcPriceIsFallback = settings.btcPriceLastUpdated == null;
  const usdEstimateTitle =
    glossaryText('usd_estimate_fallback') +
    (btcPriceIsFallback
      ? ` Using the built-in fallback price of $${settings.btcPrice.toLocaleString()}.`
      : ` Using your set BTC price of $${settings.btcPrice.toLocaleString()}.`);

  // Flash digits when the server-reported (jump) value changes, not on every RAF tick.
  const flashClass = useValueFlash(baseSats > 0 ? baseSats : null);

  // Consumer-facing line. "sats" is universal; "heating" softens "mining"
  // for the non-mining-savvy first experience. Telemetry truthfulness is
  // preserved — reported vs projected stays explicitly labeled.
  let line: React.ReactNode;
  if (reportedSats > 0) {
    line = <><OdometerDigits value={reportedSats} flashClass={flashClass} /> sats reported today</>;
  } else if (isMining) {
    line = projectedDailySats > 0
      ? <>~<OdometerDigits value={projectedDailySats} /> sats/day projected</>
      : 'Heating — first sats arrive on next block';
  } else if (visibleSats > 0) {
    line = <>~<OdometerDigits value={visibleSats} /> sats/day projected</>;
  } else {
    line = 'Turn on heater to start earning sats';
  }

  return (
    <div style={{ textAlign: 'center', padding: '8px 0 16px' }}>
      <button
        type="button"
        className={`summary-link-button sats-ticker-card${isMining ? ' is-mining' : ''}`}
        onClick={() => setCurrentPage('heater-history')}
        aria-label="Open heater earnings history"
        data-tooltip={
          reportedSats > 0
            ? glossaryText('earning_proof')
            : 'Sats = satoshis (1 BTC = 100,000,000 sats). This is a projection at the current speed until the pool credits real shares. Tap to see history.'
        }
      >
        <span className="sats-ticker-line">
          {line}
          {visibleSats > 0 && usdValue > 0 && (
            <span
              className="sats-ticker-usd"
              title={usdEstimateTitle}
              data-fallback-price={btcPriceIsFallback ? 'true' : 'false'}
            >
              (~${usdValue.toFixed(2)} est.{btcPriceIsFallback ? ', fallback' : ''})
            </span>
          )}
          <span className="sats-ticker-arrow" aria-hidden="true">&rsaquo;</span>
        </span>
        <span className="sats-ticker-label">Tap to view earnings history</span>
      </button>
    </div>
  );
}
