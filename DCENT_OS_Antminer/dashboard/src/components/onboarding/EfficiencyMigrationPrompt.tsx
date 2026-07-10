// EfficiencyMigrationPrompt — W1.3
//
// One-time prompt that surfaces the new efficiency-first autotuner
// default to existing users. DCENT_OS now defaults Heater + Mining
// (Standard) modes to `Efficiency` (J/TH-minimizing) instead of
// `Hashrate` (TH/s-greedy). Hacker mode keeps Hashrate.
//
// Rationale (DCENT_Bitcoin): home miners pay for electricity per kWh.
// A 1350 W S9 saves ~$175-236/year at $0.10/kWh when the autotuner
// targets J/TH instead of TH/s. Competitor firmwares (BraiinsOS, VNish)
// default to hashrate. We don't.
//
// Donation default = 2% (operator-locked). NOT touched by this prompt.
//
// Storage: persists the user's choice in localStorage so the prompt
// fires exactly once. Hacker mode is never prompted (it already opts
// into Hashrate by mode).

import { useEffect, useRef, useState } from 'react';
import type { RefObject } from 'react';
import { OverlayDialog } from '../common/OverlayDialog';
import { InfoDot } from '../common/Tooltip';

const STORAGE_KEY = 'dcent.onboarding.efficiency_migration.v1';
const STORAGE_VALUE_DISMISSED = 'dismissed';
const STORAGE_VALUE_ACCEPTED = 'accepted';
const STORAGE_VALUE_KEPT = 'kept_current';

// Slim contract from `/api/autotuner/target`. Mirrors the JSON shape
// returned by `get_autotuner_target` in `dcentrald-api/src/rest.rs`.
type AutotunerTargetResponse = {
  active: 'hashrate' | 'power' | 'efficiency' | 'hashrate_target';
  operating_mode: 'home' | 'standard' | 'hacker';
  mode_default: 'hashrate' | 'power' | 'efficiency' | 'hashrate_target';
  is_mode_default: boolean;
};

const TUNER_TARGETS = new Set(['hashrate', 'power', 'efficiency', 'hashrate_target']);
const OPERATING_MODES = new Set(['home', 'standard', 'hacker']);

function isAutotunerTargetResponse(value: unknown): value is AutotunerTargetResponse {
  if (!value || typeof value !== 'object') return false;
  const target = value as {
    active?: unknown;
    operating_mode?: unknown;
    mode_default?: unknown;
    is_mode_default?: unknown;
  };
  return typeof target.active === 'string'
    && typeof target.operating_mode === 'string'
    && typeof target.mode_default === 'string'
    && TUNER_TARGETS.has(target.active)
    && OPERATING_MODES.has(target.operating_mode)
    && TUNER_TARGETS.has(target.mode_default)
    && typeof target.is_mode_default === 'boolean';
}

async function fetchAutotunerTarget(): Promise<AutotunerTargetResponse | null> {
  try {
    const resp = await fetch('/api/autotuner/target', {
      headers: { Accept: 'application/json' },
      credentials: 'same-origin',
    });
    if (!resp.ok) {
      return null;
    }
    const data = await resp.json();
    return isAutotunerTargetResponse(data) ? data : null;
  } catch {
    return null;
  }
}

async function postAutotunerMode(modeBody: Record<string, unknown>): Promise<boolean> {
  // Best-effort hand-off to the existing config endpoint. We don't care
  // about the response shape here — we only persist that the user made
  // a choice in localStorage. If the backend write fails, the daemon's
  // own mode-aware default in `daemon.rs` (W1.3) still routes home
  // modes through Efficiency the next time the daemon restarts.
  try {
    const resp = await fetch('/api/config', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Accept: 'application/json',
      },
      credentials: 'same-origin',
      body: JSON.stringify({ autotuner: { tuner_mode: modeBody } }),
    });
    return resp.ok;
  } catch {
    return false;
  }
}

export function EfficiencyMigrationPrompt() {
  const [visible, setVisible] = useState(false);
  const [target, setTarget] = useState<AutotunerTargetResponse | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const keepButtonRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    let cancelled = false;

    // Already dismissed / answered? Don't prompt again.
    try {
      const stored = window.localStorage.getItem(STORAGE_KEY);
      if (stored === STORAGE_VALUE_DISMISSED
        || stored === STORAGE_VALUE_ACCEPTED
        || stored === STORAGE_VALUE_KEPT) {
        return;
      }
    } catch {
      // localStorage unavailable (private mode etc.) — bail silently.
      return;
    }

    (async () => {
      const data = await fetchAutotunerTarget();
      if (cancelled || !data) {
        return;
      }

      // Hacker mode already opted into Hashrate via `for_mode` —
      // no migration needed, no prompt.
      if (data.operating_mode === 'hacker') {
        try {
          window.localStorage.setItem(STORAGE_KEY, STORAGE_VALUE_DISMISSED);
        } catch {
          // ignore
        }
        return;
      }

      // Already on Efficiency — no migration needed.
      if (data.active === 'efficiency') {
        try {
          window.localStorage.setItem(STORAGE_KEY, STORAGE_VALUE_DISMISSED);
        } catch {
          // ignore
        }
        return;
      }

      // Home or Standard mode currently using non-efficiency target →
      // surface the prompt.
      setTarget(data);
      setVisible(true);
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  if (!visible || !target) {
    return null;
  }

  const acceptEfficiency = async () => {
    setSubmitting(true);
    try {
      // `Efficiency` TunerMode shape: { mode: "efficiency" }.
      // See `dcentrald-autotuner/src/config.rs::TunerMode::Efficiency`.
      const ok = await postAutotunerMode({ mode: 'efficiency' });
      try {
        window.localStorage.setItem(
          STORAGE_KEY,
          ok ? STORAGE_VALUE_ACCEPTED : STORAGE_VALUE_DISMISSED,
        );
      } catch {
        // ignore
      }
      setVisible(false);
    } finally {
      setSubmitting(false);
    }
  };

  const keepCurrent = () => {
    try {
      window.localStorage.setItem(STORAGE_KEY, STORAGE_VALUE_KEPT);
    } catch {
      // ignore
    }
    setVisible(false);
  };

  return (
    <OverlayDialog
      open={visible}
      onClose={keepCurrent}
      ariaLabel="Efficiency tuning migration"
      ariaLabelledBy="efficiency-migration-title"
      dismissible={false}
      initialFocusRef={keepButtonRef as RefObject<HTMLElement>}
      maxWidth={480}
      width="calc(100% - 32px)"
      chrome={false}
    >
      <div className="efficiency-migration-card ds-glass-strong">
        <h2
          id="efficiency-migration-title"
          className="efficiency-migration-title"
        >
          Switch to efficiency-first tuning?{' '}
          <InfoDot term="tuner_mode_efficiency" label="What efficiency-first tuning means" />
        </h2>
        <p className="efficiency-migration-lead">
          DCENT_OS now defaults to maximum efficiency for home miners.
          Instead of chasing raw TH/s, the autotuner minimizes
          <strong> J/TH</strong> — the watts you pay for per terahash.
        </p>
        <p className="efficiency-migration-detail">
          Your current setting:&nbsp;
          <code className="efficiency-migration-code">{target.active}</code>
          &nbsp;in&nbsp;
          <code className="efficiency-migration-code">{target.operating_mode}</code>
          &nbsp;mode. Hacker mode is unaffected.
        </p>
        <div className="efficiency-migration-actions">
          <button
            ref={keepButtonRef}
            type="button"
            className="ds-btn ghost"
            onClick={keepCurrent}
            disabled={submitting}
          >
            Keep current
          </button>
          <button
            type="button"
            className="ds-btn primary"
            onClick={acceptEfficiency}
            disabled={submitting}
          >
            {submitting ? 'Switching…' : 'Yes, switch to Efficiency'}
          </button>
        </div>
      </div>
    </OverlayDialog>
  );
}
