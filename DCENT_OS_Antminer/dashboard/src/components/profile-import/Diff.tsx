// Step 3 — three-column visualization (red removed / yellow changed
// / green added) of preset rows vs the active profile for this
// (model, hashboard, chip).
//
// Active profile is fetched via GET /api/profiles/silicon/:id
// where :id matches our (model, hashboard, chip) tuple. If no
// active profile exists, every incoming row is "added".

import React, { useEffect, useMemo, useState } from 'react';
import { siliconProfilesApi, type SiliconPresetRow, type SiliconProfileBundle, type SiliconProfileSummary } from '../../api/profiles-silicon';
import { EmptyTableRow } from '../common/EmptyTableRow';

interface Props {
  bundle: SiliconProfileBundle;
}

type DiffKind = 'removed' | 'changed' | 'added' | 'same';

interface DiffRow {
  kind: DiffKind;
  step: number;
  active?: SiliconPresetRow;
  incoming?: SiliconPresetRow;
}

function computeDiff(active: SiliconPresetRow[] | null, incoming: SiliconPresetRow[]): DiffRow[] {
  const activeMap = new Map<number, SiliconPresetRow>();
  (active ?? []).forEach(r => activeMap.set(r.step, r));
  const incomingMap = new Map<number, SiliconPresetRow>();
  incoming.forEach(r => incomingMap.set(r.step, r));

  const allSteps = new Set<number>([...activeMap.keys(), ...incomingMap.keys()]);
  const sorted = Array.from(allSteps).sort((a, b) => a - b);

  return sorted.map((step) => {
    const a = activeMap.get(step);
    const i = incomingMap.get(step);
    if (a && !i) return { kind: 'removed' as DiffKind, step, active: a };
    if (!a && i) return { kind: 'added' as DiffKind, step, incoming: i };
    if (a && i) {
      const same = a.freq_mhz === i.freq_mhz
        && Math.abs(a.voltage_v - i.voltage_v) < 1e-6;
      return { kind: same ? 'same' : 'changed', step, active: a, incoming: i };
    }
    // unreachable
    return { kind: 'same' as DiffKind, step };
  });
}

export function Diff({ bundle }: Props) {
  const [active, setActive] = useState<SiliconProfileBundle | null>(null);
  const [loadState, setLoadState] = useState<'idle' | 'loading' | 'no_match' | 'ready' | 'error'>('idle');
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoadState('loading');
    setErrorMsg(null);

    (async () => {
      try {
        const list: SiliconProfileSummary[] = await siliconProfilesApi.list();
        // Pick the live_confirmed bundle for the same (model, hashboard, chip),
        // falling back to operator_confirmed -> vendor_extracted -> baked.
        const order = ['live_confirmed', 'operator_confirmed', 'vendor_extracted', 'baked'];
        const candidates = list.filter(s =>
          s.miner_model === bundle.miner_model
          && s.hashboard === bundle.hashboard
          && s.chip === bundle.chip
        );
        candidates.sort((a, b) => order.indexOf(a.source_class) - order.indexOf(b.source_class));
        const pick = candidates[0];
        if (!pick) {
          if (!cancelled) {
            setActive(null);
            setLoadState('no_match');
          }
          return;
        }
        const full = await siliconProfilesApi.get(pick.id);
        if (!cancelled) {
          setActive(full);
          setLoadState('ready');
        }
      } catch (e) {
        if (!cancelled) {
          setActive(null);
          setErrorMsg(e instanceof Error ? e.message : 'Failed to fetch active profile');
          setLoadState('error');
        }
      }
    })();

    return () => { cancelled = true; };
  }, [bundle.miner_model, bundle.hashboard, bundle.chip]);

  const diff = useMemo(() => {
    return computeDiff(active?.presets ?? null, bundle.presets);
  }, [active, bundle.presets]);

  const counts = useMemo(() => {
    return diff.reduce((acc, row) => {
      acc[row.kind] = (acc[row.kind] || 0) + 1;
      return acc;
    }, {} as Record<DiffKind, number>);
  }, [diff]);

  return (
    <div className="section">
      {/* Loading / status / error — announced correctly to AT */}
      {(loadState === 'loading' || loadState === 'no_match' || loadState === 'ready') && (
        <div role="status" aria-live="polite" style={{ fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 8 }}>
          {loadState === 'loading' && 'Fetching active profile...'}
          {loadState === 'no_match' && (
            <>No active profile for <code>{bundle.miner_model} / {bundle.hashboard} / {bundle.chip}</code> — every row is a new addition.</>
          )}
          {loadState === 'ready' && active && (
            <>Comparing against <code>{active.id ?? `${active.source_class} / ${active.presets.length} rows`}</code></>
          )}
        </div>
      )}
      {loadState === 'error' && (
        <div role="alert" aria-live="assertive" style={{ fontSize: '0.78rem', color: 'var(--red, #EF4444)', marginBottom: 8 }}>
          Couldn't fetch active profile: {errorMsg}
        </div>
      )}

      <div style={{ display: 'flex', gap: 12, marginBottom: 12, flexWrap: 'wrap', fontSize: '0.75rem' }}>
        <Pill color="rgba(45, 212, 160, 0.16)" textColor="var(--green, #2DD4A0)">
          + {counts.added ?? 0} added
        </Pill>
        <Pill color="rgba(240, 180, 41, 0.16)" textColor="var(--yellow, #F0B429)">
          ~ {counts.changed ?? 0} changed
        </Pill>
        <Pill color="rgba(239, 68, 68, 0.16)" textColor="var(--red, #EF4444)">
          - {counts.removed ?? 0} removed
        </Pill>
        <Pill color="rgba(255,255,255,0.04)" textColor="var(--text-dim, #6E6E80)">
          = {counts.same ?? 0} same
        </Pill>
      </div>

      <div className="table-wrap" style={{ overflowX: 'auto', border: '1px solid var(--border, rgba(255,255,255,0.08))', borderRadius: 8 }}>
        <table
          aria-label={`Profile diff: ${counts.added ?? 0} added, ${counts.changed ?? 0} changed, ${counts.removed ?? 0} removed, ${counts.same ?? 0} same`}
          style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.75rem', fontFamily: 'JetBrains Mono, monospace' }}
        >
          <thead>
            <tr style={{ background: 'rgba(255,255,255,0.04)' }}>
              <th scope="col" style={th}>Step</th>
              <th scope="col" style={th}>Active freq / V</th>
              <th scope="col" style={th}>Incoming freq / V</th>
              <th scope="col" style={th}>Δ</th>
            </tr>
          </thead>
          <tbody>
            {diff.length === 0 && (
              <EmptyTableRow
                colSpan={4}
                title="No preset rows"
                hint="Neither the active profile nor the incoming bundle has freq/voltage rows to compare."
                data-testid="diff-empty"
              />
            )}
            {diff.map((row) => (
              <tr key={row.step} style={{ background: kindBg(row.kind) }}>
                <td style={td}>{row.step}</td>
                <td style={td}>
                  {row.active
                    ? `${row.active.freq_mhz.toFixed(1)} MHz / ${row.active.voltage_v.toFixed(3)} V`
                    : <span style={{ color: 'var(--text-dim)' }}>—</span>}
                </td>
                <td style={td}>
                  {row.incoming
                    ? `${row.incoming.freq_mhz.toFixed(1)} MHz / ${row.incoming.voltage_v.toFixed(3)} V`
                    : <span style={{ color: 'var(--text-dim)' }}>—</span>}
                </td>
                <td style={{ ...td, fontWeight: 700, color: kindColor(row.kind) }}>{kindGlyph(row.kind)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

const th: React.CSSProperties = {
  textAlign: 'left',
  padding: '8px 12px',
  fontWeight: 700,
  fontSize: '0.7rem',
  textTransform: 'uppercase',
  letterSpacing: '0.04em',
  color: 'var(--text-secondary, #8b8b9e)',
};

const td: React.CSSProperties = {
  padding: '6px 12px',
  borderTop: '1px solid var(--border, rgba(255,255,255,0.06))',
};

function kindBg(kind: DiffKind): string {
  if (kind === 'added') return 'rgba(45, 212, 160, 0.08)';
  if (kind === 'changed') return 'rgba(240, 180, 41, 0.08)';
  if (kind === 'removed') return 'rgba(239, 68, 68, 0.08)';
  return 'transparent';
}

function kindColor(kind: DiffKind): string {
  if (kind === 'added') return 'var(--green, #2DD4A0)';
  if (kind === 'changed') return 'var(--yellow, #F0B429)';
  if (kind === 'removed') return 'var(--red, #EF4444)';
  return 'var(--text-dim, #6E6E80)';
}

function kindGlyph(kind: DiffKind): string {
  if (kind === 'added') return '+';
  if (kind === 'changed') return '~';
  if (kind === 'removed') return '−';
  return '=';
}

function Pill({ color, textColor, children }: { color: string; textColor: string; children: React.ReactNode }) {
  return (
    <span style={{
      display: 'inline-flex',
      alignItems: 'center',
      padding: '3px 10px',
      borderRadius: 999,
      background: color,
      color: textColor,
      fontSize: '0.7rem',
      fontWeight: 700,
      letterSpacing: '0.04em',
    }}>
      {children}
    </span>
  );
}
