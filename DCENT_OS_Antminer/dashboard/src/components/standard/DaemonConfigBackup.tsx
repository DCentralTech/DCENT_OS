import React, { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../../api/client';
import { useMinerStore } from '../../store/miner';
import { OverlayDialog } from '../common/OverlayDialog';

/**
 * COMP-1 daemon config backup/restore (LuxOS / Braiins parity).
 *
 * Distinct from the browser-local "Dashboard Preferences" export above it in
 * Settings: this exports the FULL effective daemon config via
 * `GET /api/config/export` (redacted + re-importable) and restores it via
 * `POST /api/config/import` (validated fail-closed on the daemon, restart
 * required to apply).
 *
 * Truthfulness contract:
 *  - The daemon redacts every secret (passwords/tokens/keys), pool/donation
 *    wallet worker, and credential-bearing pool URL BEFORE it reaches the
 *    browser — the UI never has to (and never tries to) mask anything itself.
 *  - Import is staged behind an explicit confirm; the daemon validates and can
 *    reject, and that rejection message is surfaced verbatim. We never claim a
 *    rejected/invalid config was applied.
 */
export function DaemonConfigBackup() {
  const addAlert = useMinerStore(s => s.addAlert);

  const [exporting, setExporting] = useState(false);
  const [importing, setImporting] = useState(false);
  const importFileRef = useRef<HTMLInputElement>(null);
  const [pendingImport, setPendingImport] = useState<{ configToml: string; fileName: string } | null>(null);
  const [importError, setImportError] = useState<string | null>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);

  // Download the full effective daemon config (redacted + re-importable).
  const exportConfig = useCallback(async () => {
    setExporting(true);
    try {
      const exported = await api.getConfigExport();
      const blob = new Blob([JSON.stringify(exported, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `dcentos-daemon-config-${new Date().toISOString().slice(0, 10)}.json`;
      a.click();
      URL.revokeObjectURL(url);
      addAlert('info', 'Daemon config exported (secrets, wallet, and pool credentials redacted)');
    } catch (error) {
      addAlert('warning', error instanceof Error ? `Config export failed: ${error.message}` : 'Config export failed');
    } finally {
      setExporting(false);
    }
  }, [addAlert]);

  // Read an exported file, extract the re-importable TOML document (accepts
  // either the exported JSON envelope or a raw .toml), then stage it for an
  // explicit confirm — nothing is sent to the daemon until the operator
  // confirms.
  const handleImportFile = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    // Reset immediately so re-selecting the same file re-fires onChange.
    e.target.value = '';
    if (!file) return;
    setImportError(null);
    const reader = new FileReader();
    reader.onload = (ev) => {
      const text = (ev.target?.result as string) ?? '';
      let configToml = text;
      try {
        const parsed = JSON.parse(text);
        if (parsed && typeof parsed.config_toml === 'string') {
          configToml = parsed.config_toml;
        }
      } catch {
        /* not JSON — treat the file contents as a raw TOML document */
      }
      if (!configToml.trim()) {
        addAlert('warning', 'Import file is empty or has no config_toml content');
        return;
      }
      setPendingImport({ configToml, fileName: file.name });
    };
    reader.onerror = () => addAlert('warning', 'Failed to read the import file');
    reader.readAsText(file);
  }, [addAlert]);

  const confirmImport = useCallback(async () => {
    if (!pendingImport) return;
    setImporting(true);
    setImportError(null);
    try {
      const response = await api.importConfig(pendingImport.configToml);
      addAlert(
        'info',
        `${response.message} (${response.sections.length} section${response.sections.length === 1 ? '' : 's'})`,
      );
      setPendingImport(null);
    } catch (error) {
      // The daemon returns 400 with a JSON `{ status, message }` body on a
      // validation failure. Surface that message honestly instead of a generic
      // error — never claim an invalid config was applied.
      let detail = error instanceof Error ? error.message : 'Config import failed';
      try {
        const parsed = JSON.parse(detail);
        if (parsed && typeof parsed.message === 'string') detail = parsed.message;
      } catch {
        /* message is not JSON — show it verbatim */
      }
      setImportError(detail);
    } finally {
      setImporting(false);
    }
  }, [pendingImport, addAlert]);

  // Focus the confirm button when the dialog opens.
  useEffect(() => {
    if (!pendingImport) return;
    const timer = setTimeout(() => confirmRef.current?.focus(), 0);
    return () => clearTimeout(timer);
  }, [pendingImport]);

  const closeDialog = useCallback(() => {
    setPendingImport(null);
    setImportError(null);
  }, []);

  return (
    <div style={{ borderTop: '1px solid var(--border)', marginTop: 16, paddingTop: 16 }}>
      <div style={{ fontWeight: 700, fontSize: '0.9rem', marginBottom: 4 }}>
        Daemon Config (full backup)
      </div>
      <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.5, marginBottom: 12 }}>
        Export the full effective daemon configuration as a re-importable file.
        Secrets (passwords, tokens, keys), pool/donation wallet workers, and
        credential-bearing pool URLs are <strong>redacted</strong> before they
        leave the miner. Import is <strong>validated on the daemon (fail-closed)</strong>{' '}
        and requires a restart to apply; redacted placeholders are kept as the
        existing stored secret, so a round-trip never overwrites your credentials.
      </div>
      <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
        <button
          className="ds-btn secondary"
          onClick={() => { void exportConfig(); }}
          disabled={exporting}
        >
          {exporting ? 'Exporting…' : 'Export Config'}
        </button>
        <button
          className="ds-btn secondary"
          onClick={() => importFileRef.current?.click()}
          disabled={importing}
        >
          Import Config
        </button>
        <input
          ref={importFileRef}
          type="file"
          accept=".json,.toml"
          onChange={handleImportFile}
          style={{ display: 'none' }}
          data-testid="daemon-config-import-input"
        />
      </div>

      {pendingImport && (
        <OverlayDialog
          open={Boolean(pendingImport)}
          onClose={closeDialog}
          ariaLabel="Confirm config import"
          initialFocusRef={confirmRef as React.RefObject<HTMLElement>}
          maxWidth={460}
        >
          <div style={{ padding: 24 }}>
            <div style={{ fontWeight: 700, marginBottom: 12 }}>Import daemon config?</div>
            <div style={{ color: 'var(--text-secondary)', marginBottom: 12, fontSize: '0.9rem', lineHeight: 1.5 }}>
              The daemon will <strong>validate</strong> the config in{' '}
              <span style={{ fontFamily: 'var(--font-mono)', wordBreak: 'break-all' }}>{pendingImport.fileName}</span>{' '}
              and reject it if anything is out of range. Nothing is applied until you
              restart the miner. Redacted placeholder values keep your existing stored
              secrets.
            </div>
            {importError && (
              <div
                role="alert"
                style={{
                  fontSize: '0.8rem', lineHeight: 1.5, marginBottom: 12,
                  color: '#FCA5A5',
                  background: 'rgba(239,68,68,0.08)',
                  border: '1px solid rgba(239,68,68,0.35)',
                  borderRadius: 8, padding: '10px 12px', wordBreak: 'break-word',
                }}
              >
                Import rejected: {importError}
              </div>
            )}
            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              <button
                className="btn btn-secondary"
                onClick={closeDialog}
                disabled={importing}
              >
                Cancel
              </button>
              <button
                ref={confirmRef}
                className="btn btn-primary"
                onClick={() => { void confirmImport(); }}
                disabled={importing}
              >
                {importing ? 'Validating…' : 'Validate & Import'}
              </button>
            </div>
          </div>
        </OverlayDialog>
      )}
    </div>
  );
}
