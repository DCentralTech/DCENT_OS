import React, { useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type { MiningWorkPostureResponse, MiningWorkPostureStatus, RecentShareEvent } from '../../api/types';
import { SectionSkeleton } from '../common/skeletons';

const REFRESH_MS = 30000;

type PostureTone = 'success' | 'info' | 'warning' | 'danger' | 'muted';

function postureTone(status?: MiningWorkPostureStatus): PostureTone {
  if (status === 'active') return 'success';
  if (status === 'mining_capable') return 'info';
  if (status === 'connected') return 'muted';
  if (status === 'connecting' || status === 'waiting') return 'warning';
  if (status === 'unavailable') return 'danger';
  return 'muted';
}

function valueOrUnavailable(value: React.ReactNode | null | undefined): React.ReactNode {
  if (value === null || value === undefined || value === '') return 'Unavailable';
  return value;
}

function formatAge(seconds?: number | null): string {
  if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds < 0) return 'Unavailable';
  if (seconds < 60) return `${Math.round(seconds)}s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  return `${(seconds / 3600).toFixed(1)}h`;
}

function formatHashrate(ghs?: number | null): string {
  if (typeof ghs !== 'number' || !Number.isFinite(ghs) || ghs <= 0) return '0 TH/s';
  if (ghs >= 1000) return `${(ghs / 1000).toFixed(2)} TH/s`;
  return `${ghs.toFixed(0)} GH/s`;
}

function formatPercent(value?: number | null): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) return 'Unavailable';
  return `${value.toFixed(1)}%`;
}

function formatDifficulty(value?: number | null): string {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) return 'Unavailable';
  return value.toLocaleString(undefined, { maximumFractionDigits: 2 });
}

function formatShareTime(ms?: number | null): string {
  if (typeof ms !== 'number' || !Number.isFinite(ms) || ms <= 0) return 'Unavailable';
  const date = new Date(ms);
  return Number.isNaN(date.getTime()) ? 'Unavailable' : date.toLocaleTimeString();
}

function StatusChip({ status }: { status?: MiningWorkPostureStatus }) {
  const tone = postureTone(status);
  return (
    <span className={`mining-work-posture-chip mining-work-posture-chip-${tone}`}>
      {status || 'unavailable'}
    </span>
  );
}

function SourcePill({ children }: { children: React.ReactNode }) {
  return <span className="mining-work-posture-source-pill">{children}</span>;
}

function PostureTile({
  title,
  value,
  detail,
  tone = 'muted',
}: {
  title: string;
  value: React.ReactNode;
  detail: React.ReactNode;
  tone?: PostureTone;
}) {
  return (
    <div className={`mining-work-posture-tile mining-work-posture-tile-${tone}`}>
      <span>{title}</span>
      <strong>{valueOrUnavailable(value)}</strong>
      <p>{valueOrUnavailable(detail)}</p>
    </div>
  );
}

function ShareRows({ events }: { events: RecentShareEvent[] }) {
  if (!events.length) {
    return (
      <p className="mining-work-posture-note">
        Unavailable: no recent real share rows are recorded. No rows are inferred from hashrate or counters.
      </p>
    );
  }

  return (
    <div className="mining-work-posture-events" aria-label="Recent real share events from mining work posture">
      {events.slice(0, 4).map((event, index) => (
        <div key={`${event.timestamp_ms}-${event.job_id}-${index}`} className="mining-work-posture-event-row">
          <span>{formatShareTime(event.timestamp_ms)}</span>
          <strong>{event.result || 'unknown'}</strong>
          <em>{event.job_id || 'job unavailable'}</em>
        </div>
      ))}
    </div>
  );
}

export function MiningWorkPostureCard({ variant = 'full' }: { variant?: 'full' | 'compact' }) {
  const [posture, setPosture] = useState<MiningWorkPostureResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;

    const load = async () => {
      try {
        const next = await api.getMiningWorkPosture();
        if (cancelled) return;
        setPosture(next);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : 'Mining work posture endpoint unavailable.');
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    void load();
    const timer = window.setInterval(() => void load(), REFRESH_MS);

    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  const latestJobLabel = posture?.jobs.latest_observed_job_id || 'Unavailable';
  const poolLabel = posture?.pool.available ? posture.pool.status : 'Unavailable';
  const latestShareAge = posture?.shares.latest_event_age_s ?? posture?.pool.last_accepted_share_s ?? null;
  const latestNotifyAge = posture?.work.current_notify_age_s ?? posture?.pool.no_notify_age_s ?? null;
  const jdLabel = posture?.job_declaration.custom_job_injection_active
    ? 'custom active'
    : posture?.job_declaration.custom_job_bridge?.status === 'declared'
      ? 'declared'
      : posture?.job_declaration.custom_job_candidate_ready
        ? 'candidate ready'
        : posture?.job_declaration.mining_job_token_available
          ? 'token ready'
          : posture?.job_declaration.connected
            ? 'connected'
            : posture?.job_declaration.enabled
              ? 'pending'
              : 'disabled';
  const recentEvents = useMemo(
    () => posture?.shares.recent_events ?? [],
    [posture],
  );

  if (variant === 'compact') {
    return (
      <div className="mining-work-posture-compact" aria-label="Read-only mining work and share posture" aria-live="polite">
        <div>
          <span>Mining Work</span>
          <strong>{posture ? `${poolLabel} / job ${latestJobLabel}` : 'Unavailable'}</strong>
        </div>
        <StatusChip status={posture?.status} />
        <p>
          {posture ? `${formatHashrate(posture.work.hashrate_5s_ghs)} / share age ${formatAge(latestShareAge)}` : 'Waiting for posture evidence'}
        </p>
      </div>
    );
  }

  return (
    <section className="page-surface mining-work-posture-card" aria-label="Read-only mining work and share posture">
      <div className="page-surface-header mining-work-posture-header">
        <div>
          <div className="page-surface-title">Mining Work Posture</div>
          <div className="page-surface-copy">
            Read-only. No pool switching, failover trigger, dispatcher inspection, or mining control action.
          </div>
        </div>
        <div className="mining-work-posture-pills">
          <StatusChip status={posture?.status} />
          <SourcePill>read-only</SourcePill>
          <SourcePill>{posture?.telemetry_source || 'source unavailable'}</SourcePill>
        </div>
      </div>

      {error && <div className="mining-work-posture-alert">Unavailable: {error}</div>}
      {loading && !posture && !error && <SectionSkeleton rows={4} data-testid="mining-work-posture-loading" />}

      <div className="mining-work-posture-grid">
        <PostureTile
          title="Pool"
          value={poolLabel}
          detail={posture ? `${posture.pool.protocol} / difficulty ${formatDifficulty(posture.pool.difficulty)}` : 'Waiting for pool state'}
          tone={posture?.pool.active ? 'success' : posture?.pool.mining_capable ? 'info' : posture?.pool.connected ? 'muted' : 'warning'}
        />
        <PostureTile
          title="Latest Job"
          value={latestJobLabel}
          detail={posture?.jobs.reason || 'Waiting for recent share history'}
          tone={posture?.jobs.available ? 'success' : 'warning'}
        />
        <PostureTile
          title="Shares"
          value={`${posture?.shares.accepted_total ?? 0} / ${posture?.shares.rejected_total ?? 0}`}
          detail={`accept ${formatPercent(posture?.shares.accept_rate_pct)} / recent ${posture?.shares.recent_count ?? 0}`}
          tone={(posture?.shares.rejected_total ?? 0) > 0 ? 'warning' : 'success'}
        />
        <PostureTile
          title="Work"
          value={formatHashrate(posture?.work.hashrate_5s_ghs)}
          detail={posture?.work.reason || 'Waiting for miner state'}
          tone={posture?.work.active_hashrate ? 'success' : 'muted'}
        />
      </div>

      <div className="mining-work-posture-source-row">
        <SourcePill>pool: {posture?.pool.telemetry_source || 'unavailable'}</SourcePill>
        <SourcePill>job: {posture?.jobs.latest_observed_job_source || 'unavailable'}</SourcePill>
        <SourcePill>share age: {formatAge(latestShareAge)}</SourcePill>
        <SourcePill>{latestNotifyAge == null ? 'notify age: unavailable' : `notify age: ${formatAge(latestNotifyAge)}`}</SourcePill>
        <SourcePill>SV2: {posture?.sv2.available ? 'available' : 'unavailable'}</SourcePill>
        <SourcePill>JD: {jdLabel}</SourcePill>
        <SourcePill>BM1362 VR: {posture?.asic_version_rolling?.bm1362_status || 'not claimed'}</SourcePill>
      </div>

      <ShareRows events={recentEvents} />

      {posture?.limitations?.length ? (
        <ul className="mining-work-posture-limit-list">
          {posture.limitations.map(item => <li key={item}>{item}</li>)}
        </ul>
      ) : null}
    </section>
  );
}
