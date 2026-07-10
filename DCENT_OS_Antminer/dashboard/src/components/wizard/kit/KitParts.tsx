// DCENT_OS Setup Wizard — kit shell primitives.
//
// Byte-faithful structural recreations of `ui_kits/wizard/Wizard.jsx`:
//   • DCentralMolecule  — the 3-sphere molecule logo (welcome + header mark)
//   • StepRail           — the 13-step numbered horizontal rail with
//                          done/active states + connector lines
//   • StepFooter         — Back / Skip / Continue sticky footer
//   • RebootReconnectOverlay — the breathing-orb + twin-ring apply overlay
//
// These are pure presentational components — no api/state/flow. The real
// wiring lives in SetupWizard.tsx (every setup* api call preserved).

import React from 'react';

// ─── 12-step rail definition (kit Wizard.jsx STEPS, verbatim) ──────────
// id maps to a PRODUCTION concept; `optional` marks steps the production
// backend has no real endpoint for (Network) so the rail can honestly show
// it without a fabricated call. Mode is folded into Welcome in production,
// so the kit's separate "Mode" rail node points at the welcome step's
// mode-card sub-section (no extra api).
export interface KitStep {
  id: string;
  l: string;
  /** Honest marker: no real backend endpoint — informational/skippable. */
  optional?: boolean;
}

export const KIT_STEPS: KitStep[] = [
  { id: 'welcome',     l: 'Welcome' },
  { id: 'network',     l: 'Network', optional: true },
  { id: 'password',    l: 'Password' },
  { id: 'mode',        l: 'Mode' },
  { id: 'pool',        l: 'Pool' },
  { id: 'circuit',     l: 'Circuit' },
  { id: 'power',       l: 'Power' },
  //  added `psu_override` to SetupWizard STEPS at index 7 but NOT here,
  // desyncing the rail from index 7 on (wrong active node, dead Review node,
  // mis-targeted jumps). This node realigns KIT_STEPS 1:1 with STEPS by index.
  { id: 'psu_override', l: 'PSU Override' },
  { id: 'donation',    l: 'Donation' },
  // P2-4 (§4.E) added `home` to SetupWizard STEPS after `donation`; mirror it
  // here so KIT_STEPS stays 1:1 with STEPS by index (else the rail desyncs from
  // this node on — wrong active node, mis-targeted jumps).
  { id: 'home',        l: 'Home' },
  { id: 'calibration', l: 'Calibrate', optional: true },
  { id: 'name',        l: 'Name' },
  { id: 'review',      l: 'Review' },
];

// ─── 3-sphere molecule logo (kit DCentralMolecule + welcome SVG) ───────
export function DCentralMolecule({
  size = 26,
  glow = true,
  gradId = 'wz-mol-sph',
}: { size?: number; glow?: boolean; gradId?: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 64 64"
      aria-hidden="true"
      focusable="false"
      style={glow ? { filter: 'drop-shadow(0 0 8px rgba(250,103,0,.55))' } : undefined}
    >
      <defs>
        <radialGradient id={gradId} cx="38%" cy="28%" r="70%">
          <stop offset="0%" stopColor="#FFD47A" />
          <stop offset="55%" stopColor="#FAA500" />
          <stop offset="100%" stopColor="#FA6700" />
        </radialGradient>
      </defs>
      <line x1="22" y1="26" x2="42" y2="26" stroke="#0a0a0f" strokeWidth="3" />
      <line x1="32" y1="44" x2="22" y2="26" stroke="#0a0a0f" strokeWidth="3" />
      <line x1="32" y1="44" x2="42" y2="26" stroke="#0a0a0f" strokeWidth="3" />
      <circle cx="22" cy="26" r="10" fill={`url(#${gradId})`} />
      <circle cx="42" cy="26" r="10" fill={`url(#${gradId})`} />
      <circle cx="32" cy="44" r="10" fill={`url(#${gradId})`} />
    </svg>
  );
}

// ─── The signature 13-step numbered rail (kit StepRail, verbatim) ──────
interface StepRailProps {
  steps?: KitStep[];
  /** Index of the active step within KIT_STEPS. */
  activeIndex: number;
  /** Set of KIT_STEPS ids that are completed. */
  completed: Set<string>;
  /** Jump handler — only fires when the step is reachable. */
  onJump: (index: number) => void;
}

export function StepRail({ steps = KIT_STEPS, activeIndex, completed, onJump }: StepRailProps) {
  return (
    <nav className="wiz-rail" aria-label="Setup progress">
      {steps.map((s, i) => {
        const isActive = activeIndex === i;
        const isDone = completed.has(s.id);
        const canJump = isDone || i <= activeIndex;
        return (
          <button
            key={s.id}
            type="button"
            className={`wiz-step${isActive ? ' active' : ''}${isDone ? ' done' : ''}`}
            onClick={() => canJump && onJump(i)}
            disabled={!canJump}
            aria-current={isActive ? 'step' : undefined}
            aria-label={`${i + 1}. ${s.l}${isDone ? ' (done)' : ''}${isActive ? ' (current)' : ''}`}
            title={`${i + 1}. ${s.l}`}
          >
            <span className="wiz-step-num">{isDone && !isActive ? '✓' : i + 1}</span>
            <span className="wiz-step-label">{s.l}</span>
            {i < KIT_STEPS.length - 1 && <span className="wiz-step-line" />}
          </button>
        );
      })}
    </nav>
  );
}

// ─── Sticky footer (kit StepFooter, verbatim layout) ───────────────────
interface StepFooterProps {
  onBack?: () => void;
  onNext: () => void;
  nextLabel?: string;
  nextDisabled?: boolean;
  onSkip?: () => void;
  skipLabel?: string;
}

export function StepFooter({
  onBack,
  onNext,
  nextLabel = 'Continue →',
  nextDisabled = false,
  onSkip,
  skipLabel = 'Skip',
}: StepFooterProps) {
  return (
    <footer className="wiz-footer">
      <button
        type="button"
        className="wiz-btn ghost"
        onClick={onBack}
        disabled={!onBack}
        aria-label="Go back to previous step"
      >
        {'←'} Back
      </button>
      {onSkip && (
        <button
          type="button"
          className="wiz-btn ghost"
          onClick={onSkip}
          aria-label="Skip this step"
        >
          {skipLabel}
        </button>
      )}
      <span style={{ flex: 1 }} />
      <button
        type="button"
        className="wiz-btn primary"
        onClick={onNext}
        disabled={nextDisabled}
        aria-label="Continue to next step"
      >
        {nextLabel}
      </button>
    </footer>
  );
}

// ─── Reboot / reconnect overlay (kit RebootReconnectOverlay) ───────────
// Honest, NOT fabricated: phases are driven by the real reconnect poll in
// SetupWizard.tsx (api.getSetupStatus). 'reconnecting' stays until the
// daemon truthfully answers needs_setup=false; we never claim 'done' early.
export type RebootPhase = 'writing' | 'rebooting' | 'reconnecting' | 'done';

interface RebootOverlayProps {
  phase: RebootPhase;
  reconnectAttempts: number;
  modeLabel: string;
  /** Honest "what to finish next" rows, already computed by the shell. */
  todo?: string[];
}

const REBOOT_PHASES: { id: RebootPhase; l: string; sub: string }[] = [
  { id: 'writing',      l: 'Writing config',      sub: 'Persisting pools, mode, power, security …' },
  { id: 'rebooting',    l: 'Rebooting dcentrald', sub: 'Restarting the mining daemon and HAL.' },
  { id: 'reconnecting', l: 'Reconnecting',        sub: 'Polling for the dashboard to come back online.' },
  { id: 'done',         l: 'Online',              sub: 'DCENT_OS is up. Returning to your dashboard.' },
];

export function RebootReconnectOverlay({
  phase,
  reconnectAttempts,
  modeLabel,
  todo,
}: RebootOverlayProps) {
  const activeIdx = Math.max(0, REBOOT_PHASES.findIndex(p => p.id === phase));
  return (
    <div className="wiz-reboot-overlay" role="dialog" aria-modal="true" aria-label="Applying configuration">
      <div className="wiz-reboot">
        <div className="wiz-reboot-spin">
          <div className="wiz-reboot-orb" />
          <div className="wiz-reboot-ring" />
          <div className="wiz-reboot-ring two" />
        </div>
        <h3>Applying configuration</h3>
        <p>
          Your configuration has been saved. Don&apos;t unplug your miner — this takes
          about a minute. This page returns to the dashboard automatically once
          telemetry is back.
        </p>
        <ol className="wiz-reboot-steps">
          {REBOOT_PHASES.map((p, i) => (
            <li key={p.id} className={i < activeIdx ? 'done' : i === activeIdx ? 'now' : 'pending'}>
              <span className="wiz-reboot-step-dot" />
              <span>
                <strong>{p.l}</strong>
                <small>{p.sub}</small>
              </span>
            </li>
          ))}
        </ol>
        <div className="wiz-reboot-meta">
          <span>Mode: {modeLabel}</span>
          <span>Reconnect checks: {reconnectAttempts}</span>
        </div>
        {todo && todo.length > 0 && (
          <div className="wiz-reboot-todo">
            <div className="wiz-reboot-todo-title">What to finish next</div>
            {todo.map((t, i) => (
              <div key={i}>{t}</div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
