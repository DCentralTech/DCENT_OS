import React, { useCallback } from 'react';
import { useMinerStore } from '../../store/miner';
import { formatHashrateShort, formatUptime } from '../../utils/format';
import { getLiveWallWatts } from '../../utils/power';
import { useTransportState } from '../../hooks/useTransportState';
import { TransportChip } from '../common/TransportChip';
import { useFxPulse, useRewardFx } from '../../fx/useRewardFx';

interface StatusStripProps {
  /** When true a pulsing red REC dot is rendered (flight recorder / replay). */
  recording?: boolean;
}

export function StatusStrip({ recording = false }: StatusStripProps) {
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const hashrateHistory = useMinerStore(s => s.hashrateHistory);
  const transportState = useTransportState();
  const [shareRoll, pulseShareRoll] = useFxPulse(620);
  const [poolRing, pulsePoolRing] = useFxPulse(820);

  useRewardFx(useCallback((event) => {
    if (event.intensity <= 0) return;
    if (event.kind === 'share-accepted' || event.kind === 'share-rejected') {
      pulseShareRoll();
    } else if (event.kind === 'pool-transition') {
      pulsePoolRing();
    }
  }, [pulsePoolRing, pulseShareRoll]));

  const hashrate = status?.hashrate_ghs ?? 0;
  const hr = formatHashrateShort(hashrate);
  const accepted = status?.accepted ?? 0;
  const rejected = status?.rejected ?? 0;
  const poolStatus = status?.pool?.status ?? 'offline';
  const poolUrl = status?.pool?.url?.replace(/stratum\+tcp:\/\//, '').split(':')[0] ?? '';
  const uptimeS = status?.uptime_s ?? 0;
  const watts = getLiveWallWatts(stats?.power);

  const chainList = status?.chains ?? [];
  const totalChains = chainList.length;
  const chainsUp = chainList.filter(c => c.chips > 0 && (c.status?.toLowerCase?.() !== 'down')).length;
  const maxTemp = chainList.reduce((m, c) => (typeof c.temp_c === 'number' && c.temp_c > m ? c.temp_c : m), 0);
  const fanPwm = status?.fans?.pwm ?? 0;
  const fanRpm = status?.fans?.rpm ?? 0;

  const tempTone = maxTemp >= 70 ? 'danger' : maxTemp >= 60 ? 'warn' : maxTemp > 0 ? 'ok' : 'idle';
  const fanTone = fanPwm >= 80 ? 'danger' : fanPwm >= 50 ? 'warn' : fanPwm > 0 ? 'ok' : 'idle';
  const chainTone = totalChains === 0
    ? 'idle'
    : chainsUp === totalChains
      ? 'ok'
      : chainsUp > 0
        ? 'warn'
        : 'danger';
  const hrTone = hashrate > 0 ? 'ok' : 'idle';
  const poolTone = poolStatus.toLowerCase() === 'alive' ? 'ok' : 'danger';
  const transportTone = transportState.transport === 'ws-live'
    ? 'ok'
    : transportState.transport === 'rest-polling'
      ? 'warn'
      : 'danger';

  const sparkPoints = hashrateHistory.slice(-30).map(p => p.value);
  const sparkMax = Math.max(...sparkPoints, 1);
  const sparkSvg = sparkPoints.length > 1
    ? sparkPoints.map((v, i) =>
        `${(i / (sparkPoints.length - 1)) * 60},${16 - (v / sparkMax) * 14}`
      ).join(' ')
    : '';
  // Faint area fill under the stroke (phosphor identity). Closes the polyline
  // down to the baseline (y=16) at both ends so the polygon fills the area.
  const sparkFill = sparkSvg ? `0,16 ${sparkSvg} 60,16` : '';

  return (
    <div className="hacker-status-strip" role="status" aria-label="Live miner telemetry">
      <Field label="HR" tone={hrTone} title={`Hashrate ${hashrate.toFixed(2)} GH/s`}>
        <span className="hacker-status-strip-value">
          {hashrate > 0 ? `${hr.value}${hr.unit}` : '---'}
        </span>
        {sparkSvg && (
          <svg className="hacker-status-strip-spark" width="60" height="16" viewBox="0 0 60 16" aria-hidden="true">
            <polygon className="hacker-status-strip-spark-fill" points={sparkFill} stroke="none" />
            <polyline points={sparkSvg} fill="none" stroke="currentColor" strokeWidth="1.2" />
          </svg>
        )}
      </Field>

      <Field label="POOL" tone={poolTone} title={`${poolUrl || 'no pool'} · ${poolStatus}`} fxClassName={poolRing ? 'dcfx-pool-ring' : undefined}>
        <span className="hacker-status-strip-value">{poolUrl || '---'}</span>
      </Field>

      <Field label="SH" tone={rejected > 0 ? 'warn' : 'ok'} title={`Accepted ${accepted} · Rejected ${rejected}`}>
        <span className={`hacker-status-strip-value hacker-status-strip-good ${shareRoll ? 'dcfx-footer-digit' : ''}`}>{accepted}</span>
        <span className="hacker-status-strip-sep">/</span>
        <span className={`hacker-status-strip-value ${rejected > 0 ? 'hacker-status-strip-bad' : 'hacker-status-strip-dim'}`}>{rejected}</span>
      </Field>

      {watts > 0 && (
        <Field label="PWR" tone="ok" title={`${watts} W live wall power`}>
          <span className="hacker-status-strip-value">{watts}<small>W</small></span>
        </Field>
      )}

      <Field label="TEMP" tone={tempTone} title={`Hottest chain: ${maxTemp > 0 ? maxTemp.toFixed(1) + '°C' : 'no data'}`}>
        <span className="hacker-status-strip-value">
          {maxTemp > 0 ? `${maxTemp.toFixed(1)}°` : '---'}
        </span>
      </Field>

      <Field label="FAN" tone={fanTone} title={`Fan PWM ${fanPwm}%${fanRpm > 0 ? ` @ ${fanRpm} RPM` : ''}`}>
        <span className="hacker-status-strip-value">
          {fanPwm > 0 ? `${fanPwm}%` : '---'}
        </span>
        {fanRpm > 0 && <span className="hacker-status-strip-sub">{fanRpm}r</span>}
      </Field>

      <Field label="CHN" tone={chainTone} title={`${chainsUp} of ${totalChains} chains up`}>
        <span className="hacker-status-strip-value">
          {totalChains > 0 ? `${chainsUp}/${totalChains}` : '---'}
        </span>
      </Field>

      <Field label="UP" tone="info" title={`Uptime ${uptimeS}s`}>
        <span className="hacker-status-strip-value">
          {uptimeS > 0 ? formatUptime(uptimeS) : '---'}
        </span>
      </Field>

      <Field label="TX" tone={transportTone} title={transportState.title}>
        <TransportChip className="hacker-status-strip-value" showDot={false} />
      </Field>

      {recording && (
        <div className="hacker-status-strip-rec" title="Recording session" role="img" aria-label="Recording">
          <span className="hacker-status-strip-rec-dot" />
          <span>REC</span>
        </div>
      )}
    </div>
  );
}

interface FieldProps {
  label: string;
  tone: 'ok' | 'warn' | 'danger' | 'idle' | 'info';
  title?: string;
  fxClassName?: string;
  children: React.ReactNode;
}

function Field({ label, tone, title, fxClassName, children }: FieldProps) {
  return (
    <div className={`hacker-status-strip-field tone-${tone} ${fxClassName ?? ''}`} title={title}>
      <span className="hacker-status-strip-dot" aria-hidden="true" />
      <span className="hacker-status-strip-label">{label}</span>
      <span className="hacker-status-strip-data">{children}</span>
    </div>
  );
}
