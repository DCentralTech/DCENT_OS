import React, { useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type { ThermalPowerPostureResponse, ThermalPostureStatus } from '../../api/types';
import { useTemp } from '../../hooks/useTemp';
import { formatWatts } from '../../utils/format';
import { SectionSkeleton } from '../common/skeletons';

const REFRESH_MS = 30000;

function postureTone(status?: ThermalPostureStatus): 'success' | 'warning' | 'danger' | 'muted' {
  if (status === 'critical' || status === 'sensor_limited') return 'danger';
  if (status === 'hot' || status === 'limited' || status === 'watch' || status === 'unknown') return 'warning';
  if (status === 'ok') return 'success';
  return 'muted';
}

function label(value: React.ReactNode | null | undefined): React.ReactNode {
  if (value === null || value === undefined || value === '') return 'Unavailable';
  return value;
}

function formatAge(seconds?: number | null): string {
  if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds < 0) return 'Unavailable';
  if (seconds < 60) return `${Math.round(seconds)}s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  return `${(seconds / 3600).toFixed(1)}h`;
}

function StatusChip({ status }: { status?: ThermalPostureStatus }) {
  const tone = postureTone(status);
  return (
    <span className={`thermal-posture-chip thermal-posture-chip-${tone}`}>
      {status || 'unavailable'}
    </span>
  );
}

function SourcePill({ children }: { children: React.ReactNode }) {
  return <span className="thermal-posture-source-pill">{children}</span>;
}

export function ThermalPowerPostureCard({ variant = 'full' }: { variant?: 'full' | 'compact' }) {
  const temp = useTemp();
  const [posture, setPosture] = useState<ThermalPowerPostureResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;

    const load = async () => {
      try {
        const next = await api.getThermalPowerPosture();
        if (cancelled) return;
        setPosture(next);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : 'Thermal posture endpoint unavailable.');
      } finally {
        if (!cancelled) {
          setLoading(false);
          timer = window.setTimeout(load, REFRESH_MS);
        }
      }
    };

    void load();

    return () => {
      cancelled = true;
      if (timer) window.clearTimeout(timer);
    };
  }, []);

  const maxTemp = posture?.thermal.max_temp_c ?? null;
  const avgTemp = posture?.thermal.avg_temp_c ?? null;
  const wallWatts = posture?.power.live_power_available === true
    ? posture.power.wall_watts ?? null
    : null;
  const wattCap = posture?.power.watt_cap ?? null;
  const runtimeLimitCount = posture?.power.dispatcher_limit_count ?? 0;
  const hasLimiter = Boolean(posture?.runtime_ownership.thermal_related_limit || posture?.runtime_ownership.power_cap_active);
  const powerNote = posture?.power.note ?? posture?.power.reason;

  const thermalCopy = useMemo(() => {
    if (maxTemp === null) return 'Temperature unavailable';
    const avg = avgTemp === null ? 'avg unavailable' : `avg ${temp.format(avgTemp)}`;
    return `max ${temp.format(maxTemp)} / ${avg}`;
  }, [avgTemp, maxTemp, temp]);

  if (variant === 'compact') {
    return (
      <div className="thermal-posture-compact" aria-label="Read-only thermal and power posture">
        <div>
          <span>Thermal / Power</span>
          <strong>{thermalCopy}</strong>
        </div>
        <StatusChip status={posture?.status} />
        <p>
          {wallWatts && wallWatts > 0 ? `${formatWatts(wallWatts)} wall` : 'Power unavailable'}
          {hasLimiter ? ` / ${runtimeLimitCount} runtime limits` : ''}
        </p>
      </div>
    );
  }

  return (
    <section className="page-surface thermal-posture-card" aria-label="Read-only thermal and power posture">
      <div className="page-surface-header thermal-posture-header">
        <div>
          <div className="page-surface-title">Thermal / Power Posture</div>
          <div className="page-surface-copy">
            Read-only. No fan/voltage/frequency/PSU writes.
          </div>
        </div>
        <StatusChip status={posture?.status} />
      </div>

      {error && <div className="thermal-posture-alert">Unavailable: {error}</div>}
      {loading && !posture && !error && <SectionSkeleton rows={4} data-testid="thermal-posture-loading" />}

      <div className="thermal-posture-grid">
        <div className="thermal-posture-tile">
          <span>Thermal</span>
          <strong>{thermalCopy}</strong>
          <p>{label(posture?.thermal.reason)}</p>
        </div>
        <div className="thermal-posture-tile">
          <span>Cooling</span>
          <strong>
            {posture?.fans.pwm != null ? `PWM ${posture.fans.pwm}/100` : 'PWM Unavailable'} / {posture?.fans.rpm ? `${posture.fans.rpm.toLocaleString()} RPM` : 'RPM unavailable'}
          </strong>
          <p>{posture?.fans.tach_suspect ? 'Tachometer evidence needs attention' : label(posture?.fans.reason)}</p>
        </div>
        <div className="thermal-posture-tile">
          <span>Power</span>
          <strong>{wallWatts && wallWatts > 0 ? formatWatts(wallWatts) : 'Unavailable'}</strong>
          <p>{posture?.power.calibrated && wallWatts ? 'Wall-meter calibrated estimate' : label(powerNote)}</p>
        </div>
        <div className="thermal-posture-tile">
          <span>Limits</span>
          <strong>
            {wattCap ? `${wattCap.utilization_pct.toFixed(0)}% cap use` : `${runtimeLimitCount} runtime limits`}
          </strong>
          <p>{label(posture?.runtime_ownership.reason)}</p>
        </div>
      </div>

      <div className="thermal-posture-source-row">
        <SourcePill>{posture?.telemetry_source || 'unavailable'}</SourcePill>
        <SourcePill>{posture?.source || 'unavailable'}</SourcePill>
        <SourcePill>{posture?.thermal.thresholds.source || 'thresholds unavailable'}</SourcePill>
        <SourcePill>curtailment: {posture?.curtailment.state || 'unavailable'}</SourcePill>
        <SourcePill>power: {posture?.power.source || 'unavailable'}</SourcePill>
        <SourcePill>power detail: {posture?.power.source_detail || 'unavailable'}</SourcePill>
        <SourcePill>age: {formatAge(posture?.power.age_s)}</SourcePill>
      </div>

      {posture?.limitations?.length ? (
        <ul className="thermal-posture-limit-list">
          {posture.limitations.map(item => <li key={item}>{item}</li>)}
        </ul>
      ) : null}
    </section>
  );
}
