/* ────────────────────────────────────────────────────────────────────────────
 * DCENT_OS — InfoBanner primitive   ( / Agent F1, 2026-05-17)  · D-02
 *
 * A shared, presentational, store-DECOUPLED banner. `common/AlertBanner.tsx`
 * is a store-driven GLOBAL banner — InfoBanner is the reusable local one that
 * Phase-3 agents drop in to replace the ≥6 inline-styled ad-hoc banner
 * duplications (AdvancedDashboard phone notice, SetupWizard skip, NandBackup
 * status, AdvancedOverview phone notice, and similar dashboard banners).
 *
 * Design-system styled — no hardcoded rgba; picks up F1 glass/glow tokens.
 *
 * Tones:  info | success | warn | danger | neutral
 * Slots:  icon (auto per tone, override allowed) · title · children (body) ·
 *         action (e.g. a button) · dismissible (× → onDismiss)
 *
 * Truth-contract note for Phase-3: when you migrate a banner whose copy is
 * load-bearing (W2 staged-vs-written, 9E pool state, restore gates), pass the
 * EXACT existing copy through `title`/children verbatim. InfoBanner only
 * changes the chrome, never the words.
 *
 * Usage:
 *   <InfoBanner tone="info" title="Heads up">Body text.</InfoBanner>
 *   <InfoBanner tone="warn" title="Staged, not written"
 *               action={<button className="ds-btn sm">Review</button>}
 *               dismissible onDismiss={() => setHidden(true)}>
 *     Firmware staged · NAND backup has NOT been written yet.
 *   </InfoBanner>
 * ──────────────────────────────────────────────────────────────────────────── */

import type { ReactNode } from 'react';
import { NOTICE_GLYPH } from '../../utils/noticeGlyphs';

export type InfoBannerTone = 'info' | 'success' | 'warn' | 'danger' | 'neutral';

interface InfoBannerProps {
  tone?: InfoBannerTone;
  /** Optional bold lead line. */
  title?: ReactNode;
  /** Body content. */
  children?: ReactNode;
  /** Override the auto glyph (pass null to hide it entirely). */
  icon?: ReactNode | null;
  /** Right-aligned action slot (button / link). */
  action?: ReactNode;
  /** Show a dismiss × button. Requires onDismiss to actually remove it. */
  dismissible?: boolean;
  onDismiss?: () => void;
  /** Extra class hook for Phase-3 layout tweaks (margins live with the caller). */
  className?: string;
  /** Compact single-line density. */
  dense?: boolean;
}

// Glyphs come from the shared canonical map (utils/noticeGlyphs) so all four
// notice primitives render the same tone glyph. InfoBanner's tone vocabulary
// IS the canonical set, so this is a direct lookup.
const TONE_GLYPH: Record<InfoBannerTone, string> = NOTICE_GLYPH;

// danger uses role="alert" (assertive); the rest are polite status regions.
const TONE_ROLE: Record<InfoBannerTone, 'alert' | 'status'> = {
  info: 'status',
  success: 'status',
  warn: 'status',
  danger: 'alert',
  neutral: 'status',
};

export function InfoBanner({
  tone = 'info',
  title,
  children,
  icon,
  action,
  dismissible,
  onDismiss,
  className,
  dense,
}: InfoBannerProps) {
  const showIcon = icon !== null;
  const glyph = icon ?? TONE_GLYPH[tone];
  return (
    <div
      className={
        `ds-infobanner ds-infobanner--${tone}` +
        (dense ? ' ds-infobanner--dense' : '') +
        (className ? ` ${className}` : '')
      }
      role={TONE_ROLE[tone]}
      aria-live={tone === 'danger' ? 'assertive' : 'polite'}
    >
      {showIcon && (
        <span className="ds-infobanner__icon" aria-hidden="true">
          {glyph}
        </span>
      )}
      <div className="ds-infobanner__content">
        {title != null && title !== '' && (
          <div className="ds-infobanner__title">{title}</div>
        )}
        {children != null && children !== '' && (
          <div className="ds-infobanner__body">{children}</div>
        )}
      </div>
      {action != null && <div className="ds-infobanner__action">{action}</div>}
      {dismissible && (
        <button
          type="button"
          className="ds-infobanner__dismiss"
          aria-label="Dismiss"
          onClick={onDismiss}
        >
          <span aria-hidden="true">×</span>
        </button>
      )}
    </div>
  );
}

export default InfoBanner;
