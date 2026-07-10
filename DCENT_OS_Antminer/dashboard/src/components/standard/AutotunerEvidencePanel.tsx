import React, { useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type { AutotunerVisibilityProfileEntry, AutotunerVisibilityResponse } from '../../api/types';
import { InfoDot } from '../common/Tooltip';
import type { GlossaryKey } from '../../utils/glossary';
import { SectionSkeleton } from '../common/skeletons';

const REFRESH_MS = 30000;

function valueOrUnavailable(value: React.ReactNode | null | undefined): React.ReactNode {
  if (value === null || value === undefined || value === '') return 'Unavailable';
  return value;
}

function formatAge(seconds?: number | null): string {
  if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds < 0) {
    return 'Unavailable';
  }
  if (seconds < 60) return `${Math.round(seconds)}s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  return `${(seconds / 3600).toFixed(1)}h`;
}

function formatDuration(seconds?: number | null): string {
  if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds < 0) {
    return 'Unavailable';
  }
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  return `${Math.floor(seconds / 60)}m ${Math.round(seconds % 60)}s`;
}

function statusPill(label: string, tone: 'success' | 'warning' | 'danger' | 'muted' = 'muted') {
  return <span className={`autotuner-evidence-pill autotuner-evidence-pill-${tone}`}>{label}</span>;
}

function parseableCount(entries: AutotunerVisibilityProfileEntry[]): number {
  return entries.filter(entry => entry.parse_ok).length;
}

function evidenceTone(available?: boolean): 'success' | 'warning' | 'muted' {
  return available ? 'success' : 'warning';
}

function EvidenceTile({
  title,
  titleTerm,
  value,
  detail,
  tone = 'muted',
}: {
  title: string;
  /**
   * S3 UXFLOW-TUNE-1: optional glossary key anchoring the tile title so the
   * "Unavailable" evidence-tile pattern (flows §8) is receipts-backed, not
   * bare. Pulls existing copy from glossary.ts — no inline label.
   */
  titleTerm?: GlossaryKey;
  value: React.ReactNode;
  detail: React.ReactNode;
  tone?: 'success' | 'warning' | 'danger' | 'muted';
}) {
  return (
    <div className={`autotuner-evidence-tile autotuner-evidence-tile-${tone}`}>
      <span className="ds-chip" style={{ fontSize: '0.62rem', padding: '2px 6px', marginBottom: 4, display: 'inline-block' }}>
        {title}{titleTerm && <> <InfoDot term={titleTerm} size={11} /></>}
      </span>
      <strong>{valueOrUnavailable(value)}</strong>
      <p>{valueOrUnavailable(detail)}</p>
    </div>
  );
}

function BackupList({ entries }: { entries: AutotunerVisibilityProfileEntry[] }) {
  if (!entries.length) {
    return <p className="autotuner-evidence-note">Unavailable: no backup profile entries reported.</p>;
  }

  return (
    <div className="autotuner-evidence-backups" aria-label="Rollback backup profile evidence">
      {entries.map(entry => (
        <div key={`${entry.chain_id}-${entry.file}`} className="autotuner-evidence-backup-row">
          <span>Chain {entry.chain_id}</span>
          <strong>{entry.parse_ok ? 'Parseable' : entry.present ? 'Unreadable' : 'Missing'}</strong>
          <em>{entry.reason || 'Unavailable'}</em>
        </div>
      ))}
    </div>
  );
}

export function AutotunerEvidencePanel() {
  const [visibility, setVisibility] = useState<AutotunerVisibilityResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;

    const load = async () => {
      try {
        const next = await api.getAutotunerVisibility();
        if (cancelled) return;
        setVisibility(next);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : 'Autotuner visibility endpoint unavailable.');
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

  const savedCount = useMemo(
    () => parseableCount(visibility?.saved_profiles.entries ?? []),
    [visibility],
  );
  const backupCount = useMemo(
    () => parseableCount(visibility?.rollback.backup_profiles ?? []),
    [visibility],
  );
  const runtime = visibility?.runtime;
  const telemetry = visibility?.telemetry;
  const simulation = visibility?.simulation;

  return (
    <>
      <div className="page-hero-strip">
        <div className="page-hero-summary">
          <div
            className="page-hero-eyebrow"
            style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}
          >
            {telemetry?.recording && <span className="ds-dot-live accent" aria-hidden="true" />}
            <span>AUTOTUNER</span>
          </div>
          <div className="page-hero-title">Evidence</div>
          <div className="page-hero-stat">
            {runtime?.available ? runtime.phase : 'Unavailable'}
          </div>
          <div className="page-hero-substat">
            {runtime
              ? `${runtime.source} · ${runtime.stale ? 'stale' : 'fresh'} · age ${formatAge(runtime.age_s)}`
              : 'Waiting for runtime state'}
          </div>
        </div>
        <div className="hero-kpi-strip">
          <div className="hero-kpi">
            <div className="kpi-label">
              Saved Profiles <InfoDot term="autotuner_receipts" size={12} />
            </div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {savedCount}/{visibility?.saved_profiles.expected_chains ?? 0}
              </span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">
              Backup Profiles <InfoDot term="autotuner_receipts" size={12} />
            </div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {backupCount}/{visibility?.rollback.backup_profiles.length ?? 0}
              </span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">
              Telemetry Runs <InfoDot term="autotuner_receipts" size={12} />
            </div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {telemetry?.recording ? 'recording' : (telemetry?.run_count ?? 0).toString()}
              </span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Simulator</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {simulation?.available ? 'available' : 'unavailable'}
              </span>
            </div>
          </div>
        </div>
      </div>

    <section className="section ds-glass-card autotuner-evidence-panel" aria-label="Autotuner evidence and backup-profile evidence">
      <div className="autotuner-evidence-header">
        <div>
          <p>
            Receipts only <InfoDot term="autotuner_receipts" />
            . Every claim below comes from a file on disk or a
            live runtime probe — never a derived guess.
          </p>
        </div>
        <div className="autotuner-evidence-pills">
          {statusPill('Read-only', 'success')}
          {statusPill(visibility?.control_actions === false ? 'No tuning control applied' : 'Unavailable', visibility?.control_actions === false ? 'success' : 'warning')}
          {statusPill(visibility?.hardware_writes === false ? 'No hardware writes' : 'Unavailable', visibility?.hardware_writes === false ? 'success' : 'warning')}
        </div>
      </div>

      {error && <div className="autotuner-evidence-alert">Unavailable: {error}</div>}
      {loading && !visibility && !error && <SectionSkeleton rows={4} data-testid="autotuner-evidence-loading" />}

      <div className="autotuner-evidence-grid">
        <EvidenceTile
          title="Runtime"
          value={runtime?.available ? runtime.phase : 'Unavailable'}
          detail={runtime ? `${runtime.source} / ${runtime.stale ? 'stale' : 'fresh'} / age ${formatAge(runtime.age_s)}` : 'Waiting for runtime state'}
          tone={runtime?.available && !runtime.stale ? 'success' : 'warning'}
        />
        <EvidenceTile
          title="Saved Profiles"
          titleTerm="autotuner_receipts"
          value={`${savedCount}/${visibility?.saved_profiles.expected_chains ?? 0}`}
          detail={visibility?.saved_profiles.reason ? `${visibility.saved_profiles.reason}; disk evidence, not live-validated` : 'Unavailable'}
          tone={evidenceTone(visibility?.saved_profiles.available)}
        />
        <EvidenceTile
          title="Telemetry Runs"
          titleTerm="autotuner_receipts"
          value={telemetry?.recording ? 'Recording' : `${telemetry?.run_count ?? 0} runs`}
          detail={telemetry?.latest_run ? `${formatDuration(telemetry.latest_run.duration_s)}, ${telemetry.latest_run.sample_count} samples` : telemetry?.reason || 'Unavailable'}
          tone={evidenceTone(telemetry?.available)}
        />
        <EvidenceTile
          title="Rollback Evidence"
          titleTerm="autotuner_receipts"
          value={`${backupCount}/${visibility?.rollback.backup_profiles.length ?? 0}`}
          detail={visibility?.rollback.reason || 'Unavailable'}
          tone={evidenceTone(visibility?.rollback.available)}
        />
        <EvidenceTile
          title="Simulator"
          value={simulation?.available ? 'Available' : 'Unavailable'}
          detail={simulation?.reason || 'No production simulator state reported'}
          tone={simulation?.available ? 'success' : 'muted'}
        />
      </div>

      <BackupList entries={visibility?.rollback.backup_profiles ?? []} />

      {/* Wave-13: dropped the 5 hardcoded internal enum literals (runtime,
          saved_profile_disk, telemetry_watch, profile_backup_disk,
          not_implemented) that were rendered as visible chips — only the live
          source is operator-meaningful. */}
      <div className="autotuner-evidence-limits">
        <strong>Source</strong>
        <span>{visibility?.source || 'Unavailable'}</span>
      </div>

      {visibility?.limitations?.length ? (
        <ul className="autotuner-evidence-limit-list">
          {visibility.limitations.map(item => (
            <li key={item}>{item}</li>
          ))}
        </ul>
      ) : null}
    </section>
    </>
  );
}
