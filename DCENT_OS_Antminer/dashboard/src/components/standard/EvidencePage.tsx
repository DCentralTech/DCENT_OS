import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type {
  AuditHistoryRecord,
  CgminerCatalogResponse,
  ChainDiagnosticsResponse,
  DiagnosticsFailureModesResponse,
  HardwarePicInfoResponse,
  HistoryAuditResponse,
  LocalRejectsResponse,
  PsuCatalogResponse,
  ReCatalogIndexResponse,
  RecoveryActionsResponse,
  SystemBootTimelineResponse,
} from '../../api/types';
import { useMinerStore } from '../../store/miner';
import { isRtcSyncedMs } from '../../utils/format';
import { StatePanel } from '../common/StatePanel';
import { EmptyState } from '../common/EmptyState';
import { NoLogsIllustration } from '../common/illustrations';
import { PageSkeleton, SectionSkeleton } from '../common/skeletons';

type LoadResult<T> = {
  ok: boolean;
  data: T | null;
  error: string | null;
};

type EvidencePayload = {
  failureModes: LoadResult<DiagnosticsFailureModesResponse>;
  chainDiagnostics: LoadResult<ChainDiagnosticsResponse[]>;
  localRejects: LoadResult<LocalRejectsResponse>;
  bootTimeline: LoadResult<SystemBootTimelineResponse>;
  picInfo: LoadResult<HardwarePicInfoResponse>;
  psuCatalog: LoadResult<PsuCatalogResponse>;
  cgminerCatalog: LoadResult<CgminerCatalogResponse>;
  recoveryActions: LoadResult<RecoveryActionsResponse>;
  auditHistory: LoadResult<HistoryAuditResponse>;
  reCatalog: LoadResult<ReCatalogIndexResponse>;
};

type ProvenanceTone = 'live' | 'static' | 'mixed' | 'empty' | 'stale' | 'unavailable';

const EMPTY_RESULT: LoadResult<never> = { ok: false, data: null, error: null };

function emptyPayload(): EvidencePayload {
  return {
    failureModes: EMPTY_RESULT,
    chainDiagnostics: EMPTY_RESULT,
    localRejects: EMPTY_RESULT,
    bootTimeline: EMPTY_RESULT,
    picInfo: EMPTY_RESULT,
    psuCatalog: EMPTY_RESULT,
    cgminerCatalog: EMPTY_RESULT,
    recoveryActions: EMPTY_RESULT,
    auditHistory: EMPTY_RESULT,
    reCatalog: EMPTY_RESULT,
  };
}

async function capture<T>(promise: Promise<T>): Promise<LoadResult<T>> {
  try {
    return { ok: true, data: await promise, error: null };
  } catch (error) {
    return {
      ok: false,
      data: null,
      error: error instanceof Error ? error.message : 'Endpoint unavailable',
    };
  }
}

function formatTimeMs(ms?: number | null) {
  if (typeof ms !== 'number' || !Number.isFinite(ms) || ms <= 0) {
    return 'not reported';
  }
  // Pre-2020 epoch = daemon had no synced clock (no RTC) when this row was
  // written. Don't render it as a real 1970 wall-clock date.
  if (!isRtcSyncedMs(ms)) {
    return 'before clock sync (no RTC)';
  }
  const date = new Date(ms);
  return Number.isNaN(date.getTime()) ? 'not reported' : date.toLocaleString();
}

function formatSeconds(seconds?: number | null) {
  if (typeof seconds !== 'number' || !Number.isFinite(seconds)) {
    return 'not reported';
  }
  return `${seconds.toFixed(seconds >= 10 ? 0 : 1)}s`;
}

function formatEvent(record: AuditHistoryRecord) {
  const kind = record.event?.event || 'unknown_event';
  const detail = Object.entries(record.event || {})
    .filter(([key]) => key !== 'event')
    .slice(0, 3)
    .map(([key, value]) => `${key}=${String(value)}`)
    .join(' | ');
  return detail ? `${kind} | ${detail}` : kind;
}

function isAuditStale(history: HistoryAuditResponse | null) {
  const newest = history?.events?.reduce((max, event) => Math.max(max, event.timestamp_ms || 0), 0) ?? 0;
  if (newest <= 0) return false;
  return Date.now() - newest > 24 * 60 * 60 * 1000;
}

function ProvenancePill({ tone, children }: { tone: ProvenanceTone; children: React.ReactNode }) {
  return <span className={`evidence-provenance evidence-provenance-${tone}`}>{children}</span>;
}

function EndpointState<T>({
  result,
  children,
  empty,
}: {
  result: LoadResult<T>;
  children: (data: T) => React.ReactNode;
  empty?: React.ReactNode;
}) {
  if (!result.ok) {
    return (
      <StatePanel
        title="Endpoint unavailable"
        message={result.error || 'The API route did not return data. No fallback rows are synthesized.'}
        tone="warning"
        compact
      />
    );
  }

  if (!result.data) {
    return (
      <EmptyState
        illustration={<NoLogsIllustration />}
        title="No evidence rows yet"
        hint="Verifier hasn't observed anything to record."
      />
    );
  }

  return <>{children(result.data) || empty}</>;
}

function Surface({
  title,
  copy,
  pills,
  children,
  testId,
}: {
  title: string;
  copy: string;
  pills: React.ReactNode;
  children: React.ReactNode;
  testId?: string;
}) {
  return (
    <section className="page-surface evidence-surface" data-testid={testId}>
      <div className="page-surface-header">
        <div>
          <div className="page-surface-title">{title}</div>
          <div className="page-surface-copy">{copy}</div>
        </div>
        <div className="evidence-pill-row">{pills}</div>
      </div>
      {children}
    </section>
  );
}

function EvidenceTable({
  columns,
  rows,
}: {
  columns: string[];
  rows: React.ReactNode[][];
}) {
  if (rows.length === 0) {
    return (
      <div className="evidence-empty">
        No rows reported by this endpoint. No alternate proof is inferred.
      </div>
    );
  }

  return (
    <div className="evidence-table-wrap">
      <table className="evidence-table">
        <thead>
          <tr>
            {columns.map(column => <th key={column}>{column}</th>)}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, rowIndex) => (
            <tr key={rowIndex}>
              {row.map((cell, cellIndex) => <td key={cellIndex}>{cell}</td>)}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function EvidencePage() {
  const status = useMinerStore(s => s.status);
  const chainIds = useMemo(() => {
    const ids = status?.chains?.map(chain => chain.id).filter(id => Number.isFinite(id)) ?? [];
    return Array.from(new Set(ids)).slice(0, 4);
  }, [status?.chains]);
  const chainKey = chainIds.join(',');
  const [payload, setPayload] = useState<EvidencePayload>(() => emptyPayload());
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    setLoading(true);
    const chainPromise = chainIds.length > 0
      ? Promise.all(chainIds.map(id => api.getChainDiagnostics(id)))
      : Promise.resolve([]);

    const [
      failureModes,
      chainDiagnostics,
      localRejects,
      bootTimeline,
      picInfo,
      psuCatalog,
      cgminerCatalog,
      recoveryActions,
      auditHistory,
      reCatalog,
    ] = await Promise.all([
      capture(api.getDiagnosticFailureModes()),
      capture(chainPromise),
      capture(api.getLocalRejects(12)),
      capture(api.getSystemBootTimeline()),
      capture(api.getHardwarePicInfo()),
      capture(api.getPsuCatalog()),
      capture(api.getCgminerCatalog()),
      capture(api.getRecoveryActions()),
      capture(api.getHistoryAudit(16)),
      capture(api.getReCatalogIndex()),
    ]);

    setPayload({
      failureModes,
      chainDiagnostics,
      localRejects,
      bootTimeline,
      picInfo,
      psuCatalog,
      cgminerCatalog,
      recoveryActions,
      auditHistory,
      reCatalog,
    });
    setLoading(false);
  }, [chainKey]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // First load: nothing has come back from any of the 10 evidence routes yet.
  // Show the canonical page skeleton instead of a hero strip full of zeros.
  const anyResultLanded = payload.auditHistory.ok || payload.failureModes.ok
    || payload.bootTimeline.ok || payload.localRejects.ok
    || payload.psuCatalog.ok || payload.cgminerCatalog.ok
    || payload.recoveryActions.ok || payload.reCatalog.ok
    || payload.picInfo.ok || (payload.chainDiagnostics.data?.length ?? 0) > 0;
  if (loading && !anyResultLanded) {
    return <PageSkeleton data-testid="page-skeleton-evidence" />;
  }

  const auditTone: ProvenanceTone = payload.auditHistory.ok && payload.auditHistory.data
    ? payload.auditHistory.data.events.length === 0
      ? 'empty'
      : isAuditStale(payload.auditHistory.data) ? 'stale' : 'live'
    : 'unavailable';
  const bootObservedCount = payload.bootTimeline.data?.observed?.length ?? 0;
  const localRejectCount = payload.localRejects.data?.rejects?.length ?? 0;
  const chainDiagnosticsCount = payload.chainDiagnostics.data?.length ?? 0;

  const auditEventCount = payload.auditHistory.data?.events.length ?? 0;
  const catalogCount = (payload.failureModes.data?.count ?? 0)
    + (payload.psuCatalog.data?.count ?? 0)
    + (payload.cgminerCatalog.data?.count ?? 0)
    + (payload.recoveryActions.data?.actions?.length ?? 0);
  const signedArtifacts = (payload.psuCatalog.data?.count ?? 0)
    + (payload.cgminerCatalog.data?.count ?? 0);
  const newestAuditMs = payload.auditHistory.data?.events?.reduce(
    (max, evt) => Math.max(max, evt.timestamp_ms || 0), 0) ?? 0;
  const heroBadgeTone: 'good' | 'warn' = auditTone === 'live' ? 'good' : 'warn';
  const heroBadgeLabel = auditTone === 'live' ? 'live audit'
    : auditTone === 'stale' ? 'stale audit'
    : auditTone === 'empty' ? 'audit empty'
    : 'audit unavailable';

  return (
    <div className="page-content evidence-page">
      <div className="page-hero-strip">
        <div className="page-hero-summary">
          <div className="page-hero-eyebrow">PROVENANCE</div>
          <div className="page-hero-title">Proof And Catalog Evidence</div>
          <div className="page-hero-stat">{auditEventCount.toLocaleString()}</div>
          <div className="page-hero-substat">
            {newestAuditMs > 0
              ? `Newest audit row: ${formatTimeMs(newestAuditMs)}`
              : 'No audit rows reported.'}
          </div>
        </div>
        <div className="hero-kpi-strip">
          <div className="hero-kpi">
            <div className="kpi-label">Catalog rows</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{catalogCount.toLocaleString()}</span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Signed artifacts</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{signedArtifacts.toLocaleString()}</span>
            </div>
            <div className="kpi-sub">PSU + cgminer</div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Verifier status</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {payload.reCatalog.ok ? 'available' : 'unavailable'}
              </span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Last audit</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {newestAuditMs > 0 && isRtcSyncedMs(newestAuditMs)
                  ? new Date(newestAuditMs).toLocaleTimeString('en-US', { hour12: false })
                  : '—'}
              </span>
            </div>
          </div>
        </div>
      </div>

      <section className="section">
      <div className="page-toolbar" style={{ marginBottom: 12 }}>
        <div>
          <div className="section-title" style={{ margin: 0 }}>
            Proof And Catalog Evidence
            <span className={`small-tag ${heroBadgeTone}`}>{heroBadgeLabel}</span>
          </div>
          <div className="evidence-page-lede">
            Catalogs are static reference data. Audit, chain, local reject, and observed boot rows are only treated as evidence when their API routes return explicit rows.
          </div>
        </div>
        <button className="btn btn-secondary btn-compact" type="button" onClick={() => { void refresh(); }} disabled={loading}>
          {loading ? 'Refreshing...' : 'Refresh'}
        </button>
      </div>

      <StatePanel
        title="Honesty boundary"
        message="This page does not claim mining, boot, rollback, voltage, or recovery proof from hashrate counters, catalog entries, or unavailable endpoints."
        tone="info"
        compact
      />

      <div className="metric-grid-auto evidence-summary-grid" data-testid="evidence-provenance-summary">
        <div className="metric-card centered">
          <div className="metric-card-title">Static catalogs</div>
          <div className="metric-card-value mono accent">
            {(payload.failureModes.data?.count ?? 0)
              + (payload.psuCatalog.data?.count ?? 0)
              + (payload.cgminerCatalog.data?.count ?? 0)}
          </div>
          <div className="metric-card-note">Reference rows, not live proof</div>
        </div>
        <div className="metric-card centered">
          <div className="metric-card-title">Observed boot rows</div>
          <div className={`metric-card-value mono ${bootObservedCount > 0 ? 'green' : 'yellow'}`}>
            {payload.bootTimeline.ok ? bootObservedCount : '---'}
          </div>
          <div className="metric-card-note">Historical rows from boot_timeline</div>
        </div>
        <div className="metric-card centered">
          <div className="metric-card-title">Local rejects</div>
          <div className={`metric-card-value mono ${localRejectCount > 0 ? 'yellow' : 'green'}`}>
            {payload.localRejects.ok ? localRejectCount : '---'}
          </div>
          <div className="metric-card-note">Rows from local reject ring</div>
        </div>
        <div className="metric-card centered">
          <div className="metric-card-title">Audit surface</div>
          <div className={`metric-card-value mono ${auditTone === 'live' ? 'green' : auditTone === 'unavailable' ? 'red' : 'yellow'}`}>
            {payload.auditHistory.ok ? payload.auditHistory.data?.events.length ?? 0 : '---'}
          </div>
          <div className="metric-card-note">{auditTone === 'stale' ? 'Newest event is over 24h old' : 'Bounded history ring'}</div>
        </div>
      </div>

      <div className="evidence-grid">
        <Surface
          title="Audit History"
          copy="Operator and system action trail from /api/history/audit. Empty or stale history is not action proof."
          testId="evidence-audit-surface"
          pills={[
            <ProvenancePill key="audit" tone={auditTone}>
              {auditTone === 'live' ? 'live rows' : auditTone === 'stale' ? 'stale rows' : auditTone === 'empty' ? 'live empty' : 'unavailable'}
            </ProvenancePill>,
          ]}
        >
          <EndpointState result={payload.auditHistory}>
            {history => (
              <EvidenceTable
                columns={['Time', 'Actor', 'Event']}
                rows={history.events.slice(0, 8).map(record => [
                  <span className="mono">{formatTimeMs(record.timestamp_ms)}</span>,
                  record.actor || 'unknown',
                  <span className="mono evidence-wrap">{formatEvent(record)}</span>,
                ])}
              />
            )}
          </EndpointState>
        </Surface>

        <Surface
          title="Boot Timeline"
          copy="Canonical phase list is static. Observed rows are historical API evidence only when present."
          testId="evidence-boot-surface"
          pills={[
            <ProvenancePill key="catalog" tone="static">canonical catalog</ProvenancePill>,
            <ProvenancePill key="observed" tone={bootObservedCount > 0 ? 'live' : payload.bootTimeline.ok ? 'empty' : 'unavailable'}>
              {bootObservedCount > 0 ? 'observed rows' : payload.bootTimeline.ok ? 'no observed proof' : 'unavailable'}
            </ProvenancePill>,
          ]}
        >
          <EndpointState result={payload.bootTimeline}>
            {timeline => (
              <div className="surface-stack">
                <EvidenceTable
                  columns={['Canonical phase', 'At', 'Description']}
                  rows={timeline.canonical.slice(0, 8).map(phase => [
                    phase.phase,
                    <span className="mono">{formatSeconds(phase.at_seconds)}</span>,
                    phase.description,
                  ])}
                />
                {timeline.observed.length > 0 ? (
                  <EvidenceTable
                    columns={['Observed phase', 'Time']}
                    rows={timeline.observed.slice(0, 8).map(phase => [
                      phase.phase,
                      <span className="mono">{formatTimeMs(phase.at_unix_ms)}</span>,
                    ])}
                  />
                ) : (
                  <div className="evidence-empty">
                    No observed boot rows returned. Canonical phases do not prove boot completion or rollback commitment.
                  </div>
                )}
              </div>
            )}
          </EndpointState>
        </Surface>

        <Surface
          title="Diagnostics"
          copy="Failure modes are catalog guidance. Chain observations and local reject rows are only shown from their read-only diagnostic endpoints."
          testId="evidence-diagnostics-surface"
          pills={[
            <ProvenancePill key="failure" tone={payload.failureModes.ok ? 'static' : 'unavailable'}>failure catalog</ProvenancePill>,
            <ProvenancePill key="chain" tone={chainDiagnosticsCount > 0 ? 'live' : chainIds.length === 0 ? 'empty' : payload.chainDiagnostics.ok ? 'empty' : 'unavailable'}>chain evidence</ProvenancePill>,
            <ProvenancePill key="rejects" tone={localRejectCount > 0 ? 'live' : payload.localRejects.ok ? 'empty' : 'unavailable'}>local rejects</ProvenancePill>,
          ]}
        >
          <div className="surface-stack">
            <EndpointState result={payload.failureModes}>
              {failureModes => (
                <EvidenceTable
                  columns={['Mode', 'Severity', 'Recovery guidance']}
                  rows={failureModes.modes.slice(0, 8).map(mode => [
                    <span className="mono">{mode.mode}</span>,
                    mode.severity,
                    mode.recovery,
                  ])}
                />
              )}
            </EndpointState>
            {chainIds.length === 0 ? (
              <div className="evidence-empty">
                No chain diagnostics requested because /api/status did not expose chain IDs.
              </div>
            ) : (
              <EndpointState result={payload.chainDiagnostics}>
                {chains => (
                  <EvidenceTable
                    columns={['Chain', 'Chips', 'Nonces returning', 'Verdict', 'Repair action']}
                    rows={chains.map(chain => [
                      <span className="mono">{chain.id}</span>,
                      `${chain.observation.chips_detected}/${chain.observation.chips_expected}`,
                      chain.observation.nonces_returning ? 'yes' : 'no',
                      chain.verdict,
                      chain.repair_action,
                    ])}
                  />
                )}
              </EndpointState>
            )}
            <EndpointState result={payload.localRejects}>
              {rejects => (
                <EvidenceTable
                  columns={['Seq', 'Chain', 'Chip', 'Work', 'Reason']}
                  rows={rejects.rejects.slice(0, 8).map(reject => [
                    <span className="mono">{reject.seq}</span>,
                    <span className="mono">{reject.chain_id}</span>,
                    <span className="mono">{reject.chip_index}</span>,
                    <span className="mono">{reject.work_id}</span>,
                    reject.reason,
                  ])}
                />
              )}
            </EndpointState>
          </div>
        </Surface>

        <Surface
          title="Hardware Catalogs"
          copy="PIC, PSU, CGMiner, and RE catalog routes are read-only reference surfaces unless the API returns separate live fields."
          testId="evidence-catalog-surface"
          pills={[
            <ProvenancePill key="pic" tone={payload.picInfo.ok ? 'static' : 'unavailable'}>PIC catalog</ProvenancePill>,
            <ProvenancePill key="psu" tone={payload.psuCatalog.ok ? 'static' : 'unavailable'}>PSU catalog</ProvenancePill>,
            <ProvenancePill key="cg" tone={payload.cgminerCatalog.ok ? 'static' : 'unavailable'}>CGMiner catalog</ProvenancePill>,
          ]}
        >
          <div className="surface-stack">
            <EndpointState result={payload.picInfo}>
              {pic => (
                <EvidenceTable
                  columns={['PIC FW', 'Architecture', 'Voltage trusted', 'Wire form']}
                  rows={pic.variants.slice(0, 8).map(variant => [
                    <span className="mono">{variant.fw_byte}</span>,
                    variant.architecture,
                    variant.voltage_trusted ? 'yes' : 'no',
                    variant.wire_form,
                  ])}
                />
              )}
            </EndpointState>
            <EndpointState result={payload.psuCatalog}>
              {catalog => (
                <EvidenceTable
                  columns={['PSU', 'Voltage range', '110V W', 'Feedback']}
                  rows={catalog.models.slice(0, 8).map(model => [
                    model.label || model.model,
                    `${model.voltage_min_v}-${model.voltage_max_v} V`,
                    model.max_wattage_110v_w == null ? 'not rated' : `${model.max_wattage_110v_w} W`,
                    model.has_voltage_feedback ? 'yes' : 'no',
                  ])}
                />
              )}
            </EndpointState>
            <EndpointState result={payload.cgminerCatalog}>
              {catalog => (
                <EvidenceTable
                  columns={['CGMiner command', 'Kind', 'Destructive', 'Doc']}
                  rows={catalog.commands.slice(0, 10).map(command => [
                    <span className="mono">{command.name}</span>,
                    command.kind,
                    command.destructive ? 'yes' : 'no',
                    command.doc,
                  ])}
                />
              )}
            </EndpointState>
            <EndpointState result={payload.reCatalog}>
              {catalog => (
                <EvidenceTable
                  columns={['RE catalog', 'Path', 'Description']}
                  rows={catalog.catalogs.slice(0, 8).map(entry => [
                    entry.name,
                    <span className="mono evidence-wrap">{entry.path}</span>,
                    entry.description,
                  ])}
                />
              )}
            </EndpointState>
          </div>
        </Surface>

        <Surface
          title="Recovery Guidance"
          copy="Recovery actions are catalog entries. A listed action is not proof that recovery has run or succeeded."
          testId="evidence-recovery-surface"
          pills={[
            <ProvenancePill key="recovery" tone={payload.recoveryActions.ok ? 'static' : 'unavailable'}>catalog guidance</ProvenancePill>,
          ]}
        >
          <EndpointState result={payload.recoveryActions}>
            {recovery => (
              <div className="surface-stack">
                <EvidenceTable
                  columns={['Action', 'Destructive']}
                  rows={recovery.actions.map(action => [
                    <span className="mono">{action.action}</span>,
                    action.is_destructive ? 'yes' : 'no',
                  ])}
                />
                <div className="evidence-note">
                  {recovery.note || 'No recovery note returned.'}
                </div>
                {recovery.uninstall_steps.length > 0 && (
                  <div className="evidence-note">
                    Uninstall guidance rows: {recovery.uninstall_steps.length}. These are not execution proof.
                  </div>
                )}
              </div>
            )}
          </EndpointState>
        </Surface>
      </div>
      </section>
    </div>
  );
}
