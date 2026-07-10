import React from 'react';
import { useModeNavigation } from '../../hooks/useModeNavigation';

export function TaskHandoffBanner({
  expectedMode,
  title,
  copy,
}: {
  expectedMode: 'standard' | 'heater' | 'hacker';
  title: string;
  copy: string;
}) {
  const { taskHandoff, returnFromTaskHandoff } = useModeNavigation();

  if (!taskHandoff || taskHandoff.toMode !== expectedMode) {
    return null;
  }

  const returnLabel = taskHandoff.returnLabel
    ?? (taskHandoff.fromMode === 'heater' ? 'Back to Heat view' : 'Return');

  return (
    <div style={{
      marginBottom: 16,
      padding: '12px 14px',
      borderRadius: 12,
      border: '1px solid rgba(247,147,26,0.18)',
      background: 'rgba(247,147,26,0.08)',
      display: 'flex',
      justifyContent: 'space-between',
      gap: 12,
      alignItems: 'center',
      flexWrap: 'wrap',
    }}>
      <div style={{ minWidth: 220, flex: 1 }}>
        <div style={{ fontSize: '0.84rem', fontWeight: 700, color: 'var(--text)' }}>{title}</div>
        <div style={{ marginTop: 4, fontSize: '0.76rem', color: 'var(--text-secondary)', lineHeight: 1.5 }}>
          {copy}
        </div>
      </div>
      <button className="btn btn-secondary" onClick={() => { void returnFromTaskHandoff(); }}>
        {returnLabel}
      </button>
    </div>
  );
}
