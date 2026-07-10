// Step 1 — current state banner.
//
// Shows DCENT_OS version, miner serial, hashboard count, pool stats.
// Prominent WARNING about breaker/noise (per user feedback: "stock
// Bitmain firmware is too hard on house breakers and is alot
// noisy!").

import React, { useEffect, useState } from 'react';
import { useMinerStore } from '../../store/miner';
import {
  restoreToStockApi,
  type RestoreToStockStatus,
  type PreflightChecks,
} from '../../api/restore-to-stock';
import { StatusIcon } from '../common/StatusIcon';
import { InfoDot } from '../common/Tooltip';

interface Props {
  hashboardCountToUse: number;
  setHashboardCountToUse: (n: number) => void;
  acknowledgeBreakerWarning: boolean;
  setAcknowledgeBreakerWarning: (b: boolean) => void;
}

export function RestoreStatus({
  hashboardCountToUse,
  setHashboardCountToUse,
  acknowledgeBreakerWarning,
  setAcknowledgeBreakerWarning,
}: Props) {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const status = useMinerStore(s => s.status);

  const version = systemInfo?.version ?? '—';
  const model = systemInfo?.model ?? 'Antminer';
  const serial = systemInfo?.hardware?.miner_serial ?? systemInfo?.hostname ?? 'unknown';
  const detectedHashboardCount = systemInfo?.chain_count ?? 3;

  //  W11-C (A5''-OPS-MED-1 / C3): poll /status on Step 0 mount
  // for `last_backup_fw_setenv_present`. If a PRIOR backup on this
  // daemon lifetime lacked fw_setenv, surface a precondition warning
  // so the operator knows from the start they may only have Option-B
  // (serial console) recovery.
  const [statusSnap, setStatusSnap] = useState<RestoreToStockStatus | null>(null);
  useEffect(() => {
    let cancelled = false;
    restoreToStockApi.status().then(s => { if (!cancelled) setStatusSnap(s); }).catch(() => {});
    return () => { cancelled = true; };
  }, []);
  const fwSetenvMissingPrior = statusSnap?.last_backup_fw_setenv_present === false;

  return (
    <div>
      <h3 style={{ marginTop: 0, fontSize: '1.1rem' }}>Current state</h3>

      <div style={summaryGrid}>
        <Stat label="Model" value={model} />
        <Stat label="DCENT_OS version" value={`v${version}`} />
        <Stat label="Serial / hostname" value={serial} mono />
        <Stat label="Detected hashboards" value={`${detectedHashboardCount}`} />
        {status?.hashrate_ghs != null && (
          <Stat label="Live hashrate" value={`${(status.hashrate_ghs / 1000).toFixed(1)} TH/s`} />
        )}
      </div>

      {/* Wave 11 W11-C (C3 / A5''-OPS-MED-1): forward-looking warning */}
      {fwSetenvMissingPrior && (
        <div
          data-testid="status-prior-fwsetenv-warning"
          role="alert"
          style={priorFwsetenvWarn}
        >
          <strong style={{ color: 'var(--amber, #F59E0B)' }}>
            ⚠ Prior backup on this daemon lacked fw_setenv
          </strong>
          <div style={{ marginTop: 6, fontSize: '0.82rem', color: 'var(--text)', lineHeight: 1.5 }}>
            A previous Restore-to-Stock attempt on this miner could NOT copy a
            working <code>fw_setenv</code> into the backup. If the same
            conditions hold this time, you will only have{' '}
            <strong>Option B (serial console U-Boot env edit)</strong> for
            recovery — Option A (<code>fw_setenv bootslot</code> from inside
            booted stock) won't work.
          </div>
          <div style={{ marginTop: 6, fontSize: '0.76rem', color: 'var(--text-dim, #6E6E80)' }}>
            See <code>STOCK_BOOT_HARVEST_PROCEDURE.md §10</code>. Prepare your
            USB-TTL serial cable now or fix the miner's <code>libubootenv-tools</code>{' '}
            install before proceeding.
          </div>
        </div>
      )}

      <div style={warningBox}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, fontWeight: 700, color: 'var(--red, #EF4444)', fontSize: '0.95rem', marginBottom: 8 }}>
          <span aria-hidden>⚠</span>
          <span>This flash <u>removes DCENT_OS</u> from the inactive NAND slot and replaces it with stock Bitmain firmware.</span>
          <InfoDot term="slot_removed_warning" placement="bottom" label="Which NAND slot is removed" />
        </div>
        <ul style={{ margin: '0 0 8px 18px', padding: 0, fontSize: '0.82rem', color: 'var(--text)' }}>
          <li><strong>Breaker risk.</strong> Stock Bitmain runs at full PSU power — much harder on house breakers than DCENT_OS quiet-home defaults.</li>
          <li><strong>Noise.</strong> Stock fans run at 100% during early boot; expect a noticeable spike.</li>
          <li><strong>10-minute capture window.</strong> Once stock boots, you have ~10 minutes to run the harvest script before the watchdog cycles.</li>
          <li>
            <strong>Manual recovery REQUIRED.</strong> U-Boot auto_recovery is DEFEATED by S99upgrade in
            both DCENT_OS and stock Bitmain. To return to DCENT_OS, you must run{' '}
            <code>fw_setenv bootslot 1</code> (or whichever the prev slot was) from inside booted
            stock Bitmain (default <code>root:admin</code>), THEN power-cycle. If stock won't boot
            or auth fails, attach a USB-TTL serial cable to the S9's UART header and stop U-Boot at
            the prompt; manually run <code>setenv bootslot 1; saveenv; reset</code>.
          </li>
        </ul>
        <label style={{ display: 'flex', alignItems: 'flex-start', gap: 8, fontSize: '0.85rem', color: 'var(--text)', cursor: 'pointer' }}>
          <input
            data-testid="restore-breaker-ack"
            type="checkbox"
            checked={acknowledgeBreakerWarning}
            onChange={(e) => setAcknowledgeBreakerWarning(e.target.checked)}
            style={{ marginTop: 3 }}
          />
          <span>I understand the breaker / noise risk and have a plan for the 10-minute capture window.</span>
        </label>
      </div>

      {/* Wave 12 W12-C: dynamic pre-flight checklist (live endpoint
          probe). Falls back to the wave-11 static 8-condition list on
          404/503/network error per
          . */}
      <DynamicPreflightChecklist />

      <div style={{ marginTop: 16 }}>
        <label style={{ display: 'block', fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 6 }}>
          Hashboards to flash with stock firmware (1-{detectedHashboardCount})
        </label>
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
          {[1, 2, 3].slice(0, detectedHashboardCount).map(n => (
            <button
              key={n}
              type="button"
              onClick={() => setHashboardCountToUse(n)}
              style={{
                padding: '6px 14px',
                borderRadius: 6,
                border: `1px solid ${hashboardCountToUse === n ? 'var(--accent, #FAA500)' : 'var(--border, rgba(255,255,255,0.12))'}`,
                background: hashboardCountToUse === n ? 'rgba(247,147,26,0.12)' : 'transparent',
                color: 'var(--text)',
                fontSize: '0.85rem',
                fontWeight: 600,
                cursor: 'pointer',
              }}
            >
              {n} board{n > 1 ? 's' : ''}
            </button>
          ))}
        </div>
        <div style={{ fontSize: '0.72rem', color: 'var(--text-dim, #6E6E80)', marginTop: 6 }}>
          Tip: unplug the boards you don't want stock to drive. The user's home S9 test
          configuration uses 1 board to keep the breaker happy.
        </div>
        {hashboardCountToUse > 1 && (
          <div style={multiBoardWarning} role="note" aria-label="Multi-board breaker warning">
            <span aria-hidden style={{ marginRight: 6 }}>⚠</span>
            With 2+ hashboards plugged, stock Bitmain may exceed your residential breaker (typical
            15A circuit handles ~1500W; 3 boards at stock voltage can pull 2500W+). Consider 1
            board for safety.
          </div>
        )}
      </div>
    </div>
  );
}

const multiBoardWarning: React.CSSProperties = {
  marginTop: 10,
  padding: '8px 10px',
  borderRadius: 6,
  background: 'rgba(240,180,41,0.10)',
  border: '1px solid rgba(240,180,41,0.32)',
  color: 'var(--text)',
  fontSize: '0.74rem',
  fontWeight: 600,
  lineHeight: 1.4,
};

const summaryGrid: React.CSSProperties = {
  display: 'grid',
  gridTemplateColumns: 'repeat(auto-fill, minmax(180px, 1fr))',
  gap: 12,
  marginBottom: 16,
};

const warningBox: React.CSSProperties = {
  padding: 14,
  borderRadius: 10,
  background: 'rgba(239,68,68,0.06)',
  border: '1px solid rgba(239,68,68,0.28)',
};

const priorFwsetenvWarn: React.CSSProperties = {
  padding: 12,
  borderRadius: 10,
  background: 'rgba(245,158,11,0.10)',
  border: '1px solid rgba(245,158,11,0.45)',
  marginBottom: 14,
};

//  W11-C (C4): static pre-flight checklist. This is the
// wave-11 fallback per the W11-C plan — the auto-verify endpoint
// (/api/system/restore-to-stock/preflight-checks) is deferred to
// wave-12 because adding a new Rust route + tests requires a
// Linux/HAL build environment. The checklist below mirrors the
// 8-condition pre-flight in
// and the daemon-side gates in `dcentrald-api/src/routes/restore_to_stock.rs`
// (REVERT_SCRIPT_CANDIDATES, setsid_present check, NAND backup
// free-space gate, libubootenv-tools probe).
const PREFLIGHT_ITEMS: Array<{ label: string; detail: string }> = [
  {
    label: 'setsid available on PATH',
    detail: 'Required so the writer survives dcentrald exit (W9-C R3-HIGH/R4-H3 detach). Probe: `which setsid` — expects /usr/bin/setsid or /bin/setsid.',
  },
  {
    label: 'revert_to_stock.sh present',
    detail: 'Daemon checks /usr/sbin/, /usr/local/sbin/, /data/scripts/. The script handles flash_erase + nandwrite + fw_setenv on the inactive slot.',
  },
  {
    label: 'fw_setenv on the miner (libubootenv-tools)',
    detail: 'Required for Option-A recovery from inside booted stock. Probe: `which fw_setenv`. If missing, only Option B (serial console U-Boot edit) will work.',
  },
  {
    label: 'At least 250 MiB free on /data',
    detail: 'NAND backup writes ~200 MiB (mtd0+mtd1+mtd2 images). Backend gate refuses to start otherwise.',
  },
  {
    label: 'One hashboard physically unplugged',
    detail: 'Stock Bitmain pulls full PSU power; home circuits cannot handle it. User-campaign-scope: 1 board only.',
  },
  {
    label: 'Operator host has sshpass + nc',
    detail: 'The post-flash harvest script needs both. On Windows: WSL or Git Bash with `apt install sshpass netcat`.',
  },
  {
    label: 'Power-cycle access to the miner',
    detail: 'The recovery path may need a manual power cycle. If you can\'t reach the breaker, do not proceed.',
  },
  {
    label: 'Source firmware tarball verified',
    detail: 'Bitmain doesn\'t sign their tarballs; the W8-F safety preflight does IOC scanning at staging time.',
  },
];

function PreflightChecklist() {
  return (
    <div
      data-testid="restore-preflight-checklist"
      style={{
        marginTop: 16,
        marginBottom: 16,
        padding: 12,
        borderRadius: 10,
        background: 'rgba(18,18,26,0.6)',
        border: '1px solid var(--border, rgba(255,255,255,0.08))',
      }}
    >
      <div
        style={{
          fontSize: '0.78rem',
          fontWeight: 700,
          color: 'var(--text-secondary, #8b8b9e)',
          textTransform: 'uppercase',
          letterSpacing: '0.04em',
          marginBottom: 8,
        }}
      >
        Pre-flight checklist (operator-verified)
      </div>
      <div style={{ fontSize: '0.74rem', color: 'var(--text-dim, #6E6E80)', marginBottom: 10, lineHeight: 1.4 }}>
        The live pre-flight endpoint is unavailable right now — confirm each item manually.
        The backend still gates on these conditions independently at preflight + flash time.
      </div>
      <ul
        style={{
          margin: 0,
          padding: 0,
          listStyle: 'none',
          display: 'grid',
          gap: 6,
        }}
      >
        {PREFLIGHT_ITEMS.map((item) => (
          <li
            key={item.label}
            data-testid="restore-preflight-item"
            style={{
              display: 'grid',
              gridTemplateColumns: '20px 1fr',
              gap: 8,
              padding: '6px 0',
              borderTop: '1px solid var(--border, rgba(255,255,255,0.04))',
            }}
          >
            <span aria-hidden style={{ color: 'var(--text-dim, #6E6E80)', fontSize: '0.85rem', lineHeight: 1.4 }}>•</span>
            <div>
              <div style={{ fontSize: '0.82rem', color: 'var(--text)', fontWeight: 600 }}>
                {item.label}
              </div>
              <div style={{ fontSize: '0.72rem', color: 'var(--text-dim, #6E6E80)', lineHeight: 1.4, marginTop: 2 }}>
                {item.detail}
              </div>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

// ---------------------------------------------------------------------------
//  W12-C — DynamicPreflightChecklist
// ---------------------------------------------------------------------------
//
// Polls `GET /api/system/restore-to-stock/preflight-checks` once on
// mount. Renders 9 dynamic rows (6 path probes + disk-space +
// platform-supported + platform-verified-revertable). On endpoint
// failure (404/503/network), falls back to the wave-11 static
// `<PreflightChecklist />` with a small "endpoint unavailable" hint
//.

const W12C_MIN_FREE_MIB = 250;

function DynamicPreflightChecklist() {
  const [checks, setChecks] = useState<PreflightChecks | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    restoreToStockApi
      .preflightChecks()
      .then((data) => {
        if (!cancelled) {
          setChecks(data);
          setLoading(false);
        }
      })
      .catch((e) => {
        if (!cancelled) {
          const msg = e instanceof Error ? e.message : 'unknown error';
          setError(msg);
          setLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // While loading the first response, show a placeholder. The static
  // fallback has its own loud banner, so we don't blast the operator
  // before the probe even returns.
  if (loading) {
    return (
      <div
        data-testid="restore-preflight-checklist-loading"
        style={{
          marginTop: 16,
          marginBottom: 16,
          padding: 12,
          borderRadius: 10,
          background: 'rgba(18,18,26,0.6)',
          border: '1px solid var(--border, rgba(255,255,255,0.08))',
        }}
      >
        <div
          style={{
            fontSize: '0.78rem',
            fontWeight: 700,
            color: 'var(--text-secondary, #8b8b9e)',
            textTransform: 'uppercase',
            letterSpacing: '0.04em',
            marginBottom: 8,
          }}
        >
          Pre-flight checklist (live probe)
        </div>
        <div style={{ fontSize: '0.74rem', color: 'var(--text-dim, #6E6E80)' }}>
          Probing daemon for setsid / fw_setenv / tar / nandwrite / flash_erase ...
        </div>
      </div>
    );
  }

  // Endpoint failure → graceful fallback to the wave-11 static list.
  if (error || !checks) {
    return (
      <>
        <div
          data-testid="restore-preflight-checklist-fallback-hint"
          style={{
            marginTop: 12,
            marginBottom: 4,
            padding: '6px 10px',
            borderRadius: 8,
            background: 'rgba(245,158,11,0.06)',
            border: '1px solid rgba(245,158,11,0.28)',
            fontSize: '0.72rem',
            color: 'var(--text-dim, #6E6E80)',
            lineHeight: 1.4,
          }}
        >
          <span aria-hidden style={{ marginRight: 6 }}>⚠</span>
          Live preflight endpoint unavailable, showing static checklist. The
          backend still gates on these conditions independently at preflight +
          flash time.
        </div>
        <PreflightChecklist />
      </>
    );
  }

  const c = checks;
  const rows: Array<{
    key: string;
    label: string;
    ok: boolean;
    detail: string;
    amber?: boolean;
  }> = [
    {
      key: 'setsid',
      label: 'setsid available on PATH',
      ok: c.setsid_path != null,
      detail: c.setsid_path ?? 'missing',
    },
    {
      key: 'revert_script',
      label: 'revert_to_stock.sh present',
      ok: c.revert_script_path != null,
      detail: c.revert_script_path ?? 'missing',
    },
    {
      key: 'fw_setenv',
      label: 'fw_setenv (libubootenv-tools)',
      ok: c.fw_setenv_path != null,
      detail: c.fw_setenv_path ?? 'missing',
    },
    {
      key: 'tar',
      label: 'tar (NAND backup tarball)',
      ok: c.tar_path != null,
      detail: c.tar_path ?? 'missing',
    },
    {
      key: 'nandwrite',
      label: 'nandwrite (firmware-slot write)',
      ok: c.nandwrite_path != null,
      detail: c.nandwrite_path ?? 'missing',
    },
    {
      key: 'flash_erase',
      label: 'flash_erase (UBI revert path)',
      ok: c.flash_erase_path != null,
      detail: c.flash_erase_path ?? 'missing',
    },
    {
      key: 'data_free',
      label: `Free space on /data (>= ${W12C_MIN_FREE_MIB} MiB)`,
      ok: c.data_free_mib >= W12C_MIN_FREE_MIB,
      detail: `${c.data_free_mib} MiB free / ${W12C_MIN_FREE_MIB} MiB required`,
    },
    {
      key: 'platform_supported',
      label: 'Platform supported',
      ok: c.platform_supported,
      detail: c.platform_signature
        ? `${c.platform_signature}${c.platform_supported ? '' : ' (no PROFILE_TABLE entry)'}`
        : 'platform signature unknown',
    },
    {
      key: 'platform_verified',
      label: 'Platform verified-revertable',
      ok: c.platform_verified_revertable,
      // Amber if supported-but-not-verified: operator can dry-run.
      // Red if neither.
      amber: c.platform_supported && !c.platform_verified_revertable,
      detail: c.platform_verified_revertable
        ? 'live-test verified'
        : c.platform_supported
        ? 'supported but pending live-test (dry-run only)'
        : 'unsupported',
    },
  ];

  return (
    <div
      data-testid="restore-preflight-checklist-dynamic"
      style={{
        marginTop: 16,
        marginBottom: 16,
        padding: 12,
        borderRadius: 10,
        background: 'rgba(18,18,26,0.6)',
        border: '1px solid var(--border, rgba(255,255,255,0.08))',
      }}
    >
      <div
        style={{
          fontSize: '0.78rem',
          fontWeight: 700,
          color: 'var(--text-secondary, #8b8b9e)',
          textTransform: 'uppercase',
          letterSpacing: '0.04em',
          marginBottom: 8,
        }}
      >
        Pre-flight checklist (live probe)
      </div>
      <div
        style={{
          fontSize: '0.74rem',
          color: 'var(--text-dim, #6E6E80)',
          marginBottom: 10,
          lineHeight: 1.4,
        }}
      >
        Live probes via{' '}
        <code>/api/system/restore-to-stock/preflight-checks</code>. The backend
        still enforces these gates independently at preflight + flash time;
        this view is a heads-up for the operator.
      </div>
      <ul
        style={{
          margin: 0,
          padding: 0,
          listStyle: 'none',
          display: 'grid',
          gap: 6,
        }}
      >
        {rows.map((row) => {
          //  W13-D (Item 2): standardize iconography via the
          // shared `<StatusIcon>` helper so preflight + flash-phase +
          // confirm-step glyphs use the same visual language.
          const iconState: 'ok' | 'fail' | 'warn' = row.ok
            ? 'ok'
            : row.amber
            ? 'warn'
            : 'fail';
          return (
            <li
              key={row.key}
              data-testid={`restore-preflight-row-${row.key}`}
              data-state={row.ok ? 'ok' : row.amber ? 'amber' : 'fail'}
              style={{
                display: 'grid',
                gridTemplateColumns: '20px 1fr',
                gap: 8,
                padding: '6px 0',
                borderTop: '1px solid var(--border, rgba(255,255,255,0.04))',
              }}
            >
              <StatusIcon state={iconState} />
              <div>
                <div
                  style={{
                    fontSize: '0.82rem',
                    color: 'var(--text)',
                    fontWeight: 600,
                  }}
                >
                  {row.label}
                </div>
                <div
                  style={{
                    fontSize: '0.72rem',
                    color: 'var(--text-dim, #6E6E80)',
                    lineHeight: 1.4,
                    marginTop: 2,
                    fontFamily: row.ok && row.detail.startsWith('/')
                      ? 'JetBrains Mono, monospace'
                      : 'inherit',
                  }}
                >
                  {row.detail}
                </div>
              </div>
            </li>
          );
        })}
      </ul>
      <div
        data-testid="restore-preflight-all-present"
        data-state={c.all_present ? 'ready' : 'missing'}
        style={{
          marginTop: 10,
          paddingTop: 8,
          borderTop: '1px solid var(--border, rgba(255,255,255,0.08))',
          fontSize: '0.78rem',
          fontWeight: 700,
          color: c.all_present ? 'var(--green, #10B981)' : 'var(--red, #EF4444)',
        }}
      >
        Overall: {c.all_present ? 'READY' : 'MISSING PIECES'}
      </div>
    </div>
  );
}

function Stat({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div style={{
      padding: 10,
      borderRadius: 8,
      background: 'rgba(18,18,26,0.6)',
      border: '1px solid var(--border, rgba(255,255,255,0.08))',
    }}>
      <div style={{ fontSize: '0.7rem', color: 'var(--text-secondary, #8b8b9e)', textTransform: 'uppercase', letterSpacing: '0.04em', fontWeight: 700 }}>{label}</div>
      <div style={{ fontSize: '0.92rem', color: 'var(--text)', fontWeight: 600, marginTop: 2, fontFamily: mono ? 'JetBrains Mono, monospace' : 'inherit' }}>
        {value}
      </div>
    </div>
  );
}
