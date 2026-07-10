// ProfilesPage — list silicon profiles + import button.
//
// Hash routes:
//   #/profiles          -> ProfilesPage (list view)
//   #/profiles/import   -> ProfilesPage with wizard mounted
//
// Subpath handled via getSubPage from utils/router.

import React, { useCallback, useEffect, useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { getSubPage } from '../../utils/router';
import { siliconProfilesApi, type SiliconProfileSummary } from '../../api/profiles-silicon';
import { ProfileImportWizard } from './ProfileImportWizard';
import { SectionSkeleton } from '../common/skeletons';
import { EmptyState } from '../common/EmptyState';

export function ProfilesPage() {
  const currentPage = useMinerStore(s => s.currentPage);
  const setCurrentPage = useMinerStore(s => s.setCurrentPage);
  const sub = getSubPage(currentPage);
  const showWizard = sub === 'import';

  const [profiles, setProfiles] = useState<SiliconProfileSummary[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [reloadBusy, setReloadBusy] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setErrorMsg(null);
    try {
      const list = await siliconProfilesApi.list();
      setProfiles(list);
    } catch (e) {
      // — surface
      // the error inline, don't blank the page.
      setProfiles([]);
      setErrorMsg(e instanceof Error ? e.message : 'Failed to load profiles');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const triggerReload = async () => {
    setReloadBusy(true);
    try {
      await siliconProfilesApi.reload();
      await refresh();
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : 'Reload failed');
    } finally {
      setReloadBusy(false);
    }
  };

  if (showWizard) {
    return (
      <ProfileImportWizard
        onClose={() => { setCurrentPage('profiles'); void refresh(); }}
      />
    );
  }

  return (
    <div className="page-content" style={{ padding: '0 20px' }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 16 }}>
        <div>
          <span className="ds-section-eyebrow">Profiles</span>
          <h2 style={{ margin: 0, fontSize: '1.4rem' }}>Silicon profiles</h2>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)' }}>
            Per-(model, hashboard, chip) frequency / voltage tables. Operator-confirmed bundles
            override vendor-extracted; LiveConfirmed bundles are immutable.
          </div>
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <button type="button" className="ds-btn" onClick={triggerReload} disabled={reloadBusy}>
            {reloadBusy ? 'Reloading...' : 'Reload from disk'}
          </button>
          <button type="button" className="ds-btn primary" onClick={() => setCurrentPage('profiles/import')}>
            Import new
          </button>
        </div>
      </div>

      {errorMsg && (
        <div
          role="alert"
          aria-live="assertive"
          style={{
            padding: '10px 12px',
            borderRadius: 8,
            marginBottom: 12,
            background: 'rgba(239,68,68,0.08)',
            border: '1px solid rgba(239,68,68,0.22)',
            color: 'var(--text)',
            fontSize: '0.78rem',
          }}
        >
          Couldn't reach the daemon ({errorMsg}). The profile registry might be offline; the
          dashboard remains usable — try Reload once the daemon is back.
        </div>
      )}

      <div className="section">
        {loading && <SectionSkeleton rows={4} data-testid="profiles-loading" />}
        {!loading && !errorMsg && profiles && profiles.length === 0 && (
          <EmptyState
            title="No silicon profiles loaded"
            hint="Per-(model, hashboard, chip) frequency / voltage bundles live here. Import your first to give the autotuner operator-confirmed tables to work from."
            action={{ label: 'Import new', onClick: () => setCurrentPage('profiles/import') }}
            actionPrimary
            data-testid="profiles-empty"
          />
        )}
        {!loading && profiles && profiles.length > 0 && (
          <div className="table-wrap" style={{ overflowX: 'auto', border: '1px solid var(--border, rgba(255,255,255,0.08))', borderRadius: 8 }}>
            <table aria-label="Silicon profile registry" style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.85rem' }}>
              <thead>
                <tr style={{ background: 'rgba(255,255,255,0.04)' }}>
                  <th scope="col" style={th}>Model</th>
                  <th scope="col" style={th}>Hashboard</th>
                  <th scope="col" style={th}>Chip</th>
                  <th scope="col" style={th}>Trust</th>
                  <th scope="col" style={{ ...th, textAlign: 'right' }}>Presets</th>
                  <th scope="col" style={th}>id</th>
                </tr>
              </thead>
              <tbody>
                {profiles.map((p) => (
                  <tr key={p.id}>
                    <td style={td}>{p.miner_model}</td>
                    <td style={td}><code>{p.hashboard}</code></td>
                    <td style={td}><code>{p.chip}</code></td>
                    <td style={td}>
                      <SourceBadge source={p.source_class} />
                    </td>
                    <td style={{ ...td, textAlign: 'right', fontVariantNumeric: 'tabular-nums' }}>{p.preset_count}</td>
                    <td style={{ ...td, fontFamily: 'JetBrains Mono, monospace', fontSize: '0.7rem', color: 'var(--text-dim, #6E6E80)' }}>
                      {p.id}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}

function SourceBadge({ source }: { source: string }) {
  const palette: Record<string, [string, string]> = {
    live_confirmed: ['rgba(45,212,160,0.16)', 'var(--green, #2DD4A0)'],
    operator_confirmed: ['rgba(247,147,26,0.16)', 'var(--accent, #FAA500)'],
    vendor_extracted: ['rgba(240,180,41,0.14)', 'var(--yellow, #F0B429)'],
    baked: ['rgba(255,255,255,0.06)', 'var(--text-secondary, #8b8b9e)'],
  };
  const [bg, fg] = palette[source] ?? palette.baked;
  return (
    <span style={{
      display: 'inline-block',
      padding: '2px 8px',
      borderRadius: 999,
      background: bg,
      color: fg,
      fontSize: '0.7rem',
      fontWeight: 700,
      textTransform: 'uppercase',
      letterSpacing: '0.04em',
    }}>
      {source.replace('_', ' ')}
    </span>
  );
}

const th: React.CSSProperties = {
  textAlign: 'left',
  padding: '10px 12px',
  fontWeight: 700,
  fontSize: '0.7rem',
  textTransform: 'uppercase',
  letterSpacing: '0.04em',
  color: 'var(--text-secondary, #8b8b9e)',
};

const td: React.CSSProperties = {
  padding: '8px 12px',
  borderTop: '1px solid var(--border, rgba(255,255,255,0.06))',
};

const primaryBtn: React.CSSProperties = {
  padding: '8px 16px',
  borderRadius: 8,
  border: 'none',
  background: 'var(--accent, #FAA500)',
  color: '#0a0a0f',
  fontWeight: 700,
  fontSize: '0.85rem',
  cursor: 'pointer',
};

const secondaryBtn: React.CSSProperties = {
  padding: '8px 16px',
  borderRadius: 8,
  border: '1px solid var(--border, rgba(255,255,255,0.12))',
  background: 'transparent',
  color: 'var(--text)',
  fontWeight: 600,
  fontSize: '0.85rem',
  cursor: 'pointer',
};
