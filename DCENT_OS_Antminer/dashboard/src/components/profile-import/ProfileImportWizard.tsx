// ProfileImportWizard — 5-step container.
//
// Steps:
//   1. Upload     — drag-drop firmware tarball or JSON profile
//   2. Detection  — review parsed (model, hashboard, chip, source_class)
//   3. Diff       — preset rows vs active profile
//   4. SourceClass — operator picks trust level
//   5. Apply      — POST /api/profiles/silicon/import-json
//
// Mounted at hash route `#/profiles/import` (see ProfilesPage).

import React, { useState } from 'react';
import type { SiliconProfileBundle } from '../../api/profiles-silicon';
import { Upload } from './Upload';
import { DetectionResults } from './DetectionResults';
import { Diff } from './Diff';
import { SourceClassSelect } from './SourceClassSelect';
import { Apply } from './Apply';

interface Props {
  onClose: () => void;
}

const STEP_LABELS = ['Upload', 'Detection', 'Diff', 'Source class', 'Apply'];

export function ProfileImportWizard({ onClose }: Props) {
  const [step, setStep] = useState(0);
  const [bundle, setBundle] = useState<SiliconProfileBundle | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const goNext = () => setStep(s => Math.min(s + 1, STEP_LABELS.length - 1));
  const goBack = () => setStep(s => Math.max(s - 1, 0));

  const patchBundle = (patch: Partial<SiliconProfileBundle>) => {
    setBundle(b => (b ? { ...b, ...patch } : b));
  };

  const canAdvance = (() => {
    if (step === 0) return bundle !== null;
    if (step === 1) return !!bundle && !!bundle.miner_model && !!bundle.hashboard && !!bundle.chip;
    if (step === 2) return true;
    if (step === 3) return !!bundle && bundle.source_class !== 'live_confirmed' && bundle.source_class !== 'baked';
    return false;
  })();

  return (
    <div className="p4-wizard-shell">
      <div className="p4-wizard-head">
        <div>
          <h2 className="p4-wizard-title">Import silicon profile</h2>
          <div className="p4-wizard-sub">
            Drop a firmware tarball or a profile JSON, review what was detected, diff against
            the active profile, and apply.
          </div>
        </div>
        <button type="button" onClick={onClose} style={closeBtn} aria-label="Close wizard">×</button>
      </div>

      <Stepper step={step} labels={STEP_LABELS} />

      {errorMsg && (
        <div
          role="alert"
          aria-live="assertive"
          style={{
            padding: '10px 12px',
            borderRadius: 8,
            margin: '12px 0',
            background: 'rgba(239,68,68,0.08)',
            border: '1px solid rgba(239,68,68,0.22)',
            color: 'var(--text)',
            fontSize: '0.78rem',
          }}
        >
          {errorMsg}
          <button
            type="button"
            aria-label="Dismiss error"
            onClick={() => setErrorMsg(null)}
            style={{
              background: 'none',
              border: 'none',
              color: 'var(--text-dim)',
              marginLeft: 8,
              cursor: 'pointer',
            }}
          >dismiss</button>
        </div>
      )}

      <div style={{ marginTop: 16 }}>
        {step === 0 && (
          <Upload
            onParsed={(b) => { setBundle(b); setErrorMsg(null); setStep(1); }}
            onError={setErrorMsg}
          />
        )}
        {step === 1 && bundle && (
          <DetectionResults bundle={bundle} onPatch={patchBundle} />
        )}
        {step === 2 && bundle && (
          <Diff bundle={bundle} />
        )}
        {step === 3 && bundle && (
          <SourceClassSelect bundle={bundle} onPatch={patchBundle} />
        )}
        {step === 4 && bundle && (
          <Apply bundle={bundle} onClose={onClose} />
        )}
      </div>

      {/* Step navigation — Apply step controls its own buttons */}
      {step !== 4 && (
        <div className="p4-wizard-nav">
          <button type="button" onClick={goBack} disabled={step === 0} style={secondaryBtn}>
            Back
          </button>
          <div style={{ display: 'flex', gap: 8 }}>
            <button type="button" onClick={onClose} style={secondaryBtn}>Cancel</button>
            <button type="button" onClick={goNext} disabled={!canAdvance} style={primaryBtn}>
              Next
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function Stepper({ step, labels }: { step: number; labels: string[] }) {
  return (
    <ol aria-label="Import silicon profile steps" className="p4-stepper">
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
