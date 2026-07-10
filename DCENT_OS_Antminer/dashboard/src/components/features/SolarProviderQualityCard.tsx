import React from 'react';
import type { SolarStatus, SolarVerificationSample } from '../../api/feature-types';

export type ProviderQualityMeta = {
  stage: 'live' | 'limited' | 'staged' | 'unsupported';
  providerLiveBackend: boolean;
  maturityLabel: string;
  trustLabel: string;
  trustTone: 'good' | 'warn' | 'neutral';
  trustBoundaryLabel: string;
  trustBoundaryDetail: string;
  failSafeExpectation: string;
  trustCues: string[];
  expectedFields: string[];
  recommendedUse: string;
  offGridCue: string;
  recommendedProvider?: string | null;
  backendScope?: string | null;
  acceptedPayloadShapes: string[];
};

type SolarProviderQualityCardProps = {
  selectedLabel: string;
  statusProvider: string;
  transport: string;
  connectionLabel: string;
  lastUpdateLabel: string;
  status: SolarStatus;
  meta: ProviderQualityMeta;
  history: SolarVerificationSample[];
};

function downloadFile(content: string, filename: string, mimeType: string) {
  const blob = new Blob([content], { type: mimeType });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  URL.revokeObjectURL(url);
}

function formatAge(ms: number | null | undefined): string {
  if (ms == null) return 'n/a';
  if (ms < 1000) return `${ms} ms`;
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remSeconds = seconds % 60;
  if (minutes < 60) return remSeconds > 0 ? `${minutes}m ${remSeconds}s` : `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const remMinutes = minutes % 60;
  return remMinutes > 0 ? `${hours}h ${remMinutes}m` : `${hours}h`;
}

function formatTimestamp(timestampMs: number | null | undefined): string {
  if (!timestampMs) return 'n/a';
  return new Date(timestampMs).toLocaleString();
}

function formatShortTimestamp(timestampMs: number): string {
  return new Date(timestampMs).toLocaleTimeString([], {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

function formatPercent(value: number): string {
  return `${Math.round(value)}%`;
}

function toneColor(tone: ProviderQualityMeta['trustTone']): string {
  if (tone === 'good') return 'var(--feat-green)';
  if (tone === 'warn') return 'var(--yellow)';
  return 'var(--text-dim)';
}

function intersectCount(expected: string[], actual: string[]): number {
  if (expected.length === 0 || actual.length === 0) return 0;
  const normalizedActual = new Set(actual.map(field => field.toLowerCase()));
  return expected.filter(field => {
    const needle = field.toLowerCase();
    return Array.from(normalizedActual).some(actualField => actualField.includes(needle) || needle.includes(actualField));
  }).length;
}

function deriveReadinessLabel(meta: ProviderQualityMeta, status: SolarStatus, matchedFields: string[]): { label: string; tone: string } {
  if (!meta.providerLiveBackend || meta.stage === 'unsupported') {
    return { label: 'Backend not available', tone: 'var(--yellow)' };
  }

  if (meta.stage !== 'live') {
    if (meta.stage === 'limited') {
      return { label: 'Limited live path', tone: 'var(--yellow)' };
    }
    return { label: 'Staged only', tone: 'var(--yellow)' };
  }

  if (!status.connected) {
    return { label: 'Needs successful test', tone: 'var(--yellow)' };
  }

  if (status.stale) {
    return { label: 'Connected but stale', tone: 'var(--yellow)' };
  }

  if ((status.consecutiveFailures ?? 0) > 0) {
    return { label: 'Observe until stable', tone: 'var(--yellow)' };
  }

  const matchedExpected = intersectCount(meta.expectedFields, matchedFields);
  if (meta.expectedFields.length > 0 && matchedExpected < Math.min(3, meta.expectedFields.length)) {
    return { label: 'Partial field coverage', tone: 'var(--yellow)' };
  }

  if (transportIsCloud(status.transport)) {
    return { label: 'Ready for coarse policy', tone: 'var(--text-dim)' };
  }

  return { label: 'Ready for active enforcement', tone: 'var(--feat-green)' };
}

function transportIsCloud(transport: string | undefined): boolean {
  return !!transport && transport.includes('cloud');
}

function detailChip(label: string, value: string, tone?: string) {
  return (
    <div style={{
      background: 'rgba(255,255,255,0.03)',
      border: '1px solid var(--border)',
      borderRadius: 10,
      padding: '10px 12px',
      minWidth: 0,
    }}>
      <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 4 }}>{label}</div>
      <div style={{ fontSize: '0.8rem', color: tone || 'var(--text)', fontWeight: 600, wordBreak: 'break-word' }}>{value}</div>
    </div>
  );
}

export function SolarProviderQualityCard({
  selectedLabel,
  statusProvider,
  transport,
  connectionLabel,
  lastUpdateLabel,
  status,
  meta,
  history,
}: SolarProviderQualityCardProps) {
  const staleTone = status.stale ? 'var(--yellow)' : status.connected ? 'var(--feat-green)' : 'var(--text-dim)';
  const failureTone = (status.consecutiveFailures ?? 0) > 0 ? 'var(--yellow)' : 'var(--feat-green)';
  const matchedFields = status.matched_fields && status.matched_fields.length > 0 ? status.matched_fields : [];
  const matchedExpected = intersectCount(meta.expectedFields, matchedFields);
  const coverageTone = meta.expectedFields.length === 0
    ? 'var(--text-dim)'
    : matchedExpected >= Math.min(3, meta.expectedFields.length)
      ? 'var(--feat-green)'
      : matchedExpected > 0
        ? 'var(--yellow)'
        : 'var(--text-dim)';
  const readiness = deriveReadinessLabel(meta, status, matchedFields);
  const recentHistory = history.slice(-12).reverse();
  const recentWindow = history.slice(-24);
  const recentHealthyCount = recentWindow.filter(entry => entry.connected && !entry.stale).length;
  const recentFailureCount = recentWindow.filter(entry => !entry.connected).length;
  const recentStaleCount = recentWindow.filter(entry => entry.connected && entry.stale).length;
  const passRate = recentWindow.length > 0 ? (recentHealthyCount / recentWindow.length) * 100 : 0;
  const lastSuccessfulEntry = [...history].reverse().find(entry => entry.connected && !entry.stale);
  const lastSuccessfulFields = lastSuccessfulEntry?.matched_fields ?? lastSuccessfulEntry?.matchedFields ?? [];
  const longestFailureStreak = history.reduce((max, entry) => Math.max(max, entry.consecutiveFailures ?? 0), status.consecutiveFailures ?? 0);

  const exportJson = () => {
    downloadFile(
      JSON.stringify({ exportedAt: new Date().toISOString(), provider: statusProvider, entries: history }, null, 2),
      `dcentos-solar-provider-history-${new Date().toISOString().slice(0, 10)}.json`,
      'application/json',
    );
  };

  const exportCsv = () => {
    const rows = [
      'timestamp,provider,transport,connected,stale,sample_age_ms,consecutive_failures,last_success_ms,matched_fields,production_watts,consumption_watts,net_grid_watts,battery_soc_pct,message',
      ...history.map(entry => {
        const message = `"${entry.message.replace(/"/g, '""')}"`;
        const matched = `"${(entry.matched_fields ?? entry.matchedFields ?? []).join('|').replace(/"/g, '""')}"`;
        return [
          new Date(entry.timestampMs).toISOString(),
          entry.provider,
          entry.transport,
          entry.connected ? 'true' : 'false',
          entry.stale ? 'true' : 'false',
          entry.sampleAgeMs ?? '',
          entry.consecutiveFailures,
          entry.lastSuccessMs ? new Date(entry.lastSuccessMs).toISOString() : '',
          matched,
          entry.productionWatts,
          entry.consumptionWatts,
          entry.netGridWatts,
          entry.batterySocPct ?? '',
          message,
        ].join(',');
      }),
    ];

    downloadFile(
      rows.join('\n'),
      `dcentos-solar-provider-history-${new Date().toISOString().slice(0, 10)}.csv`,
      'text/csv;charset=utf-8',
    );
  };

  return (
    <div style={{
      marginTop: 16,
      padding: 14,
      borderRadius: 'var(--radius)',
      background: 'linear-gradient(180deg, rgba(34,197,94,0.05), rgba(255,255,255,0.02))',
      border: '1px solid rgba(34,197,94,0.14)',
    }}>
      <div style={{
        display: 'flex',
        justifyContent: 'space-between',
        alignItems: 'flex-start',
        gap: 12,
        flexWrap: 'wrap',
        marginBottom: 12,
      }}>
        <div>
          <div style={{ fontSize: '0.72rem', color: 'var(--feat-green)', fontWeight: 700, letterSpacing: '0.04em', textTransform: 'uppercase' }}>
            Provider Quality
          </div>
          <div style={{ fontSize: '0.95rem', color: 'var(--text)', fontWeight: 700, marginTop: 4 }}>
            {selectedLabel}
          </div>
          <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginTop: 4 }}>
            Active provider: {statusProvider} · Last sample: {lastUpdateLabel}
          </div>
        </div>
        <div style={{
          padding: '8px 10px',
          borderRadius: 999,
          background: 'rgba(255,255,255,0.04)',
          border: '1px solid var(--border)',
          fontSize: '0.72rem',
          color: toneColor(meta.trustTone),
          fontWeight: 700,
          whiteSpace: 'nowrap',
        }}>
          {meta.maturityLabel}
        </div>
      </div>

      <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginBottom: 12 }}>
        <button type="button" className="feat-btn feat-btn-secondary" onClick={exportCsv} disabled={history.length === 0}>Export CSV</button>
        <button type="button" className="feat-btn feat-btn-secondary" onClick={exportJson} disabled={history.length === 0}>Export JSON</button>
      </div>

        <div style={{
          marginBottom: 12,
          display: 'grid',
        gap: 10,
        gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))',
      }}>
        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Commissioning verdict</div>
          <div style={{ fontSize: '0.86rem', color: readiness.tone, fontWeight: 700 }}>{readiness.label}</div>
          <div style={{ marginTop: 6, fontSize: '0.76rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
            {!meta.providerLiveBackend || meta.stage === 'unsupported'
              ? 'This provider selection is visible in the dashboard, but DCENT_OS does not currently expose a usable live backend for enforcement.'
              : meta.stage === 'live'
              ? 'This provider can be trusted for enforcement only after the rolling history stays healthy on-site.'
              : meta.stage === 'limited'
                ? 'Treat this provider as a constrained live path and confirm its contract carefully before enabling unattended control.'
                : 'Use this provider for staging and wiring only until a live backend exists.'}
          </div>
        </div>

        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Trust boundary</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            <div style={{ color: toneColor(meta.trustTone), fontWeight: 600, marginBottom: 6 }}>{meta.trustBoundaryLabel}</div>
            <div>{meta.trustBoundaryDetail}</div>
          </div>
        </div>

        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Rolling validation window</div>
          <div style={{ fontSize: '0.86rem', color: passRate >= 80 ? 'var(--feat-green)' : 'var(--yellow)', fontWeight: 700 }}>
            {recentWindow.length > 0 ? formatPercent(passRate) : 'n/a'}
          </div>
          <div style={{ marginTop: 6, fontSize: '0.76rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
            {recentHealthyCount} healthy, {recentStaleCount} stale, {recentFailureCount} failed in the last {recentWindow.length || 0} checks.
          </div>
        </div>

        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Failure history</div>
          <div style={{ fontSize: '0.86rem', color: longestFailureStreak > 0 ? 'var(--yellow)' : 'var(--feat-green)', fontWeight: 700 }}>
            longest streak {longestFailureStreak}
          </div>
          <div style={{ marginTop: 6, fontSize: '0.76rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
            Use this to judge whether the endpoint is flapping during real site conditions rather than only during a one-shot test.
          </div>
        </div>
      </div>

      <div style={{
        display: 'grid',
        gap: 10,
        gridTemplateColumns: 'repeat(auto-fit, minmax(150px, 1fr))',
        marginBottom: 12,
      }}>
        {detailChip('Transport', transport)}
        {detailChip('Backend', meta.providerLiveBackend ? 'live' : 'not live', meta.providerLiveBackend ? 'var(--feat-green)' : 'var(--yellow)')}
        {detailChip('Freshness', status.stale ? 'Stale' : 'Fresh', staleTone)}
        {detailChip('Sample age', formatAge(status.sampleAgeMs), staleTone)}
        {detailChip('Field coverage', meta.expectedFields.length > 0 ? `${matchedExpected}/${meta.expectedFields.length}` : 'n/a', coverageTone)}
        {detailChip('Enforcement', readiness.label, readiness.tone)}
        {detailChip('Failure streak', `${status.consecutiveFailures ?? 0}`, failureTone)}
        {detailChip('Last success', formatTimestamp(status.lastSuccessMs))}
        {detailChip('Rolling pass rate', recentWindow.length > 0 ? `${recentHealthyCount}/${recentWindow.length}` : 'n/a', recentHealthyCount === recentWindow.length && recentWindow.length > 0 ? 'var(--feat-green)' : 'var(--yellow)')}
        {detailChip('Connection', connectionLabel, status.connected ? 'var(--feat-green)' : 'var(--text-dim)')}
      </div>

      <div style={{
        display: 'grid',
        gap: 10,
        gridTemplateColumns: 'minmax(0, 1.2fr) minmax(0, 1fr)',
      }}>
        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Matched fields</div>
          <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
            {(status.matched_fields && status.matched_fields.length > 0 ? status.matched_fields : ['none reported']).map(field => (
              <span key={field} style={{
                padding: '4px 8px',
                borderRadius: 999,
                background: field === 'none reported' ? 'rgba(234,179,8,0.08)' : 'rgba(59,130,246,0.12)',
                border: `1px solid ${field === 'none reported' ? 'rgba(234,179,8,0.2)' : 'rgba(59,130,246,0.18)'}`,
                color: field === 'none reported' ? 'var(--yellow)' : 'var(--text)',
                fontSize: '0.72rem',
                fontWeight: 600,
              }}>
                {field}
              </span>
            ))}
          </div>
        </div>

        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Backend scope</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            <div style={{ color: 'var(--text)', fontWeight: 600, marginBottom: 6 }}>
              {meta.backendScope || (meta.providerLiveBackend ? 'Direct provider/backend scope is not explicitly described by the API yet.' : 'No live backend scope is available for this provider.')}
            </div>
            {meta.recommendedProvider && (
              <div>Recommended fallback: {meta.recommendedProvider}</div>
            )}
          </div>
        </div>

        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Accepted payload shapes</div>
          <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
            {(meta.acceptedPayloadShapes.length > 0 ? meta.acceptedPayloadShapes : ['No explicit payload shapes reported']).map(shape => (
              <span key={shape} style={{
                padding: '4px 8px',
                borderRadius: 999,
                background: shape === 'No explicit payload shapes reported' ? 'rgba(255,255,255,0.03)' : 'rgba(59,130,246,0.12)',
                border: `1px solid ${shape === 'No explicit payload shapes reported' ? 'var(--border)' : 'rgba(59,130,246,0.18)'}`,
                color: shape === 'No explicit payload shapes reported' ? 'var(--text-dim)' : 'var(--text)',
                fontSize: '0.72rem',
                fontWeight: 600,
                lineHeight: 1.4,
              }}>
                {shape}
              </span>
            ))}
          </div>
        </div>

        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Best fit</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            <div style={{ color: 'var(--text)', fontWeight: 600, marginBottom: 6 }}>{meta.recommendedUse}</div>
            <div>{meta.offGridCue}</div>
          </div>
        </div>

        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Trust cues</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            <div style={{ color: toneColor(meta.trustTone), fontWeight: 600, marginBottom: 6 }}>{meta.trustLabel}</div>
            {meta.trustCues.map(cue => (
              <div key={cue} style={{ marginTop: 4 }}>{cue}</div>
            ))}
          </div>
        </div>

        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Fail-safe expectation</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            {meta.failSafeExpectation}
          </div>
        </div>

        <div style={{
          background: 'rgba(255,255,255,0.03)',
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: '10px 12px',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Last verified-good fields</div>
          {lastSuccessfulFields.length > 0 ? (
            <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
              {lastSuccessfulFields.map(field => (
                <span key={field} style={{
                  padding: '4px 8px',
                  borderRadius: 999,
                  background: 'rgba(34,197,94,0.08)',
                  border: '1px solid rgba(34,197,94,0.18)',
                  color: 'var(--text)',
                  fontSize: '0.72rem',
                  fontWeight: 600,
                }}>
                  {field}
                </span>
              ))}
            </div>
          ) : (
            <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
              No healthy provider sample has been captured in the current rolling window yet.
            </div>
          )}
        </div>
      </div>

      <div style={{
        marginTop: 10,
        background: 'rgba(255,255,255,0.03)',
        border: '1px solid var(--border)',
        borderRadius: 10,
        padding: '10px 12px',
      }}>
        <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 8 }}>Recent verification strip</div>
        {recentHistory.length > 0 ? (
          <>
            <div style={{ display: 'grid', gridTemplateColumns: `repeat(${recentHistory.length}, minmax(0, 1fr))`, gap: 4 }}>
              {recentHistory.map(entry => {
                const tone = !entry.connected ? 'rgba(239,68,68,0.9)' : entry.stale ? 'rgba(234,179,8,0.9)' : 'rgba(34,197,94,0.9)';
                return (
                  <div
                    key={`${entry.timestampMs}-${entry.consecutiveFailures}-${entry.stale ? 'stale' : 'fresh'}`}
                    title={`${formatTimestamp(entry.timestampMs)} | ${entry.connected ? (entry.stale ? 'stale' : 'ok') : 'failed'} | age ${formatAge(entry.sampleAgeMs)} | streak ${entry.consecutiveFailures}`}
                    style={{ height: 18, borderRadius: 999, background: tone }}
                  />
                );
              })}
            </div>
            <div style={{ marginTop: 8, fontSize: '0.72rem', color: 'var(--text-dim)' }}>
              Newest on the left. Green = usable sample, yellow = stale sample, red = fetch failure.
            </div>
          </>
        ) : (
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)' }}>No provider verification history captured yet.</div>
        )}
      </div>

      <div style={{
        marginTop: 10,
        background: 'rgba(255,255,255,0.03)',
        border: '1px solid var(--border)',
        borderRadius: 10,
        padding: '10px 12px',
      }}>
        <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 8 }}>Recent checks</div>
        {recentHistory.length > 0 ? (
          <div style={{ display: 'grid', gap: 6 }}>
            {recentHistory.slice(0, 6).map(entry => {
              const resultLabel = !entry.connected ? 'failed' : entry.stale ? 'stale' : 'ok';
              const resultTone = !entry.connected ? 'var(--feat-red)' : entry.stale ? 'var(--yellow)' : 'var(--feat-green)';
              const fields = entry.matched_fields ?? entry.matchedFields ?? [];
              return (
                <div key={`${entry.timestampMs}-${resultLabel}`} style={{
                  display: 'grid',
                  gap: 8,
                  gridTemplateColumns: '110px 72px minmax(0, 90px) minmax(0, 1fr)',
                  alignItems: 'start',
                  fontSize: '0.75rem',
                }}>
                  <span style={{ color: 'var(--text)' }}>{formatShortTimestamp(entry.timestampMs)}</span>
                  <span style={{ color: resultTone, fontWeight: 700, textTransform: 'uppercase' }}>{resultLabel}</span>
                  <span style={{ color: 'var(--text-dim)' }}>age {formatAge(entry.sampleAgeMs)}</span>
                  <span style={{ color: 'var(--text-dim)', wordBreak: 'break-word' }}>
                    streak {entry.consecutiveFailures}
                    {fields.length > 0 ? ` · ${fields.join(', ')}` : ''}
                  </span>
                </div>
              );
            })}
          </div>
        ) : (
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)' }}>
            Run a provider test or leave the page open while commissioning to build a rolling quality log.
          </div>
        )}
      </div>

      <div style={{ marginTop: 10, fontSize: '0.72rem', color: 'var(--text-dim)' }}>
        Stage: {meta.stage === 'live' ? 'live backend' : meta.stage === 'limited' ? 'limited live backend' : 'staged path'}
        {status.controlActive != null ? ` · Policy ${status.controlActive ? 'can enforce' : 'is observe-only'}` : ''}
      </div>
    </div>
  );
}
