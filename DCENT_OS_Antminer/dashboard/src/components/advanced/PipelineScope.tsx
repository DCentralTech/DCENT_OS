import React, { useMemo } from 'react';
import { useProtocolTrace } from '../../hooks/useProtocolTrace';

function ageLabel(timestamp: number | null) {
  if (!timestamp) {
    return 'never';
  }

  const ageMs = Date.now() - timestamp;
  if (ageMs < 1000) return 'now';
  if (ageMs < 60000) return `${Math.round(ageMs / 1000)}s ago`;
  return `${Math.round(ageMs / 60000)}m ago`;
}

function freshnessTone(timestamp: number | null, freshMs: number, warmMs: number) {
  if (!timestamp) return 'neutral';
  const age = Date.now() - timestamp;
  if (age <= freshMs) return 'success';
  if (age <= warmMs) return 'warning';
  return 'danger';
}

function formatLiveWallPower(watts: number | null) {
  return watts != null ? `${watts.toFixed(0)} W` : 'Unavailable';
}

export function PipelineScope() {
  const { snapshot, events } = useProtocolTrace();

  const recentInteresting = useMemo(
    () => events.filter(event => ['job', 'dispatch', 'nonce', 'share'].includes(event.lane)).slice(-8).reverse(),
    [events],
  );

  const stages = [
    {
      key: 'pool',
      title: 'Pool',
      tone: snapshot.poolStatus.toLowerCase().includes('alive') || snapshot.poolStatus.toLowerCase().includes('donating') ? 'success' : 'warning',
      detail: `${snapshot.poolStatus} · ${snapshot.protocolVersion}`,
      meta: `${(snapshot.hashrateGhs / 1000).toFixed(1)} TH/s incoming work pressure`,
    },
    {
      key: 'job',
      title: 'Job Intake',
      tone: freshnessTone(snapshot.lastJobAt, 12000, 45000),
      detail: snapshot.currentJobId ? `Job ${snapshot.currentJobId}` : 'No current job',
      meta: `Last job ${ageLabel(snapshot.lastJobAt)}`,
    },
    {
      key: 'dispatch',
      title: 'Dispatch',
      tone: freshnessTone(snapshot.lastDispatchAt, 4000, 12000),
      detail: `${snapshot.latestDispatchCount} work items in latest burst`,
      meta: `Last dispatch ${ageLabel(snapshot.lastDispatchAt)}`,
    },
    {
      key: 'nonce',
      title: 'Nonce Flow',
      tone: freshnessTone(snapshot.lastNonceAt, 4000, 12000),
      detail: `${snapshot.latestNonceCount} nonces in latest burst`,
      meta: `Last nonce burst ${ageLabel(snapshot.lastNonceAt)}`,
    },
    {
      key: 'share',
      title: 'Share Verdict',
      tone: snapshot.lastShareResult === 'rejected' ? 'danger' : snapshot.lastShareResult ? 'success' : 'neutral',
      detail: snapshot.lastShareResult ? `Last share ${snapshot.lastShareResult}` : 'No share verdict yet',
      meta: `Accepted ${snapshot.acceptedCount} · Rejected ${snapshot.rejectedCount} · Lucky ${snapshot.luckyCount}`,
    },
  ] as const;

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// pipeline scope</div>
          <h2 className="hacker-inspector-title">Mining Pipeline Stages</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className="hacker-inspector-status">{snapshot.sv2Connected ? 'SV2' : 'SV1'} · {snapshot.activeChains} CHAIN{snapshot.activeChains === 1 ? '' : 'S'}</span>
        </div>
      </header>

      <div className="hacker-inspector-body">
      <div className="ps-stage-grid">
        {stages.map(stage => (
          <div key={stage.key} className="glass-card ds-card-hover ps-stage-card">
            <div className="ps-stage-head">
              <span className="ps-stage-title">{stage.title}</span>
              <span className={`hacker-status-chip ${stage.tone}`}>{stage.tone}</span>
            </div>
            <div className="ps-stage-detail">{stage.detail}</div>
            <div className="ps-stage-meta">{stage.meta}</div>
          </div>
        ))}
      </div>

      <div className="advanced-split-layout">
        <section className="register-inspector ds-card-hover">
          <div className="adv-section-eyebrow">
            Throughput
          </div>
          <div className="adv-kv-stack is-gap-10">
            {[
              { label: 'Hashrate', value: `${(snapshot.hashrateGhs / 1000).toFixed(2)} TH/s` },
              { label: 'Wall power', value: formatLiveWallPower(snapshot.wallWatts) },
              { label: 'Latest dispatch burst', value: `${snapshot.latestDispatchCount} items` },
              { label: 'Latest nonce burst', value: `${snapshot.latestNonceCount} nonces` },
            ].map(metric => (
              <div key={metric.label} className="glass-card ps-metric-row">
                <span className="adv-kv-k">{metric.label}</span>
                <span className="adv-kv-v">{metric.value}</span>
              </div>
            ))}
          </div>
        </section>

        <section className="register-inspector ds-card-hover">
          <div className="adv-section-eyebrow">
            Recent Flow Events
          </div>
          <div className="console-output ps-flow">
            {recentInteresting.length === 0 ? (
              <div className="adv-state is-inline">No live flow events captured yet.</div>
            ) : recentInteresting.map(event => (
              <div key={event.id} className="ps-event">
                <div className="ps-event-title">{event.title}</div>
                <div className="ps-event-detail">{event.detail}</div>
              </div>
            ))}
          </div>
        </section>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{(snapshot.hashrateGhs / 1000).toFixed(2)} TH/s</span>
          <span>{snapshot.acceptedCount} accepted</span>
          <span>{snapshot.rejectedCount} rejected</span>
        </div>
      </footer>
    </div>
  );
}
