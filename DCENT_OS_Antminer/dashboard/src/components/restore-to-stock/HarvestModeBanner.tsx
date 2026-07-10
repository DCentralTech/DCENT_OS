// Step 6 (post-reboot) — visible only when the unit reports it's
// running stock-mode. Hard to detect from the DCENT_OS dashboard
// since it isn't running there, but if a recovery harness or
// self-test path eventually surfaces this state, this component
// renders the harvest script command + 10-min capture countdown.
//
// For now it's polled-on-demand off /api/system/restore-to-stock/status
// and shows the last scheduled reboot timestamp + reminder.

import React, { useEffect, useState } from 'react';
import {
  restoreToStockApi,
  statusRebootAtMs,
  type RestoreToStockStatus,
} from '../../api/restore-to-stock';

const HARVEST_WINDOW_S = 10 * 60;

export function HarvestModeBanner() {
  const [status, setStatus] = useState<RestoreToStockStatus | null>(null);
  const [now, setNow] = useState<number>(Date.now());

  useEffect(() => {
    let cancelled = false;
    const fetchStatus = async () => {
      try {
        const s = await restoreToStockApi.status();
        if (!cancelled) setStatus(s);
      } catch {
        // Silent — endpoint may be unavailable while the unit is
        // booting. Per feedback_dashboard_must_degrade_gracefully
        // we don't blank.
      }
    };
    void fetchStatus();
    const interval = window.setInterval(fetchStatus, 15000);
    const tick = window.setInterval(() => setNow(Date.now()), 1000);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
      window.clearInterval(tick);
    };
  }, []);

  const rebootAt = statusRebootAtMs(status);

  if (!rebootAt) return null;

  const elapsedS = Math.max(0, (now - rebootAt) / 1000);
  const remainingS = Math.max(0, HARVEST_WINDOW_S - elapsedS);
  const inHarvestWindow = elapsedS > 30 && remainingS > 0;
  const past = elapsedS > HARVEST_WINDOW_S;

  if (!inHarvestWindow && !past) {
    return (
      <div style={banner('rgba(247,147,26,0.08)', 'rgba(247,147,26,0.28)')}>
        <div style={{ fontWeight: 700, color: 'var(--accent, #FAA500)', fontSize: '0.9rem' }}>
          Restore-to-stock scheduled
        </div>
        <div style={{ fontSize: '0.78rem', color: 'var(--text)', marginTop: 4 }}>
          Reboot scheduled at <code>{new Date(rebootAt).toLocaleTimeString()}</code>. Once stock
          comes up, the 10-minute harvest window opens — keep your harvest host ready.
        </div>
      </div>
    );
  }

  if (inHarvestWindow) {
    const min = Math.floor(remainingS / 60);
    const sec = Math.floor(remainingS % 60);
    return (
      <div style={banner('rgba(45,212,160,0.08)', 'rgba(45,212,160,0.28)')}>
        <div style={{ fontWeight: 700, color: 'var(--green, #2DD4A0)', fontSize: '0.9rem' }}>
          Stock harvest window — {min}:{sec.toString().padStart(2, '0')} remaining
        </div>
        <div style={{ fontSize: '0.78rem', color: 'var(--text)', marginTop: 4 }}>
          Stock Bitmain is up. Run the harvest script from your host:
        </div>
        <pre style={{
          margin: '8px 0 0',
          padding: 10,
          borderRadius: 6,
          background: 'rgba(10,10,15,0.6)',
          border: '1px solid var(--border, rgba(255,255,255,0.08))',
          color: 'var(--text)',
          fontSize: '0.78rem',
          fontFamily: 'JetBrains Mono, monospace',
          overflowX: 'auto',
        }}>
{`# from operator host
DRY_RUN=1 ./scripts/wave8_stock_harvest.sh <miner-ip>   # preview
DRY_RUN=0 ./scripts/wave8_stock_harvest.sh <miner-ip>   # capture`}
        </pre>
        <div style={{ fontSize: '0.7rem', color: 'var(--text-dim, #6E6E80)', marginTop: 6 }}>
          When finished, return to DCENT_OS by running <code>fw_setenv bootslot &lt;prev&gt;</code> from
          inside stock OR via U-Boot serial console. S99upgrade defeats U-Boot auto_recovery — a plain
          power cycle stays on stock.
        </div>
      </div>
    );
  }

  // past
  return (
    <div style={banner('rgba(255,255,255,0.04)', 'var(--border, rgba(255,255,255,0.12))')}>
      <div style={{ fontWeight: 700, color: 'var(--text-secondary, #8b8b9e)', fontSize: '0.9rem' }}>
        Harvest window expired
      </div>
      <div style={{ fontSize: '0.78rem', color: 'var(--text)', marginTop: 4 }}>
        The 10-minute window has elapsed. If you haven't finished, schedule another restore
        or use the on-miner harvest path described in <code>STOCK_BOOT_HARVEST_PROCEDURE.md</code>.
      </div>
    </div>
  );
}

function banner(bg: string, border: string): React.CSSProperties {
  return {
    padding: '10px 14px',
    borderRadius: 10,
    background: bg,
    border: `1px solid ${border}`,
    margin: '0 0 12px',
  };
}
