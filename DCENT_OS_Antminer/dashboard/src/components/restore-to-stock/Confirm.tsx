// Step 5 — final confirm.
//
// Operator must:
//   1. Drag a slider all the way to the right
//   2. Type the literal phrase "RESTORE TO STOCK"
// Submit only fires POST /api/system/restore-to-stock with
// confirm:true after BOTH gates pass.
//
//  W9-G:
// - R5-H1: replaces the misleading "U-Boot auto_recovery returns DCENT_OS"
//   copy with the correct manual-recovery procedure.
// - R5-H4: renders BreakerWarningBanner at the top of the step.
// - R5-MEDIUM: rounds the operator's `highAcknowledged` checkbox state
//   to the wire as `acknowledge_high_findings` so the backend can
//   refuse `confirm:true` against unacknowledged HIGH findings.

import React, { useEffect, useState } from 'react';
import {
  REQUIRED_CONFIRM_PHRASE,
  restoreToStockApi,
  type RestoreToStockResponse,
  type RestoreToStockStatus,
} from '../../api/restore-to-stock';
import { BreakerWarningBanner } from './BreakerWarningBanner';
import { InfoDot } from '../common/Tooltip';

interface Props {
  stagedPath: string;
  typedSerial: string;
  acknowledgeBreakerWarning: boolean;
  hashboardCountToUse: number;
  acknowledgeHighFindings: boolean;
  onSubmitted: (resp: RestoreToStockResponse) => void;
}

export function Confirm({
  stagedPath,
  typedSerial,
  acknowledgeBreakerWarning,
  hashboardCountToUse,
  acknowledgeHighFindings,
  onSubmitted,
}: Props) {
  const [phrase, setPhrase] = useState('');
  const [slider, setSlider] = useState(0);
  const [submitting, setSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  //  W11-C (A5''-OPS-MED-1): poll /status when entering Confirm
  // and surface the prior-backup `last_backup_fw_setenv_present`
  // warning BEFORE the operator pulls the destructive trigger. This
  // mirrors the post-flash banner in RebootScheduled but moves the
  // warning earlier so the operator can wire the USB-TTL serial cable
  // (or abort) before they're committed. The check is forward-looking:
  // if a PRIOR backup lacked fw_setenv on this daemon lifetime, the
  // operator should expect THIS backup to lack it too unless the
  // miner's filesystem has changed.
  const [statusSnap, setStatusSnap] = useState<RestoreToStockStatus | null>(null);
  useEffect(() => {
    let cancelled = false;
    restoreToStockApi.status().then(s => { if (!cancelled) setStatusSnap(s); }).catch(() => {});
    return () => { cancelled = true; };
  }, []);
  const fwSetenvMissingPrior = statusSnap?.last_backup_fw_setenv_present === false;

  const phraseMatch = phrase.trim() === REQUIRED_CONFIRM_PHRASE;
  const sliderArmed = slider >= 100;
  const canSubmit = phraseMatch && sliderArmed && !submitting;

  const submit = async () => {
    setSubmitting(true);
    setErrorMsg(null);
    try {
      const resp = await restoreToStockApi.submit({
        stock_firmware_staged_path: stagedPath,
        operator_serial_typed: typedSerial,
        acknowledge_breaker_warning: acknowledgeBreakerWarning,
        hashboard_count_to_use: hashboardCountToUse,
        confirm_string_typed: REQUIRED_CONFIRM_PHRASE,
        confirm: true,
        acknowledge_high_findings: acknowledgeHighFindings,
      });
      onSubmitted(resp);
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : 'Submit failed');
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div>
      <BreakerWarningBanner />
      {fwSetenvMissingPrior && (
        <div
          data-testid="confirm-fwsetenv-warning"
          role="alert"
          style={{
            background: 'rgba(245,158,11,0.1)',
            border: '1px solid rgba(245,158,11,0.5)',
            padding: 12,
            borderRadius: 8,
            marginBottom: 14,
          }}
        >
          <strong style={{ color: 'var(--amber, #F59E0B)' }}>
            ⚠ Prior backup lacked fw_setenv — Option-A recovery may be unavailable
          </strong>
          <div style={{ marginTop: 6, fontSize: '0.82rem', color: 'var(--text)', lineHeight: 1.5 }}>
            The most recent NAND backup on this daemon could NOT include a
            working <code>fw_setenv</code>. If the same conditions hold for THIS
            flash you will only have <strong>Option B (serial console U-Boot env
            edit)</strong> for recovery (<code>STOCK_BOOT_HARVEST_PROCEDURE.md §10</code>).
          </div>
          <div style={{ marginTop: 6, fontSize: '0.78rem', color: 'var(--text-dim, #6E6E80)', lineHeight: 1.5 }}>
            <strong>Wire the USB-TTL serial cable to the S9 UART header BEFORE
            pressing the slider</strong>, or abort now and install
            libubootenv-tools on the miner first.
          </div>
        </div>
      )}
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap', marginBottom: 6 }}>
        <h3 style={{ margin: 0, fontSize: '1.1rem' }}>Commit the flash</h3>
        <span className="ds-chip ds-danger" aria-label="Irreversible step">
          <span className="ds-dot" aria-hidden />
          Point of no return
        </span>
        <InfoDot term="scheduled_not_booted" placement="bottom" label="What 'committed' means here" />
      </div>
      <div id="confirm-irreversibility-note" style={{ fontSize: '0.82rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 14, lineHeight: 1.5 }}>
        Two gates between you and the flash. Backend re-checks both at the wire — direct curl
        bypasses don't help. <strong>Once you arm the slider and submit</strong>, the daemon
        starts the NAND backup, writes stock firmware to the inactive slot, flips{' '}
        <code>bootslot</code>, and schedules a reboot. There is no "undo" button after submit —
        recovery is the manual procedure described below.
      </div>

      <div style={{
        padding: 12,
        borderRadius: 8,
        background: 'rgba(18,18,26,0.6)',
        border: '1px solid var(--border, rgba(255,255,255,0.08))',
        marginBottom: 14,
        fontSize: '0.78rem',
      }}>
        <div>Staged: <code>{stagedPath}</code></div>
        <div>Hashboards: {hashboardCountToUse}</div>
        <div>Serial typed: <code>{typedSerial || '(empty)'}</code></div>
      </div>

      <div style={{ marginBottom: 14 }}>
        <label htmlFor="restore-phrase-input" style={{ display: 'block', fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 6 }}>
          Type the phrase: <code style={{ color: 'var(--accent, #FAA500)' }}>{REQUIRED_CONFIRM_PHRASE}</code>
        </label>
        <input
          id="restore-phrase-input"
          type="text"
          className="ds-input"
          value={phrase}
          onChange={(e) => setPhrase(e.target.value)}
          autoComplete="off"
          spellCheck={false}
          aria-invalid={phrase.length > 0 && !phraseMatch}
          style={{
            fontFamily: 'var(--font-mono)',
            textTransform: 'none',
            ...(phraseMatch ? { borderColor: 'var(--green)' } : {}),
          }}
          placeholder={REQUIRED_CONFIRM_PHRASE}
        />
        <div
          role="status"
          aria-live="polite"
          style={{ marginTop: 6, fontSize: '0.72rem', color: phraseMatch ? 'var(--green, #2DD4A0)' : 'var(--text-dim, #6E6E80)' }}
        >
          {phraseMatch ? '✓ Phrase matches.' : 'Type the literal phrase exactly (case sensitive).'}
        </div>
      </div>

      <div style={{ marginBottom: 14 }}>
        <label htmlFor="restore-slider-input" style={{ display: 'block', fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 6 }}>
          Drag the slider to arm the flash
        </label>
        <input
          id="restore-slider-input"
          type="range"
          min={0}
          max={100}
          value={slider}
          onChange={(e) => setSlider(Number(e.target.value))}
          aria-label="Arm the flash — drag fully to the right to enable submit"
          aria-valuetext={sliderArmed ? 'Armed — flash enabled' : `${slider} of 100 — drag fully right to arm`}
          aria-describedby="confirm-irreversibility-note"
          className="p4-arm-slider"
          style={{ width: '100%', accentColor: sliderArmed ? 'var(--red, #EF4444)' : 'var(--accent, #FAA500)' }}
        />
        <div
          role="status"
          aria-live="polite"
          style={{ fontSize: '0.72rem', color: sliderArmed ? 'var(--red, #EF4444)' : 'var(--text-dim, #6E6E80)' }}
        >
          {sliderArmed ? '✓ Slider armed.' : `Slider at ${slider}/100. Drag fully right.`}
        </div>
      </div>

      {errorMsg && (
        <div
          role="alert"
          className="adv-msg is-error"
          style={{
            padding: '10px 12px',
            borderRadius: 'var(--radius-sm)',
            background: 'var(--red-dim)',
            border: '1px solid var(--red)',
            fontSize: '0.82rem',
            marginBottom: 12,
          }}
        >
          {errorMsg}
        </div>
      )}

      <button
        type="button"
        className="ds-btn danger lg"
        onClick={submit}
        disabled={!canSubmit}
        aria-label="Flash now — restore to stock"
        aria-describedby="confirm-irreversibility-note"
        style={{ width: '100%' }}
      >
        {submitting ? 'Submitting...' : 'Flash now — restore to stock'}
      </button>

      <div style={{ fontSize: '0.72rem', color: 'var(--text-dim, #6E6E80)', marginTop: 8, textAlign: 'center', lineHeight: 1.5 }}>
        On success the daemon schedules a reboot in 30s. <strong>Manual recovery REQUIRED</strong> to
        return to DCENT_OS — U-Boot auto_recovery is DEFEATED by S99upgrade in both DCENT_OS and
        stock Bitmain. From inside booted stock (default <code>root:admin</code>) run{' '}
        <code>fw_setenv bootslot 1</code> (or whichever the prev slot was) THEN power-cycle. If
        stock won't boot or auth fails, attach a USB-TTL serial cable to the S9's UART header and
        stop U-Boot at the prompt; manually run <code>setenv bootslot 1; saveenv; reset</code>.
      </div>
    </div>
  );
}
