import React, { useRef } from 'react';
import { OverlayDialog } from './OverlayDialog';
import { useDashboardHealth } from '../../hooks/useDashboardHealth';
import { useSetupReadiness } from '../../hooks/useSetupReadiness';
import { useModeNavigation } from '../../hooks/useModeNavigation';

export function ReadinessCenter({
  open,
  onClose,
  mode,
}: {
  open: boolean;
  onClose: () => void;
  mode: 'heater' | 'standard';
}) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const health = useDashboardHealth();
  const readiness = useSetupReadiness(mode);
  const { taskHandoff, returnFromTaskHandoff } = useModeNavigation();
  const activeHandoff = taskHandoff?.toMode === mode ? taskHandoff : null;
  const returnLabel = taskHandoff?.returnLabel
    ?? (taskHandoff?.fromMode === 'heater' ? 'Back to Heat view' : 'Return');

  return (
    <OverlayDialog
      open={open}
      onClose={onClose}
      ariaLabel="Readiness center"
      initialFocusRef={closeRef as React.RefObject<HTMLElement>}
      maxWidth={720}
      width="94%"
    >
      <div style={{ padding: 24, display: 'grid', gap: 18 }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', gap: 16, alignItems: 'flex-start' }}>
          <div>
            <div className="ds-section-eyebrow" style={{ color: 'var(--amber, #F59E0B)', fontSize: '0.78rem', letterSpacing: '0.08em' }}>
              Readiness Center
            </div>
            <div style={{ marginTop: 6, fontSize: '1.15rem', fontWeight: 700, color: 'var(--text)' }}>
              {readiness.summary}
            </div>
            <div style={{ marginTop: 6, fontSize: '0.85rem', lineHeight: 1.6, color: 'var(--text-secondary)', maxWidth: 560 }}>
              Review remaining setup tasks, current miner blockers, and any active mode handoff in one place.
            </div>
          </div>
          <button
            ref={closeRef}
            onClick={onClose}
            aria-label="Close readiness center"
            style={{
              width: 36,
              height: 36,
              borderRadius: 10,
              border: '1px solid var(--border)',
              background: 'rgba(255,255,255,0.04)',
              color: 'var(--text-dim)',
              cursor: 'pointer',
              fontSize: '1.15rem',
            }}
          >
            &times;
          </button>
        </div>

        <div style={{
          display: 'grid',
          gridTemplateColumns: 'repeat(auto-fit, minmax(160px, 1fr))',
          gap: 10,
        }}>
          <ReadinessMetric
            label="Tasks remaining"
            value={String(readiness.remainingTasks)}
            tone={readiness.remainingTasks > 0 ? 'var(--amber, #F59E0B)' : 'var(--green)'}
          />
          <ReadinessMetric
            label="Device readiness"
            value={readiness.setupStatus?.device_ready ? 'Ready' : 'Pending'}
          />
          <ReadinessMetric
            label="Mining readiness"
            value={readiness.setupStatus?.mining_ready ? 'Ready' : 'Blocked'}
          />
        </div>

        {activeHandoff && (
          <section style={{
            padding: '14px 16px',
            borderRadius: 14,
            border: '1px solid rgba(247,147,26,0.18)',
            background: 'rgba(247,147,26,0.08)',
            display: 'flex',
            justifyContent: 'space-between',
            gap: 12,
            alignItems: 'center',
            flexWrap: 'wrap',
          }}>
            <div style={{ minWidth: 220, flex: 1 }}>
              <div style={{ fontSize: '0.86rem', fontWeight: 700, color: 'var(--text)' }}>
                Active task handoff
              </div>
              <div style={{ marginTop: 4, fontSize: '0.8rem', lineHeight: 1.5, color: 'var(--text-secondary)' }}>
                You opened a readiness task from {activeHandoff.fromMode === 'heater' ? 'Heat view' : 'another dashboard view'}. Return when you are done.
              </div>
            </div>
            <button
              onClick={() => { onClose(); void returnFromTaskHandoff(); }}
              className="btn btn-secondary"
            >
              {returnLabel}
            </button>
          </section>
        )}

        <section style={{ display: 'grid', gap: 10 }}>
          <div style={{ fontSize: '0.78rem', fontWeight: 700, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--text-dim)' }}>
            Live Health Issues
          </div>
          {health.issues.length > 0 ? health.issues.map(issue => (
            <div
              key={issue.key}
              style={{
                padding: '12px 14px',
                borderRadius: 12,
                border: `1px solid ${issue.level === 'critical' ? 'rgba(239,68,68,0.25)' : issue.level === 'warning' ? 'rgba(234,179,8,0.22)' : 'rgba(96,165,250,0.22)'}`,
                background: issue.level === 'critical' ? 'rgba(239,68,68,0.08)' : issue.level === 'warning' ? 'rgba(234,179,8,0.08)' : 'rgba(96,165,250,0.08)',
                color: 'var(--text-secondary)',
                fontSize: '0.82rem',
                lineHeight: 1.5,
              }}
            >
              {issue.message}
            </div>
          )) : (
            <div style={{
              padding: '12px 14px',
              borderRadius: 12,
              border: '1px solid rgba(45,212,160,0.18)',
              background: 'rgba(45,212,160,0.08)',
              color: 'var(--text-secondary)',
              fontSize: '0.82rem',
              lineHeight: 1.5,
            }}>
              No live health issues are currently blocking the miner telemetry path.
            </div>
          )}
        </section>

        <section style={{ display: 'grid', gap: 10 }}>
          <div style={{ fontSize: '0.78rem', fontWeight: 700, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--text-dim)' }}>
            Setup Tasks
          </div>
          {readiness.tasks.length > 0 ? readiness.tasks.map(task => (
            <div key={task.id} style={{
              display: 'flex',
              justifyContent: 'space-between',
              gap: 12,
              flexWrap: 'wrap',
              alignItems: 'center',
              padding: '14px 16px',
              borderRadius: 14,
              border: '1px solid rgba(255,255,255,0.08)',
              background: 'rgba(255,255,255,0.03)',
            }}>
              <div style={{ minWidth: 220, flex: 1 }}>
                <div style={{ fontSize: '0.9rem', fontWeight: 700, color: 'var(--text)' }}>{task.label}</div>
                <div style={{ marginTop: 5, fontSize: '0.8rem', color: 'var(--text-secondary)', lineHeight: 1.55 }}>
                  {task.detail}
                </div>
              </div>
              <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', justifyContent: 'flex-end' }}>
                <button
                  type="button"
                  className="ds-btn"
                  onClick={() => { onClose(); task.onAction(); }}
                  aria-label={`${task.actionLabel}: ${task.label}`}
                >
                  {task.actionLabel}
                </button>
                <button
                  type="button"
                  className="ds-btn ghost sm"
                  onClick={() => readiness.dismissTask(task.id)}
                  aria-label={`Dismiss ${task.label}`}
                >
                  Dismiss
                </button>
              </div>
            </div>
          )) : (
            <div style={{
              padding: '12px 14px',
              borderRadius: 12,
              border: '1px solid var(--border)',
              background: 'rgba(255,255,255,0.03)',
              color: 'var(--text-secondary)',
              fontSize: '0.82rem',
              lineHeight: 1.5,
            }}>
              {readiness.setupComplete
                ? 'No guided setup tasks are currently queued. If mining is still blocked, check the live health issues above.'
                : 'Setup is still in progress, so guided readiness tasks stay locked until setup is complete.'}
            </div>
          )}
        </section>
      </div>
    </OverlayDialog>
  );
}

function ReadinessMetric({
  label,
  value,
  tone = 'var(--text)',
}: {
  label: string;
  value: string;
  tone?: string;
}) {
  return (
    <div style={{
      padding: '12px 14px',
      borderRadius: 14,
      border: '1px solid rgba(255,255,255,0.08)',
      background: 'rgba(255,255,255,0.03)',
    }}>
      <div style={{ fontSize: '0.74rem', fontWeight: 700, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--text-dim)' }}>
        {label}
      </div>
      <div style={{ marginTop: 6, fontSize: '1rem', fontWeight: 700, color: tone }}>
        {value}
      </div>
    </div>
  );
}
