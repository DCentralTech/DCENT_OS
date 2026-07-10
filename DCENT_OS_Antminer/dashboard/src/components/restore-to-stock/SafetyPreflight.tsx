// Step 4 — show source firmware checksum + safety preflight
// findings. CRITICAL findings hard-block (no override); HIGH
// findings require explicit acknowledgment; MEDIUM/LOW are info.
//
// Detector list (per W8-F.md):
//   DCENT-2026-008  hotelfee.json filename            High
//   DCENT-2026-009  atlas@anthill.farm needle         High
//   DCENT-2026-010  SECURE_BOOT_SET 1024B + sha       Critical (no-override)
//   DCENT-2026-011  Hashcore $6$4rQjfxJBpRYbzeys$...  High
//   DCENT-2026-012  monitor-ipsig daemons:22322       Critical (no-override)
//   DCENT-2026-013  39.104.179.132 (Aliyun dtu)       Medium
//   DCENT-2026-014  --enable-factory-reset            Medium

import React from 'react';
import type { RestoreToStockResponse, SafetyFinding } from '../../api/restore-to-stock';
import { BreakerWarningBanner } from './BreakerWarningBanner';
import { InfoDot } from '../common/Tooltip';

interface Props {
  preflight: RestoreToStockResponse | null;
  highAcknowledged: boolean;
  setHighAcknowledged: (b: boolean) => void;
}

export function SafetyPreflight({ preflight, highAcknowledged, setHighAcknowledged }: Props) {
  if (!preflight) {
    return (
      <div>
        <BreakerWarningBanner />
        <div style={{ color: 'var(--text-secondary, #8b8b9e)' }}>
          No preflight result yet — go back and stage the firmware first.
        </div>
      </div>
    );
  }

  const findings = preflight.safety_findings ?? [];
  const critical = findings.filter(f => f.severity === 'critical');
  const high = findings.filter(f => f.severity === 'high');
  const medium = findings.filter(f => f.severity === 'medium' || f.severity === 'low');
  const info = findings.filter(f => f.severity === 'info');

  // Preconditions met: 5 gates total — (1) preflight ran, (2) no critical
  // findings, (3) no no-override findings, (4) high findings either absent
  // or acknowledged, (5) source SHA captured. The Confirm step gates on
  // the same wire-level checks (backend re-verifies); this pill is the
  // operator-facing summary of where they stand.
  const gateRan = true; // we only render with preflight != null
  const gateNoCritical = critical.length === 0;
  const gateNoOverride = !findings.some(f => f.no_override);
  const gateHighAck = high.length === 0 || highAcknowledged;
  // staged_sha256 is informational (shown verbatim below) and is NOT an advance
  // precondition — the modal's Next predicate gates only on blocking findings +
  // HIGH ack. Excluded from this count so the pill can't read "4/5" while Next
  // is enabled (a pill-vs-button truth mismatch).
  const gatesMet = [gateRan, gateNoCritical, gateNoOverride, gateHighAck].filter(Boolean).length;
  const gatesTotal = 4;
  const allGatesMet = gatesMet === gatesTotal;

  return (
    <div>
      <BreakerWarningBanner />
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginTop: 0, marginBottom: 10, flexWrap: 'wrap' }}>
        <h3 style={{ margin: 0, fontSize: '1.1rem' }}>
          Safety preflight <InfoDot term="restore_gates" placement="bottom" />
        </h3>
        <span
          className={`ds-pill-numeric p4-gate-pill ${allGatesMet ? 'success' : 'warning'}`}
          aria-label={`${gatesMet} of ${gatesTotal} preflight gates met`}
          data-testid="restore-safety-gates-pill"
        >
          <span className="ds-pill-num">{gatesMet}/{gatesTotal}</span>
          <span className="ds-pill-label">Gates met</span>
        </span>
      </div>

      <div style={{
        padding: 12,
        borderRadius: 8,
        background: 'rgba(18,18,26,0.6)',
        border: '1px solid var(--border, rgba(255,255,255,0.08))',
        marginBottom: 12,
        fontSize: '0.78rem',
      }}>
        <div>
          <span style={{ color: 'var(--text-secondary, #8b8b9e)' }}>Source SHA-256: </span>
          <code style={{ wordBreak: 'break-all' }}>{preflight.staged_sha256 ?? '—'}</code>
        </div>
        <div style={{ marginTop: 4 }}>
          <span style={{ color: 'var(--text-secondary, #8b8b9e)' }}>Backend status: </span>
          <code>{preflight.status}</code>
        </div>
      </div>

      {critical.length > 0 && (
        <Section
          title="Critical — flash refused"
          color="var(--red, #EF4444)"
          bg="rgba(239,68,68,0.08)"
          findings={critical}
          locked
        >
          <div style={{ fontSize: '0.78rem', color: 'var(--text)', marginTop: 8 }}>
            These findings can't be overridden from the UI. Per
            <code> </code>,
            <code> </code>, and the
            daemons:22322 listener IOC, the backend refuses confirm:true at the wire — the
            slider on the next step won't even submit.
          </div>
        </Section>
      )}

      {high.length > 0 && (
        <Section
          title="High — explicit acknowledgment required"
          color="var(--yellow, #F0B429)"
          bg="rgba(240,180,41,0.08)"
          findings={high}
        >
          <label style={{ display: 'flex', alignItems: 'flex-start', gap: 8, marginTop: 12, fontSize: '0.85rem', color: 'var(--text)', cursor: 'pointer' }}>
            <input
              type="checkbox"
              checked={highAcknowledged}
              onChange={(e) => setHighAcknowledged(e.target.checked)}
              style={{ marginTop: 3 }}
            />
            <span>
              I've reviewed the high-severity findings and accept them for this restore. (Atlas SSH,
              hotelfee.json, and Hashcore root hashes are common third-party-mod IOCs — they don't
              brick the unit, but they shouldn't ship downstream.)
            </span>
          </label>
        </Section>
      )}

      {medium.length > 0 && (
        <Section
          title="Medium / low — informational"
          color="var(--text-secondary, #8b8b9e)"
          bg="rgba(255,255,255,0.04)"
          findings={medium}
        />
      )}

      {info.length > 0 && (
        <Section
          title="Info"
          color="var(--text-secondary, #8b8b9e)"
          bg="rgba(255,255,255,0.04)"
          findings={info}
        />
      )}

      {findings.length === 0 && (
        <div style={{
          padding: 14,
          borderRadius: 10,
          background: 'rgba(45,212,160,0.08)',
          border: '1px solid rgba(45,212,160,0.22)',
          color: 'var(--green, #2DD4A0)',
          fontWeight: 700,
          fontSize: '0.9rem',
        }}>
          ✓ Preflight clean — no IOCs found. Safe to proceed.
        </div>
      )}
    </div>
  );
}

interface SectionProps {
  title: string;
  color: string;
  bg: string;
  findings: SafetyFinding[];
  locked?: boolean;
  children?: React.ReactNode;
}

function Section({ title, color, bg, findings, locked, children }: SectionProps) {
  return (
    <div style={{
      padding: 12,
      borderRadius: 8,
      background: bg,
      border: `1px solid ${color}`,
      marginBottom: 12,
    }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, fontWeight: 700, color, fontSize: '0.9rem', marginBottom: 8 }}>
        {locked && <span aria-hidden>🔒</span>}
        {title}
        <span style={{ marginLeft: 'auto', fontSize: '0.75rem', color: 'var(--text-dim, #6E6E80)', fontWeight: 600 }}>
          {findings.length} finding{findings.length === 1 ? '' : 's'}
        </span>
      </div>
      <ul style={{ margin: 0, paddingLeft: 20, color: 'var(--text)', fontSize: '0.82rem' }}>
        {findings.map((f, idx) => (
          <li key={`${f.id}-${idx}`} style={{ marginBottom: 4 }}>
            <code style={{ fontSize: '0.72rem', color: 'var(--text-secondary, #8b8b9e)' }}>{f.id}</code>
            {' · '}
            <strong>{f.title}</strong>
            {f.remediation ? ` — ${f.remediation}` : null}
            {f.no_override && (
              <span style={{ marginLeft: 6, fontSize: '0.65rem', color: 'var(--red, #EF4444)', textTransform: 'uppercase', letterSpacing: '0.05em', fontWeight: 700 }}>
                no override
              </span>
            )}
            {f.matched_path && (
              <div style={{ fontSize: '0.7rem', color: 'var(--text-dim, #6E6E80)', fontFamily: 'JetBrains Mono, monospace', marginTop: 2 }}>
                {f.matched_path}
              </div>
            )}
          </li>
        ))}
      </ul>
      {children}
    </div>
  );
}

export function preflightBlocksFlash(preflight: RestoreToStockResponse | null): boolean {
  if (!preflight) return true;
  return (preflight.safety_findings ?? []).some(f => f.severity === 'critical' || f.no_override);
}

export function preflightRequiresHighAck(preflight: RestoreToStockResponse | null): boolean {
  if (!preflight) return false;
  return (preflight.safety_findings ?? []).some(f => f.severity === 'high');
}
