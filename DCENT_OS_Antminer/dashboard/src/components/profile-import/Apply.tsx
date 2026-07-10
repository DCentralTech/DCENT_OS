// Step 5 — POST /api/profiles/silicon/import-json with the patched
// bundle. On success, show new active-profile state + close wizard
// CTA. On 400 (SECURE_BOOT_SET / Hashcore / validation), show the
// error verbatim.

import React, { useState } from 'react';
import { siliconProfilesApi, type SiliconImportResponse, type SiliconProfileBundle } from '../../api/profiles-silicon';
import { InfoBanner } from '../common/InfoBanner';

interface Props {
  bundle: SiliconProfileBundle;
  onClose: () => void;
}

export function Apply({ bundle, onClose }: Props) {
  const [phase, setPhase] = useState<'idle' | 'submitting' | 'done' | 'error'>('idle');
  const [response, setResponse] = useState<SiliconImportResponse | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [setActiveBusy, setSetActiveBusy] = useState(false);
  const [activeMsg, setActiveMsg] = useState<string | null>(null);

  const submit = async () => {
    setPhase('submitting');
    setErrorMsg(null);
    try {
      const res = await siliconProfilesApi.importJson(bundle);
      setResponse(res);
      setPhase('done');
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : 'Import failed');
      setPhase('error');
    }
  };

  const setActive = async () => {
    if (!response) return;
    setSetActiveBusy(true);
    setActiveMsg(null);
    try {
      const r = await siliconProfilesApi.setActive(bundle.miner_model, bundle.hashboard, response.id);
      // CC-1: surface applied-live vs saved-for-next-cycle honestly (the backend
      // returns this precisely so the wizard doesn't imply a live apply that
      // didn't happen).
      const rt = r.runtime;
      if (rt?.applied_runtime === true) {
        setActiveMsg('Applied to the running miner now.');
      } else if (rt && ['ack_timeout', 'closed', 'unavailable', 'closed_before_ack'].includes(rt.status)) {
        setActiveMsg('Saved — will apply on the next autotuner cycle (live channel unavailable).');
      } else {
        setActiveMsg(r.note ?? 'Active selection saved.');
      }
    } catch (e) {
      setActiveMsg(e instanceof Error ? e.message : 'Set-active failed');
    } finally {
      setSetActiveBusy(false);
    }
  };

  return (
    <div className="section">
      <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 12 }}>
        Last step. Review the summary below — once you confirm, the bundle is written to
        <code> /etc/dcentrald/profiles.d/operator/</code> and the registry is reloaded.
      </div>

      <div style={{
        background: 'rgba(18,18,26,0.6)',
        border: '1px solid var(--border, rgba(255,255,255,0.08))',
        borderRadius: 8,
        padding: 12,
        fontSize: '0.78rem',
        marginBottom: 16,
      }}>
        <SummaryRow label="Model" value={bundle.miner_model} />
        <SummaryRow label="Hashboard" value={bundle.hashboard} />
        <SummaryRow label="Chip" value={bundle.chip} />
        <SummaryRow label="Source class" value={bundle.source_class} />
        <SummaryRow label="Preset rows" value={String(bundle.presets.length)} />
      </div>

      {phase === 'idle' && (
        <button type="button" onClick={submit} style={primaryBtn}>
          Apply import
        </button>
      )}

      {phase === 'submitting' && (
        <div role="status" aria-live="polite" style={{ color: 'var(--text-secondary, #8b8b9e)', fontSize: '0.85rem' }}>
          Submitting to <code>/api/profiles/silicon/import-json</code>...
        </div>
      )}

      {phase === 'done' && response && (
        <InfoBanner tone="success" title="Bundle written and registry reloaded.">
          <div style={{ fontSize: '0.78rem', color: 'var(--text)' }}>
            Profile id: <code>{response.id}</code>
          </div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)' }}>
            Path: <code>{response.path}</code> · loaded {response.loaded}
          </div>

          <div style={{ display: 'flex', gap: 8, marginTop: 12, flexWrap: 'wrap' }}>
            <button type="button" onClick={setActive} disabled={setActiveBusy} style={primaryBtn}
              aria-label={setActiveBusy ? 'Setting as active profile, please wait' : 'Set as active profile for this hashboard'}>
              {setActiveBusy ? 'Setting active...' : 'Set as active for this hashboard'}
            </button>
            <button type="button" onClick={onClose} style={secondaryBtn}>
              Close wizard
            </button>
          </div>

          {/* activeMsg announced politely — confirmation of set-active, not a second alert */}
          <div aria-live="polite" aria-atomic="true" style={{ marginTop: 10, fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', minHeight: '1em' }}>
            {activeMsg ?? ''}
          </div>
        </InfoBanner>
      )}

      {phase === 'error' && (
        <InfoBanner
          tone="danger"
          title="Import refused."
          action={
            <div style={{ display: 'flex', gap: 8 }}>
              <button type="button" onClick={() => { setPhase('idle'); setErrorMsg(null); }} style={secondaryBtn}>
                Back
              </button>
              <button type="button" onClick={onClose} style={secondaryBtn}>
                Close
              </button>
            </div>
          }
        >
          <div style={{ fontSize: '0.78rem' }}>
            {errorMsg}
          </div>
        </InfoBanner>
      )}
    </div>
  );
}

function SummaryRow({ label, value }: { label: string; value: string }) {
  return (
    <div style={{ display: 'flex', gap: 12, padding: '4px 0' }}>
      <span style={{ color: 'var(--text-secondary, #8b8b9e)', minWidth: 120 }}>{label}</span>
      <span style={{ color: 'var(--text)', fontFamily: 'JetBrains Mono, monospace' }}>{value}</span>
    </div>
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
