// KitDeviceContext — structural recreation of the design-kit's
// `DashboardPage.jsx` DeviceContext + HeroKpi composition.
//
// Kit reference: ui_kits/dashboard/DashboardPage.jsx (DeviceContext + HeroKpi).
// The kit renders:
//   <div className="device-context">
//     <div className="device-info"> model + MAC/CB/PSU meta </div>
//     <div className="hero-kpi-strip"> 4× <div className="hero-kpi"> </div>
//   </div>
//
// Every value here is fed from the REAL Zustand store (systemInfo / status /
// derived health). Where the kit shows a value production has no real source
// for, an honest placeholder is rendered — NEVER a fabricated number.
import React from 'react';
import { useMinerStore } from '../../store/miner';
import { Sparkline } from '../common/Sparkline';
import { glossaryText } from '../../utils/glossary';

interface HeroKpiProps {
  label: string;
  value: string;
  unit?: string;
  /** Glossary key → honest hover explanation via the kit `data-tip`. */
  tipKey?: string;
  /** Renders the kit's pulsing live dot (only when telemetry is truthful). */
  live?: boolean;
  sub?: string;
  /** Tween-on-change animation marker class from useValueFlash. */
  flashClass?: string;
  /** Inline sparkline (kit hero-kpi has none, but production keeps the
   *  honest trend visual the kit-skinned `.hero-kpi` already accommodates). */
  sparkData?: number[];
}

// Mirrors the kit's HeroKpi: a `.hero-kpi` cell with optional live dot,
// `.kpi-label`, and a `.kpi-value` whose number animates on change
// (kit `.kpi-num-anim`).
function HeroKpi({ label, value, unit, tipKey, live, sub, flashClass, sparkData }: HeroKpiProps) {
  return (
    <div
      className={`hero-kpi ${flashClass ?? ''}`}
      data-tip={tipKey ? glossaryText(tipKey) : undefined}
      data-tooltip={tipKey ? glossaryText(tipKey) : undefined}
    >
      {live && <span className="hero-kpi-live" aria-hidden="true" />}
      <div className="kpi-label">{label}</div>
      <div className="kpi-value">
        <span className="kpi-num-anim">{value}</span>
        {unit && <span className="unit">{unit}</span>}
      </div>
      {sub && <div className="kpi-sub">{sub}</div>}
      {sparkData && sparkData.length > 5 && (
        <Sparkline
          data={sparkData.slice(-30)}
          width={120}
          height={22}
          color="var(--accent)"
          className="sparkline-inline"
        />
      )}
    </div>
  );
}

export interface KitDeviceContextProps {
  /** Live hashrate display value + unit (already formatted, honest). */
  hashrateValue: string;
  hashrateUnit: string;
  hashrateLabel: string;
  showHashrate: boolean;
  hashrateFlashClass: string;
  hashrateSpark: number[];
  isProxyTelemetry: boolean;
  topKpiStatus: string;
  uptimeValue: string;
  uptimeFlashClass: string;
  poolDisplay: string;
  poolHost: string;
  poolLive: boolean;
  sharesValue: string;
  sharesSub: string;
}

// The kit's DeviceContext header row: model name (last word accented), the
// MAC / control-board / PSU meta line, and the 4-cell hero KPI strip.
// All real-data fed.
export function KitDeviceContext(props: KitDeviceContextProps) {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const model = systemInfo?.model ?? 'Antminer';
  const mac = systemInfo?.mac ?? '';
  const hostname = systemInfo?.hostname ?? '';
  const controlBoard = systemInfo?.board ?? systemInfo?.hardware?.control_board ?? '';
  const psu = systemInfo?.hardware?.psu_model ?? '';

  const parts = (model || 'Antminer').split(' ');
  const head = parts.slice(0, -1).join(' ');
  const tail = parts.slice(-1)[0] ?? '';

  return (
    <div className="device-context" data-testid="device-context">
      <div className="device-info">
        <div className="device-model">
          {head ? <>{head} </> : null}
          <span className="accent-mark">{tail}</span>
        </div>
        <div className="device-meta">
          {hostname && (<>HOST: <strong>{hostname}</strong>{mac ? ' · ' : ''}</>)}
          {mac && (<>MAC: <strong>{mac}</strong></>)}
          {(hostname || mac) && <br />}
          CB: <strong>{controlBoard || '—'}</strong>
          {' · '}PSU: <strong>{psu || '—'}</strong>
          {systemInfo?.version ? <>{' · '}FW: <strong>{systemInfo.version}</strong></> : null}
        </div>
      </div>
      <div className="hero-kpi-strip">
        <HeroKpi
          label={props.hashrateLabel}
          value={props.showHashrate ? props.hashrateValue : props.topKpiStatus}
          unit={props.showHashrate ? ` ${props.hashrateUnit}` : undefined}
          tipKey={props.isProxyTelemetry ? 'hashrate_proxied' : 'hashrate_local_vs_pool'}
          live={props.showHashrate}
          flashClass={props.showHashrate ? props.hashrateFlashClass : ''}
          sparkData={props.showHashrate ? props.hashrateSpark : undefined}
        />
        <HeroKpi
          label="System Uptime"
          value={props.uptimeValue}
          tipKey="uptime"
          flashClass={props.uptimeFlashClass}
        />
        <HeroKpi
          label="Pool"
          value={props.poolDisplay}
          tipKey="pool_state"
          live={props.poolLive}
          sub={props.poolHost}
        />
        <HeroKpi
          label="Shares (acc / rej)"
          value={props.sharesValue}
          tipKey="share_accepted"
          sub={props.sharesSub}
        />
      </div>
    </div>
  );
}
