// DCENT_OS Setup Wizard — Welcome step.
//
// Structural recreation of the kit `WelcomeStep` (ui_kits/wizard/Wizard.jsx):
// the 3-sphere molecule logo + "DCENT_OS" wordmark, the "Welcome to your
// miner" hero, the 3 value cards (Home fan cap / No mandatory dev fee /
// Flexible PSU + hashboard support), and the Quick Start / Guided setup / Skip
// CTAs. Mode selection now lives in its own kit ModeStep (the kit splits Welcome
// and Mode); this screen is purely the kit welcome.
//
// Real wiring preserved: onNext advances the wizard; onSkipAll opens the
// production skip-confirm (the real terminal-skip path — NOT a fake skip).

import React from 'react';

interface WelcomeStepProps {
  onQuickStart: () => void;
  onGuidedStart: () => void;
  onSkipAll: () => void;
}

export function WelcomeStep({ onQuickStart, onGuidedStart, onSkipAll }: WelcomeStepProps) {
  return (
    <div className="wiz-step-body welcome wizard-step-pane wizard-step-pane--welcome">
      <div className="wiz-logo">
        <svg width="72" height="72" viewBox="0 0 64 64" aria-hidden="true" focusable="false">
          <defs>
            <radialGradient id="wz-welcome-sph" cx="38%" cy="28%" r="70%">
              <stop offset="0%" stopColor="#FFD47A" />
              <stop offset="55%" stopColor="#FAA500" />
              <stop offset="100%" stopColor="#FA6700" />
            </radialGradient>
          </defs>
          <line x1="22" y1="26" x2="42" y2="26" stroke="#0a0a0f" strokeWidth="3" />
          <line x1="32" y1="44" x2="22" y2="26" stroke="#0a0a0f" strokeWidth="3" />
          <line x1="32" y1="44" x2="42" y2="26" stroke="#0a0a0f" strokeWidth="3" />
          <circle cx="22" cy="26" r="10" fill="url(#wz-welcome-sph)" />
          <circle cx="42" cy="26" r="10" fill="url(#wz-welcome-sph)" />
          <circle cx="32" cy="44" r="10" fill="url(#wz-welcome-sph)" />
        </svg>
        <div className="wiz-logo-text">
          DCENT<span style={{ color: 'var(--wz-accent)' }}>_</span>OS
        </div>
      </div>

      <h1 className="wiz-h1">Welcome to your miner</h1>

      <p className="wiz-lede">
        Open mining firmware for people who want real control. We&apos;ll walk through
        network, password, mode, and pool in about <strong>3 minutes</strong>. You own
        the hardware — these are your choices to make.
      </p>

      <div className="wiz-welcome-grid">
        <div className="wiz-welcome-card">
          <div className="wiz-welcome-card-icon" aria-hidden="true">↓</div>
          <h3>Home fan cap</h3>
          <p>Low fan command at boot. AM2/XIL noise needs tach/RPM proof before it is called quiet.</p>
        </div>
        <div className="wiz-welcome-card">
          <div className="wiz-welcome-card-icon" aria-hidden="true">⌫</div>
          <h3>No mandatory dev fee</h3>
          <p>Voluntary donation only. The &quot;DONATING&quot; indicator is visible whenever it&apos;s on.</p>
        </div>
        <div className="wiz-welcome-card">
          <div className="wiz-welcome-card-icon" aria-hidden="true">⚙</div>
          <h3>Flexible PSU + hashboard support</h3>
          <p>PSU-bypass by default; auto-detects common Zynq-era Antminer hashboards (BM1387/1397/1398/1362). Mixed-board rigs are validated per-route, not guaranteed for every combination.</p>
        </div>
      </div>

      <div className="wiz-welcome-cta">
        <button type="button" className="wiz-btn primary lg" onClick={onQuickStart}>
          Quick Start
        </button>
        <button type="button" className="wiz-btn" onClick={onGuidedStart}>
          Guided setup
        </button>
        <button type="button" className="wiz-btn ghost" onClick={onSkipAll}>
          Skip - I know what I&apos;m doing
        </button>
      </div>

      <p className="wiz-lede" style={{ marginTop: 4, fontSize: '.78rem' }}>
        Quick Start asks for pool and password, then saves Standard mode. Power,
        circuit, calibration, miner name, and home comfort are deferred. Donation
        settings stay at the miner default until you change them. Skipping uses
        safe defaults - no owner password and no circuit check.
      </p>

      <div style={{ marginTop: 8, color: 'var(--wz-fg-dim)', fontSize: '.78rem' }}>
        Powered by D-Central Technologies
      </div>
    </div>
  );
}
