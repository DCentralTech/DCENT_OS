// noticeGlyphs — ONE canonical tone->glyph map for every notice/banner
// primitive (InfoBanner, AlertBanner, StatePanel, Toast).
//
// WHY THIS EXISTS:
//   Four notice primitives each defined their own tone->glyph map and they
//   DISAGREED for the same tone — danger was rendered as cross (U+2715) vs
//   'x' vs triangle (U+25B2); success as check (U+2713) vs '+'; info as 'i'
//   vs U+2139. Every glyph span in those components is aria-hidden, so this
//   is a purely visual-grammar consistency fix: the WORD/severity semantics
//   (role="alert" vs "status", aria-live, the message text) are unchanged —
//   only the decorative leading glyph is unified.
//
// CANONICAL SET (adopted from InfoBanner, the  design-system primitive):
//   info     i        success  U+2713 ✓     warn  !
//   danger   U+2715 ✕  neutral  U+2022 •
//
// Each component keeps its OWN tone ENUM/props working; only the glyph LOOKUP
// is centralized. `noticeGlyph()` accepts the various tone spellings the
// components use (critical/error -> danger, warning -> warn) and resolves to
// the canonical glyph. Components with a fixed tone vocabulary can also read
// NOTICE_GLYPH directly.

/** The five canonical notice tones (InfoBanner's vocabulary). */
export type NoticeTone = 'info' | 'success' | 'warn' | 'danger' | 'neutral';

/**
 * Canonical tone -> glyph. aria-hidden decoration only — never the source of
 * severity semantics. Do not fork these per-component; import from here.
 */
export const NOTICE_GLYPH: Readonly<Record<NoticeTone, string>> = {
  info: 'i',
  success: '✓', // ✓
  warn: '!',
  danger: '✕', // ✕
  neutral: '•', // •
} as const;

/**
 * Tone aliases used by the individual primitives, folded onto the canonical
 * five so each component's existing tone enum keeps working unchanged:
 *   - AlertBanner: critical / warning / info
 *   - StatePanel:  info / warning / danger / success / neutral
 *   - Toast:       success / error / warning / info
 * `critical` and `error` both map to the danger glyph; `warning` maps to warn.
 */
export type NoticeToneInput =
  | NoticeTone
  | 'critical'
  | 'error'
  | 'warning';

const TONE_ALIAS: Readonly<Record<string, NoticeTone>> = {
  info: 'info',
  success: 'success',
  warn: 'warn',
  warning: 'warn',
  danger: 'danger',
  critical: 'danger',
  error: 'danger',
  neutral: 'neutral',
} as const;

/**
 * Resolve any supported tone spelling to its canonical glyph. Unknown tones
 * fall back to the neutral glyph so a caller can never render `undefined`.
 */
export function noticeGlyph(tone: NoticeToneInput): string {
  const canonical = TONE_ALIAS[tone] ?? 'neutral';
  return NOTICE_GLYPH[canonical];
}
