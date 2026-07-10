import React from 'react';
import { useMinerStore } from '../../store/miner';
import { setHealthIssueDismissed } from '../../utils/health';
import { noticeGlyph } from '../../utils/noticeGlyphs';
import type { Alert } from '../../store/miner';

export function AlertBanner() {
  const alerts = useMinerStore(s => s.alerts);
  const dismissAlert = useMinerStore(s => s.dismissAlert);
  const active = alerts.filter(a => !a.dismissed).slice(-3); // Show last 3

  // Freedom-first: dismissing a derived health advisory (e.g. the
  // "no owner password" reminder) must persist across reloads so the
  // operator's choice sticks. It still self-clears if the underlying
  // condition resolves (password set ⇒ issue not emitted ⇒ alert cleared).
  const handleDismiss = (alert: Alert) => {
    if (alert.source === 'health' && alert.dedupeKey) {
      setHealthIssueDismissed(alert.dedupeKey, true);
    }
    dismissAlert(alert.id);
  };

  if (active.length === 0) return null;

  // Severity glyphs come from the shared canonical map (utils/noticeGlyphs) so
  // this store-driven global banner renders the SAME tone glyph as InfoBanner /
  // StatePanel / Toast (critical -> danger glyph, warning -> warn glyph). Visual
  // is token-driven via `.cp-alert*` in common.css ( P5, D-03). The glyph
  // span is aria-hidden, so this is a visual-grammar change only: role / message
  // / dismiss persistence are byte-preserved \u2014 no copy softened.

  return (
    <div className="cp-alert-stack">
      {active.map(alert => (
        <div
          key={alert.id}
          className="cp-alert"
          role={alert.level === 'critical' || alert.level === 'warning' ? 'alert' : 'status'}
          data-severity={alert.level}
        >
          <span className="cp-alert-msg">
            <span className="cp-alert-glyph" aria-hidden>
              {noticeGlyph(alert.level)}
            </span>
            <span className="cp-alert-text">{alert.message}</span>
          </span>
          <button
            type="button"
            className="cp-alert-dismiss"
            onClick={() => handleDismiss(alert)}
            aria-label="Dismiss alert"
          >
            {'\u2715'}
          </button>
        </div>
      ))}
    </div>
  );
}
