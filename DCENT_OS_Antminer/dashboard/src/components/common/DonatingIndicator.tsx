// DonatingIndicator - lightweight topbar chip shown while the miner is
// currently on the donation pool (pool.donating === true in /api/status).
// Mode-agnostic; safe to drop into Basic, Standard, or Hacker topbars.
//
// W5.5: when donation routing falls over from the primary D-Central
// donation pool to the visible Braiins fallback worker
// (DungeonMaster), the chip and tooltip surface the active route
// instead of just saying "DONATING". Operators can tell at a glance
// whether the primary endpoint is up.

import React, { useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import { useMinerStore } from '../../store/miner';
import { Tooltip } from './Tooltip';
import type { PoolsDonationStatus } from '../../api/types';

interface DonatingIndicatorProps {
  /** Optional override of the estimated cycle duration (seconds) used in tooltip. */
  cycleDurationS?: number;
}

const POLL_INTERVAL_MS = 5_000;

export function DonatingIndicator({ cycleDurationS = 3600 }: DonatingIndicatorProps) {
  const status = useMinerStore(s => s.status);
  const donating = status?.pool?.donating === true;
  const [percent, setPercent] = useState(2);
  const [configuredCycleS, setConfiguredCycleS] = useState(cycleDurationS);
  const [route, setRoute] = useState<PoolsDonationStatus | null>(null);

  // Track how long we've observed the donation window so the tooltip can
  // give an honest read of current cycle position without needing extra API
  // endpoints. Resets when `donating` flips back to false.
  const [donatingSince, setDonatingSince] = useState<number | null>(null);
  const [now, setNow] = useState(Date.now());

  useEffect(() => {
    let cancelled = false;
    api.getDonationConfig()
      .then(config => {
        if (cancelled) return;
        const pct = typeof config.percent === 'number' ? config.percent : 2;
        const cycle = typeof config.cycle_duration_s === 'number'
          ? config.cycle_duration_s
          : cycleDurationS;
        setPercent(Math.max(0, Math.min(5, pct)));
        setConfiguredCycleS(Math.max(60, Math.min(86400, cycle)));
      })
      .catch(() => {
        // The live chip can still explain the default 2% donation window.
      });
    return () => { cancelled = true; };
  }, [cycleDurationS]);

  // W5.5: poll /api/pools while the donation window is active so the
  // route label tracks failover from primary -> Braiins fallback. We only
  // poll while donating so the dashboard stays quiet on the user pool.
  useEffect(() => {
    if (!donating) {
      setRoute(null);
      return;
    }
    let cancelled = false;
    const fetchRoute = () => {
      api.getPools()
        .then(resp => {
          if (cancelled) return;
          if (resp && resp.donation) {
            setRoute(resp.donation);
          }
        })
        .catch(() => {
          // Silent — the chip falls back to the generic donation tooltip.
        });
    };
    fetchRoute();
    const t = setInterval(fetchRoute, POLL_INTERVAL_MS);
    return () => { cancelled = true; clearInterval(t); };
  }, [donating]);

  useEffect(() => {
    if (donating && donatingSince === null) {
      setDonatingSince(Date.now());
    } else if (!donating && donatingSince !== null) {
      setDonatingSince(null);
    }
  }, [donating, donatingSince]);

  useEffect(() => {
    if (!donating) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [donating]);

  const elapsedS = useMemo(() => {
    if (donatingSince === null) return 0;
    return Math.max(0, Math.round((now - donatingSince) / 1000));
  }, [now, donatingSince]);

  if (!donating) return null;

  const formattedPercent = percent.toFixed(percent % 1 === 0 ? 0 : 1);

  // W5.5: derive a short badge label and a long tooltip from the active
  // donation route. Falls back to the generic donation messaging when the
  // /api/pools call hasn't completed yet, when the daemon predates W5.5
  // (donation field absent), or when fallback is disabled.
  const isFallback = route?.route === 'donation_fallback';
  const routeTitle = (() => {
    const cycleLine = `Configured donation: ${formattedPercent}% of a ${configuredCycleS}s cycle.`;
    const elapsedLine = `Currently mining on the donation pool for ${elapsedS}s in this cycle.`;
    if (!route) {
      // No route info yet — keep the legacy tooltip shape so existing
      // accessibility / aria-label expectations are preserved.
      return `${elapsedLine} ${cycleLine}`;
    }
    if (route.route === 'donation_fallback') {
      const worker = route.active_worker || 'DungeonMaster';
      const url = route.active_url || 'visible Braiins fallback';
      return `${elapsedLine} Donating via Braiins fallback worker ${worker} (${url}). ${cycleLine}`;
    }
    if (route.route === 'donation_primary') {
      const url = route.active_url || 'pool.d-central.tech';
      return `${elapsedLine} Donating to D-Central primary (${url}). ${cycleLine}`;
    }
    return `${elapsedLine} ${cycleLine}`;
  })();
  const routeBadge = isFallback ? 'DONATING (FALLBACK)' : 'DONATING';

  // D-01: the bare `title=` is replaced with the F1 Tooltip primitive
  // (delay/glass/touch/keyboard). The route-aware copy is the exact same
  // string — the donation≠devfee truth-contract wording is byte-preserved.
  // aria-label stays so SR users get the same announcement.
  return (
    <Tooltip content={routeTitle}>
      <span
        className={`ds-chip ds-accent ds-live${isFallback ? ' ds-warn' : ''}`}
        aria-label={routeTitle}
        role="status"
      >
        <span className="ds-dot" /> {routeBadge} {formattedPercent}%
      </span>
    </Tooltip>
  );
}

export default DonatingIndicator;
