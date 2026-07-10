import React, { useCallback, useEffect, useState } from 'react';
import { api } from '../../../api/client';
import type { NetworkInfoResponse } from '../../../api/types';
import { useMinerStore } from '../../../store/miner';
import { NotificationSettings } from '../NotificationSettings';

export function NetworkTab() {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const settings = useMinerStore(s => s.settings);
  const updateSettings = useMinerStore(s => s.updateSettings);
  const addAlert = useMinerStore(s => s.addAlert);
  const [diagRunning, setDiagRunning] = useState(false);
  const [diagResult, setDiagResult] = useState<{
    dns_ok?: boolean;
    gateway_reachable?: boolean;
    pool_reachable?: boolean;
    ntp_synced?: boolean;
    message?: string;
  } | null>(null);
  const [networkInfo, setNetworkInfo] = useState<NetworkInfoResponse | null | undefined>(undefined);
  const [hostnameInput, setHostnameInput] = useState('');
  const [hostnameDirty, setHostnameDirty] = useState(false);
  const [hostnameSaving, setHostnameSaving] = useState(false);

  const displayedNetworkHostname = networkInfo?.hostname || systemInfo?.hostname || '';
  const displayedNetworkIp = networkInfo?.ipv4 || window.location.hostname;
  const displayedNetworkMac = networkInfo?.mac || systemInfo?.mac || '';

  const fetchNetworkInfo = useCallback(async () => {
    try {
      setNetworkInfo(await api.getNetworkInfo());
    } catch {
      setNetworkInfo(null);
    }
  }, []);

  useEffect(() => {
    void fetchNetworkInfo();
  }, [fetchNetworkInfo]);

  useEffect(() => {
    if (!hostnameDirty) {
      setHostnameInput(displayedNetworkHostname);
    }
  }, [displayedNetworkHostname, hostnameDirty]);

  const runDiagnostics = async () => {
    setDiagRunning(true);
    setDiagResult(null);
    try {
      const result = await api.troubleshootNetwork();
      setDiagResult(result);
    } catch {
      setDiagResult({ message: 'Failed to run diagnostics. Miner may be unreachable.' });
    }
    setDiagRunning(false);
  };

  const saveHostname = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const nextHostname = hostnameInput.trim().toLowerCase();
    if (!nextHostname) {
      addAlert('warning', 'Hostname is required');
      return;
    }
    setHostnameSaving(true);
    try {
      const response = await api.updateNetworkHostname(nextHostname);
      setHostnameDirty(false);
      setHostnameInput(response.hostname);
      setNetworkInfo(current => current ? { ...current, hostname: response.hostname } : current);
      addAlert('info', response.note || 'Hostname saved to daemon config');
    } catch (error) {
      addAlert('warning', error instanceof Error ? error.message : 'Failed to save hostname');
    } finally {
      setHostnameSaving(false);
    }
  };

  return (
    <>
      <div className="section">
        <div className="section-title">Network</div>
        <div style={{
          background: 'var(--card-bg)', borderRadius: 'var(--radius)',
          padding: 16, border: '1px solid var(--border)',
        }}>
          <div className="standard-grid-2 settings-network-grid" style={{
            fontSize: '0.85rem', marginBottom: 12,
          }}>
            <div>
              <span style={{ color: 'var(--text-dim)' }}>IP Address: </span>
              <span style={{ fontFamily: 'var(--font-mono)' }}>
                {displayedNetworkIp || '---'}
              </span>
            </div>
            <div>
              <span style={{ color: 'var(--text-dim)' }}>Hostname: </span>
              <span data-testid="settings-network-hostname-current">{displayedNetworkHostname || '---'}</span>
            </div>
            <div>
              <span style={{ color: 'var(--text-dim)' }}>MAC: </span>
              <span style={{ fontFamily: 'var(--font-mono)' }}>
                {displayedNetworkMac || '---'}
              </span>
            </div>
            <div>
              <span style={{ color: 'var(--text-dim)' }}>Board: </span>
              <span>{systemInfo?.board ?? '---'}</span>
            </div>
            {networkInfo?.gateway && (
              <div>
                <span style={{ color: 'var(--text-dim)' }}>Gateway: </span>
                <span style={{ fontFamily: 'var(--font-mono)' }}>{networkInfo.gateway}</span>
              </div>
            )}
            {networkInfo?.dns && (
              <div>
                <span style={{ color: 'var(--text-dim)' }}>DNS: </span>
                <span style={{ fontFamily: 'var(--font-mono)' }}>{networkInfo.dns}</span>
              </div>
            )}
          </div>

          <form
            onSubmit={saveHostname}
            data-testid="settings-network-hostname-form"
            style={{ borderTop: '1px solid var(--border)', paddingTop: 12, marginBottom: 12 }}
          >
            <label style={{ fontSize: '0.75rem', color: 'var(--text-dim)', display: 'block', marginBottom: 4 }}>
              Hostname
            </label>
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'center' }}>
              <input
                value={hostnameInput}
                onChange={event => {
                  setHostnameDirty(true);
                  setHostnameInput(event.target.value);
                }}
                data-testid="settings-network-hostname-input"
                aria-label="Miner hostname"
                style={{ maxWidth: 280 }}
              />
              <button
                type="submit"
                className="btn btn-secondary"
                disabled={hostnameSaving || !hostnameDirty}
                data-testid="settings-network-hostname-save"
                style={{ fontSize: '0.8rem', padding: '6px 14px' }}
              >
                {hostnameSaving ? 'Saving...' : 'Save Hostname'}
              </button>
            </div>
            <div style={{ marginTop: 6, fontSize: '0.75rem', color: 'var(--text-dim)' }}>
              Saves the hostname to daemon config. The active OS hostname updates after the next daemon or host restart.
            </div>
          </form>

          <div
            data-testid="settings-network-static-ip-roadmap"
            style={{ borderTop: '1px solid var(--border)', paddingTop: 12, marginBottom: 12, fontSize: '0.8rem', color: 'var(--text-dim)' }}
          >
            Static IP: configure via your router&apos;s DHCP reservation (recommended). Direct static-IP configuration is on the roadmap.
          </div>

          <div style={{ borderTop: '1px solid var(--border)', paddingTop: 12 }}>
            <button
              className="btn btn-secondary"
              onClick={runDiagnostics}
              disabled={diagRunning}
              style={{ fontSize: '0.8rem', padding: '6px 14px' }}
            >
              {diagRunning ? 'Running Diagnostics...' : 'Run Network Diagnostics'}
            </button>
            {diagResult && (
              <div style={{
                marginTop: 10, padding: 10,
                background: 'var(--bg)', borderRadius: 'var(--radius-sm)',
                fontSize: '0.8rem',
              }}>
                <div className="standard-grid-2 settings-diagnostics-grid" style={{ gap: 6 }}>
                  {diagResult.dns_ok !== undefined && (
                    <div>
                      <span style={{ color: diagResult.dns_ok ? 'var(--green)' : 'var(--red)' }}>
                        {diagResult.dns_ok ? 'PASS' : 'FAIL'}
                      </span>
                      <span style={{ color: 'var(--text-dim)', marginLeft: 6 }}>DNS Resolution</span>
                    </div>
                  )}
                  {diagResult.gateway_reachable !== undefined && (
                    <div>
                      <span style={{ color: diagResult.gateway_reachable ? 'var(--green)' : 'var(--red)' }}>
                        {diagResult.gateway_reachable ? 'PASS' : 'FAIL'}
                      </span>
                      <span style={{ color: 'var(--text-dim)', marginLeft: 6 }}>Gateway</span>
                    </div>
                  )}
                  {diagResult.pool_reachable !== undefined && (
                    <div>
                      <span style={{ color: diagResult.pool_reachable ? 'var(--green)' : 'var(--red)' }}>
                        {diagResult.pool_reachable ? 'PASS' : 'FAIL'}
                      </span>
                      <span style={{ color: 'var(--text-dim)', marginLeft: 6 }}>Pool Connectivity</span>
                    </div>
                  )}
                  {diagResult.ntp_synced !== undefined && (
                    <div>
                      <span style={{ color: diagResult.ntp_synced ? 'var(--green)' : 'var(--red)' }}>
                        {diagResult.ntp_synced ? 'PASS' : 'FAIL'}
                      </span>
                      <span style={{ color: 'var(--text-dim)', marginLeft: 6 }}>NTP Sync</span>
                    </div>
                  )}
                </div>
                {diagResult.message && (
                  <div style={{ marginTop: 6, color: 'var(--text-dim)', fontSize: '0.75rem' }}>
                    {diagResult.message}
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      </div>

      <div className="section">
        <div className="section-title">Browser Alerts</div>
        <div style={{
          background: 'var(--card-bg)', borderRadius: 'var(--radius)',
          padding: 16, border: '1px solid var(--border)',
          display: 'grid', gap: 12, marginBottom: 16,
        }}>
          <label style={{
            display: 'flex', alignItems: 'center', gap: 10,
            fontSize: '0.9rem', cursor: 'pointer',
          }}>
            <input
              type="checkbox"
              checked={settings.soundAlerts}
              onChange={e => updateSettings({ soundAlerts: e.target.checked })}
            />
            Sound alerts for critical events
          </label>
          <label style={{
            display: 'flex', alignItems: 'center', gap: 10,
            fontSize: '0.9rem', cursor: 'pointer',
          }}>
            <input
              type="checkbox"
              checked={settings.browserNotifications}
              onChange={e => {
                if (e.target.checked && Notification.permission !== 'granted') {
                  Notification.requestPermission().then(perm => {
                    updateSettings({ browserNotifications: perm === 'granted' });
                  });
                } else {
                  updateSettings({ browserNotifications: e.target.checked });
                }
              }}
            />
            Browser notifications for critical events
          </label>
        </div>

        <NotificationSettings />
      </div>
    </>
  );
}
