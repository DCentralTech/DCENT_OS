// DCENT_OS Setup Wizard — PSU Override step ( / Agent 1A, 2026-05-22;
// Agent 1F stock-APW12 enablement, 2026-05-22).
//
// Loki-attached vs Bare-APW3 vs Stock-APW12 fleet workflow. SE+DevOps Option
// B: a single firmware ships, and the operator declares (here at first-boot)
// which PSU hardware variant they fitted. Three scenarios per the EE-LOKI
// analysis:
//   (a) Loki spoof present → psu_override.enabled=true, hardware_variant=loki,
//       lenient probe responds (APW3+spoof talks SMBus to DCENT_OS).
//   (b) Bare APW3 → psu_override.enabled=true, hardware_variant=bare-apw3,
//       lenient probe falls through silently in ~200 ms; daemon proceeds in
//       PWR_CONTROL-only mode.
//   (c) Stock APW12 → psu_override.enabled=FALSE, hardware_variant=
//       stock-apw12; the daemon uses the canonical smart-APW12 handshake.
//
// Loki and Bare-APW3 write byte-identical Rust `[power.psu_override]` blocks
// (`enabled=true model="APW3" voltage_v=12.8`). Stock-APW12 sets
// `enabled=false` (omits model/voltage_v — the smart-APW12 path doesn't read
// them). The radio choice is recorded as `psu_hardware_variant` for fleet
// inventory and surfaced in /api/status by 's daemon (Agent 1B extends
// the /api/config/psu-override endpoint shape;  daemons that lack the
// field ignore it harmlessly because it is optional on the request body).
//
// Truth-contract via InfoBanner + glossary.ts ( pattern, load-bearing):
//   • "Loki = APW3 + spoof board" — declared, not autodetected
//   • "Bare APW3 = no spoof board" — Phase-0c lenient probe falls through
//     silently in ~200 ms; daemon proceeds in PWR_CONTROL-only mode
//   • "Stock APW12 = canonical smart-APW12 handshake" — psu_override is
//     disabled entirely, daemon uses the original Bitmain SMBus path
//   • The Loki and Bare-APW3 options share an identical Rust config
//     (psu_override.enabled=true); stock APW12 disables psu_override entirely
//
// Gate (enforced by SetupWizard's renderStep visibility): show only if
// `powerSource === 'grid' || 'hybrid'`. PSU override is meaningful only when
// AC grid is the power source.

import React from 'react';
import { InfoBanner } from '../common/InfoBanner';
import { glossaryText, type GlossaryKey } from '../../utils/glossary';

export type PsuHardwareVariant = 'loki' | 'bare-apw3' | 'stock-apw12';

// S3 UXFLOW-ONBOARD-1: anchor each radio's existing inline `detail` copy to its
// canonical glossary key. The radio is a `role="radio"` <button>, so an InfoDot
// (itself a <button>) can't nest inside it — instead the label uses the cheap
// CSS `data-tooltip` path, which still pulls verbatim from glossary.ts (no
// re-hardcode; the truth-contract vocabulary stays single-sourced).
const PSU_OPTION_GLOSSARY: Record<PsuHardwareVariant, GlossaryKey> = {
  loki: 'psu_override_loki',
  'bare-apw3': 'psu_override_bare_apw3',
  'stock-apw12': 'psu_override_stock_apw12',
};

interface PsuOption {
  id: PsuHardwareVariant;
  glyph: string;
  l: string;
  sub: string;
  detail: string;
}

const PSU_OPTIONS: PsuOption[] = [
  {
    id: 'loki',
    glyph: '⎇',
    l: 'Loki spoof board attached',
    sub: 'Test / bench unit',
    detail:
      'APW3 with a small daughter-board on i2c-0 @ 0x10 that spoofs the smart-APW12 SMBus handshake. DCENT_OS feeds the spoof watchdog at 1 Hz so the rail stays up. Chip-side voltage is identical to bare APW3.',
  },
  {
    id: 'bare-apw3',
    glyph: '⎓',
    l: 'Bare APW3 — no spoof board',
    sub: 'Recommended for new fleet builds',
    detail:
      'Modded APW3 without a Loki daughter-board. The Phase-0c lenient probe falls through silently in ~200 ms and the daemon proceeds in PWR_CONTROL-only mode. Rail stays at 12.8 V — identical to the Loki-attached case at the chip side.',
  },
  {
    id: 'stock-apw12',
    glyph: '✓',
    l: 'Stock APW12 PSU (smart)',
    sub: 'Unmodified factory unit',
    detail:
      'Genuine factory smart-APW12 with no Loki and no APW3 mod. No psu_override needed — the daemon runs the canonical smart-APW12 SMBus handshake (SetVoltage, Watchdog). Default for unmodified Antminer S19j Pro units.',
  },
];

export interface PsuOverrideStepValue {
  /**
   * Operator-declared PSU hardware variant. `null` means "not yet declared"
   * (default for first boot — wizard advance is allowed because the step is
   * skippable; daemon falls back to the baked default).
   */
  psuHardwareVariant: PsuHardwareVariant | null;
}

interface PsuOverrideStepProps {
  value: PsuOverrideStepValue;
  onChange: (next: PsuOverrideStepValue) => void;
}

export function PsuOverrideStep({ value, onChange }: PsuOverrideStepProps) {
  const selected = value.psuHardwareVariant;

  const pick = (id: PsuHardwareVariant) => {
    onChange({ psuHardwareVariant: id });
  };

  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">PSU Override</h2>
      <p className="wiz-lede">
        Tell DCENT_OS which PSU hardware you fitted to this miner. Both the
        Loki-attached and bare-APW3 paths run the same firmware configuration —
        this declaration records which variant is on the unit for fleet
        inventory and surfaces it in <code>/api/status</code>.
      </p>

      <InfoBanner tone="warn">
        Loki = APW3 + spoof board that talks SMBus to DCENT_OS. Bare APW3 = no
        spoof board; the daemon&apos;s lenient probe falls through silently in
        ~200 ms and proceeds in PWR_CONTROL-only mode. Stock APW12 = the
        original Bitmain smart PSU; daemon uses the canonical smart-APW12
        handshake. The Loki and Bare-APW3 options share an identical Rust
        config (<code>psu_override.enabled=true</code>); stock APW12 disables
        psu_override entirely. This choice records which hardware variant you
        fitted, surfaced in <code>/api/status</code> for fleet inventory.
      </InfoBanner>

      <div
        className="wiz-power-grid"
        role="radiogroup"
        aria-label="PSU hardware variant"
        style={{ marginTop: 16 }}
      >
        {PSU_OPTIONS.map(opt => {
          const isActive = selected === opt.id;
          return (
            <button
              key={opt.id}
              type="button"
              role="radio"
              aria-checked={isActive}
              className={
                `wiz-power-card${isActive ? ' active' : ''}` +
                `${isActive ? ' wizard-tile-selected-halo' : ''}`
              }
              onClick={() => pick(opt.id)}
              title={opt.detail}
            >
              <span className="wiz-power-icon" aria-hidden="true">
                {opt.glyph}
              </span>
              <strong data-tooltip={glossaryText(PSU_OPTION_GLOSSARY[opt.id])}>
                {opt.l}
              </strong>
              <span>{opt.sub}</span>
              <small>{opt.detail}</small>
            </button>
          );
        })}
      </div>

      {selected && (
        <div className="wiz-info" style={{ marginTop: 16 }}>
          Selected: <strong style={{ color: 'inherit' }}>{
            selected === 'loki' ? 'Loki spoof board' :
            selected === 'bare-apw3' ? 'Bare APW3 (no Loki)' :
            'Stock APW12'
          }</strong>
          {selected === 'stock-apw12' ? (
            <>
              . The Rust <code>[power.psu_override]</code> block will be
              written with <code>enabled=false</code>; the daemon will run the
              canonical smart-APW12 handshake. The variant tag is recorded for
              fleet inventory.
            </>
          ) : (
            <>
              . The Rust <code>[power.psu_override]</code> block will be
              written with <code>enabled=true, model=&quot;APW3&quot;,
              voltage_v=12.8</code>; the variant tag is recorded for fleet
              inventory.
            </>
          )}
        </div>
      )}

      {!selected && (
        <div className="wiz-info" style={{ marginTop: 16 }}>
          Skip this step to leave the baked default in place. The daemon will
          run the same opportunistic SMBus handshake either way; declaring the
          variant only sharpens fleet inventory.
        </div>
      )}
    </div>
  );
}

export default PsuOverrideStep;
