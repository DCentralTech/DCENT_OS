// DCENT_OS Setup Wizard — Password step.
//
// Structural recreation of the kit `PasswordStep` (ui_kits/wizard/Wizard.jsx):
// the password field + show/hide toggle, the 4-segment strength bar with a
// strength label, the confirm field with match/mismatch state, and the
// "Unauthenticated until set" info note.
//
// Real wiring preserved verbatim: resumeExistingPassword / alreadyAuthenticated
// gates (hacker mode + resuming a password-protected unit stay strict), the
// onChange/onConfirmChange callbacks SetupWizard uses to drive
// /api/auth/setup + skipPassword, and the freedom-first "Continue without a
// password" opt-out. Strength uses the kit's 5-tier scoring.

import React, { useState } from 'react';
import type { OperatingMode } from '../../api/types';

interface PasswordStepProps {
  value: string;
  confirmValue: string;
  mode: OperatingMode | null;
  resumeExistingPassword?: boolean;
  alreadyAuthenticated?: boolean;
  onChange: (password: string) => void;
  onConfirmChange: (password: string) => void;
  /** Freedom-first: present when skipping a password is allowed. */
  onSkip?: () => void;
}

// Kit strength model (Wizard.jsx `strength` + STR_LABELS/STR_COLORS).
function strength(pw: string): number {
  let s = 0;
  if (pw.length >= 8) s++;
  if (pw.length >= 12) s++;
  if (/[A-Z]/.test(pw) && /[a-z]/.test(pw)) s++;
  if (/[0-9]/.test(pw)) s++;
  if (/[^A-Za-z0-9]/.test(pw)) s++;
  return Math.min(s, 4);
}
const STR_LABELS = ['Too short', 'Weak', 'OK', 'Good', 'Strong'];
const STR_COLORS = [
  'var(--wz-red)',
  'var(--wz-red)',
  'var(--wz-yellow)',
  'var(--wz-accent)',
  'var(--wz-green)',
];

export function PasswordStep({
  value,
  confirmValue,
  mode,
  resumeExistingPassword = false,
  alreadyAuthenticated = false,
  onChange,
  onConfirmChange,
  onSkip,
}: PasswordStepProps) {
  const [show, setShow] = useState(false);

  const isRequired = resumeExistingPassword || mode === 'hacker';
  const s = strength(value);
  const mismatch = value.length > 0 && confirmValue.length > 0 && value !== confirmValue;
  const matches = value.length > 0 && confirmValue.length > 0 && value === confirmValue;

  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">
        {resumeExistingPassword ? 'Resume setup' : 'Set a dashboard password'}
      </h2>
      <p className="wiz-lede">
        {resumeExistingPassword
          ? 'This miner already has an owner password. Enter it to resume onboarding from this browser.'
          : isRequired
            ? 'Advanced mode has access to raw hardware controls. A password is required to prevent accidental or unauthorized changes.'
            : 'Used to unlock the local dashboard. There is no recovery email — write this down, keep it in a password manager, or pick something memorable.'}
      </p>

      {isRequired && (
        <div className="wiz-info danger">
          <strong>
            {resumeExistingPassword
              ? 'Owner password required to continue.'
              : 'Password required for Advanced mode.'}
          </strong>{' '}
          {resumeExistingPassword
            ? 'The owner password is required before the wizard can continue changing setup state.'
            : 'Raw FPGA and voltage controls can damage hardware if misused.'}
        </div>
      )}

      {!isRequired && !alreadyAuthenticated && (
        <div className="wiz-info amber">
          <strong>Strongly recommended (not required).</strong> A dashboard password
          protects every write &amp; control action (tuning, pools, restart, restore).
          You can run without one and add it later in Settings — your call.
        </div>
      )}

      {alreadyAuthenticated && (
        <div className="wiz-info ok">
          <strong>Already authenticated.</strong> This browser already has credentials
          for the current owner. Continue to review when ready.
        </div>
      )}

      {!alreadyAuthenticated && (
        <>
          <div className="wiz-fld">
            <label htmlFor="wiz-password">
              Password {!isRequired && <span style={{ color: 'var(--wz-fg-dim)', fontWeight: 400 }}>(optional)</span>}
            </label>
            <div className="wiz-input-wrap">
              <input
                id="wiz-password"
                type={show ? 'text' : 'password'}
                value={value}
                onChange={e => onChange(e.target.value)}
                placeholder={isRequired ? 'Enter a password (8+ characters)' : 'Leave empty to skip — at least 8 characters'}
                autoComplete="new-password"
                aria-describedby={value.length > 0 && !resumeExistingPassword ? 'wiz-password-strength' : undefined}
              />
              <button
                type="button"
                className="wiz-input-show"
                onClick={() => setShow(v => !v)}
                aria-label={show ? 'Hide password' : 'Show password'}
                aria-pressed={show}
              >
                {show ? 'hide' : 'show'}
              </button>
            </div>
            {value && !resumeExistingPassword && (
              <div className="wiz-strength">
                <div className="wiz-strength-bar">
                  {[0, 1, 2, 3].map(i => (
                    <div
                      key={i}
                      style={{ background: i < s ? STR_COLORS[s] : 'rgba(255,255,255,.06)' }}
                    />
                  ))}
                </div>
                <span id="wiz-password-strength" style={{ color: STR_COLORS[s] }}>
                  {STR_LABELS[s]}
                </span>
              </div>
            )}
          </div>

          {value.length > 0 && !resumeExistingPassword && (
            <div className="wiz-fld">
              <label htmlFor="wiz-password-confirm">Confirm password</label>
              <div className="wiz-input-wrap">
                <input
                  id="wiz-password-confirm"
                  type={show ? 'text' : 'password'}
                  value={confirmValue}
                  onChange={e => onConfirmChange(e.target.value)}
                  placeholder="Type it again"
                  aria-invalid={mismatch}
                  aria-describedby={confirmValue.length > 0 ? 'wiz-password-confirm-status' : undefined}
                />
              </div>
              {mismatch && (
                <span id="wiz-password-confirm-status" className="wiz-err">
                  Passwords don&apos;t match.
                </span>
              )}
              {matches && (
                <span id="wiz-password-confirm-status" className="wiz-ok">
                  Passwords match.
                </span>
              )}
            </div>
          )}
        </>
      )}

      <div className="wiz-info">
        <strong>Unauthenticated until set.</strong>{' '}
        {mode === 'hacker'
          ? 'The password protects the web dashboard only. SSH access uses separate credentials. You can change or remove it later in Settings.'
          : resumeExistingPassword
            ? 'Enter the existing owner password to continue setup from this browser session.'
            : 'Until you set a password, write & control actions stay locked and the dashboard refuses non-local connections. You can change or remove it later in Settings.'}
      </div>

      {onSkip && !isRequired && !alreadyAuthenticated && value.length === 0 && (
        <button type="button" className="wiz-btn lg full" onClick={onSkip}>
          Continue without a password
        </button>
      )}
    </div>
  );
}
