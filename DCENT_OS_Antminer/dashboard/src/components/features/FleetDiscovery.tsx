// Fleet Snapshot: local daemon state plus browser-pinned manual IP probes.
// The firmware endpoint is honest about its current scope: it does not scan
// subnets or contact peer miners.

import React, { useState, useCallback, useEffect } from 'react';
import api from '../../api/client';
import type { DiscoveredMiner, FleetStats } from '../../api/feature-types';
import { useTranslation } from '../../i18n/i18n';
import { useMinerStore } from '../../store/miner';
import { getLiveWallWatts } from '../../utils/power';
import { EmptyState } from '../common/EmptyState';
import { NoFleetIllustration } from '../common/illustrations';
import { InfoDot } from '../common/Tooltip';

const MANUAL_MINERS_KEY = 'dcentos-fleet-manual-miners';

function loadManualIPs(): string[] {
  try {
    const raw = localStorage.getItem(MANUAL_MINERS_KEY);
    if (raw) return JSON.parse(raw);
  } catch { /* ignore */ }
  return [];
}

function saveManualIPs(ips: string[]) {
  localStorage.setItem(MANUAL_MINERS_KEY, JSON.stringify(ips));
}

function isIpv4Address(value: string): boolean {
  return /^\d{1,3}(?:\.\d{1,3}){3}$/.test(value.trim());
}

function finitePositiveNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : null;
}

function finiteNonNegativeNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

function stringField(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

function booleanField(value: unknown): boolean | null {
  return typeof value === 'boolean' ? value : null;
}

function liveWallWattsFromRecord(record: Record<string, unknown>): number | null {
  const liveWatts = getLiveWallWatts({
    wall_watts: finitePositiveNumber(record.wall_watts),
    watts: finitePositiveNumber(record.watts) ?? finitePositiveNumber(record.power_watts),
    source: stringField(record.power_source) ?? stringField(record.source),
    source_detail: stringField(record.power_source_detail) ?? stringField(record.source_detail),
    live_power_available: booleanField(record.live_power_available),
    modeled: booleanField(record.power_modeled) ?? booleanField(record.modeled),
  });
  return liveWatts > 0 ? liveWatts : null;
}

function probeLivePowerWatts(data: Record<string, unknown>): number | null {
  const nestedPower = data.power;
  if (nestedPower && typeof nestedPower === 'object') {
    const nestedLiveWatts = liveWallWattsFromRecord(nestedPower as Record<string, unknown>);
    if (nestedLiveWatts != null) {
      return nestedLiveWatts;
    }
  }

  return liveWallWattsFromRecord({
    wall_watts: data.wall_watts ?? data.powerWatts,
    power_watts: data.power_watts,
    power_source: data.power_source,
    power_source_detail: data.power_source_detail,
    source: data.source,
    source_detail: data.source_detail,
    live_power_available: data.live_power_available,
    power_modeled: data.power_modeled,
    modeled: data.modeled,
  });
}

// Telemetry-truth contract (Wave 9D9): firmware detection is best-effort.
// We never relabel "Unknown" to a specific firmware just to look cleaner;
// the operator needs to see when probe failed or returned no fingerprint.
function firmwareTone(fw: string): "success" | "warning" | "danger" | "info" {
  if (fw === "Unreachable") return "danger";
  if (fw === "Unknown") return "warning";
  if (/dcentos|dcent_os|dcent os/i.test(fw)) return "success";
  return "info";
}

// Status pill grammar separates "online" (probe returned positive hashrate)
// from "sleeping" (probe returned, hashrate=0 — miner is up but idle) from
// "error" (probe failed). Don't collapse these — they tell the operator
// different things.
function statusTone(status: DiscoveredMiner["status"]): "success" | "warning" | "danger" {
  switch (status) {
    case "online":
      return "success";
    case "sleeping":
      return "warning";
    case "error":
      return "danger";
  }
}

export function FleetDiscovery() {
  const { t } = useTranslation();
  const addAlert = useMinerStore(s => s.addAlert);

  const [miners, setMiners] = useState<DiscoveredMiner[]>([]);
  const [scanning, setScanning] = useState(false);
  const [scanned, setScanned] = useState(false);
  const [apiUnavailable, setApiUnavailable] = useState(false);
  const [snapshotLimitations, setSnapshotLimitations] = useState<string[]>([]);

  // Manual add state
  const [manualIPs, setManualIPs] = useState<string[]>(loadManualIPs);
  const [manualMiners, setManualMiners] = useState<DiscoveredMiner[]>([]);
  const [newIP, setNewIP] = useState('');
  const [probing, setProbing] = useState(false);

  // Probe a manually added IP for miner info
  const probeMiner = useCallback(async (ip: string): Promise<DiscoveredMiner | null> => {
    try {
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), 5000);
      const res = await fetch(`http://${ip}/api/status`, { signal: controller.signal });
      clearTimeout(timeout);
      if (res.ok) {
        const data = await res.json() as Record<string, unknown>;
        const hashrateGhs = finiteNonNegativeNumber(data.hashrate_ghs) ?? 0;
        const powerWatts = probeLivePowerWatts(data);
        return {
          ip,
          hostname: typeof data.hostname === 'string'
            ? data.hostname
            : typeof data.miner_name === 'string' ? data.miner_name : ip,
          model: typeof data.model === 'string' ? data.model : 'Unknown',
          firmware: data.firmware_version ? `DCENTos v${data.firmware_version}` : 'Unknown',
          hashrateThs: hashrateGhs / 1000,
          powerWatts,
          status: hashrateGhs > 0 ? 'online' : 'sleeping',
          uptimeS: finiteNonNegativeNumber(data.uptime_s) ?? 0,
          mac: typeof data.mac === 'string' ? data.mac : '00:00:00:00:00:00',
        };
      }
    } catch { /* unreachable or timeout */ }
    return {
      ip,
      hostname: ip,
      model: 'Unknown',
      firmware: 'Unreachable',
      hashrateThs: 0,
      powerWatts: null,
      status: 'error' as const,
      uptimeS: 0,
      mac: '00:00:00:00:00:00',
    };
  }, []);

  // Probe all manual IPs on mount
  useEffect(() => {
    if (manualIPs.length === 0) return;
    let cancelled = false;
    (async () => {
      const results: DiscoveredMiner[] = [];
      for (const ip of manualIPs) {
        if (cancelled) break;
        const miner = await probeMiner(ip);
        if (miner) results.push(miner);
      }
      if (!cancelled) setManualMiners(results);
    })();
    return () => { cancelled = true; };
  }, [manualIPs, probeMiner]);

  const handleAddManual = async () => {
    const ip = newIP.trim();
    if (!ip) return;
    if (manualIPs.includes(ip)) {
      addAlert('warning', `${ip} is already in the list`);
      return;
    }
    setProbing(true);
    const miner = await probeMiner(ip);
    const updatedIPs = [...manualIPs, ip];
    setManualIPs(updatedIPs);
    saveManualIPs(updatedIPs);
    if (miner) setManualMiners(prev => [...prev, miner]);
    setNewIP('');
    setProbing(false);
  };

  const handleRemoveManual = (ip: string) => {
    const updatedIPs = manualIPs.filter(i => i !== ip);
    setManualIPs(updatedIPs);
    saveManualIPs(updatedIPs);
    setManualMiners(prev => prev.filter(m => m.ip !== ip));
  };

  // "Onboard to fleet" = pin this discovered miner to the local manual list
  // so it stays visible across sessions (the same persistence the manual-add
  // section uses). No backend onboarding API exists today — we don't fake
  // one.
  const handleOnboard = (miner: DiscoveredMiner) => {
    if (manualIPs.includes(miner.ip)) {
      addAlert('info', `${miner.ip} is already pinned to your fleet.`);
      return;
    }
    const updatedIPs = [...manualIPs, miner.ip];
    setManualIPs(updatedIPs);
    saveManualIPs(updatedIPs);
    setManualMiners(prev =>
      prev.some(m => m.ip === miner.ip) ? prev : [...prev, miner],
    );
    addAlert('info', `${miner.hostname} pinned to fleet.`);
  };

  const handleDiscover = useCallback(async () => {
    setScanning(true);
    setMiners([]);
    setApiUnavailable(false);
    setSnapshotLimitations([]);
    try {
      const hintIps = [window.location.hostname].filter(isIpv4Address);
      const request = {
        includeConfigured: true,
        manualIps: manualIPs,
        hintIps,
      };
      const data = await api.discoverFleet(request);
      setMiners(data.miners ?? []);
      setSnapshotLimitations(data.limitations ?? []);
    } catch {
      setApiUnavailable(true);
    }
    setScanning(false);
    setScanned(true);
  }, [manualIPs]);

  const allMinersByIp = new Map<string, DiscoveredMiner>();
  for (const miner of [...manualMiners, ...miners]) {
    allMinersByIp.set(miner.ip, miner);
  }
  const allMiners = Array.from(allMinersByIp.values());
  const manualMinerCount = allMiners.filter(miner => manualIPs.includes(miner.ip)).length;
  const snapshotMinerCount = Math.max(0, allMiners.length - manualMinerCount);
  const fleetSourceLabel = [
    snapshotMinerCount > 0 ? `${snapshotMinerCount} local snapshot` : null,
    manualMinerCount > 0 ? `${manualMinerCount} manual probe${manualMinerCount === 1 ? '' : 's'}` : null,
  ].filter(Boolean).join(', ') || 'No active source';
  const freshnessLabel = scanned ? 'Current session snapshot' : 'Browser-stored manual probes';
  const reportedPowerReadings = allMiners
    .map(miner => miner.powerWatts)
    .filter((watts): watts is number =>
      typeof watts === 'number' && Number.isFinite(watts) && watts > 0,
    );
  const reportedPowerSourceLabel = reportedPowerReadings.length > 0
    ? `Reported by ${reportedPowerReadings.length} live wall-power source${reportedPowerReadings.length === 1 ? '' : 's'}`
    : 'Power not reported by current sources; live wall-power provenance required';

  const stats: FleetStats = {
    totalMiners: allMiners.length,
    totalHashrateThs: allMiners.reduce((sum, m) => sum + m.hashrateThs, 0),
    totalPowerWatts: reportedPowerReadings.length > 0
      ? reportedPowerReadings.reduce((sum, watts) => sum + watts, 0)
      : null,
    onlineCount: allMiners.filter(m => m.status === 'online').length,
    sleepingCount: allMiners.filter(m => m.status === 'sleeping').length,
    errorCount: allMiners.filter(m => m.status === 'error').length,
  };

  const statusColor = (status: DiscoveredMiner['status']): string => {
    switch (status) {
      case 'online': return 'var(--feat-green)';
      case 'sleeping': return 'var(--feat-yellow)';
      case 'error': return 'var(--feat-red)';
    }
  };

  const formatUptime = (s: number): string => {
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    if (h > 24) return `${Math.floor(h / 24)}d ${h % 24}h`;
    return `${h}h ${m}m`;
  };

  return (
    <div className="feat-page">
      <div className="feat-header">
        <h2 className="feat-title">
          {t('fleet.title')}
          <InfoDot
            placement="bottom"
            label="What fleet discovery does"
            content={
              <>
                Shows this miner's local daemon snapshot plus browser-pinned manual IP
                probes. The firmware endpoint is read-only local state; it does not
                scan subnets or contact peer miners. Use DCENT_Toolbox for broader
                fleet discovery and rollout workflows. A unit's reported firmware
                string is never relabeled — "Unknown" stays "Unknown".
              </>
            }
          />
        </h2>
        <p className="feat-subtitle">{t('fleet.subtitle')}</p>
      </div>

      {/* Discover button */}
      <div className="feat-card" style={{ textAlign: 'center', padding: 24 }}>
        <button
          className="feat-btn feat-btn-primary feat-btn-large"
          onClick={handleDiscover}
          disabled={scanning}
          data-testid="fleet-discovery-scan"
        >
          {scanning ? (
            <>
              <span className="feat-spinner" aria-hidden="true" />
              {t('fleet.scanning')}
            </>
          ) : (
            t('fleet.discover')
          )}
        </button>
        {/* aria-live region must be permanently in the DOM so AT picks up
            the announcement when scanning flips true. Content is empty
            when not scanning — assistive technology only announces
            non-empty changes. */}
        <div
          data-testid="fleet-discovery-progress"
          aria-live="polite"
          aria-atomic="true"
          style={{
            marginTop: scanning ? 14 : 0,
            display: 'inline-flex',
            alignItems: 'center',
            gap: 8,
            color: 'var(--text-secondary)',
            fontSize: '0.82rem',
            minHeight: scanning ? undefined : 0,
            overflow: 'hidden',
          }}
        >
          {scanning && (
            <>
              <span className="ds-dot-live accent" aria-hidden="true" />
              <span>
                Refreshing local daemon snapshot — this does not scan subnets.
              </span>
            </>
          )}
        </div>
      </div>

      {/* API unavailable message */}
      {apiUnavailable && (
        <div className="feat-card" style={{ padding: 16, textAlign: 'center' }}>
          <div style={{ color: 'var(--feat-yellow)', fontSize: '0.9rem', fontWeight: 600, marginBottom: 8 }}>
            Local fleet snapshot API in development.
          </div>
          <div style={{ color: 'var(--text-dim)', fontSize: '0.85rem' }}>
            You can manually add miners by IP address below.
          </div>
        </div>
      )}

      {snapshotLimitations.length > 0 && (
        <div className="feat-card" style={{ padding: 16 }} data-testid="fleet-discovery-limitations">
          <div style={{ color: 'var(--text-secondary)', fontSize: '0.85rem', fontWeight: 600, marginBottom: 8 }}>
            Snapshot scope
          </div>
          <ul style={{ margin: 0, paddingLeft: 18, color: 'var(--text-dim)', fontSize: '0.78rem', lineHeight: 1.5 }}>
            {snapshotLimitations.map(item => (
              <li key={item}>{item}</li>
            ))}
          </ul>
        </div>
      )}

      {/* Manual Add section — for miners on a different VLAN, firewalled
          from broadcast, or otherwise invisible to discovery. */}
      <div className="feat-card" style={{ padding: 16 }}>
        <label
          htmlFor="fleet-discovery-manual-ip"
          style={{ fontSize: '0.85rem', fontWeight: 600, marginBottom: 4, color: 'var(--text-secondary)', display: 'block' }}
        >
          Add miner by IP
        </label>
        <div
          id="fleet-discovery-manual-ip-hint"
          style={{ fontSize: '0.74rem', color: 'var(--text-dim)', marginBottom: 10 }}
        >
          Use this for a known miner IP. Firmware LAN discovery is not linked yet.
        </div>
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
          <input
            id="fleet-discovery-manual-ip"
            type="text"
            inputMode="numeric"
            value={newIP}
            onChange={e => setNewIP(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter') handleAddManual(); }}
            placeholder="IP address (e.g. 203.0.113.100)"
            aria-describedby="fleet-discovery-manual-ip-hint"
            data-testid="fleet-discovery-manual-input"
            className="ds-input"
            style={{ flex: '1 1 220px', minWidth: 0 }}
            disabled={probing}
          />
          <button
            className="feat-btn feat-btn-primary"
            onClick={handleAddManual}
            disabled={probing || !newIP.trim()}
            data-testid="fleet-discovery-manual-add"
            style={{ whiteSpace: 'nowrap' }}
          >
            {probing ? (
              <>
                <span className="feat-spinner" aria-hidden="true" />
                Probing
              </>
            ) : (
              'Add'
            )}
          </button>
        </div>
        {/* STD-B-04 honest-state: a manual probe is a direct browser fetch to
            the target's /api/status. Cross-origin (CORS) or mixed-content rules
            commonly block that, so a reachable miner can still be shown as
            "Unreachable". The local snapshot above reads this miner's daemon
            state only; it does not proxy manual IP probes or scan the LAN. */}
        <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)', marginTop: 10 }}>
          Your browser probes this IP directly. Cross-origin (CORS) or mixed-content rules can
          block the probe and mark a reachable miner as "Unreachable". The local snapshot above
          reads this miner's daemon state only; it does not scan the LAN or proxy manual probes.
        </div>
      </div>

      {/* Aggregate stats */}
      {allMiners.length > 0 && (
        <div className="feat-fleet-stats">
          <div className="feat-fleet-stat">
            <div className="feat-fleet-stat-value">{stats.totalMiners}</div>
            <div className="feat-fleet-stat-label">{t('fleet.found')}</div>
            <div className="feat-fleet-stat-source">{fleetSourceLabel}</div>
          </div>
          <div className="feat-fleet-stat">
            <div className="feat-fleet-stat-value">{stats.totalHashrateThs.toFixed(1)}</div>
            <div className="feat-fleet-stat-label">{t('fleet.totalHashrate')} (TH/s)</div>
            <div className="feat-fleet-stat-source">{freshnessLabel}</div>
          </div>
          <div className="feat-fleet-stat">
            <div className="feat-fleet-stat-value">
              {stats.totalPowerWatts === null ? 'Not reported' : (stats.totalPowerWatts / 1000).toFixed(1)}
            </div>
            <div className="feat-fleet-stat-label">{t('fleet.totalPower')} (kW)</div>
            <div className="feat-fleet-stat-source">{reportedPowerSourceLabel}</div>
          </div>
        </div>
      )}

      {/* Miner cards */}
      {allMiners.length > 0 && (
        <div className="feat-fleet-grid">
          {allMiners.map(miner => {
            const isManual = manualIPs.includes(miner.ip);
            const sTone = statusTone(miner.status);
            const fwTone = firmwareTone(miner.firmware);
            return (
              <div
                key={miner.ip}
                className="feat-fleet-card ds-hoverable"
                data-testid={`fleet-discovery-card-${miner.ip}`}
              >
                <div className="feat-fleet-card-header">
                  <h3 className="feat-fleet-card-name">
                    <span
                      className="feat-status-dot"
                      style={{ background: statusColor(miner.status) }}
                      aria-hidden="true"
                    />
                    {miner.hostname}
                  </h3>
                  <span
                    className={`ds-chip ds-${sTone}${miner.status === 'online' ? ' ds-live' : ''}`}
                    data-testid={`fleet-discovery-status-${miner.ip}`}
                  >
                    <span className="ds-dot" />
                    {miner.status}
                  </span>
                </div>

                <div className="feat-fleet-card-body">
                  <div className="feat-fleet-row">
                    <span className="feat-fleet-label">Model</span>
                    <span className="feat-fleet-value">{miner.model}</span>
                  </div>
                  <div className="feat-fleet-row">
                    <span className="feat-fleet-label">IP</span>
                    <span className="feat-fleet-value mono">{miner.ip}</span>
                  </div>
                  <div className="feat-fleet-row">
                    <span className="feat-fleet-label">Firmware</span>
                    <span
                      className={`ds-chip ds-${fwTone}`}
                      title="Detected firmware fingerprint from /api/status. Unknown = no fingerprint returned; Unreachable = probe failed."
                    >
                      {miner.firmware}
                    </span>
                  </div>
                  <div className="feat-fleet-row">
                    <span className="feat-fleet-label">Hashrate</span>
                    <span className="feat-fleet-value">{miner.hashrateThs.toFixed(1)} TH/s</span>
                  </div>
                  <div className="feat-fleet-row">
                    <span className="feat-fleet-label">Power</span>
                    <span className="feat-fleet-value">
                      {typeof miner.powerWatts === 'number' && Number.isFinite(miner.powerWatts) && miner.powerWatts > 0
                        ? `${miner.powerWatts.toFixed(0)} W`
                        : 'Not reported'}
                    </span>
                  </div>
                  <div className="feat-fleet-row">
                    <span className="feat-fleet-label">Uptime</span>
                    <span className="feat-fleet-value">{formatUptime(miner.uptimeS)}</span>
                  </div>
                </div>

                <div
                  className="feat-fleet-card-footer"
                  style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}
                >
                  {!isManual && miner.status !== 'error' && (
                    <button
                      className="feat-btn feat-btn-primary feat-btn-sm"
                      onClick={() => handleOnboard(miner)}
                      data-testid={`fleet-discovery-onboard-${miner.ip}`}
                      title="Pin this miner to your fleet so it stays visible across sessions."
                    >
                      Onboard to fleet
                    </button>
                  )}
                  <button
                    className="feat-btn feat-btn-secondary feat-btn-sm"
                    onClick={() => window.open(`http://${miner.ip}`, '_blank')}
                    aria-label={`${t('fleet.openInNewTab')} — ${miner.hostname} (opens in new tab)`}
                  >
                    {t('fleet.openInNewTab')}
                  </button>
                  {isManual && (
                    <button
                      className="feat-btn feat-btn-secondary feat-btn-sm"
                      onClick={() => handleRemoveManual(miner.ip)}
                      style={{ color: 'var(--feat-red)', borderColor: 'var(--feat-red)' }}
                      data-testid={`fleet-discovery-remove-${miner.ip}`}
                      aria-label={`Remove ${miner.hostname} (${miner.ip}) from fleet`}
                    >
                      Remove
                    </button>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* No miners found */}
      {scanned && allMiners.length === 0 && !scanning && (
        <EmptyState
          illustration={<NoFleetIllustration />}
          title="No miners found"
          hint="The firmware snapshot only reports this miner. Add known peer IPs above, or use DCENT_Toolbox for broader fleet workflows."
          data-testid="fleet-discovery-empty"
        />
      )}
    </div>
  );
}
