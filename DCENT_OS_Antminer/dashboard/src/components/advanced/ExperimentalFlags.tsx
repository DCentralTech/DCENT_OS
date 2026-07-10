import React, { useState, useEffect } from 'react';

export interface FlagDef {
  key: string;
  label: string;
  description: string;
  warning?: string;
  /** Whether this flag is waiting on daemon integration. Defaults to `true`
   * (coming-soon) for every flag in this panel — none affect dcentrald until
   * backend integration ships. A future dev flips this to `false` per-flag as
   * the wiring lands, which removes the "Coming soon" badge and enables the
   * toggle. Until then the toggle is disabled so it can't be mistaken for a
   * live control (FE-DEAD-2). */
  comingSoon?: boolean;
}

const HARDWARE_FLAGS: FlagDef[] = [
  {
    key: 'asicboost',
    label: 'AsicBoost',
    description: 'Enable version-rolling AsicBoost for ~20% hashrate improvement.',
    warning: 'Pool must support AsicBoost (most major pools do). MiscControl bit 7 will be set.',
  },
  {
    key: 'custom_fan_curve',
    label: 'Custom Fan Curve',
    description: 'Override PID-based fan control with a user-defined temperature/PWM curve.',
  },
  {
    key: 'aggressive_freq',
    label: 'Aggressive Frequency Stepping',
    description: 'Use larger frequency steps during autotuning for faster convergence at the cost of stability.',
    warning: 'May cause chip resets during tuning. Not recommended for production.',
  },
  {
    key: 'disable_thermal',
    label: 'Disable Thermal Throttling',
    description: 'Prevent automatic frequency reduction when temperature exceeds thresholds.',
    warning: 'DANGEROUS: Can cause permanent chip damage from overheating. Only for controlled testing environments.',
  },
  {
    key: 'per_chip_tuning',
    label: 'Per-Chip Tuning',
    description: 'Enable individual frequency tuning for each ASIC chip based on silicon quality.',
  },
  {
    key: 'voltage_offset',
    label: 'Voltage Offset Override',
    description: 'Allow manual voltage offset on top of auto-tuned voltage. Applied per-chain.',
    warning: 'Incorrect offsets can damage hash boards.',
  },
];

const DEVELOPER_FLAGS: FlagDef[] = [
  {
    key: 'verbose_logging',
    label: 'Verbose Logging',
    description: 'Enable debug-level logging in dcentrald. Increases log volume significantly.',
  },
  {
    key: 'debug_ws',
    label: 'Debug WebSocket Messages',
    description: 'Log all WebSocket frames to the console for debugging.',
  },
  {
    key: 'mock_data',
    label: 'Mock Data Mode',
    description: 'Use generated mock data instead of live hardware data. Useful for UI development.',
  },
  {
    key: 'dev_api',
    label: 'Developer API Endpoints',
    description: 'Enable extra debug API endpoints (/api/debug/*) that are normally hidden.',
  },
  {
    key: 'raw_register_no_confirm',
    label: 'Raw Register Access Without Confirmation',
    description: 'Skip confirmation dialogs for register read/write operations.',
    warning: 'Removes safety guardrail for FPGA register access. Use only if you know what you are doing.',
  },
];

const ALL_FLAGS = [...HARDWARE_FLAGS, ...DEVELOPER_FLAGS];
const STORAGE_KEY = 'dcentos-experimental-flags';

function loadFlags(): Record<string, boolean> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
      return parsed as Record<string, boolean>;
    }
  } catch { /* ignore */ }
  return {};
}

function saveFlags(flags: Record<string, boolean>) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(flags));
}

export function flagIsAvailable(flag: Pick<FlagDef, 'comingSoon'>): boolean {
  return flag.comingSoon === false;
}

export function normalizeExperimentalFlagState(
  flags: Record<string, boolean>,
  flagDefs: readonly FlagDef[],
): Record<string, boolean> {
  const normalized: Record<string, boolean> = {};
  for (const flag of flagDefs) {
    if (flagIsAvailable(flag) && flags[flag.key] === true) {
      normalized[flag.key] = true;
    }
  }
  return normalized;
}

export function countEnabledAvailableFlags(
  flags: Record<string, boolean>,
  flagDefs: readonly FlagDef[],
): number {
  return flagDefs.filter(flag => flagIsAvailable(flag) && flags[flag.key] === true).length;
}

function FlagToggle({ flag, enabled, onToggle }: { flag: FlagDef; enabled: boolean; onToggle: () => void }) {
  // FE-DEAD-2: roadmap-preview flags wait on daemon integration, so their
  // toggles stay disabled with a "Coming soon" badge
  // (no dead control that looks live — esp. the dangerous ones like "Disable
  // Thermal Throttling"). `comingSoon` defaults to true; a future dev sets it
  // false per-flag when the backend wiring lands.
  const available = flagIsAvailable(flag);
  const comingSoon = !available;
  const effectiveEnabled = available && enabled;
  return (
    <div className={`xf-flag-row${comingSoon ? ' is-coming-soon' : ''}`}>
      <div className="xf-flag-meta">
        <div className="xf-flag-name">
          {flag.label}
          {comingSoon && <span className="xf-flag-soon">Coming soon</span>}
        </div>
        <div className={`xf-flag-desc${flag.warning ? ' has-warn' : ''}`}>
          {flag.description}
        </div>
        {flag.warning && (
          <div className="xf-flag-warn">
            {flag.warning}
          </div>
        )}
      </div>
      <button
        onClick={comingSoon ? undefined : onToggle}
        disabled={comingSoon}
        className={`jd-toggle xf-toggle${effectiveEnabled ? ' is-on' : ''}${comingSoon ? ' is-coming-soon' : ''}`}
        aria-pressed={comingSoon ? undefined : effectiveEnabled}
        aria-disabled={comingSoon || undefined}
        title={comingSoon ? 'Coming soon: daemon integration is in development' : undefined}
        aria-label={
          comingSoon
            ? `${flag.label} (coming soon)`
            : `${flag.label} ${effectiveEnabled ? 'enabled' : 'disabled'}`
        }
      >
        <div className="jd-toggle-knob" />
      </button>
    </div>
  );
}

export function ExperimentalFlags() {
  const [flags, setFlags] = useState<Record<string, boolean>>(() =>
    normalizeExperimentalFlagState(loadFlags(), ALL_FLAGS),
  );

  useEffect(() => {
    saveFlags(flags);
  }, [flags]);

  const toggle = (key: string) => {
    const flag = ALL_FLAGS.find(f => f.key === key);
    if (!flag || !flagIsAvailable(flag)) return;
    setFlags(prev => normalizeExperimentalFlagState({ ...prev, [key]: !prev[key] }, ALL_FLAGS));
  };

  const enabledCount = countEnabledAvailableFlags(flags, ALL_FLAGS);

  const totalFlags = ALL_FLAGS.length;

  // SLOP-TOOL-06: in this build every FlagDef defaults `comingSoon = true`
  // (daemon integration is in development for all of them). Rather than ship a full panel of
  // permanently-disabled toggles — which reads like a wall of dead controls —
  // collapse to a single honest "none available" note. The full toggle grid
  // returns automatically the moment any flag flips `comingSoon: false`.
  const anyAvailable = ALL_FLAGS.some(flagIsAvailable);

  if (!anyAvailable) {
    return (
      <div className="hacker-inspector">
        <header className="hacker-inspector-header">
          <div className="hacker-inspector-title-group">
            <div className="hacker-inspector-eyebrow">// experimental flags</div>
            <h2 className="hacker-inspector-title">Lab Switches</h2>
          </div>
          <div className="hacker-inspector-actions">
            <span className="hacker-inspector-status neutral">NONE AVAILABLE</span>
          </div>
        </header>

        <div className="hacker-inspector-body">
          <div
            className="register-inspector ds-card-hover"
            style={{ display: 'flex', flexDirection: 'column', gap: 8 }}
          >
            <div className="xf-col-title">Experimental Flags</div>
            <p style={{ margin: 0, fontSize: '0.82rem', lineHeight: 1.5, color: 'var(--fg-secondary)' }}>
              None available in this build. {totalFlags} experimental flags are on the
              roadmap, and daemon integration is in development. Inert toggles are
              hidden until their integration ships, so nothing here can be mistaken
              for a live control.
            </p>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// experimental flags</div>
          <h2 className="hacker-inspector-title">Lab Switches</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${enabledCount > 0 ? 'warning' : ''}`}>
            {enabledCount}/{totalFlags} ENABLED
          </span>
        </div>
      </header>

      <div className="hacker-inspector-toolbar">
        <span className="xf-toolbar-note">
          Flags persist locally in your browser. Only daemon-available flags can be enabled.
        </span>
      </div>

      <div className="hacker-inspector-body">
      <div className="adv-grid-2">
        {/* Hardware experiments */}
        <div className="register-inspector ds-card-hover">
          <div className="xf-col-title">
            Hardware Experiments
          </div>
          {HARDWARE_FLAGS.map(flag => (
            <FlagToggle
              key={flag.key}
              flag={flag}
              enabled={flagIsAvailable(flag) && !!flags[flag.key]}
              onToggle={() => toggle(flag.key)}
            />
          ))}
        </div>

        {/* Developer options */}
        <div className="register-inspector ds-card-hover">
          <div className="xf-col-title">
            Developer Options
          </div>
          {DEVELOPER_FLAGS.map(flag => (
            <FlagToggle
              key={flag.key}
              flag={flag}
              enabled={flagIsAvailable(flag) && !!flags[flag.key]}
              onToggle={() => toggle(flag.key)}
            />
          ))}
        </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{HARDWARE_FLAGS.length} hardware</span>
          <span>{DEVELOPER_FLAGS.length} developer</span>
          <span>{enabledCount} active</span>
        </div>
      </footer>
    </div>
  );
}
