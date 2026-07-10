// DangerZonePage — destructive operations live here, segregated
// from the main dashboard to avoid accidental clicks.
//
// Mounted at hash route #/system/danger-zone. The
// Restore-to-Stock button is the marquee feature; future
// destructive ops (factory reset, key revocation, NAND wipe)
// belong here too.

import React, { useState } from 'react';
import { RestoreToStockModal } from './RestoreToStockModal';
import { HarvestModeBanner } from './HarvestModeBanner';
import { InfoDot } from '../common/Tooltip';

export function DangerZonePage() {
  const [restoreOpen, setRestoreOpen] = useState(false);

  return (
    <div className="page-content p4-danger-page">
      <div className="p4-danger-head">
        <h2 className="p4-danger-title">Danger zone</h2>
        <span className="ds-chip ds-danger" aria-label="Destructive operations">
          <span className="ds-dot" aria-hidden />
          Destructive
        </span>
        <InfoDot term="brick_anxiety" placement="bottom" label="Why these operations are gated" />
      </div>
      <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', maxWidth: 720, marginBottom: 16 }}>
        Operations on this page <strong>permanently overwrite firmware slots</strong>. Each one
        walks through an explicit multi-step confirm (typed serial + typed phrase + slider).
        Backend re-checks every gate at the wire. You can back out of the modal until you submit
        the final slider — after that point the flash is committed.
      </div>

      <HarvestModeBanner />

      <div className="section p4-restore-card">
        <div style={{ display: 'flex', alignItems: 'flex-start', gap: 16, flexWrap: 'wrap' }}>
          <div style={{ flex: 1, minWidth: 280 }}>
            <h3 style={{ margin: 0, fontSize: '1rem', display: 'inline-flex', alignItems: 'center', gap: 6 }}>
              Flash stock Bitmain firmware
              <InfoDot term="slot_removed_warning" size={12} label="What this keeps and what it clears" />
            </h3>
            <div style={{ fontSize: '0.82rem', color: 'var(--text-secondary, #8b8b9e)', marginTop: 6, lineHeight: 1.5 }}>
              <strong>This removes DCENT_OS</strong> and writes stock Bitmain firmware to the
              inactive NAND slot so you can capture vendor telemetry for RE / harvest workflows.
              Any tuning, autotuner profiles, or operator config baked into DCENT_OS will not
              follow you to stock. Safety preflight refuses tainted firmware (SECURE_BOOT_SET,
              daemons:22322 listener, Hashcore root hash). NAND backup is mandatory.{' '}
              <strong>Manual recovery REQUIRED</strong> — full procedure is shown inside the
              modal and in <code>STOCK_BOOT_HARVEST_PROCEDURE.md §10</code>.
            </div>
            <ul style={{ marginTop: 8, fontSize: '0.78rem', color: 'var(--text)', paddingLeft: 18 }}>
              <li>Default <code>confirm:false</code> dry-run.</li>
              <li>Type miner serial + phrase <code>RESTORE TO STOCK</code> + slider.</li>
              <li>Critical IOC findings cannot be overridden from this UI.</li>
              <li>NAND backup written to <code>/data/restore-backup-&lt;timestamp&gt;/</code>.</li>
            </ul>
          </div>
          <button
            type="button"
            onClick={() => setRestoreOpen(true)}
            className="btn-danger"
            style={{
              padding: '12px 20px',
              borderRadius: 8,
              border: '1px solid var(--red, #EF4444)',
              background: 'rgba(239,68,68,0.08)',
              color: 'var(--red, #EF4444)',
              fontWeight: 800,
              fontSize: '0.85rem',
              letterSpacing: '0.04em',
              textTransform: 'uppercase',
              cursor: 'pointer',
              alignSelf: 'flex-start',
            }}
            data-testid="restore-to-stock-trigger"
          >
            Flash to stock firmware...
          </button>
        </div>
      </div>

      <RestoreToStockModal open={restoreOpen} onClose={() => setRestoreOpen(false)} />
    </div>
  );
}
