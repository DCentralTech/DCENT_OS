// RestoreToStockModal — multi-step modal container.
//
// 6 steps:
//   1. RestoreStatus     — current state + breaker warning + hashboard pick
//   2. TypeSerial        — type miner serial verbatim
//   3. NandBackup        — stage tarball + run preflight
//   4. SafetyPreflight   — show findings; lock on critical, ack on high
//   5. Confirm           — phrase + slider, POST confirm:true
//   6. HarvestModeBanner — post-reboot banner

import React, { useEffect, useMemo, useRef, useState } from 'react';
import { OverlayDialog } from '../common/OverlayDialog';
import { useMinerStore } from '../../store/miner';
import {
  isNonTerminalPhase,
  phaseLabel,
  recoveryGuidanceFor,
  restoreToStockApi,
  type RestoreToStockResponse,
  type RestoreToStockStatus,
  type RestoreStatePhase,
} from '../../api/restore-to-stock';
import { RestoreStatus } from './RestoreStatus';
import { TypeSerial, isSerialMatch } from './TypeSerial';
import { NandBackup } from './NandBackup';
import {
  SafetyPreflight,
  preflightBlocksFlash,
  preflightRequiresHighAck,
} from './SafetyPreflight';
import { Confirm } from './Confirm';

interface Props {
  open: boolean;
  onClose: () => void;
}

const STEP_LABELS = ['Status', 'Serial', 'Stage', 'Preflight', 'Confirm', 'Reboot'];

export function RestoreToStockModal({ open, onClose }: Props) {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const expectedSerial = systemInfo?.hardware?.miner_serial ?? systemInfo?.hostname ?? '';
  const resetTimer = useRef<number | null>(null);

  const [step, setStep] = useState(0);
  // R5-MEDIUM: hashboard_count_to_use UI/backend default-divergence —
  // resolved by defaulting the UI to 1 to match the user's stated home
  // breaker safety intent (see W9-G report). The backend default is
  // also being pulled to 1 in this wave so curl callers without an
  // explicit value behave the same way as the modal-driven flow.
  const [hashboardCountToUse, setHashboardCountToUse] = useState(1);
  const [acknowledgeBreakerWarning, setAcknowledgeBreakerWarning] = useState(false);
  const [typedSerial, setTypedSerial] = useState('');
  const [stagedPath, setStagedPath] = useState<string | null>(null);
  const [preflight, setPreflight] = useState<RestoreToStockResponse | null>(null);
  const [highAcknowledged, setHighAcknowledged] = useState(false);
  const [submittedResponse, setSubmittedResponse] = useState<RestoreToStockResponse | null>(null);

  const reset = () => {
    setStep(0);
    setHashboardCountToUse(1);
    setAcknowledgeBreakerWarning(false);
    setTypedSerial('');
    setStagedPath(null);
    setPreflight(null);
    setHighAcknowledged(false);
    setSubmittedResponse(null);
  };

  const close = () => {
    onClose();
    // Reset on next open — wait one tick so the step doesn't flicker.
    if (resetTimer.current != null) {
      window.clearTimeout(resetTimer.current);
    }
    resetTimer.current = window.setTimeout(() => {
      reset();
      resetTimer.current = null;
    }, 200);
  };

  useEffect(() => {
    if (open && resetTimer.current != null) {
      window.clearTimeout(resetTimer.current);
      resetTimer.current = null;
      reset();
    }
  }, [open]);

  useEffect(() => {
    return () => {
      if (resetTimer.current != null) {
        window.clearTimeout(resetTimer.current);
      }
    };
  }, []);

  const blocksFlash = preflightBlocksFlash(preflight);
  const needsHighAck = preflightRequiresHighAck(preflight);

  const canAdvance = useMemo(() => {
    if (step === 0) return acknowledgeBreakerWarning && hashboardCountToUse > 0;
    if (step === 1) return isSerialMatch(typedSerial, expectedSerial);
    if (step === 2) return !!stagedPath && !!preflight;
    if (step === 3) {
      if (blocksFlash) return false;
      if (needsHighAck && !highAcknowledged) return false;
      return true;
    }
    return false;
  }, [step, acknowledgeBreakerWarning, hashboardCountToUse, typedSerial, expectedSerial, stagedPath, preflight, blocksFlash, needsHighAck, highAcknowledged]);

  const advance = () => setStep(s => Math.min(s + 1, STEP_LABELS.length - 1));
  const back = () => setStep(s => Math.max(s - 1, 0));

  // R5'-M3 (W10-C): backdrop-click + ESC dismissal is gated on step.
  // Steps 0-1 (Status/Serial) are pre-destructive — modal can be
  // dismissed by clicking outside or pressing ESC. Step 2 (Stage)
  // onward triggers tarball upload + safety preflight + (eventually)
  // NAND backup; an accidental click-outside or ESC keypress should
  // NOT cancel mid-flow. The explicit Cancel/Close buttons inside the
  // modal still call `close` directly so the operator always has an
  // intentional escape hatch.
  const dismissible = step < 2;

  return (
    <OverlayDialog
      open={open}
      onClose={close}
      ariaLabel="Restore to stock confirmation"
      maxWidth={720}
      width="92%"
      dismissible={dismissible}
    >
      <div className="p4-restore-modal">
        <div className="p4-restore-modal__head">
          <h2 className="p4-restore-modal__title">Restore to stock</h2>
          <button type="button" onClick={close} style={closeBtn} aria-label="Close">×</button>
        </div>
        <Stepper step={step} labels={STEP_LABELS} />

        <div style={{ marginTop: 16 }}>
          {step === 0 && (
            <RestoreStatus
              hashboardCountToUse={hashboardCountToUse}
              setHashboardCountToUse={setHashboardCountToUse}
              acknowledgeBreakerWarning={acknowledgeBreakerWarning}
              setAcknowledgeBreakerWarning={setAcknowledgeBreakerWarning}
            />
          )}
          {step === 1 && (
            <TypeSerial typedSerial={typedSerial} setTypedSerial={setTypedSerial} />
          )}
          {step === 2 && (
            <NandBackup
              stagedPath={stagedPath}
              setStagedPath={setStagedPath}
              onPreflightDone={(p) => setPreflight(p)}
            />
          )}
          {step === 3 && (
            <SafetyPreflight
              preflight={preflight}
              highAcknowledged={highAcknowledged}
              setHighAcknowledged={setHighAcknowledged}
            />
          )}
          {step === 4 && stagedPath && (
            <Confirm
              stagedPath={stagedPath}
              typedSerial={typedSerial}
              acknowledgeBreakerWarning={acknowledgeBreakerWarning}
              hashboardCountToUse={hashboardCountToUse}
              acknowledgeHighFindings={highAcknowledged}
              onSubmitted={(resp) => { setSubmittedResponse(resp); setStep(5); }}
            />
          )}
          {step === 5 && (
            <RebootScheduled response={submittedResponse} onClose={close} />
          )}
        </div>

        {step !== 4 && step !== 5 && (
          <div style={{ display: 'flex', gap: 8, justifyContent: 'space-between', marginTop: 24 }}>
            <button type="button" onClick={back} disabled={step === 0} style={secondaryBtn}>Back</button>
            <div style={{ display: 'flex', gap: 8 }}>
              <button type="button" onClick={close} style={secondaryBtn}>Cancel</button>
              <button type="button" onClick={advance} disabled={!canAdvance} style={primaryBtn}>
                Next
              </button>
            </div>
          </div>
        )}
      </div>
    </OverlayDialog>
  );
}

function RebootScheduled({ response, onClose }: { response: RestoreToStockResponse | null; onClose: () => void }) {
  const ms = response?.reboot_at_ms ?? null;
  const remaining = ms ? Math.max(0, Math.round((ms - Date.now()) / 1000)) : null;

  // -prep A5''-OPS-HIGH-1 +  W11-C (A5''-OPS-MED-3): poll
  // /status both for `last_backup_fw_setenv_present` AND for the live
  // mid-flash phase. While the daemon is in a non-terminal phase
  // (nand_backup_running / staging / scheduled / flash_running) the
  // dashboard polls once per second and renders a phase-specific
  // label + spinner. On a terminal phase (flash_succeeded /
  // flash_failed / preflight_failed / staging_failed /
  // nand_backup_failed / idle) the interval is cleared so we don't
  // burn CPU. The polled status overrides the response snapshot,
  // because the response is captured at the moment of the
  // confirm:true POST and stales out within seconds.
  const [statusSnap, setStatusSnap] = useState<RestoreToStockStatus | null>(null);
  useEffect(() => {
    let cancelled = false;
    let intervalId: number | null = null;

    const tick = async () => {
      try {
        const s = await restoreToStockApi.status();
        if (cancelled) return;
        setStatusSnap(s);
        // Stop polling once the daemon reaches a terminal phase OR
        // there's no state_detail at all (older daemon / pre-W9-C
        // backend). The initial mount-time fetch already populates
        // `last_backup_fw_setenv_present` for the wave-11-prep banner.
        const phase = s.state_detail?.phase;
        if (!isNonTerminalPhase(phase) && intervalId != null) {
          window.clearInterval(intervalId);
          intervalId = null;
        }
      } catch {
        // Network errors during a flash are expected (the miner is
        // mid-reboot); silently swallow so we don't render error
        // noise. Polling continues until terminal phase reached or
        // the modal unmounts.
      }
    };

    // Mount-time fetch (also surfaces last_backup_fw_setenv_present).
    void tick();
    intervalId = window.setInterval(() => { void tick(); }, 1000);
    return () => {
      cancelled = true;
      if (intervalId != null) window.clearInterval(intervalId);
    };
  }, []);

  // Live phase from polled status takes precedence over the captured
  // response. Falls back to response.state_detail (W10-C) when the
  // status endpoint doesn't carry one, then to the legacy
  // `response.status` string for very old daemons.
  const livePhase: RestoreStatePhase | undefined =
    statusSnap?.state_detail?.phase ?? response?.state_detail?.phase;
  const isPolling = isNonTerminalPhase(livePhase);

  // R5'-#24 (W10-C): when the backend captured a state_detail snapshot
  // and that snapshot is `flash_failed`, surface the reason inline so
  // the operator doesn't have to hunt through /var/log/dcentrald.log.
  // Prefer the freshest source — the polled status has the latest
  // reason; fall back to the captured response.
  const flashFailedDetail =
    statusSnap?.state_detail?.phase === 'flash_failed'
      ? statusSnap.state_detail
      : response?.state_detail?.phase === 'flash_failed'
        ? response.state_detail
        : null;
  const recovery = flashFailedDetail
    ? recoveryGuidanceFor(flashFailedDetail.reason)
    : null;

  const fwSetenvMissing = statusSnap?.last_backup_fw_setenv_present === false;
  return (
    <div>
      <h3 style={{ marginTop: 0, fontSize: '1.1rem' }}>Reboot scheduled</h3>
      <div style={{ fontSize: '0.85rem', color: 'var(--text)', marginBottom: 12 }}>
        Backend status: <code>{response?.status ?? 'unknown'}</code>
        {remaining != null && <> · reboot in <strong>{remaining}s</strong></>}
      </div>
      {/* Wave 11 W11-C (A5''-OPS-MED-3): live phase rendering. */}
      {livePhase && (
        <div
          data-testid="restore-phase-row"
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            padding: '10px 12px',
            borderRadius: 8,
            background: 'rgba(18,18,26,0.6)',
            border: '1px solid var(--border, rgba(255,255,255,0.08))',
            marginBottom: 12,
          }}
        >
          {isPolling && (
            <span
              aria-label="In progress"
              style={{
                width: 12,
                height: 12,
                borderRadius: '50%',
                border: '2px solid rgba(247,147,26,0.3)',
                borderTopColor: 'var(--accent, #FAA500)',
                animation: 'spin 0.8s linear infinite',
                display: 'inline-block',
              }}
            />
          )}
          <span style={{ fontSize: '0.85rem', color: 'var(--text)', fontWeight: 600 }}>
            {phaseLabel(livePhase)}
          </span>
          <code style={{ fontSize: '0.7rem', color: 'var(--text-dim, #6E6E80)', marginLeft: 'auto' }}>
            {livePhase}
          </code>
        </div>
      )}
      {/*
        Wave 13 W13-D (A2'-#1): VNish-style polled progress streaming.
        While the daemon is in `flash_running`, render the last ~10
        stderr/stdout lines streamed by the spawned writer. Hidden on
        terminal phases (the red flash_failed callout below already
        carries the failure reason). Lines prefixed `[err] ` are stderr;
        unprefixed are stdout.
      */}
      {livePhase === 'flash_running' &&
        statusSnap?.recent_log_lines &&
        statusSnap.recent_log_lines.length > 0 && (
          <div
            data-testid="restore-progress-stream"
            style={{
              marginBottom: 12,
            }}
          >
            <div
              style={{
                fontSize: '0.7rem',
                fontWeight: 700,
                color: 'var(--text-secondary, #8b8b9e)',
                textTransform: 'uppercase',
                letterSpacing: '0.04em',
                marginBottom: 6,
              }}
            >
              Writer output (live)
            </div>
            <pre
              className="p4-restore-log"
              style={{
                margin: 0,
                fontFamily: 'JetBrains Mono, monospace',
                fontSize: '0.72rem',
                lineHeight: 1.4,
                maxHeight: 200,
                overflowY: 'auto',
                color: 'var(--text)',
                padding: 10,
                borderRadius: 6,
                border: '1px solid var(--border, rgba(255,255,255,0.08))',
                whiteSpace: 'pre-wrap',
                wordBreak: 'break-word',
              }}
            >
              {statusSnap.recent_log_lines.slice(-10).join('\n')}
            </pre>
          </div>
        )}
      {flashFailedDetail && (
        <div style={{
          background: 'rgba(239,68,68,0.1)',
          border: '1px solid rgba(239,68,68,0.4)',
          padding: 12,
          borderRadius: 4,
          marginTop: 12,
          marginBottom: 12,
        }}>
          <strong style={{ color: 'var(--red, #EF4444)' }}>Flash failed:</strong>
          <div style={{ marginTop: 6, fontFamily: 'JetBrains Mono, monospace', fontSize: '0.85rem', color: 'var(--text)' }}>
            {flashFailedDetail.reason}
          </div>
          {recovery && (
            <div
              data-testid="restore-recovery-guidance"
              data-severity={recovery.severity}
              style={{ marginTop: 8, fontSize: '0.78rem', color: 'var(--text)', lineHeight: 1.5 }}
            >
              <strong style={{ color: 'var(--accent, #FAA500)' }}>Recovery: </strong>
              {recovery.text}
            </div>
          )}
          <div style={{ marginTop: 8, fontSize: '0.72rem', color: 'var(--text-dim, #6E6E80)', lineHeight: 1.5 }}>
            Reference: <code>STOCK_BOOT_HARVEST_PROCEDURE.md §10</code> — power-cycle
            stays on the active slot since bootslot was not flipped.
          </div>
        </div>
      )}
      {response?.backup_path && (
        <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 8 }}>
          NAND backup: <code>{response.backup_path}</code>
        </div>
      )}
      {fwSetenvMissing && (
        <div style={{
          background: 'rgba(245,158,11,0.1)',
          border: '1px solid rgba(245,158,11,0.5)',
          padding: 12,
          borderRadius: 4,
          marginTop: 12,
          marginBottom: 12,
        }}>
          <strong style={{ color: 'var(--amber, #F59E0B)' }}>
            Option-A recovery unavailable for this backup
          </strong>
          <div style={{ marginTop: 6, fontSize: '0.85rem', color: 'var(--text)', lineHeight: 1.5 }}>
            The daemon could NOT copy a working <code>fw_setenv</code> into your
            backup directory ({response?.backup_path ?? 'see /status'}). The
            in-stock recovery command <code>fw_setenv bootslot &lt;prev&gt;</code>
            may not work if stock Bitmain doesn't ship libubootenv-tools.
          </div>
          <div style={{ marginTop: 8, fontSize: '0.78rem', color: 'var(--text-dim, #6E6E80)', lineHeight: 1.5 }}>
            <strong>Plan now:</strong> have your USB-TTL serial cable wired to
            the S9 UART header BEFORE the reboot. Use Option B (serial console
            U-Boot env edit) per <code>STOCK_BOOT_HARVEST_PROCEDURE.md §10</code>.
            If the cable isn't wired, abort the reboot now via SSH:{' '}
            <code>fw_setenv bootslot {/* prev */}</code> from this host.
          </div>
        </div>
      )}
      <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', lineHeight: 1.5 }}>
        After the reboot, stock Bitmain comes up. Run the harvest script from your host within
        the 10-minute window (see <code>STOCK_BOOT_HARVEST_PROCEDURE.md</code> §10).{' '}
        <strong>Manual recovery REQUIRED to return to DCENT_OS.</strong> U-Boot auto_recovery is
        DEFEATED by S99upgrade in both DCENT_OS and stock Bitmain. From inside booted stock
        (default <code>root:admin</code>) run <code>fw_setenv bootslot 1</code> (or whichever the
        prev slot was) THEN power-cycle. If stock won't boot or auth fails, attach a USB-TTL
        serial cable to the S9's UART header and stop U-Boot at the prompt; manually run{' '}
        <code>setenv bootslot 1; saveenv; reset</code>.
      </div>
      <button type="button" onClick={onClose} style={{ ...primaryBtn, marginTop: 16 }}>Close</button>
    </div>
  );
}

function Stepper({ step, labels }: { step: number; labels: string[] }) {
  return (
    <ol aria-label="Restore to stock steps" className="p4-stepper p4-stepper--danger">
      {labels.map((label, idx) => {
        const active = idx === step;
        const done = idx < step;
        return (
          <li
            key={label}
            aria-current={active ? 'step' : undefined}
            className={`p4-step${active ? ' is-active' : ''}${done ? ' is-done' : ''}`}
          >
            <span className="p4-step__num">{idx + 1}</span>
            {label}
          </li>
        );
      })}
    </ol>
  );
}

const primaryBtn: React.CSSProperties = {
  padding: '8px 16px',
  borderRadius: 8,
  border: 'none',
  background: 'var(--accent, #FAA500)',
  color: '#0a0a0f',
  fontWeight: 700,
  fontSize: '0.85rem',
  cursor: 'pointer',
};

const secondaryBtn: React.CSSProperties = {
  padding: '8px 16px',
  borderRadius: 8,
  border: '1px solid var(--border, rgba(255,255,255,0.12))',
  background: 'transparent',
  color: 'var(--text)',
  fontWeight: 600,
  fontSize: '0.85rem',
  cursor: 'pointer',
};

const closeBtn: React.CSSProperties = {
  width: 32,
  height: 32,
  borderRadius: '50%',
  border: '1px solid var(--border, rgba(255,255,255,0.12))',
  background: 'transparent',
  color: 'var(--text)',
  fontSize: '1.2rem',
  lineHeight: '30px',
  cursor: 'pointer',
};
