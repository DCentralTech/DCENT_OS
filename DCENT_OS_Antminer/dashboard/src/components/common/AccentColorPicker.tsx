import React, { useId, useState, useSyncExternalStore } from 'react';
import {
  PALETTE_PACKS,
  DEFAULT_PALETTE_ID,
  applyAccent,
  applyPalette,
  getAccent,
  getPaletteId,
  isValidHex,
  normalizeHex,
  subscribeAccent,
  type ContrastResult,
} from '../../theme/accent';

/**
 * Theme Studio — curated palette-pack gallery + live preview + custom builder.
 *
 * Evolution of the design-handoff theme picker (`polish.js` + Settings-panel
 * integration) into a richer studio. Production recreation:
 *
 *  - Pack gallery: 8 curated palette packs, each a card with a mini 3-stop
 *    gradient + label + blurb. The default D-Central pack is byte-faithful
 *    (injects nothing). Clicking a card commits it via `applyPalette`.
 *  - Live preview: an inert sample button + pill + dot. Hovering a pack card
 *    previews its candidate vars on the preview only (scoped style); clicking
 *    commits. The preview reflects the CURRENT theme by default.
 *  - Custom builder: the original hex text input PLUS a native color input;
 *    the contrast guard reports when it had to nudge the accent for AA.
 *
 * Mounted in each mode's Settings/Appearance surface; all mounted instances
 * stay in sync via the accent module's external store. The
 * `AccentColorPicker` named export is preserved as a thin back-compat wrapper
 * so the existing mount sites need no change.
 */
export function ThemeStudio() {
  const current = useSyncExternalStore(subscribeAccent, getAccent, getAccent);
  const activePaletteId = useSyncExternalStore(
    subscribeAccent,
    getPaletteId,
    getPaletteId,
  );
  const [draft, setDraft] = useState('');
  const [invalid, setInvalid] = useState(false);
  const [hoverHex, setHoverHex] = useState<string | null>(null);
  const [lastResult, setLastResult] = useState<ContrastResult | null>(null);
  const inputId = useId();
  const rawPreviewId = useId();
  // useId() yields ":r0:"-style ids whose colons break a CSS id selector.
  // Sanitize once and use the SAME string for the element id + the selector
  // so the scoped preview style reliably matches its stage.
  const previewId = `ts-preview-${rawPreviewId.replace(/[^a-zA-Z0-9_-]/g, '')}`;

  const submitCustom = (raw?: string) => {
    const v = (raw ?? draft).trim();
    if (!isValidHex(v)) {
      setInvalid(true);
      window.setTimeout(() => setInvalid(false), 1200);
      return;
    }
    const result = applyAccent(v);
    setLastResult(result);
    setDraft('');
  };

  // 3-stop preview gradient for a pack swatch (purely visual; lightens/darkens
  // the pack hex without importing the engine's private shade()).
  const packGradient = (hex: string) =>
    `linear-gradient(135deg, ${shadeHex(hex, 0.32)} 0%, ${hex} 50%, ${shadeHex(
      hex,
      -0.3,
    )} 100%)`;

  // When hovering a pack card, scope a candidate --accent onto the preview
  // region only so the user previews-before-commit without touching the app.
  const previewVars = hoverHex
    ? `#${previewId} .theme-studio-preview-stage { --accent: ${hoverHex}; --accent-gradient: ${packGradient(
        hoverHex,
      )}; --accent-glow: ${withAlpha(hoverHex, 0.16)}; --accent-border: ${withAlpha(
        hoverHex,
        0.32,
      )}; }`
    : '';

  return (
    <div className="accent-picker theme-studio">
      {/* Scoped preview-on-hover style — affects ONLY the preview stage. */}
      {previewVars ? (
        <style dangerouslySetInnerHTML={{ __html: previewVars }} />
      ) : null}

      <div className="accent-picker-head">
        <span className="accent-picker-eyebrow">Theme Studio</span>
        <span
          className="accent-picker-current"
          data-tooltip="Current accent. Affects every mode (Hacker keeps phosphor-green on the default); saved locally in this browser."
        >
          <span
            className="accent-picker-current-dot"
            style={{ background: 'var(--accent)' }}
            aria-hidden="true"
          />
          <span className="accent-picker-current-hex">
            {normalizeHex(current).toUpperCase()}
          </span>
        </span>
      </div>

      {/* ── Pack gallery ── */}
      <div
        className="theme-studio-gallery"
        role="group"
        aria-label="Palette packs"
      >
        {PALETTE_PACKS.map(pack => {
          const active = activePaletteId === pack.id;
          return (
            <button
              key={pack.id}
              type="button"
              className={`theme-studio-pack${active ? ' is-active' : ''}`}
              aria-pressed={active}
              onClick={() => {
                const result = applyPalette(pack.id);
                setLastResult(result);
              }}
              onMouseEnter={() => setHoverHex(pack.hex)}
              onMouseLeave={() => setHoverHex(null)}
              onFocus={() => setHoverHex(pack.hex)}
              onBlur={() => setHoverHex(null)}
            >
              <span
                className="theme-studio-pack-swatch"
                style={{ background: packGradient(pack.hex) }}
                aria-hidden="true"
              />
              <span className="theme-studio-pack-label">{pack.label}</span>
              <span className="theme-studio-pack-blurb">{pack.blurb}</span>
            </button>
          );
        })}
      </div>

      {/* ── Live preview ── */}
      <div className="theme-studio-preview" id={previewId}>
        <span className="theme-studio-preview-eyebrow">Preview</span>
        <div className="theme-studio-preview-stage" aria-hidden="true">
          <span className="ds-btn primary theme-studio-preview-btn">Mining</span>
          <span className="ds-chip theme-studio-preview-pill">
            <span className="ds-dot-live theme-studio-preview-dot" />
            Live
          </span>
        </div>
      </div>

      {/* ── Custom builder ── */}
      <div className="accent-picker-custom theme-studio-custom">
        <label className="accent-picker-custom-label" htmlFor={inputId}>
          Custom accent
        </label>
        <div className="accent-picker-custom-row theme-studio-custom-row">
          <input
            id={inputId}
            className={`accent-picker-input${invalid ? ' is-invalid' : ''}`}
            type="text"
            inputMode="text"
            spellCheck={false}
            maxLength={7}
            placeholder="#FAA500"
            value={draft}
            aria-invalid={invalid}
            aria-describedby={`${inputId}-hint`}
            onChange={e => setDraft(e.target.value)}
            onKeyDown={e => {
              if (e.key === 'Enter') {
                e.preventDefault();
                submitCustom();
              }
            }}
          />
          <input
            type="color"
            className="theme-studio-color-input"
            aria-label="Pick a custom accent color"
            value={isValidHex(draft) ? normalizeHex(draft) : normalizeHex(current)}
            onChange={e => submitCustom(e.target.value)}
          />
          <button
            type="button"
            className="accent-picker-apply"
            onClick={() => submitCustom()}
          >
            Apply
          </button>
        </div>
      </div>

      <p id={`${inputId}-hint`} className="accent-picker-hint">
        {lastResult?.adjusted
          ? `Adjusted to ${lastResult.applied.toUpperCase()} so button text stays readable (AA). `
          : 'Affects every mode. Saved locally per browser. '}
        Pick{' '}
        <button
          type="button"
          className="accent-picker-reset"
          onClick={() => {
            const result = applyPalette(DEFAULT_PALETTE_ID);
            setLastResult(result);
          }}
        >
          D-Central
        </button>{' '}
        to reset.
      </p>
    </div>
  );
}

/**
 * Back-compat wrapper. The three existing mount sites import
 * `AccentColorPicker`; keeping this named export unchanged means ZONE C
 * needs no edits in any other zone's files.
 */
export function AccentColorPicker() {
  return <ThemeStudio />;
}

/* ── tiny local color helpers (preview-only; the canonical math lives in
   accent.ts — these never feed the engine, only the inert preview gradient) ── */

function clamp255(n: number): number {
  return Math.max(0, Math.min(255, Math.round(n)));
}

function parseHex(hex: string): [number, number, number] {
  let h = (hex || '').trim().replace(/^#/, '');
  if (h.length === 3)
    h = h
      .split('')
      .map(c => c + c)
      .join('');
  const n = parseInt(h || '000000', 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/** Shade toward white (pct>0) or black (pct<0) by |pct|. */
function shadeHex(hex: string, pct: number): string {
  const [r, g, b] = parseHex(hex);
  const f = pct < 0 ? 0 : 255;
  const t = Math.abs(pct);
  const c = (v: number) => clamp255(v + (f - v) * t).toString(16).padStart(2, '0');
  return `#${c(r)}${c(g)}${c(b)}`;
}

function withAlpha(hex: string, a: number): string {
  const [r, g, b] = parseHex(hex);
  return `rgba(${r}, ${g}, ${b}, ${a})`;
}
