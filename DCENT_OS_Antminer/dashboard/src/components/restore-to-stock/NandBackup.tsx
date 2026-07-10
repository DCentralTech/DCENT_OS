// Step 3 — kicks off NAND backup. Backend reports backup path in
// the preflight response; we run preflight here as a side-effect
// since W8-F's preflight endpoint also runs the safety scan + slot
// plan (but no flash, no NAND write yet).
//
// Real NAND dd happens server-side only on confirmed flash submit
// (per W8-F: "Mandatory NAND backup before any flash trigger" —
// the dd writes start when /api/system/restore-to-stock receives
// confirm:true). We display this step's progress as a "preflight
// completed" milestone — the backup_path from the eventual
// scheduled response is shown on the final confirmation step.

import React, { useEffect, useState } from 'react';
import { restoreToStockApi, type RestoreToStockResponse } from '../../api/restore-to-stock';
import { BreakerWarningBanner } from './BreakerWarningBanner';
import { InfoBanner } from '../common/InfoBanner';
import { InfoDot } from '../common/Tooltip';

interface Props {
  stagedPath: string | null;
  setStagedPath: (p: string | null) => void;
  onPreflightDone: (resp: RestoreToStockResponse) => void;
}

export function NandBackup({ stagedPath, setStagedPath, onPreflightDone }: Props) {
  const [phase, setPhase] = useState<'idle' | 'uploading' | 'preflight' | 'done' | 'error'>('idle');
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [tarFile, setTarFile] = useState<File | null>(null);
  const [stagedSize, setStagedSize] = useState<number | null>(null);

  // We don't poll — preflight is a single call and the dashboard
  // updates the parent on completion.
  useEffect(() => {
    return () => {};
  }, []);

  const onUpload = async () => {
    if (!tarFile) {
      setErrorMsg('Pick a stock firmware tarball first.');
      return;
    }
    setPhase('uploading');
    setErrorMsg(null);
    try {
      // Reuse the existing /api/system/upgrade staging endpoint
      // per W8-F.md: "the same staging dir /tmp/dcentos-upgrade/<uuid>/
      // is what W8-F's is_inside_staging_root accepts".
      const fd = new FormData();
      fd.append('firmware', tarFile);
      const res = await fetch('/api/system/upgrade', {
        method: 'POST',
        body: fd,
        credentials: 'include',
      });
      if (!res.ok) throw new Error(`upgrade staging failed: HTTP ${res.status}`);
      const body = await res.json() as { staged_path?: string; path?: string; size?: number };
      const path = body.staged_path ?? body.path ?? null;
      if (!path) throw new Error('staging response missing path');
      setStagedPath(path);
      setStagedSize(body.size ?? tarFile.size);
      setPhase('preflight');

      // Run preflight immediately. confirm:false guarantees no flash.
      const pre = await restoreToStockApi.preflight({
        stock_firmware_staged_path: path,
        operator_serial_typed: '',
        acknowledge_breaker_warning: false,
        hashboard_count_to_use: 1,
        confirm_string_typed: '',
      });
      onPreflightDone(pre);
      setPhase('done');
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : 'Upload or preflight failed');
      setPhase('error');
    }
  };

  return (
    <div>
      <BreakerWarningBanner />
      <h3 style={{ marginTop: 0, fontSize: '1.1rem' }}>Stage firmware + safety preflight</h3>
      <div style={{ fontSize: '0.82rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 14 }}>
        Drop the stock Bitmain firmware tarball here. The daemon stages it to a UUID-scoped
        directory under <code>/tmp/dcentos-upgrade/</code> and runs the safety preflight (IOC scan,
        SHA-256, slot plan). NAND <code>dd</code> for <code>mtd4</code> / <code>mtd7</code> /{' '}
        <code>mtd8</code> runs server-side at the confirm step — not here.
      </div>

      {!stagedPath && phase === 'idle' && (
        <div>
          <label htmlFor="restore-firmware-file" style={{ display: 'block', fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 6 }}>
            Stock Bitmain firmware tarball (.tar, .tar.gz, .tgz)
          </label>
          <input
            id="restore-firmware-file"
            type="file"
            accept=".tar,.tar.gz,.tgz"
            onChange={(e) => setTarFile(e.target.files?.[0] ?? null)}
            style={{ marginBottom: 8 }}
          />
          <div style={{ fontSize: '0.72rem', color: 'var(--text-dim, #6E6E80)', marginBottom: 8 }}>
            {tarFile ? <>Picked: <code>{tarFile.name}</code> · {(tarFile.size / 1024 / 1024).toFixed(1)} MB</> : 'No file selected.'}
          </div>
          <button type="button" onClick={onUpload} disabled={!tarFile} style={primaryBtn}>
            Stage firmware + run safety preflight
          </button>
        </div>
      )}

      {(phase === 'uploading' || phase === 'preflight') && (
        <div>
          <div role="status" aria-live="polite" style={{ fontSize: '0.85rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 8 }}>
            {phase === 'uploading' ? 'Uploading tarball to staging directory...' : 'Running safety preflight...'}
          </div>
          <ProgressBar />
        </div>
      )}

      {phase === 'done' && stagedPath && (
        <InfoBanner
          tone="success"
          title={<>Firmware staged · preflight scan complete <InfoDot term="firmware_staged" placement="bottom" /></>}
        >
          <div style={{ fontSize: '0.78rem', color: 'var(--text)' }}>
            Staged path: <code>{stagedPath}</code>
            {stagedSize != null && <> · {(stagedSize / 1024 / 1024).toFixed(1)} MB</>}
          </div>
          <div style={{ fontSize: '0.72rem', color: 'var(--text-dim, #6E6E80)', marginTop: 6, lineHeight: 1.5 }}>
            <strong>NAND backup has NOT been written yet.</strong>{' '}
            <InfoDot term="nand_backup_not_written" placement="bottom" />{' '}
            The actual partition dump
            (<code>mtd4</code> / <code>mtd7</code> / <code>mtd8</code>) runs server-side once
            you submit the final confirm slider — estimated 30–60 seconds. On success the
            backup directory appears at <code>/data/restore-backup-&lt;timestamp&gt;/</code>.
          </div>
        </InfoBanner>
      )}

      {phase === 'error' && (
        <InfoBanner
          tone="danger"
          title="Staging or preflight failed."
          action={
            <button type="button" onClick={() => { setPhase('idle'); setErrorMsg(null); }} style={secondaryBtn}>
              Try again
            </button>
          }
        >
          <div style={{ fontSize: '0.78rem', color: 'var(--text)' }}>{errorMsg}</div>
        </InfoBanner>
      )}
    </div>
  );
}

function ProgressBar() {
  return (
    <div style={{
      height: 6,
      borderRadius: 3,
      background: 'rgba(255,255,255,0.06)',
      overflow: 'hidden',
      position: 'relative',
    }}>
      <div style={{
        position: 'absolute',
        top: 0, left: 0, bottom: 0,
        width: '40%',
        background: 'linear-gradient(90deg, transparent, var(--accent, #FAA500), transparent)',
        animation: 'restore-stock-progress 1.6s linear infinite',
      }} />
      <style>{`
        @keyframes restore-stock-progress {
          0%   { left: -40%; }
          100% { left: 100%; }
        }
        @media (prefers-reduced-motion: reduce) {
          .restore-stock-progress-bar { animation: none !important; left: 20% !important; width: 60% !important; }
        }
      `}</style>
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
