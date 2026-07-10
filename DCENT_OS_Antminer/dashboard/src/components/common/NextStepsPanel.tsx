import React from 'react';
import { useDashboardHealth } from '../../hooks/useDashboardHealth';
import { useSetupReadiness } from '../../hooks/useSetupReadiness';

export function NextStepsPanel({ mode }: { mode: 'heater' | 'standard' }) {
  const health = useDashboardHealth();
  const { setupComplete, setupStatus, tasks: steps, summary, dismissTask } = useSetupReadiness(mode);

  if (!setupComplete || !setupStatus || setupStatus.mining_ready || steps.length === 0) {
    return null;
  }

  return (
    <div className="cp-nextsteps">
      <div className="cp-nextsteps-head">
        <div>
          <div className="ds-section-eyebrow" style={{ color: 'var(--amber, #F59E0B)' }}>
            Next Steps
          </div>
          <div className="cp-nextsteps-summary">{summary}</div>
          <div className="cp-nextsteps-sub">
            The dashboard is ready, but DCENT_OS still sees setup work left before the miner is fully mining-ready.
          </div>
        </div>
        <div className="cp-nextsteps-count">
          {steps.length} task{steps.length === 1 ? '' : 's'} remaining
        </div>
      </div>

      {health.issues.length > 0 && (
        <div className="cp-nextsteps-blockers">
          <div className="cp-nextsteps-blockers-label">
            Live blockers
          </div>
          {health.issues.slice(0, 2).map(issue => (
            <div
              key={issue.key}
              className="cp-nextsteps-blocker"
              data-level={issue.level === 'critical' ? 'critical' : undefined}
            >
              {issue.message}
            </div>
          ))}
        </div>
      )}

      <div className="cp-nextsteps-list">
        {steps.map(step => (
          <div key={step.id} className="cp-nextsteps-item">
            <div className="cp-nextsteps-item-copy">
              <div className="cp-nextsteps-item-label">{step.label}</div>
              <div className="cp-nextsteps-item-detail">
                {step.detail}
              </div>
            </div>
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', justifyContent: 'flex-end' }}>
              <button
                type="button"
                className="ds-btn"
                onClick={step.onAction}
                aria-label={`${step.actionLabel}: ${step.label}`}
              >
                {step.actionLabel}
              </button>
              <button
                type="button"
                className="ds-btn ghost sm"
                onClick={() => dismissTask(step.id)}
                aria-label={`Dismiss ${step.label}`}
              >
                Dismiss
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
