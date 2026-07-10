// Autotuner mode/preset slug → display map — STRING SOURCE (the canonical data
// layer). DCENT Design Language — Terminology Lexicon, TERM-1 (§1.1/§1.2).
// Source of truth: docs/design-system/DCENT_DESIGN_LANGUAGE/terminology-lexicon.md.
//
// WHY THIS LIVES IN THE STRING SOURCE (and not a .tsx):
//   The autotuner preset slug→display map is *server-driven* — the daemon sends
//   `requested_preset_display_name` / `effective_preset_display_name`
//   (api/types.ts :: AutotunerPolicyStatus) and the dashboard renders them. The
//   dashboard only owns a FALLBACK slug→display map (the inline `formatPolicyValue`
//   switch in components/standard/AutotunerCard.tsx) used when the daemon omits
//   the *_display_name fields. The lexicon's canonical-display decision is a
//   label/data decision, so it is emitted HERE as importable data; wave 2 wires
//   AutotunerCard's fallback switch to `presetDisplayName()` below. The firmware
//   slugs (the API contract) are NOT renamed — only the display strings are
//   canonicalized to Title Case.
//
// CANONICAL DISPLAY (lexicon §1.1, Title Case, EXACTLY these four words):
//   max_hashrate    → "Max Hashrate"
//   best_efficiency → "Best Efficiency"
//   target_watts    → "Target Watts"
//   target_temp     → "Target Temp"   (short form — NOT "Target temperature")
//
// OS keeps its own home/Hacker presets (lexicon §1.2, [OS-only]):
//   quiet_home → "Quiet Home" · balanced_home → "Balanced Home"
//   advanced_manual → "Advanced Manual"
//
// Back-compat: OS today ships the legacy slugs `hashrate_max` / `efficiency_max`
// / `watt_cap` (components/standard/AutotunerCard.tsx fallback + the daemon's
// dcentrald-autotuner/src/config.rs). The resolver maps BOTH the canonical slugs
// AND the legacy slugs to the canonical display, so a persisted config or a
// daemon that still serves a legacy slug resolves correctly with no churn.
//
// This module is PURE DATA + pure functions. It does NOT import the firmware
// config or any .tsx. It is additive: nothing imports it yet (wave 2 does).

import type { GlossaryKey } from './glossary';

export type CanonicalTunerMode =
  | 'max_hashrate'
  | 'best_efficiency'
  | 'target_watts'
  | 'target_temp';

export interface TunerModeDescriptor {
  /** Canonical slug (the API contract slug — NOT renamed). */
  slug: CanonicalTunerMode;
  /** Canonical Title-Case display name (lexicon §1.1). */
  display: string;
  /** One-line canonical description (lexicon §1.1). */
  description: string;
  /** Glossary key for the rich tooltip body. */
  glossaryKey: GlossaryKey;
}

/**
 * The 4 canonical [shared] autotuner modes, in lexicon order. These are the
 * names DCENT_OS and DCENT_axe both render. Display capitalization is Title
 * Case and is load-bearing — do not lower-case it.
 */
export const CANONICAL_TUNER_MODES: readonly TunerModeDescriptor[] = [
  {
    slug: 'max_hashrate',
    display: 'Max Hashrate',
    description:
      'Push frequency up until power or thermals cap it. Maximum hash power.',
    glossaryKey: 'tuner_mode_max_hashrate',
  },
  {
    slug: 'best_efficiency',
    display: 'Best Efficiency',
    description:
      'Find the lowest J/TH sweet spot, typically 60–70% of the max frequency.',
    glossaryKey: 'tuner_mode_best_efficiency',
  },
  {
    slug: 'target_watts',
    display: 'Target Watts',
    description:
      'Hit a power budget and squeeze the best hashrate under that ceiling.',
    glossaryKey: 'tuner_mode_target_watts',
  },
  {
    slug: 'target_temp',
    display: 'Target Temp',
    description:
      'Keep the chip at a chosen temperature — the autotuner raises frequency ' +
      'until it is just warm enough.',
    glossaryKey: 'tuner_mode_target_temp',
  },
] as const;

/**
 * OS-only home/Hacker presets (lexicon §1.2, [OS-only]). These keep their
 * existing slugs + display names and MUST NOT be cloned onto DCENT_axe. They
 * are NOT part of the canonical 4 — they are OS identity surface.
 */
export const OS_ONLY_PRESET_DISPLAY: Readonly<Record<string, string>> = {
  quiet_home: 'Quiet Home',
  balanced_home: 'Balanced Home',
  advanced_manual: 'Advanced Manual',
} as const;

/**
 * Slug → canonical Title-Case display, covering:
 *   - the 4 canonical shared slugs,
 *   - the OS legacy slugs (back-compat aliases) that map to the canonical 4,
 *   - the OS-only home/Hacker presets (kept as themselves).
 * A slug not present here resolves to `null` (the caller can humanize it or
 * show the raw slug) — this map is intentionally NOT exhaustive over every
 * daemon-emitted objective/limiting-factor token.
 */
export const PRESET_SLUG_TO_DISPLAY: Readonly<Record<string, string>> = {
  // Canonical shared modes (lexicon §1.1).
  max_hashrate: 'Max Hashrate',
  best_efficiency: 'Best Efficiency',
  target_watts: 'Target Watts',
  target_temp: 'Target Temp',
  // Back-compat aliases: OS legacy slugs → canonical display (lexicon §1.1
  // EMISSION NOTE — OS). The firmware slugs are not renamed in this phase, so
  // a daemon still serving these resolves to the canonical display.
  hashrate_max: 'Max Hashrate',
  efficiency_max: 'Best Efficiency',
  watt_cap: 'Target Watts',
  // OS-only home/Hacker presets (lexicon §1.2) — kept as themselves.
  ...OS_ONLY_PRESET_DISPLAY,
} as const;

/**
 * Resolve an autotuner preset/mode slug to its canonical display name.
 * Returns `null` for an unknown/empty slug so the caller can fall back to its
 * own humanization (or render the raw value) without this module guessing.
 *
 *  usage: AutotunerCard's `formatPolicyValue` fallback should prefer the
 * daemon's `*_display_name`, then `presetDisplayName(slug)`, then its own
 * non-preset cases (heat/offgrid/quiet/etc., which are not autotuner presets).
 */
export function presetDisplayName(slug?: string | null): string | null {
  if (!slug) return null;
  return PRESET_SLUG_TO_DISPLAY[slug] ?? null;
}

/**
 * Normalize a slug (canonical or legacy) to its canonical slug, or `null` if
 * it is not one of the 4 shared modes. Useful when the daemon serves a legacy
 * slug but the UI wants to key off the canonical id.
 */
export function canonicalTunerSlug(slug?: string | null): CanonicalTunerMode | null {
  switch (slug) {
    case 'max_hashrate':
    case 'hashrate_max':
      return 'max_hashrate';
    case 'best_efficiency':
    case 'efficiency_max':
      return 'best_efficiency';
    case 'target_watts':
    case 'watt_cap':
      return 'target_watts';
    case 'target_temp':
      return 'target_temp';
    default:
      return null;
  }
}
