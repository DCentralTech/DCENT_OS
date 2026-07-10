import React, { useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type {
  CompetitiveReadinessFeature,
  CompetitiveReadinessResponse,
  CompetitiveReadinessStatus,
} from '../../api/types';
import { InfoDot } from '../common/Tooltip';
import { SectionSkeleton } from '../common/skeletons';

const REFRESH_MS = 60000;

function statusTone(status: CompetitiveReadinessStatus): 'success' | 'warning' | 'danger' | 'muted' {
  if (status === 'proven') return 'success';
  if (status === 'partial' || status === 'saved_only' || status === 'requires_restart') return 'warning';
  if (status === 'blocked' || status === 'unsafe') return 'danger';
  return 'muted';
}

function statusLabel(status: CompetitiveReadinessStatus): string {
  return status.replace(/_/g, ' ');
}

function GatePill({
  tone,
  children,
}: {
  tone: 'success' | 'warning' | 'danger' | 'muted';
  children: React.ReactNode;
}) {
  return (
    <span className={`mining-pipeline-manifest-pill mining-pipeline-manifest-pill-${tone}`}>
      {children}
    </span>
  );
}

function FeatureRow({ feature }: { feature: CompetitiveReadinessFeature }) {
  const tone = statusTone(feature.status);
  return (
    <div
      className="mining-pipeline-manifest-field-row"
      data-competitive-feature={feature.id}
      data-competitive-feature-status={feature.status}
      data-competitive-feature-source-basis={feature.source_basis}
      data-competitive-feature-telemetry-source={feature.telemetry_source}
      data-competitive-feature-confidence={feature.confidence}
    >
      <div>
        <strong>{feature.label}</strong>
        <span>{feature.home_miner_value}</span>
        <span>{feature.current_behavior}</span>
      </div>
      <div className="mining-pipeline-manifest-row-pills">
        <GatePill tone={tone}>{statusLabel(feature.status)}</GatePill>
        <GatePill tone={feature.promotion_allowed ? 'success' : 'warning'}>
          {feature.promotion_allowed ? 'promotion ok' : 'promotion blocked'}
        </GatePill>
        <GatePill tone={feature.license_required || feature.mandatory_fee ? 'danger' : 'success'}>
          {feature.license_required || feature.mandatory_fee ? 'closed gate' : 'open gate'}
        </GatePill>
        <GatePill tone={feature.priority.startsWith('P0') ? 'warning' : 'muted'}>
          {feature.priority}
        </GatePill>
        <GatePill tone="muted">{feature.source_basis}</GatePill>
      </div>
    </div>
  );
}

export function CompetitiveReadinessCard({ compact = false }: { compact?: boolean }) {
  const [readiness, setReadiness] = useState<CompetitiveReadinessResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;

    const load = async () => {
      try {
        const next = await api.getCompetitiveReadiness();
        if (cancelled) return;
        setReadiness(next);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        setReadiness(null);
        setError(err instanceof Error ? err.message : 'Competitive readiness unavailable.');
      } finally {
        if (!cancelled) {
          setLoading(false);
          timer = window.setTimeout(() => void load(), REFRESH_MS);
        }
      }
    };

    void load();

    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, []);

  const summary = useMemo(() => {
    const features = readiness?.features ?? [];
    return {
      proven: features.filter(feature => feature.status === 'proven').length,
      blocked: features.filter(feature => feature.status === 'blocked').length,
      partial: features.filter(feature => feature.status === 'partial' || feature.status === 'saved_only').length,
    };
  }, [readiness]);

  const gate = readiness?.decentralization_gate;
  const visibleFeatures = compact
    ? (readiness?.features ?? []).slice(0, 5)
    : readiness?.features ?? [];

  return (
    <section
      className="page-surface mining-pipeline-manifest-card"
      aria-label="Competitive firmware readiness and decentralization gate"
      data-competitive-schema={readiness?.schema ?? 'dcentos.competitive.readiness.v1'}
      data-competitive-read-only={String(readiness?.read_only ?? true)}
      data-competitive-control-actions={String(readiness?.control_actions ?? false)}
      data-competitive-hardware-writes={String(readiness?.hardware_writes ?? false)}
      data-competitive-filesystem-mutation={String(readiness?.filesystem_mutation ?? false)}
      data-competitive-content-collected={String(readiness?.content_collected ?? false)}
      data-competitive-probe-performed={String(readiness?.probe_performed ?? false)}
      data-competitive-handlers-executed={String(readiness?.handlers_executed ?? false)}
      data-competitive-license-required={String(gate?.license_required ?? false)}
      data-competitive-license-server-required={String(gate?.license_server_required ?? false)}
      data-competitive-mandatory-fee={String(gate?.mandatory_fee ?? false)}
      data-competitive-fee-route={gate?.fee_route ?? 'unavailable'}
      data-competitive-offline-behavior={gate?.offline_behavior ?? 'unavailable'}
      data-competitive-home-miner-safe={String(gate?.home_miner_safe ?? false)}
      data-competitive-home-miner-safe-status={gate?.home_miner_safe_status ?? 'unavailable'}
    >
      <div className="page-surface-header mining-pipeline-manifest-header">
        <div>
          <div className="page-surface-title">Competitive Firmware Readiness</div>
          <div className="page-surface-copy">
            Read-only RALPH contract for competitor parity and decentralization gates.
            Blocked rows are not controls and do not imply live support.
          </div>
        </div>
        <div className="mining-pipeline-manifest-pills">
          <GatePill tone={statusTone(readiness?.status ?? 'not_implemented')}>
            {readiness?.status ?? 'unavailable'}
          </GatePill>
          <GatePill tone="muted">read-only</GatePill>
          <GatePill tone="muted">no hardware writes</GatePill>
        </div>
      </div>

      {loading && !readiness && !error && (
        <SectionSkeleton rows={4} data-testid="competitive-readiness-loading" />
      )}
      {error && (
        <div className="mining-pipeline-manifest-alert">
          Unavailable: {error}. No readiness was inferred.
        </div>
      )}

      <div className="mining-pipeline-manifest-grid">
        <div className="mining-pipeline-manifest-tile">
          <span>Proven <InfoDot term="competitive_proven" size={12} /></span>
          <strong>{summary.proven}</strong>
          <p>Features with enough evidence to claim live behavior.</p>
        </div>
        <div className="mining-pipeline-manifest-tile">
          <span>Blocked <InfoDot term="competitive_blocked" size={12} /></span>
          <strong>{summary.blocked}</strong>
          <p>Rows needing hardware proof, rollback proof, or missing implementation.</p>
        </div>
        <div className="mining-pipeline-manifest-tile">
          <span>Partial / Saved</span>
          <strong>{summary.partial}</strong>
          <p>Architecture or config exists, but live behavior is limited or not proven.</p>
        </div>
        <div className="mining-pipeline-manifest-tile">
          <span>Fee Route <InfoDot term="fee_route" size={12} /></span>
          <strong>{gate?.fee_route ?? 'unavailable'}</strong>
          <p>No hidden route; donation routing must remain visible and disableable.</p>
        </div>
        <div className="mining-pipeline-manifest-tile">
          <span>Donation Proof</span>
          <strong>{gate?.donation?.donation_off_test_status ?? 'unavailable'}</strong>
          <p>
            Default {gate?.donation?.default_percent ?? '?'}%, pool visible:{' '}
            {gate?.donation?.pool_visible ? 'yes' : 'unknown'}.
          </p>
        </div>
      </div>

      <div className="mining-pipeline-manifest-section">
        <div className="mining-pipeline-manifest-section-title">Decentralization Gate</div>
        <div className="mining-pipeline-manifest-source-row">
          <GatePill tone={gate?.license_required ? 'danger' : 'success'}>
            {gate?.license_required ? 'license required' : 'license-free'}
          </GatePill>
          <GatePill tone={gate?.license_server_required ? 'danger' : 'success'}>
            {gate?.license_server_required ? 'license server' : 'no license server'}
          </GatePill>
          <GatePill tone={gate?.mandatory_fee ? 'danger' : 'success'}>
            {gate?.mandatory_fee ? 'mandatory fee' : 'no mandatory fee'}
          </GatePill>
          <GatePill tone="success">{gate?.offline_behavior ?? 'local-first unknown'}</GatePill>
          <GatePill tone="muted">
            {gate ? `${gate.external_dependencies.length} dependencies listed` : 'dependencies unavailable'}
          </GatePill>
          <GatePill tone="muted">{gate?.repair_diagnostic ?? 'repair default unavailable'}</GatePill>
          <GatePill tone={gate?.home_miner_safe_status === 'proven' ? 'success' : 'warning'}>
            home safety {gate?.home_miner_safe_status ?? 'unknown'}
          </GatePill>
        </div>
      </div>

      <div className="mining-pipeline-manifest-section">
        <div className="mining-pipeline-manifest-section-title">Evidence Contract</div>
        <div className="mining-pipeline-manifest-list">
          <div className="mining-pipeline-manifest-field-row">
            <div>
              <strong>Source basis</strong>
              <span>{gate?.source_basis?.join(', ') ?? 'unavailable'}</span>
            </div>
            <div className="mining-pipeline-manifest-row-pills">
              <GatePill tone="muted">static contract</GatePill>
            </div>
          </div>
          <div className="mining-pipeline-manifest-field-row">
            <div>
              <strong>Docs</strong>
              <span>{gate?.docs_link ?? 'unavailable'}</span>
              <span>{gate?.recovery_link ?? 'unavailable'}</span>
            </div>
            <div className="mining-pipeline-manifest-row-pills">
              <GatePill tone="warning">{gate?.docs_link_status ?? 'not linked'}</GatePill>
              <GatePill tone="warning">{gate?.recovery_link_status ?? 'not linked'}</GatePill>
            </div>
          </div>
          {!compact && gate?.write_surfaces?.map(surface => (
            <div className="mining-pipeline-manifest-field-row" key={surface.surface}>
              <div>
                <strong>{surface.surface}</strong>
                <span>{surface.write_gate}</span>
              </div>
              <div className="mining-pipeline-manifest-row-pills">
                <GatePill tone={surface.audit_status === 'present' ? 'success' : 'warning'}>
                  {surface.audit_status}
                </GatePill>
                <GatePill tone="muted">{surface.default}</GatePill>
              </div>
            </div>
          ))}
        </div>
      </div>

      <div className="mining-pipeline-manifest-section">
        <div className="mining-pipeline-manifest-section-title">Competitive Gaps</div>
        <div className="mining-pipeline-manifest-list">
          {visibleFeatures.map(feature => (
            <FeatureRow key={feature.id} feature={feature} />
          ))}
          {!visibleFeatures.length && (
            <p className="mining-pipeline-manifest-note">Unavailable: no readiness rows loaded.</p>
          )}
        </div>
      </div>

      {!compact && readiness?.limitations?.length ? (
        <ul className="mining-pipeline-manifest-limit-list">
          {readiness.limitations.map(item => <li key={item}>{item}</li>)}
        </ul>
      ) : null}
    </section>
  );
}

export default CompetitiveReadinessCard;
