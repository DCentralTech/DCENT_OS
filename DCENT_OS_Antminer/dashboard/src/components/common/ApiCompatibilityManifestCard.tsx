import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type {
  ApiCompatibilityManifestResponse,
  ApiCompatibilityRouteEntry,
  ApiCompatibilityCommandEntry,
} from '../../api/types';
import { SectionSkeleton } from './skeletons/SectionSkeleton';

const UNAVAILABLE_COPY = 'API compatibility manifest unavailable. No endpoint status was inferred.';

function supportLabel(value: string): string {
  switch (value) {
    case 'implemented':
      return 'Implemented';
    case 'implemented_alias':
      return 'Alias';
    case 'recognized_unsupported':
      return 'Recognized unsupported';
    case 'documented_only':
      return 'Documented only';
    default:
      return value.replace(/_/g, ' ');
  }
}

function supportColor(value: string): string {
  if (value === 'implemented') return 'var(--green, #3FB950)';
  if (value === 'implemented_alias') return 'var(--accent, #FAA500)';
  if (value === 'recognized_unsupported') return 'var(--yellow, #D29922)';
  return 'var(--text-dim)';
}

function EntryPill({ children, color = 'var(--text-dim)' }: { children: React.ReactNode; color?: string }) {
  return (
    <span style={{
      display: 'inline-flex',
      alignItems: 'center',
      padding: '2px 7px',
      borderRadius: 6,
      border: `1px solid ${color}`,
      color,
      fontSize: '0.62rem',
      fontWeight: 700,
      textTransform: 'uppercase',
      lineHeight: 1.4,
    }}>
      {children}
    </span>
  );
}

function RouteRow({ route }: { route: ApiCompatibilityRouteEntry }) {
  return (
    <div style={{
      display: 'grid',
      gridTemplateColumns: '42px minmax(0, 1fr)',
      gap: 8,
      padding: '6px 0',
      borderTop: '1px solid rgba(255,255,255,0.05)',
    }}>
      <span style={{ color: route.method === 'GET' ? 'var(--green)' : 'var(--yellow)', fontWeight: 700 }}>
        {route.method}
      </span>
      <div style={{ minWidth: 0 }}>
        <div style={{
          fontFamily: "'JetBrains Mono', monospace",
          color: 'var(--text, #E8E8E8)',
          overflowWrap: 'anywhere',
        }}>
          {route.path}
        </div>
        <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', marginTop: 5 }}>
          <EntryPill color={supportColor(route.support)}>{supportLabel(route.support)}</EntryPill>
          {route.mutates && <EntryPill color="var(--red, #F85149)">Side-effecting</EntryPill>}
          {route.unsupported_fields.length > 0 && (
            <EntryPill>Unsupported placeholders: {route.unsupported_fields.join(', ')}</EntryPill>
          )}
        </div>
      </div>
    </div>
  );
}

function CommandRow({ command }: { command: ApiCompatibilityCommandEntry }) {
  return (
    <div style={{
      display: 'flex',
      justifyContent: 'space-between',
      gap: 8,
      padding: '5px 0',
      borderTop: '1px solid rgba(255,255,255,0.05)',
      fontFamily: "'JetBrains Mono', monospace",
      fontSize: '0.72rem',
    }}>
      <span style={{ color: 'var(--text, #E8E8E8)' }}>{command.name}</span>
      <span style={{ color: supportColor(command.support), textAlign: 'right' }}>
        {supportLabel(command.support)}
      </span>
    </div>
  );
}

export function ApiCompatibilityManifestCard() {
  const [manifest, setManifest] = useState<ApiCompatibilityManifestResponse | null>(null);
  const [manifestError, setManifestError] = useState(false);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let alive = true;
    api.getApiCompatibilityManifest()
      .then((response) => {
        if (!alive) return;
        setManifest(response);
        setManifestError(false);
      })
      .catch(() => {
        if (!alive) return;
        setManifest(null);
        setManifestError(true);
      })
      .finally(() => {
        if (alive) setLoading(false);
      });

    return () => {
      alive = false;
    };
  }, []);

  const summary = useMemo(() => {
    if (!manifest) return { routeCount: 0, commandCount: 0, omissionCount: 0 };
    return manifest.surfaces.reduce((acc, surface) => ({
      routeCount: acc.routeCount + surface.routes.length,
      commandCount: acc.commandCount + surface.commands.length,
      omissionCount: manifest.omissions.length,
    }), { routeCount: 0, commandCount: 0, omissionCount: manifest.omissions.length });
  }, [manifest]);

  const downloadCompatibilityManifest = useCallback(() => {
    if (!manifest) return;
    const body = JSON.stringify(manifest, null, 2);
    const blob = new Blob([body], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = `dcentos-api-compatibility-manifest-${new Date().toISOString().slice(0, 10)}.json`;
    document.body.appendChild(anchor);
    anchor.click();
    document.body.removeChild(anchor);
    URL.revokeObjectURL(url);
  }, [manifest]);

  return (
    <div style={{
      background: 'var(--card-bg, #242432)',
      borderRadius: 12,
      padding: 16,
      border: '1px solid var(--border, rgba(255,255,255,0.06))',
      marginBottom: 16,
    }}>
      <div style={{
        fontSize: '0.75rem',
        color: 'var(--text-dim)',
        textTransform: 'uppercase',
        marginBottom: 12,
        letterSpacing: '0.05em',
      }}>
        API Compatibility
      </div>

      <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', marginBottom: 10 }}>
        <EntryPill color="var(--green, #3FB950)">READ-ONLY</EntryPill>
        <EntryPill color="var(--accent, #FAA500)">DECLARED BY FIRMWARE</EntryPill>
        <EntryPill>NO PROBING</EntryPill>
      </div>

      <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)', lineHeight: 1.55, marginBottom: 12 }}>
        Firmware-declared compatibility manifest for DCENT REST, /api/v1 aliases, AxeOS/ESP-Miner discovery, and CGMiner TCP.
        This panel does not call, probe, or test the listed endpoints.
      </div>

      {loading && (
        <SectionSkeleton rows={4} data-testid="skeleton-api-manifest" />
      )}

      {!loading && manifestError && (
        <div style={{ fontSize: '0.8rem', color: 'var(--yellow)', lineHeight: 1.5 }}>
          {UNAVAILABLE_COPY}
        </div>
      )}

      {!loading && manifest && (
        <>
          <div style={{
            display: 'grid',
            gridTemplateColumns: 'repeat(3, minmax(0, 1fr))',
            gap: 8,
            marginBottom: 12,
          }}>
            <InfoTile label="Declared routes" value={String(summary.routeCount)} />
            <InfoTile label="CGMiner commands" value={String(summary.commandCount)} />
            <InfoTile label="Omissions" value={String(summary.omissionCount)} />
          </div>

          <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap', marginBottom: 12 }}>
            <button
              onClick={downloadCompatibilityManifest}
              style={{
                border: '1px solid var(--accent, #FAA500)',
                background: 'rgba(247, 147, 26, 0.08)',
                color: 'var(--accent, #FAA500)',
                borderRadius: 6,
                padding: '6px 10px',
                fontSize: '0.72rem',
                fontFamily: "'JetBrains Mono', monospace",
                fontWeight: 700,
                cursor: 'pointer',
              }}
            >
              Export Declared Manifest
            </button>
            <span style={{ fontSize: '0.72rem', color: 'var(--text-dim)' }}>
              Browser-local JSON export; no endpoints are probed.
            </span>
          </div>

          {manifest.surfaces.map(surface => (
            <div key={surface.id} style={{ marginTop: 12 }}>
              <div style={{ display: 'flex', justifyContent: 'space-between', gap: 8, flexWrap: 'wrap', marginBottom: 6 }}>
                <div style={{ fontSize: '0.82rem', color: 'var(--text, #E8E8E8)', fontWeight: 700 }}>
                  {surface.label}
                </div>
                <div style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: '0.7rem', color: 'var(--text-dim)' }}>
                  {surface.protocol}{surface.default_port ? ` :${surface.default_port}` : ''}
                </div>
              </div>
              {surface.routes.slice(0, 5).map(route => (
                <RouteRow key={`${route.method}-${route.path}`} route={route} />
              ))}
              {surface.commands.slice(0, 13).map(command => (
                <CommandRow key={command.name} command={command} />
              ))}
            </div>
          ))}

          {manifest.omissions.length > 0 && (
            <div style={{ marginTop: 12, fontSize: '0.75rem', color: 'var(--text-dim)', lineHeight: 1.5 }}>
              {manifest.omissions.map((item, index) => (
                <div key={`${item.path || item.surface}-${index}`}>
                  <span style={{ color: 'var(--yellow)' }}>Omitted:</span> {item.path || item.surface} - {item.reason}
                </div>
              ))}
            </div>
          )}

          <div style={{ marginTop: 12, fontSize: '0.72rem', color: 'var(--text-dim)', lineHeight: 1.45 }}>
            {manifest.limitations[0]}
          </div>
        </>
      )}
    </div>
  );
}

function InfoTile({ label, value }: { label: string; value: string }) {
  return (
    <div style={{
      border: '1px solid rgba(255,255,255,0.06)',
      borderRadius: 6,
      padding: '8px 10px',
      minWidth: 0,
    }}>
      <div style={{ color: 'var(--accent, #FAA500)', fontWeight: 700, fontSize: '0.95rem' }}>
        {value}
      </div>
      <div style={{ color: 'var(--text-dim)', fontSize: '0.65rem', textTransform: 'uppercase' }}>
        {label}
      </div>
    </div>
  );
}

export default ApiCompatibilityManifestCard;
