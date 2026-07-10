import React from 'react';
import { noticeGlyph } from '../../utils/noticeGlyphs';

type StateTone = 'info' | 'warning' | 'danger' | 'success' | 'neutral';

export function StatePanel({
  title,
  message,
  tone = 'neutral',
  compact = false,
  action,
}: {
  title: string;
  message: string;
  tone?: StateTone;
  compact?: boolean;
  action?: React.ReactNode;
}) {
  return (
    <div
      className={`state-panel ${tone}${compact ? ' compact' : ''}`}
      // : dynamically-injected danger/warning panels were not announced
      // to screen readers. Danger is assertive; everything else polite.
      role={tone === 'danger' ? 'alert' : 'status'}
      aria-live={tone === 'danger' ? 'assertive' : 'polite'}
    >
      <div className="state-panel-row">
        <div className="state-panel-main">
          <span className="state-panel-badge" aria-hidden="true">{noticeGlyph(tone)}</span>
          <div className="state-panel-copy">
            <div className="state-panel-title">{title}</div>
            <div className="state-panel-message">{message}</div>
          </div>
        </div>
        {action && <div className="state-panel-action">{action}</div>}
      </div>
    </div>
  );
}
