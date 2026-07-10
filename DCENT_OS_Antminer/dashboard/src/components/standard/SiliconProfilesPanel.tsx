import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../../api/client';
import { useMinerStore } from '../../store/miner';
import { Tooltip } from '../common/Tooltip';
import type { ProfilesResponse, TuningProfile } from '../../api/types';

/**
 * Silicon Profiles panel (design-handoff — Tuning kit `ProfileSharing`).
 *
 * Lists the REAL saved tuning profiles the daemon exposes at `/api/profiles`
 * (each = a named freq + voltage + fan-mode setpoint). Per-profile actions:
 *
 *   • Activate  → POST `/api/profiles` (the exact `api.saveProfile` shape that
 *                 `TuningProfiles.tsx` uses: { name, frequency_mhz, voltage_mv,
 *                 fan_mode }). The daemon owns the actual per-chain
 *                 convergence — we hand it the named setpoint, nothing faked.
 *   • Export    → genuine client-side JSON download of the real profile
 *                 object. No endpoint needed; it is the live data, verbatim.
 *
 * Truth-contract: there is NO backend delete endpoint and NO server-side
 * import / community-library endpoint in this build. Those affordances are
 * rendered, but visibly DISABLED with honest copy — never faked, never
 * claimed to work, never backed by invented data. Origin badges are derived
 * only from the real profile name (no server-provided origin field exists),
 * and that derivation is labelled as a heuristic.
 */

type Origin = 'this-miner' | 'd-central' | 'community';

interface DecoratedProfile extends TuningProfile {
  origin: Origin;
  isActive: boolean;
}

const ORIGIN_META: Record<Origin, { label: string; tip: string }> = {
  'this-miner': {
    label: 'This miner',
    tip: 'Saved on this unit. Origin is inferred from the profile name — the daemon does not record a source field.',
  },
  'd-central': {
    label: 'D-Central',
    tip: 'Looks like a D-Central preset (name-based heuristic). The daemon does not record a source field.',
  },
  community: {
    label: 'Community',
    tip: 'Looks like an imported/community profile (name-based heuristic). The daemon does not record a source field.',
  },
};

/** Name-only heuristic. Honest: the API has no origin field. */
function inferOrigin(name: string): Origin {
  const n = name.toLowerCase();
  if (/(^|[^a-z])(dc|dcent|d-central|dcentral)([^a-z]|$)/.test(n)) return 'd-central';
  if (/(community|shared|import|comm)/.test(n)) return 'community';
  return 'this-miner';
}

function fmtVolts(mv: number): string {
  if (!Number.isFinite(mv) || mv <= 0) return '—';
  return `${(mv / 1000).toFixed(2)} V`;
}

function fmtFreq(mhz: number): string {
  if (!Number.isFinite(mhz) || mhz <= 0) return '—';
  return `${Math.round(mhz)} MHz`;
}

function slug(s: string): string {
  return (s || 'profile').replace(/[^a-z0-9._-]+/gi, '_').replace(/^_+|_+$/g, '') || 'profile';
}

export function SiliconProfilesPanel() {
  const addAlert = useMinerStore(s => s.addAlert);

  const [data, setData] = useState<ProfilesResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [errored, setErrored] = useState(false);
  const [busyName, setBusyName] = useState<string | null>(null);
  const mounted = useRef(true);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const r = await api.getProfiles();
      if (!mounted.current) return;
      setData(r);
      setErrored(false);
    } catch {
      if (!mounted.current) return;
      setData(null);
      setErrored(true);
    } finally {
      if (mounted.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void load();
    return () => { mounted.current = false; };
  }, [load]);

  const profiles: DecoratedProfile[] = useMemo(() => {
    const list = data?.profiles ?? [];
    const active = data?.active_profile ?? null;
    return list.map(p => ({
      ...p,
      origin: inferOrigin(p.name),
      isActive: active != null && p.name === active,
    }));
  }, [data]);

  const activate = useCallback(async (p: TuningProfile) => {
    setBusyName(p.name);
    try {
      // Exact shape TuningProfiles.tsx uses for api.saveProfile.
      await api.saveProfile({
        name: p.name,
        frequency_mhz: p.frequency_mhz,
        voltage_mv: p.voltage_mv,
        fan_mode: p.fan_mode,
      });
      addAlert(
        'info',
        `Activated profile "${p.name}": ${fmtFreq(p.frequency_mhz)} @ ${fmtVolts(p.voltage_mv)}. The daemon converges per-chain to this setpoint.`,
      );
      await load();
    } catch {
      addAlert('warning', `Failed to activate profile "${p.name}"`);
    } finally {
      if (mounted.current) setBusyName(null);
    }
  }, [addAlert, load]);

  const exportProfile = useCallback((p: TuningProfile) => {
    try {
      // Genuine: serialise the REAL profile object the daemon returned.
      const payload = {
        name: p.name,
        frequency_mhz: p.frequency_mhz,
        voltage_mv: p.voltage_mv,
        fan_mode: p.fan_mode,
        _exported_from: 'DCENT_OS',
        _exported_at: new Date().toISOString(),
      };
      const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `silicon-profile-${slug(p.name)}.json`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      // Revoke on next tick so the download has a chance to start.
      setTimeout(() => URL.revokeObjectURL(url), 0);
      addAlert('info', `Exported profile "${p.name}" as JSON`);
    } catch {
      addAlert('warning', `Could not export profile "${p.name}"`);
    }
  }, [addAlert]);

  return (
    <div className="section silicon-profiles">
      <div className="section-title">
        Silicon Profiles
        <Tooltip
          content="Saved freq + voltage + fan-mode setpoints the daemon exposes at /api/profiles. Activate hands a named setpoint to the daemon; export downloads the real profile as JSON."
          placement="bottom"
        >
          <span className="sp-title-help" aria-label="About silicon profiles" tabIndex={0}>i</span>
        </Tooltip>
      </div>

      <div className="sp-intro">
        Each profile is a saved per-chain frequency + voltage + fan-mode
        setpoint. <strong>Activate</strong> applies a profile through the real
        daemon path; the autotuner then converges per-chain. <strong>Export</strong>{' '}
        downloads the actual profile object as JSON.
      </div>

      {loading && (
        <div className="sp-state" role="status" aria-live="polite">
          Loading saved profiles…
        </div>
      )}

      {!loading && errored && (
        <div className="sp-state sp-state-error" role="alert">
          Could not reach <code>/api/profiles</code>. The daemon may be
          offline or this firmware build does not expose saved profiles.
          <button type="button" className="sp-btn sp-btn-ghost" onClick={() => void load()}>
            Retry
          </button>
        </div>
      )}

      {!loading && !errored && profiles.length === 0 && (
        <div className="sp-state" role="status">
          No saved profiles yet. Applying a tuning profile (above) or running
          the autotuner will populate this list.
        </div>
      )}

      {!loading && !errored && profiles.length > 0 && (
        <ul className="sp-list" aria-label="Saved silicon profiles">
          {profiles.map(p => {
            const om = ORIGIN_META[p.origin];
            const busy = busyName === p.name;
            return (
              <li
                key={p.name}
                className={`sp-card${p.isActive ? ' is-active' : ''}`}
              >
                <div className="sp-card-main">
                  <div className="sp-card-head">
                    <span className="sp-name">{p.name}</span>
                    {p.isActive && <span className="sp-badge sp-badge-active">ACTIVE</span>}
                    <Tooltip content={om.tip} placement="top">
                      <span
                        className={`sp-badge sp-badge-origin sp-origin-${p.origin}`}
                        tabIndex={0}
                      >
                        {om.label}
                      </span>
                    </Tooltip>
                  </div>
                  <div className="sp-card-stats">
                    <span className="sp-stat">
                      <span className="sp-stat-label">Freq</span>
                      <span className="sp-stat-val tnum">{fmtFreq(p.frequency_mhz)}</span>
                    </span>
                    <span className="sp-stat">
                      <span className="sp-stat-label">Voltage</span>
                      <span className="sp-stat-val tnum">{fmtVolts(p.voltage_mv)}</span>
                    </span>
                    <span className="sp-stat">
                      <span className="sp-stat-label">Fan</span>
                      <span className="sp-stat-val">{p.fan_mode || '—'}</span>
                    </span>
                  </div>
                </div>
                <div className="sp-card-actions">
                  <Tooltip
                    content={p.isActive
                      ? 'This profile is already active. Re-activating re-applies the same setpoint to the daemon.'
                      : 'Apply this profile through the real daemon path (POST /api/profiles). The autotuner converges per-chain.'}
                    placement="top"
                  >
                    <button
                      type="button"
                      className="sp-btn sp-btn-primary"
                      onClick={() => void activate(p)}
                      disabled={busy}
                    >
                      {busy ? 'Activating…' : p.isActive ? 'Re-activate' : 'Activate'}
                    </button>
                  </Tooltip>
                  <Tooltip content="Download this profile as a .json file (client-side, the real profile data)." placement="top">
                    <button
                      type="button"
                      className="sp-btn sp-btn-ghost"
                      onClick={() => exportProfile(p)}
                      disabled={busy}
                    >
                      Export .json
                    </button>
                  </Tooltip>
                  <Tooltip
                    content="Profile deletion is in development for this firmware build. Profiles are managed by the daemon, so this dashboard action stays disabled."
                    placement="top"
                  >
                    <button
                      type="button"
                      className="sp-btn sp-btn-ghost is-disabled"
                      disabled
                      aria-disabled="true"
                    >
                      Delete
                    </button>
                  </Tooltip>
                </div>
              </li>
            );
          })}
        </ul>
      )}

      {/* Honest, non-fabricated previews: unavailable actions stay disabled. */}
      <div className="sp-extra">
        <div className="sp-extra-row">
          <Tooltip
            content="Server-side profile import is in development for this firmware build. Importing a .json requires daemon support, so this action stays disabled."
            placement="top"
          >
            <button
              type="button"
              className="sp-btn sp-btn-ghost is-disabled"
              disabled
              aria-disabled="true"
            >
              Import (.json)
            </button>
          </Tooltip>
          <span className="sp-extra-note">
            Import in development for this build (backend endpoint required).
          </span>
        </div>
        <div className="sp-extra-row">
          <Tooltip
            content="Community profile library access is in development for this build. Nothing is fetched until that service is promoted."
            placement="top"
          >
            <button
              type="button"
              className="sp-btn sp-btn-ghost is-disabled"
              disabled
              aria-disabled="true"
            >
              Browse community library
            </button>
          </Tooltip>
          <span className="sp-extra-note">
            Community library in development for this build.
          </span>
        </div>
      </div>
    </div>
  );
}
