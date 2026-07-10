import React, { useState, useCallback, useRef } from 'react';
import { api } from '../../api/client';
import { ActionButton } from '../common/ActionButton';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';
import { OverlayDialog } from '../common/OverlayDialog';
import { UpgradeStatusPanel } from '../common/UpgradeStatusPanel';
import type {
  NetworkTroubleshootResponse,
  PsuTroubleshootResponse,
  FpgaTroubleshootResponse,
} from '../../api/types';
import { useMinerStore } from '../../store/miner';
import {
  clearStagedUpgrade,
  loadStagedUpgrade,
  saveStagedUpgrade,
} from '../../utils/stagedUpgrade';

interface HealthCheckResult {
  type: string;
  status: 'pending' | 'running' | 'pass' | 'fail' | 'warn';
  message: string;
  data?: unknown;
}

export function MaintenanceMode() {
  const [inMaintenance, setInMaintenance] = useState(false);
  const [entering, setEntering] = useState(false);
  const [exiting, setExiting] = useState(false);
  const [checks, setChecks] = useState<HealthCheckResult[]>([]);
  const [runningChecks, setRunningChecks] = useState(false);

  // Firmware update state
  const [fwFile, setFwFile] = useState<File | null>(null);
  const [stagedUpgrade, setStagedUpgrade] = useState(loadStagedUpgrade);
  const [fwUrl, setFwUrl] = useState('');
  const [fwProgress, setFwProgress] = useState<number | null>(null);
  const [fwUploading, setFwUploading] = useState(false);
  const [fwError, setFwError] = useState('');
  const [dragOver, setDragOver] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const addAlert = useMinerStore(s => s.addAlert);
  const logEntries = useMinerStore(s => s.logEntries);
  const systemInfo = useMinerStore(s => s.systemInfo);
  const sleepWakeSupported = systemInfo?.hardware?.capabilities?.sleep_wake_supported ?? false;

  // Reboot confirmation state
  const [showRebootConfirm, setShowRebootConfirm] = useState(false);
  const rebootCancelRef = useRef<HTMLButtonElement>(null);

  const updateCheck = (type: string, update: Partial<HealthCheckResult>) => {
    setChecks(prev => prev.map(c => c.type === type ? { ...c, ...update } : c));
  };

  const enterMaintenance = useCallback(async () => {
    setEntering(true);
    echoCli(sleepWakeSupported ? 'maint open --standby' : 'maint open');
    if (sleepWakeSupported) {
      try {
        await api.sleep();
        addAlert('info', 'Standby requested. Hash boards will power down on the next safety tick.');
      } catch {
        addAlert('warning', 'Failed to enter standby before opening maintenance tools.');
      }
    } else {
      addAlert('info', 'Maintenance tools opened. Standby sleep/wake control is in development for this firmware, so mining state was not changed.');
    }
    setInMaintenance(true);
    setEntering(false);
  }, [addAlert, sleepWakeSupported]);

  const exitMaintenance = useCallback(async () => {
    setExiting(true);
    if (sleepWakeSupported) {
      try {
        await api.wake();
        addAlert('info', 'Wake requested. Controller outputs will be restored on the next safety tick.');
      } catch {
        addAlert('warning', 'Failed to request wake while closing maintenance tools.');
      }
    }
    setInMaintenance(false);
    setExiting(false);
    setChecks([]);
  }, [addAlert, sleepWakeSupported]);

  const runHealthChecks = useCallback(async () => {
    setRunningChecks(true);
    const initialChecks: HealthCheckResult[] = [
      { type: 'network', status: 'pending', message: 'Waiting...' },
      { type: 'psu', status: 'pending', message: 'Waiting...' },
      { type: 'fpga', status: 'pending', message: 'Waiting...' },
    ];
    setChecks(initialChecks);

    // Network check
    updateCheck('network', { status: 'running', message: 'Testing network...' });
    try {
      const net: NetworkTroubleshootResponse = await api.troubleshootNetwork();
      const ok = net.ethernet.link_up && (net.dns_ok ?? true);
      updateCheck('network', {
        status: ok ? 'pass' : 'warn',
        message: net.message,
        data: net,
      });
    } catch (e: unknown) {
      updateCheck('network', { status: 'fail', message: e instanceof Error ? e.message : 'Network check failed' });
    }

    // PSU check
    updateCheck('psu', { status: 'running', message: 'Checking PSU...' });
    try {
      const psu: PsuTroubleshootResponse = await api.troubleshootPsu();
      updateCheck('psu', {
        status: psu.detected ? 'pass' : 'warn',
        message: psu.message,
        data: psu,
      });
    } catch (e: unknown) {
      updateCheck('psu', { status: 'fail', message: e instanceof Error ? e.message : 'PSU check failed' });
    }

    // FPGA check
    updateCheck('fpga', { status: 'running', message: 'Checking FPGA...' });
    try {
      const fpga: FpgaTroubleshootResponse = await api.troubleshootFpga();
      const ok = !!fpga.fpga_version;
      updateCheck('fpga', {
        status: ok ? 'pass' : 'warn',
        message: fpga.message,
        data: fpga,
      });
    } catch (e: unknown) {
      updateCheck('fpga', { status: 'fail', message: e instanceof Error ? e.message : 'FPGA check failed' });
    }

    setRunningChecks(false);
  }, []);

  const handleFwDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    const file = e.dataTransfer.files[0];
    if (file && file.name.toLowerCase().endsWith('.tar')) {
      setFwFile(file);
      setFwError('');
    } else {
      setFwError('Please drop a signed sysupgrade .tar package');
    }
  }, []);

  const handleFwFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (file && file.name.toLowerCase().endsWith('.tar')) {
      setFwFile(file);
      setFwError('');
    } else if (file) {
      setFwError('Please choose a signed sysupgrade .tar package');
    }
  };

  const uploadFirmware = useCallback(async (apply: boolean) => {
    if (!fwFile) return;
    setFwUploading(true);
    setFwProgress(0);
    setFwError('');
    echoCli(apply ? 'sysupgrade --slot inactive --confirm' : 'sysupgrade --slot inactive --test');
    try {
      const response = await api.uploadFirmware({
        file: fwFile,
        apply,
        onProgress: pct => setFwProgress(pct),
      });
      addAlert(apply ? 'warning' : 'info', response.message);
      if (apply) {
        clearStagedUpgrade();
        setStagedUpgrade(null);
        setFwFile(null);
      } else if (response.staged_path) {
        saveStagedUpgrade(response);
        setStagedUpgrade(loadStagedUpgrade());
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Firmware upload failed';
      setFwError(message);
      addAlert('warning', message);
    } finally {
      setFwUploading(false);
      setFwProgress(null);
    }
  }, [fwFile, addAlert]);

  const flashStagedFirmware = useCallback(async () => {
    if (!stagedUpgrade) return;
    setFwUploading(true);
    setFwError('');
    echoCli('sysupgrade --slot inactive --staged --confirm');
    try {
      const response = await api.uploadFirmware({
        stagedPath: stagedUpgrade.stagedPath,
        apply: true,
      });
      addAlert('warning', response.message);
      clearStagedUpgrade();
      setStagedUpgrade(null);
      setFwFile(null);
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Firmware flash failed';
      setFwError(message);
      addAlert('warning', message);
      if (message.includes('missing') || message.includes('outside the browser update staging area')) {
        clearStagedUpgrade();
        setStagedUpgrade(null);
      }
    } finally {
      setFwUploading(false);
    }
  }, [addAlert, stagedUpgrade]);

  const downloadSystemLogs = useCallback(() => {
    // Combine all log entries into a text file
    const lines = logEntries.map(entry => {
      const d = new Date(entry.timestamp);
      const ts = d.toISOString();
      return `[${ts}] [${entry.level.toUpperCase().padEnd(5)}] [${entry.source}] ${entry.message}`;
    });
    const text = lines.join('\n') || '(No log entries captured in this session)';
    const blob = new Blob([text], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `dcentos-logs-${new Date().toISOString().split('T')[0]}.txt`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  }, [logEntries]);

  const handleReboot = useCallback(async () => {
    echoCli('reboot');
    try {
      await api.reboot();
      addAlert('critical', 'Reboot initiated. Configuration was not changed.');
    } catch {
      addAlert('warning', 'Failed to initiate reboot');
    }
    setShowRebootConfirm(false);
  }, [addAlert]);

  const statusColor = (status: HealthCheckResult['status']) => {
    switch (status) {
      case 'pass': return 'var(--green)';
      case 'fail': return 'var(--red)';
      case 'warn': return 'var(--yellow)';
      case 'running': return 'var(--accent)';
      default: return 'var(--text-dim)';
    }
  };

  const statusIcon = (status: HealthCheckResult['status']) => {
    switch (status) {
      case 'pass': return '[PASS]';
      case 'fail': return '[FAIL]';
      case 'warn': return '[WARN]';
      case 'running': return '[....]';
      default: return '[----]';
    }
  };

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// maintenance mode</div>
          <h2 className="hacker-inspector-title">Service & Diagnostics</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${inMaintenance ? 'warning' : 'neutral'}`}>
            {inMaintenance ? 'TOOLS OPEN' : 'STANDBY'}
          </span>
        </div>
      </header>

      <div className="hacker-inspector-body">
      {!inMaintenance ? (
        <div className="register-inspector ds-card-hover mm-enter">
          <div className="mm-enter-title">
            Open Maintenance Tools
          </div>
          <div className="mm-enter-desc">
            {sleepWakeSupported
              ? 'Requests standby first, then opens maintenance and troubleshooting tools.'
              : 'Opens maintenance and troubleshooting tools. Standby sleep/wake control is in development for this firmware, so entering this screen does not change mining or power state.'}
          </div>
          <ActionButton
            label={entering ? 'Opening tools...' : 'Open Maintenance Tools'}
            onClick={enterMaintenance}
            variant="danger"
            confirm={sleepWakeSupported
              ? 'This requests standby and then opens maintenance tools. Continue?'
              : 'This opens maintenance tools only. Mining state will not change yet. Continue?'}
            disabled={entering}
          />
          <CliHint cmd={sleepWakeSupported ? 'maint open --standby' : 'maint open'} />
        </div>
      ) : (
        <>
          <div className="mm-active-banner">
            {sleepWakeSupported
              ? 'MAINTENANCE TOOLS ACTIVE -- Standby has been requested for this runtime.'
              : 'MAINTENANCE TOOLS ACTIVE -- Mining state is unchanged on this firmware.'}
          </div>

          <div className="mm-action-row">
            <ActionButton
              label={runningChecks ? 'Running...' : 'Run Health Checks'}
              onClick={runHealthChecks}
              disabled={runningChecks}
            />
            <ActionButton
              label={exiting ? 'Closing...' : 'Close Maintenance'}
              onClick={exitMaintenance}
              variant="secondary"
              confirm={sleepWakeSupported
                ? 'Close maintenance tools and request wake?'
                : 'Close maintenance tools and return to the previous dashboard view?'}
              disabled={exiting}
            />
          </div>

          {/* Health check results */}
          {checks.length > 0 && (
            <div className="register-inspector ds-card-hover mm-block">
              <div className="mm-block-title">
                Health Check Results
              </div>
              {checks.map(check => (
                <div key={check.type} className="mm-check-row">
                  <span className="mm-check-icon" style={{ color: statusColor(check.status) }}>
                    {statusIcon(check.status)}
                  </span>
                  <div className="mm-check-body">
                    <div className="mm-check-name">
                      {check.type}
                    </div>
                    <div className="mm-check-msg">
                      {check.message}
                    </div>
                    {check.data != null && check.status !== 'running' && (
                      <details className="mm-check-details">
                        <summary className="mm-check-summary">
                          Details
                        </summary>
                        <div className="json-response mm-check-json">
                          {JSON.stringify(check.data, null, 2)}
                        </div>
                      </details>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* Firmware Update Section */}
          <div className="register-inspector ds-card-hover mm-block">
            <div className="mm-block-title is-lg">
              Firmware Update
            </div>
            <div className="mm-block-desc">
              Upload a signed sysupgrade `.tar` package. The backend verifies the package signature, runs target preflight, then schedules inactive-slot flashing only after explicit confirmation.
            </div>
            <UpgradeStatusPanel compact />

            {/* File drop zone */}
            <button
              type="button"
              onDragOver={e => { e.preventDefault(); setDragOver(true); }}
              onDragLeave={() => setDragOver(false)}
              onDrop={handleFwDrop}
              onClick={() => fileInputRef.current?.click()}
              aria-label="Choose firmware package"
              className={`mm-dropzone${dragOver ? ' is-dragover' : ''}`}
            >
                <input
                  ref={fileInputRef}
                  type="file"
                  accept=".tar"
                  onChange={handleFwFileSelect}
                  className="mm-file-hidden"
                />
              {fwFile ? (
                <div>
                  <div className="mm-fw-name">
                    {fwFile.name}
                  </div>
                  <div className="mm-fw-size">
                    {(fwFile.size / (1024 * 1024)).toFixed(1)} MB
                  </div>
                </div>
              ) : (
                <div>
                  <div className="mm-drop-glyph">
                    {'\u2B06'}
                  </div>
                  <div className="mm-drop-hint">
                    Drop or browse a signed sysupgrade `.tar` package.
                  </div>
                </div>
              )}
            </button>

            {/* Upload progress */}
            {fwProgress !== null && (
              <div className="mm-progress">
                <div className="mm-progress-track">
                  <div className="mm-progress-fill" style={{ width: `${fwProgress}%` }} />
                </div>
                <div className="mm-progress-label">
                  {fwProgress >= 100
                    ? 'Upload complete; waiting for validation result...'
                    : `Uploading package... ${fwProgress.toFixed(0)}%`}
                </div>
              </div>
            )}

            {/* Upload error */}
            {fwError && (
              <div className="mm-fw-error">
                {fwError}
              </div>
            )}

            {stagedUpgrade && fwProgress === null && (
              <div className="mm-staged">
                <div className="mm-staged-name">
                  {stagedUpgrade.filename || 'Staged package ready'}
                </div>
                <div className="mm-staged-desc">
                  Reuse this staged package without uploading it again. The backend will re-run signature verification and target preflight before scheduling any flash.
                </div>
                <div className="mm-staged-path">
                  {stagedUpgrade.stagedPath}
                </div>
                <div className="mm-staged-actions">
                  <ActionButton
                    label={fwUploading ? 'Scheduling...' : 'Schedule Staged Flash'}
                    onClick={flashStagedFirmware}
                    variant="danger"
                    confirm="Re-verify the previously staged signed package, run target preflight, and schedule inactive-slot flashing? The miner may reboot after sysupgrade completes."
                    disabled={fwUploading}
                  />
                  <CliHint cmd="sysupgrade --slot inactive --staged --confirm" />
                  <button
                    className="btn btn-secondary mm-btn-sm"
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

            {/* Upload button */}
            {fwFile && fwProgress === null && (
              <div className="mm-btn-row">
                <ActionButton
                  label={fwUploading ? 'Validating...' : 'Validate Package'}
                  onClick={() => uploadFirmware(false)}
                  variant="secondary"
                  disabled={fwUploading}
                />
                <ActionButton
                  label={fwUploading ? 'Scheduling...' : 'Validate + Schedule'}
                  onClick={() => uploadFirmware(true)}
                  variant="danger"
                  confirm="Verify this signed package, run target preflight, and schedule inactive-slot flashing? The miner may reboot after sysupgrade completes."
                  disabled={fwUploading}
                />
                <CliHint cmd="sysupgrade --slot inactive --confirm" />
                <button
                  className="btn btn-secondary mm-btn-sm"
                  onClick={() => { setFwFile(null); setFwError(''); }}
                >
                  Cancel
                </button>
              </div>
            )}

            {/* URL-based update */}
            <div className="mm-url-section">
              <div className="mm-url-note">
                Remote fetch is in development:
              </div>
              <div className="mm-btn-row">
                <input
                  type="url"
                  value={fwUrl}
                  onChange={e => setFwUrl(e.target.value)}
                  placeholder="https://d-central.tech/firmware/dcentos-v0.3.1.squashfs"
                  className="mm-url-input"
                />
                <ActionButton
                  label="Fetch Coming Soon"
                  onClick={async () => {
                    addAlert('warning', 'Remote firmware fetch is in development. Use SSH-based update tooling instead.');
                  }}
                  disabled
                  variant="danger"
                />
              </div>
            </div>
          </div>

          {/* System Tools */}
          <div className="register-inspector ds-card-hover mm-block">
            <div className="mm-block-title is-lg">
              System Tools
            </div>
            <div className="mm-tools-row">
              {/* Download System Logs */}
              <button
                onClick={downloadSystemLogs}
                className="btn btn-secondary mm-btn-sm"
              >
                {'\u2913'} Download System Logs
              </button>

              {/* Reboot */}
              <button
                onClick={() => setShowRebootConfirm(true)}
                className="btn btn-danger mm-btn-sm"
              >
                Reboot Miner
              </button>
            </div>
            <div className="mm-logs-line">
              Logs: {logEntries.length} entries captured this session
            </div>
          </div>

          {/* Reboot Confirmation Dialog */}
          {showRebootConfirm && (
            <OverlayDialog
              open
              onClose={() => setShowRebootConfirm(false)}
              ariaLabel="Reboot confirmation"
              initialFocusRef={rebootCancelRef as React.RefObject<HTMLElement>}
              maxWidth={440}
            >
              <div className="mm-reboot-modal">
                <div className="mm-reboot-title">
                  Reboot Miner
                </div>
                <div className="mm-reboot-lead">
                  This sends a reboot request only. It does not reset or erase configuration.
                </div>
                <ul className="mm-reboot-list">
                  <li>Mining will stop while the controller reboots</li>
                  <li>Existing pools, profiles, fan settings, and dashboard settings remain unchanged</li>
                  <li>Use signed firmware tooling for upgrade or rollback operations</li>
                </ul>
                <div className="mm-reboot-warn">
                  This action cannot be undone.
                </div>
                <div className="adv-modal-foot mm-reboot-foot">
                  <button ref={rebootCancelRef} className="btn btn-secondary" onClick={() => setShowRebootConfirm(false)}>
                    Cancel
                  </button>
                  <button className="btn btn-danger" onClick={handleReboot}>
                    Reboot
                  </button>
                </div>
                <CliHint cmd="reboot" />
              </div>
            </OverlayDialog>
          )}
        </>
      )}
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{inMaintenance ? 'tools open' : 'tools closed'}</span>
          <span>{sleepWakeSupported ? 'sleep/wake supported' : 'sleep/wake N/A'}</span>
        </div>
      </footer>
    </div>
  );
}
