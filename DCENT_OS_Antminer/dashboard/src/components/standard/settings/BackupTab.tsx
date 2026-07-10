import React, { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../../../api/client';
import type { ConfigBackupManifestResponse } from '../../../api/types';
import { useMinerStore } from '../../../store/miner';
import { ActionButton } from '../../common/ActionButton';
import { InfoDot } from '../../common/Tooltip';
import { UpgradeStatusPanel } from '../../common/UpgradeStatusPanel';
import { DaemonConfigBackup } from '../DaemonConfigBackup';
import {
  clearStagedUpgrade,
  loadStagedUpgrade,
  saveStagedUpgrade,
} from '../../../utils/stagedUpgrade';

function formatBackupManifestSize(bytes: number | null) {
  if (bytes == null || !Number.isFinite(bytes)) return 'unknown size';
  if (bytes < 1024) return `${bytes} B`;
  const kib = bytes / 1024;
  if (kib < 1024) return `${kib.toFixed(kib >= 10 ? 0 : 1)} KiB`;
  const mib = kib / 1024;
  return `${mib.toFixed(mib >= 10 ? 0 : 1)} MiB`;
}

function formatBackupManifestTime(ms: number | null) {
  if (ms == null || !Number.isFinite(ms)) return 'unknown modified time';
  const date = new Date(ms);
  return Number.isNaN(date.getTime()) ? 'unknown modified time' : date.toLocaleString();
}

function backupSupportLabel(value: boolean) {
  return value ? 'Available' : 'Not exposed';
}

export function BackupTab() {
  const settings = useMinerStore(s => s.settings);
  const updateSettings = useMinerStore(s => s.updateSettings);
  const addAlert = useMinerStore(s => s.addAlert);
  const [otaFile, setOtaFile] = useState<File | null>(null);
  const [stagedUpgrade, setStagedUpgrade] = useState(loadStagedUpgrade);
  const [otaProgress, setOtaProgress] = useState<number | null>(null);
  const [otaUploading, setOtaUploading] = useState(false);
  const [dragOver, setDragOver] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const importFileRef = useRef<HTMLInputElement>(null);
  const [backupManifest, setBackupManifest] = useState<ConfigBackupManifestResponse | null>(null);
  const [backupManifestError, setBackupManifestError] = useState<string | null>(null);

  const fetchConfigBackupManifest = useCallback(async () => {
    try {
      setBackupManifest(await api.getConfigBackupManifest());
      setBackupManifestError(null);
    } catch (error) {
      setBackupManifest(null);
      setBackupManifestError(
        error instanceof Error ? error.message : 'Config backup manifest unavailable',
      );
    }
  }, []);

  useEffect(() => {
    void fetchConfigBackupManifest();
  }, [fetchConfigBackupManifest]);

  const handleDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    const file = e.dataTransfer.files[0];
    if (file && file.name.toLowerCase().endsWith('.tar')) {
      setOtaFile(file);
    } else {
      addAlert('warning', 'Please drop a signed sysupgrade .tar package');
    }
  }, [addAlert]);

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    if (file.name.toLowerCase().endsWith('.tar')) {
      setOtaFile(file);
      return;
    }
    addAlert('warning', 'Please choose a signed sysupgrade .tar package');
  };

  const uploadFirmware = async (apply: boolean) => {
    if (!otaFile) return;
    setOtaUploading(true);
    setOtaProgress(0);
    try {
      const response = await api.uploadFirmware({
        file: otaFile,
        apply,
        onProgress: pct => setOtaProgress(pct),
      });
      addAlert(apply ? 'warning' : 'info', response.message);
      if (apply) {
        clearStagedUpgrade();
        setStagedUpgrade(null);
        setOtaFile(null);
      } else if (response.staged_path) {
        saveStagedUpgrade(response);
        setStagedUpgrade(loadStagedUpgrade());
      }
    } catch (error) {
      addAlert('warning', error instanceof Error ? error.message : 'Firmware upload failed');
    } finally {
      setOtaUploading(false);
      setOtaProgress(null);
    }
  };

  const flashStagedFirmware = async () => {
    if (!stagedUpgrade) return;
    setOtaUploading(true);
    try {
      const response = await api.uploadFirmware({
        stagedPath: stagedUpgrade.stagedPath,
        apply: true,
      });
      addAlert('warning', response.message);
      clearStagedUpgrade();
      setStagedUpgrade(null);
      setOtaFile(null);
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Firmware flash failed';
      addAlert('warning', message);
      if (message.includes('missing') || message.includes('outside the browser update staging area')) {
        clearStagedUpgrade();
        setStagedUpgrade(null);
      }
    } finally {
      setOtaUploading(false);
    }
  };

  const exportConfig = useCallback(() => {
    const config = {
      version: 1,
      exportedAt: new Date().toISOString(),
      settings: settings,
      notifications: (() => {
        try {
          const raw = localStorage.getItem('dcentos-notifications');
          return raw ? JSON.parse(raw) : null;
        } catch { return null; }
      })(),
    };
    const blob = new Blob([JSON.stringify(config, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `dcentos-dashboard-preferences-${new Date().toISOString().slice(0, 10)}.json`;
    a.click();
    URL.revokeObjectURL(url);
    addAlert('info', 'Dashboard preferences exported');
  }, [settings, addAlert]);

  const importConfig = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = (ev) => {
      try {
        const text = ev.target?.result as string;
        const config = JSON.parse(text);
        if (!config.version || !config.settings) {
          addAlert('warning', 'Invalid config file: missing version or settings');
          return;
        }
        const s = config.settings;
        if (typeof s.electricityRate !== 'number' || typeof s.btcPrice !== 'number') {
          addAlert('warning', 'Invalid config file: corrupt settings data');
          return;
        }
        updateSettings(s);
        if (config.notifications) {
          localStorage.setItem('dcentos-notifications', JSON.stringify(config.notifications));
        }
        addAlert('info', 'Dashboard preferences imported successfully');
      } catch {
        addAlert('warning', 'Failed to parse config file');
      }
    };
    reader.readAsText(file);
    e.target.value = '';
  }, [updateSettings, addAlert]);

  return (
    <>
      <div className="section">
        <div className="section-title">Backup & Restore</div>
        <div style={{
          background: 'var(--card-bg)', borderRadius: 'var(--radius)',
          padding: 16, border: '1px solid var(--border)',
        }}>
          <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)', marginBottom: 12 }}>
            Dashboard preference export is browser-local. Daemon config backup readiness is reported as read-only metadata below.
          </div>

          {backupManifest ? (
            <div style={{ marginBottom: 14 }}>
              <div style={{
                display: 'grid',
                gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))',
                gap: 8,
                marginBottom: 10,
              }}>
                <div style={{ padding: 10, border: '1px solid var(--border)', borderRadius: 'var(--radius)' }}>
                  <div style={{ fontSize: '0.7rem', color: 'var(--text-dim)', textTransform: 'uppercase' }}>
                    Daemon Export
                  </div>
                  <div style={{ fontWeight: 600 }}>
                    {backupSupportLabel(backupManifest.daemon_config_export_supported)}
                  </div>
                </div>
                <div style={{ padding: 10, border: '1px solid var(--border)', borderRadius: 'var(--radius)' }}>
                  <div style={{ fontSize: '0.7rem', color: 'var(--text-dim)', textTransform: 'uppercase' }}>
                    Daemon Restore
                  </div>
                  <div style={{ fontWeight: 600 }}>
                    {backupSupportLabel(backupManifest.restore_supported)}
                  </div>
                </div>
                <div style={{ padding: 10, border: '1px solid var(--border)', borderRadius: 'var(--radius)' }}>
                  <div style={{ fontSize: '0.7rem', color: 'var(--text-dim)', textTransform: 'uppercase' }}>
                    Content Collected
                  </div>
                  <div style={{ fontWeight: 600 }}>
                    {backupManifest.content_collected ? 'Yes' : 'No'}
                  </div>
                </div>
              </div>

              <div style={{ display: 'grid', gap: 8 }}>
                {(backupManifest.sources ?? []).map(source => (
                  <div
                    key={source.id}
                    style={{
                      display: 'grid',
                      gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))',
                      gap: 10,
                      alignItems: 'center',
                      padding: 10,
                      border: '1px solid var(--border)',
                      borderRadius: 'var(--radius)',
                    }}
                  >
                    <div>
                      <div style={{ fontWeight: 600 }}>{source.label}</div>
                      <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>
                        {source.active ? 'active source' : source.writable_target ? 'persistent target' : 'fallback source'}
                      </div>
                    </div>
                    <div style={{ fontFamily: 'var(--font-mono)', fontSize: '0.75rem', wordBreak: 'break-all' }}>
                      {source.path}
                    </div>
                    <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)' }}>
                      <div>{source.metadata_status}</div>
                      <div>{formatBackupManifestSize(source.size_bytes)}</div>
                      <div>{formatBackupManifestTime(source.modified_ms)}</div>
                    </div>
                  </div>
                ))}
              </div>

              <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginTop: 10 }}>
                No TOML content was collected. Secret-bearing keys are reserved for redaction policy: {(backupManifest.redaction_policy?.secret_key_patterns ?? []).join(', ') || 'not reported'}.
              </div>
            </div>
          ) : (
            <div style={{ marginBottom: 14, padding: 10, border: '1px solid var(--border)', borderRadius: 'var(--radius)', color: 'var(--text-dim)' }}>
              Config backup manifest unavailable{backupManifestError ? `: ${backupManifestError}` : ''}
            </div>
          )}

          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            <button className="btn btn-secondary" onClick={exportConfig}>
              Export Dashboard Preferences
            </button>
            <button
              className="btn btn-secondary"
              onClick={() => importFileRef.current?.click()}
            >
              Import Dashboard Preferences
            </button>
            <input
              ref={importFileRef}
              type="file"
              accept=".json"
              onChange={importConfig}
              style={{ display: 'none' }}
            />
          </div>

          <DaemonConfigBackup />
        </div>
      </div>

      <div className="section">
        <div className="section-title" style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
          Firmware Update
          <InfoDot term="ota_uploaded" label="Upload is not flash — the proof ladder" />
        </div>
        <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)', marginBottom: 12 }}>
          Upload a signed DCENT_OS sysupgrade `.tar` package. The backend verifies the package signature, runs target preflight, then schedules inactive-slot flashing only after explicit confirmation.
        </div>
        <UpgradeStatusPanel />
        <div
          onDragOver={e => { e.preventDefault(); setDragOver(true); }}
          onDragLeave={() => setDragOver(false)}
          onDrop={handleDrop}
          onClick={() => fileInputRef.current?.click()}
          style={{
            background: dragOver ? 'var(--accent-glow)' : 'var(--card-bg)',
            border: `2px dashed ${dragOver ? 'var(--accent)' : 'var(--border)'}`,
            borderRadius: 'var(--radius)', padding: 32,
            textAlign: 'center', cursor: 'pointer', transition: 'all 0.2s',
          }}
        >
          <input
            ref={fileInputRef}
            type="file"
            accept=".tar"
            onChange={handleFileSelect}
            style={{ display: 'none' }}
          />
          {otaFile ? (
            <div>
              <div style={{ fontWeight: 600, marginBottom: 4 }}>{otaFile.name}</div>
              <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)' }}>
                {(otaFile.size / (1024 * 1024)).toFixed(1)} MB
              </div>
            </div>
          ) : (
            <div>
              <div style={{ fontSize: '1.5rem', marginBottom: 8, color: 'var(--text-dim)' }}>
                {'\u2B06'}
              </div>
              <div style={{ color: 'var(--text-secondary)' }}>
                Drop or browse a signed sysupgrade `.tar` package.
              </div>
            </div>
          )}
        </div>

        {otaProgress !== null && (
          <div style={{ marginTop: 12 }}>
            <div style={{
              height: 8, background: 'var(--bg)', borderRadius: 4,
              overflow: 'hidden',
            }}>
              <div style={{
                height: '100%', width: `${otaProgress}%`,
                background: 'var(--accent)', borderRadius: 4,
                transition: 'width 0.3s',
              }} />
            </div>
            <div style={{
              fontSize: '0.75rem', color: 'var(--text-dim)',
              textAlign: 'center', marginTop: 4,
            }}>
              {otaProgress >= 100
                ? 'Upload complete; waiting for validation result...'
                : `Uploading package... ${otaProgress.toFixed(0)}%`}
            </div>
          </div>
        )}

        {stagedUpgrade && otaProgress === null && (
          <div style={{ marginTop: 12, padding: 12, border: '1px solid var(--border)', borderRadius: 'var(--radius)' }}>
            <div style={{ fontWeight: 600 }}>{stagedUpgrade.filename || 'Staged package ready'}</div>
            <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginTop: 4 }}>
              Reuse this staged package without uploading it again. The backend will re-run signature verification and target preflight before scheduling any flash.
            </div>
            <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginTop: 4, wordBreak: 'break-all' }}>
              {stagedUpgrade.stagedPath}
            </div>
            <div style={{ marginTop: 12, display: 'flex', gap: 8 }}>
              <ActionButton
                label={otaUploading ? 'Scheduling...' : 'Schedule Staged Flash'}
                onClick={flashStagedFirmware}
                variant="danger"
                confirm="Re-verify the previously staged signed package, run target preflight, and schedule inactive-slot flashing? The miner may reboot after sysupgrade completes."
                disabled={otaUploading}
              />
              <button
                className="btn btn-secondary"
                onClick={() => {
                  clearStagedUpgrade();
                  setStagedUpgrade(null);
                }}
              >
                Forget Staged Package
              </button>
            </div>
          </div>
        )}

        {otaFile && otaProgress === null && (
          <div style={{ marginTop: 12, display: 'flex', gap: 8 }}>
            <ActionButton
              label={otaUploading ? 'Validating...' : 'Validate Package'}
              onClick={() => uploadFirmware(false)}
              variant="secondary"
              disabled={otaUploading}
            />
            <ActionButton
              label={otaUploading ? 'Scheduling...' : 'Validate + Schedule'}
              onClick={() => uploadFirmware(true)}
              variant="danger"
              confirm="Verify this signed package, run target preflight, and schedule inactive-slot flashing? The miner may reboot after sysupgrade completes."
              disabled={otaUploading}
            />
            <button className="btn btn-secondary" onClick={() => setOtaFile(null)}>
              Cancel
            </button>
          </div>
        )}
      </div>
    </>
  );
}
