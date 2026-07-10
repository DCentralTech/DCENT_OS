import React, { useEffect, useState } from 'react';
import { api } from '../../api/client';
import type {
  MiningPipelineManifestResponse,
  MiningPipelineManifestStatus,
} from '../../api/types';
import { SectionSkeleton } from '../common/skeletons';

const REFRESH_MS = 60000;

type PipelinePillStatus = MiningPipelineManifestStatus | 'read-only' | 'no writes' | 'unavailable' | 'warning' | 'muted';

const HARDWARE_SMOKE_PLAN_SOURCE = 'internal-hardware-smoke-test-plan';
const HARDWARE_SMOKE_PLAN_FALLBACK_MODELS = 's9,s19pro,s21';

function hardwareSmokeModelId(model: string): string {
  return model.toLowerCase().replace(/[^a-z0-9]+/g, '');
}

function manifestTone(status?: MiningPipelineManifestStatus): 'success' | 'warning' | 'danger' | 'muted' {
  if (status === 'available') return 'success';
  if (status === 'degraded') return 'warning';
  if (status === 'publisher_unavailable') return 'warning';
  return 'muted';
}

function pillTone(status?: PipelinePillStatus): 'success' | 'warning' | 'danger' | 'muted' {
  if (status === 'warning') return 'warning';
  if (status === 'read-only' || status === 'no writes' || status === 'muted') return 'muted';
  return manifestTone(status);
}

function StatusPill({
  status,
  children,
}: {
  status?: PipelinePillStatus;
  children?: React.ReactNode;
}) {
  const tone = pillTone(status);
  return (
    <span className={`mining-pipeline-manifest-pill mining-pipeline-manifest-pill-${tone}`}>
      {children ?? status ?? 'unavailable'}
    </span>
  );
}

export function MiningPipelineManifestCard({ compact = false }: { compact?: boolean }) {
  const [manifest, setManifest] = useState<MiningPipelineManifestResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;

    const load = async () => {
      try {
        const next = await api.getMiningPipelineManifest();
        if (cancelled) return;
        setManifest(next);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        setManifest(null);
        setError(err instanceof Error ? err.message : 'Mining pipeline manifest unavailable.');
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

  const contractSchema = manifest?.snapshot_schema || manifest?.snapshot_contract?.schema || 'dcentos.mining.pipeline.snapshot.v1';
  const liveSnapshotEndpoint = manifest?.publisher_gate?.live_snapshot_endpoint ?? null;
  const snapshotContract = manifest?.snapshot_contract;
  const freshnessContract = manifest?.freshness_contract;
  const freshnessClassifier = manifest?.freshness_classifier;
  const publisherDesign = manifest?.publisher_design;
  const snapshotDesign = manifest?.snapshot_design;
  const promotionChecklist = manifest?.publisher_promotion_checklist;
  const fleetParserNotes = manifest?.fleet_parser_notes;
  const fleetParserActiveBlockerHint = fleetParserNotes?.static_aliases?.active_blocker_ids;
  const fleetParserFixtureHint = fleetParserNotes?.static_aliases?.freshness_classifier_example_fixtures;
  const snapshotDesignBlocks = snapshotDesign?.blocks;
  const snapshotStatus = snapshotContract?.status ?? 'unavailable';
  const freshnessWindowMs = freshnessContract?.default_stale_after_ms ?? null;
  const snapshotAvailableRule = freshnessContract?.snapshot_available_only_when ?? 'unavailable';
  const doesNotPopulate = freshnessContract?.does_not_populate ?? [];
  const freshnessClassifierOutputs = freshnessClassifier?.outputs ?? [];
  const freshnessClassifierFailClosed = freshnessClassifier?.fail_closed_when ?? [];
  const freshnessClassifierFixtures = freshnessClassifier?.example_fixtures ?? [];
  const freshnessClassifierFixtureIds = freshnessClassifierFixtures.map(item => item.id).join(',');
  const freshnessClassifierFixtureCount = freshnessClassifier
    ? freshnessClassifier.example_fixture_count ?? freshnessClassifierFixtures.length
    : null;
  const publisherMaxHz = publisherDesign?.bounded_publish_cadence?.max_hz ?? null;
  const publisherPromotionRequires = publisherDesign?.promotion_requires ?? [];
  const promotionChecklistRequirements = promotionChecklist?.requirements ?? [];
  const promotionBlockers = promotionChecklist?.blockers ?? [];
  const activePromotionBlockers = promotionBlockers.filter(item => item.active);
  const promotionBlockerCount = promotionChecklist
    ? promotionChecklist.active_blocker_count ?? activePromotionBlockers.length
    : null;
  const promotionBlockerIds = promotionBlockers.map(item => item.id).join(',');
  const activePromotionBlockerIds = promotionChecklist
    ? promotionChecklist.active_blocker_ids ?? activePromotionBlockers.map(item => item.id)
    : [];
  const activePromotionBlockerIdsLoaded = Boolean(promotionChecklist?.active_blocker_ids?.length);
  const hardwareSmokeRequirements = promotionChecklistRequirements.filter(item => item.id.startsWith('hardware_smoke_'));
  const hardwareSmokePlanModels = publisherDesign?.hardware_smoke_required?.length
    ? publisherDesign.hardware_smoke_required.map(item => hardwareSmokeModelId(item.model)).join(',')
    : HARDWARE_SMOKE_PLAN_FALLBACK_MODELS;
  const hardwareSmokeStatus = hardwareSmokeRequirements.length > 0 && hardwareSmokeRequirements.every(item => item.status === 'pass')
    ? 'pass'
    : 'not_run';
  const domainFreshnessStatus = snapshotDesign?.domain_freshness_status ?? 'unavailable';
  const domainFreshnessBlockIds = snapshotDesignBlocks
    ? 'job_freshness,work_freshness,nonce_freshness,share_freshness'
    : '';
  const summaryItems = [
    ['Publisher', manifest?.live_publisher.enabled ? 'enabled' : 'default off'],
    ['Snapshot', manifest?.snapshot_available ? 'available' : 'unavailable'],
    ['Contract', manifest ? 'schema declared' : 'unavailable'],
    ['Receiver', manifest?.publisher_gate?.receiver_configured ? 'present' : 'none'],
    ['Freshness', freshnessWindowMs !== null ? `${freshnessWindowMs} ms` : 'unavailable'],
    ['Classifier', freshnessClassifier?.runtime_wired ? 'wired' : 'design only'],
    ['Fixtures', freshnessClassifierFixtureCount === null ? 'unavailable' : `${freshnessClassifierFixtureCount} static`],
    ['Design status', publisherDesign?.implemented ? 'implemented' : 'design only'],
    ['Checklist', promotionChecklist?.promotion_state ?? 'blocked'],
    ['Blockers', promotionBlockerCount === null ? 'unavailable' : `${promotionBlockerCount} active`],
    ['Domain freshness', domainFreshnessStatus],
  ] as const;

  const contractDataProps = {
    'data-contract-schema': manifest?.schema ?? 'dcentos.mining.pipeline.manifest.v1',
    'data-contract-snapshot-schema': contractSchema,
    'data-contract-publisher-default-enabled': String(manifest?.publisher_gate?.publisher_default_enabled ?? false),
    'data-contract-publisher-enabled': String(manifest?.live_publisher.enabled ?? false),
    'data-contract-publisher-receiver-configured': String(manifest?.publisher_gate?.receiver_configured ?? false),
    'data-contract-live-snapshot-endpoint': String(liveSnapshotEndpoint),
    'data-contract-live-telemetry': "false",
    'data-contract-freshness-window-ms': String(freshnessWindowMs),
    'data-contract-snapshot-available-rule': snapshotAvailableRule,
    'data-contract-snapshot-status': snapshotStatus,
    'data-contract-default-stale-after-ms': String(freshnessWindowMs),
    'data-contract-publisher-last-update-ms': String(snapshotContract?.publisher_last_update_ms ?? null),
    'data-contract-snapshot-age-ms': String(snapshotContract?.snapshot_age_ms ?? null),
    'data-contract-snapshot-available-only-when': snapshotAvailableRule,
    'data-contract-does-not-populate': doesNotPopulate.join(','),
    'data-contract-freshness-classifier-schema': freshnessClassifier?.schema ?? 'dcentos.mining.pipeline.freshness.classifier.v1',
    'data-contract-freshness-classifier-status': freshnessClassifier?.status ?? 'unavailable',
    'data-contract-freshness-classifier-runtime-wired': String(freshnessClassifier?.runtime_wired ?? false),
    'data-contract-freshness-classifier-live-telemetry': "false",
    'data-contract-freshness-classifier-outputs': freshnessClassifierOutputs.join(','),
    'data-contract-freshness-classifier-fail-closed': freshnessClassifierFailClosed.join(','),
    'data-contract-freshness-classifier-max-future-skew-ms': String(freshnessClassifier?.max_future_skew_ms ?? null),
    'data-contract-freshness-classifier-future-clock-skew-maps-to': freshnessClassifier?.snapshot_status_mapping?.future_clock_skew ?? 'unavailable',
    'data-contract-freshness-classifier-invalid-maps-to': freshnessClassifier?.snapshot_status_mapping?.invalid ?? 'unavailable',
    'data-contract-freshness-classifier-fixtures-loaded': String(Boolean(freshnessClassifierFixtures.length)),
    'data-contract-freshness-classifier-fixtures-schema': freshnessClassifier?.example_fixtures_schema ?? 'dcentos.mining.pipeline.freshness.classifier.fixture.v1',
    'data-contract-freshness-classifier-fixture-count': freshnessClassifierFixtureCount === null ? 'unavailable' : String(freshnessClassifierFixtureCount),
    'data-contract-freshness-classifier-fixtures': freshnessClassifierFixtureIds,
    'data-contract-freshness-classifier-fixtures-design-only': String(freshnessClassifier?.example_fixtures_are_design_only ?? false),
    'data-contract-freshness-classifier-fixtures-non-telemetry': "true",
    'data-contract-freshness-classifier-fixtures-live-telemetry': "false",
    'data-contract-publisher-design-status': publisherDesign?.status ?? 'unavailable',
    'data-contract-publisher-design-implemented': String(publisherDesign?.implemented ?? false),
    'data-contract-publisher-design-live-route-mounted': String(publisherDesign?.live_route_mounted ?? false),
    'data-contract-publisher-design-max-hz': String(publisherMaxHz),
    'data-contract-publisher-design-publish-per-nonce': String(publisherDesign?.bounded_publish_cadence?.publish_per_nonce ?? false),
    'data-contract-publisher-design-promotion-requires': publisherPromotionRequires.join(','),
    'data-contract-publisher-promotion-checklist': promotionChecklist?.status ?? 'unavailable',
    'data-contract-publisher-promotion-state': promotionChecklist?.promotion_state ?? 'blocked',
    'data-contract-publisher-promotion-ready': "false",
    'data-contract-publisher-promotion-blockers-loaded': String(Boolean(promotionChecklist)),
    'data-contract-publisher-promotion-blockers': promotionBlockerIds,
    'data-contract-publisher-promotion-blocker-count': promotionBlockerCount === null ? 'unavailable' : String(promotionBlockerCount),
    'data-contract-publisher-promotion-active-blocker-count': promotionBlockerCount === null ? 'unavailable' : String(promotionBlockerCount),
    'data-contract-publisher-promotion-all-blockers-active': String(promotionChecklist?.all_blockers_active ?? false),
    'data-contract-publisher-promotion-active-blocker-ids': activePromotionBlockerIds.join(','),
    'data-contract-publisher-promotion-active-blocker-ids-loaded': String(activePromotionBlockerIdsLoaded),
    'data-contract-publisher-promotion-active-blocker-ids-source': "static_manifest",
    'data-contract-publisher-promotion-active-blocker-ids-readiness-evidence': "false",
    'data-contract-fleet-parser-notes-schema': fleetParserNotes?.schema ?? 'dcentos.mining.pipeline.fleet_parser_notes.v1',
    'data-contract-fleet-parser-notes-read-only': String(fleetParserNotes?.read_only ?? true),
    'data-contract-fleet-parser-notes-live-telemetry': String(fleetParserNotes?.live_telemetry ?? false),
    'data-contract-fleet-parser-notes-readiness-evidence': String(fleetParserNotes?.readiness_evidence ?? false),
    'data-contract-fleet-parser-active-blocker-source': fleetParserActiveBlockerHint?.source ?? 'static_manifest',
    'data-contract-fleet-parser-active-blocker-mirrors': fleetParserActiveBlockerHint?.mirrors ?? 'publisher_promotion_checklist.blockers where active == true',
    'data-contract-fleet-parser-fixtures-source': fleetParserFixtureHint?.source ?? 'static_design_fixture',
    'data-contract-fleet-parser-fixtures-miner-state': String(fleetParserFixtureHint?.must_not_display_as_miner_state ?? true),
    'data-contract-publisher-hardware-smoke-status': hardwareSmokeStatus,
    'data-contract-publisher-hardware-smoke-plan': "docs_only",
    'data-contract-publisher-hardware-smoke-plan-source': HARDWARE_SMOKE_PLAN_SOURCE,
    'data-contract-publisher-hardware-smoke-plan-read-only': "true",
    'data-contract-publisher-hardware-smoke-plan-readiness-evidence': "false",
    'data-contract-publisher-hardware-smoke-plan-models': hardwareSmokePlanModels,
    'data-contract-publisher-hardware-smoke-plan-live-route-mounted': "true",
    'data-contract-publisher-promotion-route-required': String(promotionChecklist?.route_required ?? false),
    'data-contract-publisher-promotion-dispatcher-reads': String(promotionChecklist?.dispatcher_reads ?? false),
    'data-contract-publisher-promotion-hardware-reads': String(promotionChecklist?.hardware_reads ?? false),
    'data-contract-publisher-promotion-pool-socket-reads': String(promotionChecklist?.pool_socket_reads ?? false),
    'data-contract-snapshot-design-v2-status': snapshotDesign?.status ?? 'unavailable',
    'data-contract-snapshot-design-v2-implemented': String(snapshotDesign?.implemented ?? false),
    'data-contract-snapshot-design-v2-live-route-mounted': String(snapshotDesign?.live_route_mounted ?? false),
    'data-contract-snapshot-design-v2-snapshot-available': String(snapshotDesign?.snapshot_available ?? false),
    'data-contract-domain-freshness-status': domainFreshnessStatus,
    'data-contract-domain-freshness-blocks': domainFreshnessBlockIds,
    'data-contract-job-freshness-status': snapshotDesignBlocks?.job_freshness?.status ?? 'unavailable',
    'data-contract-job-freshness-last-update-ms': String(snapshotDesignBlocks?.job_freshness?.last_update_ms ?? null),
    'data-contract-job-freshness-age-ms': String(snapshotDesignBlocks?.job_freshness?.age_ms ?? null),
    'data-contract-job-freshness-stale-after-ms': String(snapshotDesignBlocks?.job_freshness?.stale_after_ms ?? null),
    'data-contract-job-freshness-source': String(snapshotDesignBlocks?.job_freshness?.source ?? null),
    'data-contract-nonce-freshness-status': snapshotDesignBlocks?.nonce_freshness?.status ?? 'unavailable',
    'data-contract-share-freshness-status': snapshotDesignBlocks?.share_freshness?.status ?? 'unavailable',
    'data-contract-work-freshness-status': snapshotDesignBlocks?.work_freshness?.status ?? 'unavailable',
    'data-contract-read-only': String(manifest?.read_only ?? true),
    'data-contract-control-actions': String(manifest?.control_actions ?? false),
    'data-contract-hardware-writes': String(manifest?.hardware_writes ?? false),
    'data-contract-content-collected': String(manifest?.content_collected ?? false),
    'data-contract-probe-performed': String(manifest?.probe_performed ?? false),
    'data-contract-handlers-executed': String(manifest?.handlers_executed ?? false),
    'data-contract-snapshot-available': String(manifest?.snapshot_available ?? false),
  };

  if (compact) {
    return (
      <section
        className="page-surface mining-pipeline-manifest-card mining-pipeline-manifest-card-compact"
        aria-label="Read-only default-off mining pipeline manifest"
        {...contractDataProps}
      >
        <div className="page-surface-header mining-pipeline-manifest-header">
          <div>
            <div className="page-surface-title">Mining Pipeline Publisher Gate</div>
            <div className="page-surface-copy">
              Default-off schema contract. Default-off publisher contract. Schema declared; Snapshot unavailable.
            </div>
          </div>
          <div className="mining-pipeline-manifest-pills">
            <StatusPill status={manifest?.status} />
            <StatusPill status="read-only">read-only</StatusPill>
          </div>
        </div>
        {error && <div className="mining-pipeline-manifest-alert">Unavailable: {error}</div>}
        {!error && (
          <div className="mining-pipeline-manifest-summary-grid">
            {summaryItems.map(([label, value]) => (
              <div key={label}>
                <span>{label}</span>
                <strong>{value}</strong>
              </div>
            ))}
          </div>
        )}
      </section>
    );
  }

  return (
    <section
      className="page-surface mining-pipeline-manifest-card"
      aria-label="Read-only default-off mining pipeline manifest"
      {...contractDataProps}
    >
      <div className="page-surface-header mining-pipeline-manifest-header">
        <div>
          <div className="page-surface-title">Mining Pipeline Publisher Gate</div>
          <div className="page-surface-copy">
            Read-only default-off snapshot contract. Snapshot unavailable. The live snapshot route is mounted as a read-only clone endpoint, but the publisher remains default-off and unavailable until validated. Live publisher unavailable. No dispatcher or hardware reads, no mining control action. No readiness was inferred.
          </div>
        </div>
        <div className="mining-pipeline-manifest-pills">
          <StatusPill status={manifest?.status} />
          <StatusPill status="read-only">read-only</StatusPill>
          <StatusPill status="no writes">no hardware writes</StatusPill>
          <StatusPill status="unavailable">{manifest?.telemetry_source || 'telemetry_source unavailable'}</StatusPill>
        </div>
      </div>

      {error && <div className="mining-pipeline-manifest-alert">Unavailable: {error}. No readiness was inferred.</div>}
      {loading && !manifest && !error && <SectionSkeleton rows={5} data-testid="mining-pipeline-manifest-loading" />}

      <div className="mining-pipeline-manifest-summary-grid">
        {summaryItems.map(([label, value]) => (
          <div key={label}>
            <span>{label}</span>
            <strong>{value}</strong>
          </div>
        ))}
        <div>
          <span>Schema</span>
          <strong>{contractSchema}</strong>
        </div>
        <div>
          <span>Hardware smoke</span>
          <strong>{hardwareSmokeStatus}</strong>
        </div>
        <div>
          <span>Smoke plan</span>
          <strong>{hardwareSmokePlanModels || 'unavailable'}</strong>
        </div>
        <div>
          <span>Mutation</span>
          <strong>{manifest?.hardware_writes ? 'writes enabled' : 'no hardware writes'}</strong>
        </div>
      </div>

      <div className="mining-pipeline-manifest-section">
        <div className="mining-pipeline-manifest-section-title">Contract Evidence</div>
        <p className="mining-pipeline-manifest-note">
          Detailed schema, blocker, fixture, parser, and freshness fields remain on this card as read-only data-contract attributes for automated audit. They are not displayed as live miner state, readiness evidence, or mining telemetry.
        </p>
      </div>
    </section>
  );
}
