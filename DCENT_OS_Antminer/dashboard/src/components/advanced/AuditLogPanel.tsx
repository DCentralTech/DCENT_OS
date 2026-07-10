import React, { useCallback, useEffect, useState } from 'react';
import { api } from '../../api/client';
import type { AuditLogResponse, AuditRecord } from '../../api/types';

// AuditLogPanel — operator/fleet-facing view of the PERSISTENT, redacted audit
// log (`GET /api/audit-log`, W10). This is the on-disk `/data/audit.log` that
// survives reboots (distinct from the in-memory history ring). Records are
// newest-first and already redacted server-side — passwords / worker names are
// never present in the wire payload. Closes the W8 parity gap (LuxOS/BraiinsOS
// expose a persistent audit trail; DCENT had the backend but no UI surface).

const PAGE_LIMIT = 50;

function humanizeEventKind(kind: string): string {
  return kind
    .split('_')
    .filter(Boolean)
    .map(w => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ');
}

function formatTimestamp(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return 'unknown time';
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return String(ms);
  }
}

// Render the variant-specific fields of a tagged audit event (everything
// except the `event` discriminator) as a compact, defensively-stringified
// summary. The backend has already redacted secrets, so we render whatever
// fields remain verbatim.
function summarizeEvent(event: AuditRecord['event']): string {
  const entries = Object.entries(event).filter(([k]) => k !== 'event');
  if (entries.length === 0) return '';
  return entries
    .map(([k, v]) => {
      let val: string;
      if (v === null || v === undefined) val = '∅';
      else if (Array.isArray(v)) val = v.length ? v.join(', ') : '[]';
      else if (typeof v === 'object') val = JSON.stringify(v);
      else val = String(v);
      return `${k}: ${val}`;
    })
    .join(' · ');
}

export function AuditLogPanel() {
  const [page, setPage] = useState<AuditLogResponse | null>(null);
  const [offset, setOffset] = useState(0);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async (nextOffset: number) => {
    setLoading(true);
    setError(null);
    try {
      const res = await api.getAuditLog(nextOffset, PAGE_LIMIT);
      setPage(res);
      setOffset(nextOffset);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'failed to load audit log');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { void load(0); }, [load]);

  const total = page?.total ?? 0;
  const records = page?.events ?? [];
  const showingFrom = total === 0 ? 0 : offset + 1;
  const showingTo = offset + records.length;
  const hasPrev = offset > 0;
  const hasNext = offset + PAGE_LIMIT < total;

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// persistent audit log</div>
          <h2 className="hacker-inspector-title">Audit Trail</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className="hacker-inspector-status neutral">{total} event{total === 1 ? '' : 's'}</span>
          <button
            className="btn btn-secondary"
            onClick={() => void load(offset)}
            disabled={loading}
          >
            {loading ? 'Loading…' : 'Refresh'}
          </button>
        </div>
      </header>

      <div className="hacker-inspector-body" aria-busy={loading}>
        <div className="audit-meta-note" role="note">
          Persistent, reboot-surviving record from <code>{page?.path ?? '/data/audit.log'}</code>.
          Newest first. Secrets (passwords, worker names) are redacted server-side and never
          appear here.
        </div>

        {error && (
          <div className="audit-error" role="alert">Audit log error — {error}.</div>
        )}

        {!error && !loading && records.length === 0 && (
          <div className="sf-empty">
            No audit entries yet. Operator actions (mode changes, pool switches, sysupgrades,
            voltage overrides) are recorded here as they happen.
          </div>
        )}

        {records.length > 0 && (
          <ol className="audit-list">
            {records.map((rec, i) => {
              const kind = rec.event?.event ?? 'unknown';
              const detail = summarizeEvent(rec.event ?? { event: kind });
              return (
                <li key={`${rec.timestamp_ms}-${i}`} className="register-inspector audit-row">
                  <div className="audit-row-head">
                    <span className="audit-kind">{humanizeEventKind(kind)}</span>
                    <span className="audit-actor">{rec.actor || 'unknown'}</span>
                    <span className="audit-time">{formatTimestamp(rec.timestamp_ms)}</span>
                  </div>
                  {detail && <div className="audit-detail">{detail}</div>}
                </li>
              );
            })}
          </ol>
        )}
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span role="status" aria-live="polite">
            {loading ? 'loading audit log…' : total === 0 ? 'no events' : `showing ${showingFrom}–${showingTo} of ${total}`}
          </span>
        </div>
        <div className="audit-pager">
          <button
            className="btn btn-secondary"
            onClick={() => void load(Math.max(0, offset - PAGE_LIMIT))}
            disabled={!hasPrev || loading}
          >
            ‹ Newer
          </button>
          <button
            className="btn btn-secondary"
            onClick={() => void load(offset + PAGE_LIMIT)}
            disabled={!hasNext || loading}
          >
            Older ›
          </button>
        </div>
      </footer>
    </div>
  );
}
