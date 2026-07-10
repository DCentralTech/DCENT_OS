//  (2026-05-13): per-platform overview card for the Operations Overview.
//
// Resolves the active platform from `systemInfo.model` via the existing
// `getModelProfile()` infra (`utils/modelProfiles.ts`). Surfaces operator-facing
// platform identity and rated specs alongside live API-reported actuals so a
// mixed-fleet operator can confirm at a glance:
//
//   - Which platform is talking (S9 / S17 / S19 Pro / S19j Pro / S21 / S19k Pro)
//   - Rated TH/s + power vs. their live readings
//   - BTU/h (always-on)
//   - Default mining freq + voltage (helpful when autotuner is off)
//   - PIC firmware byte hint (helps RE / install preflight troubleshooting)
//
// Renders gracefully when `systemInfo` is undefined (loading) and when the
// platform isn't in the registered profile table (unknown — operators on
// pre-supported hardware get a clear "platform not registered" message instead
// of a blank panel).

import { useMinerStore } from '../../store/miner';
import { formatHashrate } from '../../utils/format';
import {
  getModelProfile,
  isModelProfileProven,
  wattsToBtuPerHour,
  type ModelProfile,
} from '../../utils/modelProfiles';
import { useModelProfiles } from '../../hooks/useModelProfiles';
import {
  ExperimentalWarningBanner,
  SupportTierBadge,
  supportTierLabel,
} from '../common/SupportTierBadge';

const SECTION_TITLE = 'Platform Overview';

/** Render the BTU/h figure scaled to the actual mode-effective wattage. */
function modeEffectiveBtu(ratedW: number, fraction: number): number {
  return wattsToBtuPerHour(Math.round(ratedW * fraction));
}

function formatChainChipGeometry(profile: ModelProfile): string {
  if (profile.chipCountPerChain === null) {
    return `${profile.chainCount} chains; chip count in development`;
  }
  return `${profile.chainCount} × ${profile.chipCountPerChain} (${profile.chainCount * profile.chipCountPerChain} total)`;
}

function formatFrequency(mhz: number | null): string {
  return mhz === null ? 'In development' : `${mhz} MHz`;
}

function formatVoltage(volts: number | null): string {
  return volts === null ? 'In development' : `${volts.toFixed(2)} V`;
}

interface InfoRow {
  label: string;
  value: string;
  hint?: string;
}

export function PlatformOverviewCard() {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const status = useMinerStore(s => s.status);
  const mode = useMinerStore(s => s.mode) ?? 'standard';
  // W5.7: kick off the live silicon-profile fetch (`/api/profiles/silicon`)
  // so the chip-id source-of-truth from the daemon is loaded once per
  // session. Rendering still uses the embedded `getModelProfile()` snapshot
  // as the last-known-good fallback when the API is unreachable.
  const { liveChips, fellBackToSnapshot } = useModelProfiles();

  // Loading: render a 1-row skeleton-style placeholder so layout doesn't
  // jump in when systemInfo arrives. Honors graceful-degradation rule.
  if (!systemInfo) {
    return (
      <div className="section" data-testid="platform-overview-card">
        <div className="section-title">{SECTION_TITLE}</div>
        <div
          style={{
            background: 'var(--card-bg)',
            border: '1px solid var(--border)',
            borderRadius: 'var(--radius)',
            padding: 16,
            color: 'var(--text-dim)',
            fontSize: '0.85rem',
          }}
        >
          Awaiting platform identification from dcentrald…
        </div>
      </div>
    );
  }

  const profile = getModelProfile(systemInfo.model);

  // Unknown / unregistered platform: show what we know from the live API
  // without inventing per-mode targets. Operators on pre-registration
  // hardware (e.g. early Avalon integration, S23) get a clear hint.
  if (!profile) {
    return (
      <div className="section" data-testid="platform-overview-card">
        <div className="section-title section-title-inline">
          <span>{SECTION_TITLE}</span>
          <span
            data-testid="platform-overview-unregistered-pill"
            style={{
              fontSize: '0.65rem',
              fontWeight: 600,
              color: 'var(--text-dim)',
              textTransform: 'uppercase',
              letterSpacing: '0.04em',
              padding: '2px 8px',
              borderRadius: 4,
              border: '1px solid var(--border)',
            }}
          >
            Unregistered
          </span>
        </div>
        <div
          style={{
            background: 'var(--card-bg)',
            border: '1px solid var(--border)',
            borderRadius: 'var(--radius)',
            padding: 16,
          }}
        >
          <div style={{ fontWeight: 600, color: 'var(--text)', marginBottom: 6 }}>
            {systemInfo.model || 'Unknown platform'}
          </div>
          <div style={{ fontSize: '0.85rem', color: 'var(--text-dim)' }}>
            This platform isn&rsquo;t in the dashboard model registry yet —
            live readings still flow, but rated specs and per-mode targets
            aren&rsquo;t shown. Add a profile in
            <code style={{ marginLeft: 4, fontSize: '0.8rem' }}>
              utils/modelProfiles.ts
            </code>
            to surface them.
          </div>
          <div
            style={{
              marginTop: 12,
              display: 'grid',
              gridTemplateColumns: 'repeat(auto-fit, minmax(140px, 1fr))',
              gap: 12,
              fontSize: '0.8rem',
              color: 'var(--text)',
            }}
          >
            <div>
              <div style={{ color: 'var(--text-dim)', fontSize: '0.7rem' }}>
                Chip
              </div>
              <div style={{ fontWeight: 600 }}>
                {systemInfo.chip_type || '---'}
              </div>
            </div>
            <div>
              <div style={{ color: 'var(--text-dim)', fontSize: '0.7rem' }}>
                Chains
              </div>
              <div style={{ fontWeight: 600 }}>
                {systemInfo.chain_count ?? '---'}
              </div>
            </div>
            <div>
              <div style={{ color: 'var(--text-dim)', fontSize: '0.7rem' }}>
                Chips
              </div>
              <div style={{ fontWeight: 600 }}>
                {systemInfo.chip_count ?? '---'}
              </div>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Registered profile: render the full per-platform overview.
  const liveHashGhs = status?.hashrate_ghs ?? 0;
  const liveHashStr = liveHashGhs > 0 ? formatHashrate(liveHashGhs) : '---';
  const ratedHashStr = `${profile.ratedHashrateTh} TH/s`;

  // Mode-effective targets — match the active mode's powerFraction.
  const modeKey: 'heater' | 'standard' | 'hacker' =
    mode === 'heater' || mode === 'hacker' ? mode : 'standard';
  const modeFraction = profile.modes[modeKey].powerFraction;
  const modeBtu = modeEffectiveBtu(profile.ratedPowerW, modeFraction);
  const modeWatts = Math.round(profile.ratedPowerW * modeFraction);
  const modeSummary = profile.modes[modeKey].summary;

  const rows: InfoRow[] = [
    {
      label: 'Platform',
      value: profile.platformDisplay,
      hint: profile.platformKey,
    },
    {
      label: 'ASIC Chip',
      value: profile.chip,
    },
    {
      label: 'Chains × Chips',
      value: formatChainChipGeometry(profile),
    },
    {
      label: 'Rated Hashrate',
      value: ratedHashStr,
      hint: liveHashGhs > 0 ? `live ${liveHashStr}` : undefined,
    },
    {
      label: 'Rated Wall Power',
      value: `${profile.ratedPowerW} W`,
      hint: profile.thermalDesign,
    },
    {
      label: 'BTU/h (rated)',
      value: profile.ratedBtuPerHour.toLocaleString('en-US'),
    },
    {
      label: 'Default Frequency',
      value: formatFrequency(profile.defaultFrequencyMhz),
    },
    {
      label: 'Default Voltage',
      value: formatVoltage(profile.defaultVoltageV),
    },
    {
      label: 'PIC FW',
      value: profile.picFwByte,
    },
  ];

  // Mode badge label — use the operator-facing names.
  const modeLabel =
    modeKey === 'heater'
      ? 'Space Heater'
      : modeKey === 'hacker'
        ? 'Hacker'
        : 'Mining';

  // W5.7: surface "live" vs. "snapshot" sourcing so operators know whether
  // the chip-id set came from the daemon or from the embedded fallback.
  const profileSourceBadge = liveChips !== null && !fellBackToSnapshot
    ? { label: 'live', testid: 'platform-overview-source-live' }
    : { label: 'snapshot', testid: 'platform-overview-source-snapshot' };
  const profileIsProven = isModelProfileProven(profile);
  const tierLabel = supportTierLabel(profile.supportTier);

  return (
    <div className="section" data-testid="platform-overview-card">
      <div className="section-title section-title-inline">
        <span>{SECTION_TITLE}</span>
        <span
          data-testid="platform-overview-display-name"
          style={{
            fontSize: '0.7rem',
            color: 'var(--text-dim)',
            textTransform: 'none',
            letterSpacing: 'normal',
            fontWeight: 500,
          }}
        >
          {profile.displayName}
        </span>
        {!profileIsProven && (
          <SupportTierBadge
            tier={profile.supportTier}
            testId="platform-overview-development-pill"
            title="Rated specs shown from datasheet and D-Central reverse-engineering. This support tier is not part of the public-beta install set."
          />
        )}
        <span
          data-testid={profileSourceBadge.testid}
          title={
            profileSourceBadge.label === 'live'
              ? 'Profile chip set fetched from /api/profiles/silicon'
              : 'Profile fetch failed — using embedded last-known-good snapshot'
          }
          style={{
            fontSize: '0.6rem',
            fontWeight: 600,
            color: 'var(--text-dim)',
            textTransform: 'uppercase',
            letterSpacing: '0.05em',
            padding: '2px 6px',
            borderRadius: 4,
            border: '1px solid var(--border)',
          }}
        >
          {profileSourceBadge.label}
        </span>
      </div>

      <div
        className="platform-overview-card-body"
        style={{
          background: 'var(--card-bg)',
          border: '1px solid var(--border)',
          borderRadius: 'var(--radius)',
          padding: 16,
        }}
      >
        {!profileIsProven && (
          <ExperimentalWarningBanner
            tier={profile.supportTier}
            testId="platform-overview-development-note"
          >
            {tierLabel} profile. Specs are shown from Bitmain datasheets and
            D-Central reverse-engineering; this model is outside the public-beta
            install set until its promotion gate is completed. Live readings
            still flow when this unit reports them.
          </ExperimentalWarningBanner>
        )}
        <div
          className="platform-overview-grid"
          style={{
            display: 'grid',
            gridTemplateColumns: 'repeat(auto-fit, minmax(140px, 1fr))',
            gap: 12,
            marginBottom: 14,
          }}
        >
          {rows.map(row => (
            <div
              key={row.label}
              data-testid={`platform-row-${row.label.toLowerCase().replace(/[^a-z0-9]/g, '-')}`}
              style={{ minWidth: 0 }}
            >
              <div
                style={{
                  color: 'var(--text-dim)',
                  fontSize: '0.7rem',
                  textTransform: 'uppercase',
                  letterSpacing: '0.04em',
                  marginBottom: 2,
                }}
              >
                {row.label}
              </div>
              <div
                style={{
                  fontWeight: 600,
                  color: 'var(--text)',
                  fontSize: '0.92rem',
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                }}
              >
                {row.value}
              </div>
              {row.hint && (
                <div
                  style={{
                    fontSize: '0.7rem',
                    color: 'var(--text-dim)',
                    marginTop: 2,
                    fontFamily: 'JetBrains Mono, monospace',
                  }}
                >
                  {row.hint}
                </div>
              )}
            </div>
          ))}
        </div>

        <div
          data-testid="platform-overview-mode-summary"
          style={{
            paddingTop: 12,
            borderTop: '1px solid var(--border)',
            display: 'flex',
            flexWrap: 'wrap',
            gap: 12,
            alignItems: 'baseline',
          }}
        >
          <span
            style={{
              fontSize: '0.65rem',
              fontWeight: 700,
              color: 'var(--accent)',
              padding: '3px 10px',
              borderRadius: 12,
              background: 'rgba(247,147,26,0.12)',
              border: '1px solid rgba(247,147,26,0.35)',
              textTransform: 'uppercase',
              letterSpacing: '0.06em',
              flex: '0 0 auto',
            }}
          >
            {modeLabel} mode
          </span>
          <span
            style={{
              fontSize: '0.78rem',
              color: 'var(--text-dim)',
              fontFamily: 'JetBrains Mono, monospace',
              flex: '0 0 auto',
            }}
          >
            target ~{modeWatts} W &middot; ~{modeBtu.toLocaleString('en-US')} BTU/h
          </span>
          <span
            style={{
              fontSize: '0.82rem',
              color: 'var(--text)',
              flex: '1 1 240px',
              minWidth: 0,
              lineHeight: 1.45,
            }}
          >
            {modeSummary}
          </span>
        </div>
      </div>
    </div>
  );
}
