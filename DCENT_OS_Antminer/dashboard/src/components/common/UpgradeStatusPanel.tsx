import React, { useCallback, useEffect, useState } from 'react';
import { api } from '../../api/client';
import type { SystemUpgradeStatusResponse } from '../../api/types';
import { InfoDot } from './Tooltip';
import type { GlossaryKey } from '../../utils/glossary';

function getUpgradeStateCopy(state?: string) {
  switch (state) {
    case 'idle':
      return {
        label: 'No staged upgrade',
        detail: 'No staged package or rollback-armed boot-commit flag is reported by this read-only status endpoint.',
      };
    case 'validated_or_staged':
      return {
        label: 'Package staged',
        detail: 'A sysupgrade package is present in the browser staging directory. Treat this as staged only until the upload response reports signature verification and target preflight for the package you are about to use.',
      };
    case 'pending_boot_commit':
      return {
        label: 'Boot commit pending',
        detail: 'An upgrade_stage value is reported. A new boot may be under observation, but rollback is not committed until the boot-health path clears this state.',
      };
    default:
      return {
        label: state ? state.replace(/_/g, ' ') : 'Unknown',
        detail: 'Backend-reported state shown without local inference.',
      };
  }
}

function formatSize(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) return 'unknown size';
  const mib = bytes / (1024 * 1024);
  return `${mib.toFixed(mib >= 10 ? 0 : 1)} MiB`;
}

function formatTime(ms: number | null) {
  if (ms == null || !Number.isFinite(ms)) return 'unknown time';
  const date = new Date(ms);
  return Number.isNaN(date.getTime()) ? 'unknown time' : date.toLocaleString();
}

function FieldRow({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, minWidth: 0 }}>
      <span style={{ color: 'var(--text-dim)' }}>{label}</span>
      <span style={{ color: 'var(--text)', textAlign: 'right', wordBreak: 'break-word' }}>{value}</span>
    </div>
  );
}

function StageRow({
  label,
  state,
  detail,
  term,
}: {
  label: string;
  state: 'reported' | 'not-reported' | 'pending';
  detail: string;
  /**
   * Optional glossary key tying this rung's label to the centralized OTA
   * proof-ladder vocabulary (uploaded → signature-verified → preflight-passed
   * → scheduled → booted; "scheduled ≠ booted"). Phase-4 os-flows: the rung
   * labels render the exact shared words from `glossary.ts`, so this InfoDot
   * surfaces the same truth-contract text the rest of the flow surfaces use.
   */
  term?: GlossaryKey;
}) {
  const color = state === 'reported'
    ? 'var(--green)'
    : state === 'pending'
      ? 'var(--yellow)'
      : 'var(--text-dim)';
  const text = state === 'reported' ? 'reported' : state === 'pending' ? 'pending' : 'not reported';

  return (
    <div style={{ display: 'grid', gridTemplateColumns: 'minmax(120px, 0.9fr) minmax(86px, auto) minmax(0, 1.4fr)', gap: 8, alignItems: 'start' }}>
      <span style={{ color: 'var(--text)', display: 'inline-flex', alignItems: 'center', gap: 6 }}>
        {label}
        {term && <InfoDot term={term} size={12} />}
      </span>
      <span style={{ color, fontFamily: "'JetBrains Mono', monospace", whiteSpace: 'nowrap' }}>{text}</span>
      <span style={{ color: 'var(--text-dim)' }}>{detail}</span>
    </div>
  );
}

export function UpgradeStatusPanel({ compact = false }: { compact?: boolean }) {
  const [status, setStatus] = useState<SystemUpgradeStatusResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');

  const refresh = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      setStatus(await api.getSystemUpgradeStatus());
    } catch (err) {
      setStatus(null);
      setError(err instanceof Error ? err.message : 'Upgrade status unavailable');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const packages = status?.staged_packages ?? [];
  const state = getUpgradeStateCopy(status?.state);
  const readOnly = status?.read_only === true;
  const hasStagedPackage = packages.length > 0;
  const hasPendingBootCommit = status?.state === 'pending_boot_commit';

  return (
    <div style={{
      marginBottom: compact ? 12 : 16,
      padding: compact ? 12 : 14,
      border: '1px solid var(--border)',
      borderRadius: compact ? 'var(--radius-sm)' : 'var(--radius)',
      background: 'rgba(0,0,0,0.35)',
    }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginBottom: 8 }}>
        <div>
          <div style={{ fontWeight: 700, color: 'var(--accent)', fontSize: compact ? '0.85rem' : '0.9rem' }}>
            Upgrade Status
          </div>
          <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>
            Read-only package, preflight, and rollback state.
          </div>
        </div>
        <button
          className="btn btn-secondary"
          onClick={() => void refresh()}
          disabled={loading}
          style={{ fontSize: '0.75rem', flexShrink: 0 }}
        >
          {loading ? 'Refreshing...' : 'Refresh'}
        </button>
      </div>

      {error ? (
        <div className="cp-empty-note is-error">
          Upgrade status unavailable: {error}
        </div>
      ) : (
        <>
          <div style={{
            display: 'grid',
            gridTemplateColumns: compact ? '1fr' : 'repeat(auto-fit, minmax(150px, 1fr))',
            gap: 8,
            fontSize: '0.75rem',
            fontFamily: "'JetBrains Mono', monospace",
          }}>
            <FieldRow label="State" value={state.label} />
            <FieldRow label="Read-only" value={readOnly ? 'yes' : 'unknown'} />
            <FieldRow label="Staged" value={`${status?.staged_package_count ?? 0}`} />
            <FieldRow label="Rollback gate" value={status?.upgrade_stage ?? 'not reported'} />
            <FieldRow label="Boot slot" value={status?.boot_slot ?? 'unavailable'} />
            <FieldRow label="Boot count" value={status?.bootcount ?? 'unavailable'} />
          </div>

          <div style={{ marginTop: 8, color: 'var(--text-dim)', fontSize: '0.74rem', lineHeight: 1.45 }}>
            {state.detail}
          </div>

          <div style={{
            marginTop: 10,
            paddingTop: 8,
            borderTop: '1px solid var(--border)',
            fontSize: '0.72rem',
            lineHeight: 1.45,
          }}>
            <StageRow
              label="Uploaded"
              term="ota_uploaded"
              state={hasStagedPackage ? 'reported' : 'not-reported'}
              detail="Reported only when a .tar exists in browser staging."
            />
            <StageRow
              label="Signature verified"
              term="ota_signature_verified"
              state="not-reported"
              detail="Only the upload/apply response reports fresh signature verification; this panel does not re-run it."
            />
            <StageRow
              label="Target preflight"
              term="ota_preflight_passed"
              state="not-reported"
              detail="Only the upload/apply response reports target sysupgrade preflight for a specific request."
            />
            <StageRow
              label="Scheduled"
              term="ota_scheduled"
              state="not-reported"
              detail="A scheduled flash is returned by the apply response, not persisted here."
            />
            <StageRow
              label="Boot observed"
              term="ota_booted"
              state={hasPendingBootCommit ? 'pending' : 'not-reported'}
              detail={hasPendingBootCommit ? 'Rollback gate is still armed; wait for boot health to clear it.' : 'No boot observation signal is reported by this endpoint.'}
            />
            <StageRow
              label="Rollback committed"
              term="scheduled_not_booted"
              state="not-reported"
              detail="Committed rollback state is not claimed here; absence of upgrade_stage is not a boot/version proof by itself."
            />
          </div>

          {packages.length > 0 ? (
            <div style={{ marginTop: 10 }}>
              {packages.map(pkg => (
                <div
                  key={pkg.path}
                  style={{
                    padding: '8px 0',
                    borderTop: '1px solid var(--border)',
                    fontSize: '0.74rem',
                    color: 'var(--text-dim)',
                  }}
                >
                  <div style={{ color: 'var(--text)', fontWeight: 600, wordBreak: 'break-word' }}>
                    {pkg.filename}
                  </div>
                  <div>{formatSize(pkg.size_bytes)} - modified {formatTime(pkg.modified_ms)}</div>
                  <div style={{ wordBreak: 'break-all', fontFamily: "'JetBrains Mono', monospace" }}>{pkg.path}</div>
                </div>
              ))}
            </div>
          ) : (
            <div style={{ marginTop: 10, color: 'var(--text-dim)', fontSize: '0.75rem' }}>
              No staged sysupgrade package is reported by the backend.
            </div>
          )}

          {status?.limitations != null && status.limitations.length > 0 && (
            <div style={{
              marginTop: 10,
              paddingTop: 8,
              borderTop: '1px solid var(--border)',
              color: 'var(--text-dim)',
              fontSize: '0.72rem',
              lineHeight: 1.5,
            }}>
              {status.limitations.map(item => (
                <div key={item}>- {item}</div>
              ))}
            </div>
          )}
        </>
      )}
    </div>
  );
}
