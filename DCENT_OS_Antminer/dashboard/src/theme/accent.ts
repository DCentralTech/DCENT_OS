/* ─────────────────────────────────────────────────────────────────────────
   DCENT_OS — runtime accent-color theme engine

   Ports the design-handoff `window.dcpTheme` engine (design-handoff bundle
   `polish.js`) into the production React app.

   Why this is NOT a straight port of polish.js:

   - polish.js set the derived custom-properties on `document.documentElement`
     (`:root`). The prototype could do that because it never rebinds `--accent`
     per mode. **Production does**: `.mode-standard`, `.mode-basic` and
     `.mode-hacker` each redeclare `--accent` (and its family) at the mode-class
     scope (`design-system.css` / `advanced.css`). A `:root` inline override is
     therefore shadowed inside every mode subtree. The production-correct fix is
     a managed <style> element whose `.mode-standard, .mode-basic, :root`
     selectors carry the SAME specificity as the per-mode rebinds but win on
     source order (appended last, at module-eval, before first paint).

   - The "D-Central" default (#FAA500) is special-cased to **remove** the
     override entirely, so the hand-tuned shipped palettes (Standard's
     brand-orange refs, Basic's warm tweaks) stay byte-faithful — zero
     regression on the default theme that ~all users see. Only a non-default
     accent injects derived values.

   - Hacker mode (`.mode-hacker`): the design spec is "affects every mode".
     Wave 4 honours that WITHOUT regressing Hacker's #00FF41 phosphor-green
     identity: the default "D-Central" accent injects NOTHING, so
     `.mode-hacker { --accent:#00FF41 }` (advanced.css) still wins and Hacker
     stays green out of the box. Only a NON-default accent (a preset or
     custom hex the operator explicitly chose) extends the override to
     `.mode-hacker` and recolors it — exactly what "affects every mode" with
     an opt-in means. Green is the default identity, not a hard lock.

   Persistence is browser-local (localStorage), exactly as the design spec
   states ("Affects every mode. Saved locally per browser."). It is NOT a
   daemon config — no REST/store contract changes.
   ───────────────────────────────────────────────────────────────────────── */

export interface AccentPreset {
  id: string;
  label: string;
  hex: string;
}

export const ACCENT_PRESETS: AccentPreset[] = [
  { id: 'orange', label: 'D-Central', hex: '#FAA500' },
  { id: 'red', label: 'Bloody Red', hex: '#DC2626' },
  { id: 'green', label: 'Matrix', hex: '#00FF41' },
  { id: 'cyan', label: 'Neon Cyan', hex: '#06D6F4' },
  { id: 'purple', label: 'Neon Purple', hex: '#A855F7' },
  { id: 'magenta', label: 'Magenta', hex: '#EC4899' },
];

/* ─────────────────────────────────────────────────────────────────────────
   Theme Studio — curated palette packs (Wave 9, ZONE C headline feature).

   A "palette pack" is a named, hand-tuned accent identity. Selecting one
   drives the SAME derived accent custom-property family that a raw custom
   hex drives (see PALETTE LAYER INVARIANT below), plus a friendly label +
   blurb for the gallery UI. The default `dcentral` pack is byte-faithful:
   its hex IS the shipped DEFAULT_ACCENT, so applying it injects NOTHING
   (paint() short-circuits on the default), keeping every shipped per-mode
   palette pixel-identical for the ~all users who never touch the studio.
   ───────────────────────────────────────────────────────────────────────── */
export interface PalettePack {
  id: string;
  label: string;
  /** Short gallery blurb (one line). */
  blurb: string;
  /** The accent hex this pack resolves to. */
  hex: string;
  /** Marks the byte-faithful shipped default (injects nothing). */
  isDefault?: boolean;
}

export const PALETTE_PACKS: PalettePack[] = [
  {
    id: 'dcentral',
    label: 'D-Central',
    blurb: 'The shipped brand orange. Byte-faithful default.',
    hex: '#FAA500',
    isDefault: true,
  },
  { id: 'ember', label: 'Ember', blurb: 'Hot-coal orange-red — space-heater warmth.', hex: '#FF6A1A' },
  { id: 'solar', label: 'Solar', blurb: 'Bright amber-gold, high-noon energy.', hex: '#FFB703' },
  { id: 'cyan', label: 'Cyan', blurb: 'Cool neon cyan, cold-aisle clarity.', hex: '#06D6F4' },
  { id: 'matrix', label: 'Matrix', blurb: 'Phosphor-green terminal glow.', hex: '#00FF41' },
  { id: 'synthwave', label: 'Synthwave', blurb: 'Retro magenta-pink neon.', hex: '#EC4899' },
  { id: 'violet', label: 'Violet', blurb: 'Electric violet, deep-night calm.', hex: '#A855F7' },
  { id: 'crimson', label: 'Crimson', blurb: 'Bold alert-red, maximum urgency.', hex: '#F2364E' },
];

export const DEFAULT_ACCENT = '#FAA500';
export const DEFAULT_PALETTE_ID = 'dcentral';
const STORAGE_KEY = 'dcent_accent_v1';
const PALETTE_STORAGE_KEY = 'dcent_palette_v1';
const STYLE_EL_ID = 'dcent-accent-theme';

/** Result of resolving a requested accent through the contrast guard. */
export interface ContrastResult {
  /** The hex the caller asked for. */
  requested: string;
  /** The hex actually applied (== requested unless nudged for AA). */
  applied: string;
  /** True if the guard had to nudge `requested` to clear AA. */
  adjusted: boolean;
  /** Contrast ratio of `--accent-ink` text on the applied accent. */
  ratio: number;
}

interface Rgb {
  r: number;
  g: number;
  b: number;
}

export function isValidHex(s: string): boolean {
  const v = (s || '').trim();
  return /^#?[0-9a-f]{6}$/i.test(v) || /^#?[0-9a-f]{3}$/i.test(v);
}

export function normalizeHex(s: string): string {
  let h = (s || '').trim().replace(/^#/, '');
  if (h.length === 3) h = h.split('').map(c => c + c).join('');
  return '#' + h.toLowerCase();
}

function hexToRgb(hex: string): Rgb {
  const n = parseInt(normalizeHex(hex).slice(1), 16);
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
}

function rgbToHex({ r, g, b }: Rgb): string {
  const c = (n: number) =>
    Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, '0');
  return `#${c(r)}${c(g)}${c(b)}`;
}

/** Shade toward white (pct>0) or black (pct<0) by |pct| — polish.js algorithm. */
function shade({ r, g, b }: Rgb, pct: number): Rgb {
  const f = pct < 0 ? 0 : 255;
  const t = Math.abs(pct);
  return { r: r + (f - r) * t, g: g + (f - g) * t, b: b + (f - b) * t };
}

function rgba({ r, g, b }: Rgb, a: number): string {
  return `rgba(${Math.round(r)}, ${Math.round(g)}, ${Math.round(b)}, ${a})`;
}

function isDefault(hex: string): boolean {
  return normalizeHex(hex) === normalizeHex(DEFAULT_ACCENT);
}

/* ─────────────────────────────────────────────────────────────────────────
   [P0] CONTRAST GUARD — WCAG AA legibility for accent-driven surfaces.

   Two derived vars come out of this guard (consumed via CSS fallbacks so the
   frozen tokens.css is never touched — buttons read var(--accent-ink, …)):

   - `--accent-ink`: ink color for text/icons that sit ON the accent (e.g. a
     filled `.ds-btn.primary` caption). We pick whichever of near-black
     (#1a0f00) or white (#ffffff) clears AA (4.5:1) on the accent. If NEITHER
     clears (mid-tone accents like a muddy orange), we nudge the accent itself
     in HSL — at most 6 small lightness steps — until the better ink option
     reaches AA, and report `adjusted=true`.

   No deps beyond the existing hexToRgb. Pure functions, ~contained here.
   ───────────────────────────────────────────────────────────────────────── */

const INK_DARK = '#1a0f00';
const INK_LIGHT = '#ffffff';
const AA_TEXT = 4.5; // WCAG AA normal text

/** sRGB channel (0-255) → linear-light component. */
function srgbToLin(c: number): number {
  const s = c / 255;
  return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
}

/** Relative luminance per WCAG 2.x. */
function relLum(rgb: Rgb): number {
  return (
    0.2126 * srgbToLin(rgb.r) +
    0.7152 * srgbToLin(rgb.g) +
    0.0722 * srgbToLin(rgb.b)
  );
}

/** WCAG contrast ratio between two hex colors. */
function contrast(a: string, b: string): number {
  const la = relLum(hexToRgb(a));
  const lb = relLum(hexToRgb(b));
  const hi = Math.max(la, lb);
  const lo = Math.min(la, lb);
  return (hi + 0.05) / (lo + 0.05);
}

interface Hsl {
  h: number;
  s: number;
  l: number;
}

function hexToHsl(hex: string): Hsl {
  const { r, g, b } = hexToRgb(hex);
  const rn = r / 255;
  const gn = g / 255;
  const bn = b / 255;
  const max = Math.max(rn, gn, bn);
  const min = Math.min(rn, gn, bn);
  const l = (max + min) / 2;
  let h = 0;
  let s = 0;
  const d = max - min;
  if (d !== 0) {
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    switch (max) {
      case rn:
        h = (gn - bn) / d + (gn < bn ? 6 : 0);
        break;
      case gn:
        h = (bn - rn) / d + 2;
        break;
      default:
        h = (rn - gn) / d + 4;
        break;
    }
    h /= 6;
  }
  return { h: h * 360, s: s * 100, l: l * 100 };
}

function hslToHex({ h, s, l }: Hsl): string {
  const sn = Math.max(0, Math.min(100, s)) / 100;
  const ln = Math.max(0, Math.min(100, l)) / 100;
  const k = (n: number) => (n + h / 30) % 12;
  const a = sn * Math.min(ln, 1 - ln);
  const f = (n: number) => {
    const kn = k(n);
    return ln - a * Math.max(-1, Math.min(kn - 3, Math.min(9 - kn, 1)));
  };
  return rgbToHex({ r: f(0) * 255, g: f(8) * 255, b: f(4) * 255 });
}

/** Best ink (dark vs light) for `hex` + its achieved contrast ratio. */
function bestInk(hex: string): { ink: string; ratio: number } {
  const dark = contrast(hex, INK_DARK);
  const light = contrast(hex, INK_LIGHT);
  return dark >= light
    ? { ink: INK_DARK, ratio: dark }
    : { ink: INK_LIGHT, ratio: light };
}

/**
 * Resolve a requested accent so a button caption ON it clears AA, nudging
 * lightness in HSL by ≤6 small steps if neither ink option reaches 4.5:1.
 * Returns the (possibly adjusted) accent + the ink + the achieved ratio.
 */
function guardAccent(requested: string): {
  result: ContrastResult;
  ink: string;
} {
  const req = normalizeHex(requested);
  let best = bestInk(req);
  if (best.ratio >= AA_TEXT) {
    return {
      result: { requested: req, applied: req, adjusted: false, ratio: best.ratio },
      ink: best.ink,
    };
  }
  // Neither ink clears AA on the raw accent — nudge lightness toward whichever
  // direction improves the better ink option. Dark ink wants a LIGHTER accent;
  // light ink wants a DARKER accent. Try ≤6 steps of 6% lightness each.
  const hsl = hexToHsl(req);
  // Pick nudge direction from whichever ink is currently closer to passing.
  const dir = contrast(req, INK_DARK) >= contrast(req, INK_LIGHT) ? +1 : -1;
  let applied = req;
  for (let step = 1; step <= 6; step++) {
    const candidate = hslToHex({ ...hsl, l: hsl.l + dir * 6 * step });
    const cb = bestInk(candidate);
    applied = candidate;
    best = cb;
    if (cb.ratio >= AA_TEXT) break;
  }
  return {
    result: {
      requested: req,
      applied,
      adjusted: normalizeHex(applied) !== req,
      ratio: best.ratio,
    },
    ink: best.ink,
  };
}

/* ═════════════════════════════════════════════════════════════════════════
   PALETTE LAYER INVARIANT
   ─────────────────────────────────────────────────────────────────────────
   A palette / accent override drives ONLY the accent family:
     accent · accent-light · accent-deep · accent-hover · sphere-{hi,mid,lo}
     · accent-glow{,-hi} · accent-border · accent-gradient · accent-rgb
     · accent-deep-rgb · accent-tint-* · accent-shadow-* · accent-deep-10
     · shadow-btn · shadow-glow{,-hi} · glow-orange{,-strong}
   …PLUS the contrast-guard ink var (--accent-ink).

   It MUST NEVER emit --bg* / --card* / --surface* / --text* / --fg* /
   --radius* / --space* / or any status color (--green/--red/--yellow/…).
   Those are structural/semantic tokens owned by the frozen token files; the
   theme studio recolors the BRAND, not the chrome or the truth-signal colors.
   Anyone extending this block: keep it inside the accent family above.
   ═════════════════════════════════════════════════════════════════════════ */

/**
 * Build the full derived custom-property block for a non-default accent.
 * Runs the requested hex through the contrast guard first (so the emitted
 * accent is the AA-safe `applied` value), then derives the accent family +
 * the two ink vars from it. Returns both the CSS text and the ContrastResult
 * so callers can surface adjustment hints in the UI.
 *
 * Resting opacities mirror the shipped Standard/token values so a custom
 * accent reads as proportionally as the orange it replaces.
 */
function buildVarBlock(hex: string): { css: string; result: ContrastResult } {
  const { result, ink } = guardAccent(hex);
  const applied = result.applied;
  const rgb = hexToRgb(applied);
  const hi = shade(rgb, 0.3);
  const deep = shade(rgb, -0.32);
  const hover = shade(rgb, 0.15);
  const hiHex = rgbToHex(hi);
  const deepHex = rgbToHex(deep);
  const hoverHex = rgbToHex(hover);
  const deepRgb = hexToRgb(deepHex);

  const css = [
    `--accent: ${applied};`,
    `--accent-light: ${hiHex};`,
    `--accent-deep: ${deepHex};`,
    `--accent-hover: ${hoverHex};`,
    `--sphere-hi: ${hiHex};`,
    `--sphere-mid: ${applied};`,
    `--sphere-lo: ${deepHex};`,
    `--accent-glow: ${rgba(rgb, 0.16)};`,
    `--accent-glow-hi: ${rgba(rgb, 0.34)};`,
    `--accent-border: ${rgba(rgb, 0.32)};`,
    `--accent-gradient: linear-gradient(135deg, ${hiHex} 0%, ${applied} 50%, ${deepHex} 100%);`,
    `--accent-rgb: ${rgb.r}, ${rgb.g}, ${rgb.b};`,
    `--accent-deep-rgb: ${deepRgb.r}, ${deepRgb.g}, ${deepRgb.b};`,
    `--accent-tint-08: ${rgba(rgb, 0.08)};`,
    `--accent-tint-14: ${rgba(rgb, 0.14)};`,
    `--accent-shadow-28: ${rgba(rgb, 0.28)};`,
    `--accent-shadow-55: ${rgba(rgb, 0.55)};`,
    `--accent-deep-10: ${rgba(deepRgb, 0.1)};`,
    `--shadow-btn: 0 4px 14px ${rgba(rgb, 0.32)};`,
    `--shadow-glow: 0 0 20px ${rgba(rgb, 0.16)};`,
    `--shadow-glow-hi: 0 0 24px ${rgba(rgb, 0.34)};`,
    `--glow-orange: 0 0 18px ${rgba(rgb, 0.18)};`,
    `--glow-orange-strong: 0 0 30px ${rgba(rgb, 0.34)};`,
    // Contrast-guard ink var (consumed via CSS fallbacks; not in tokens.css):
    `--accent-ink: ${ink};`,
  ].join(' ');

  return { css, result };
}

function styleEl(): HTMLStyleElement {
  let el = document.getElementById(STYLE_EL_ID) as HTMLStyleElement | null;
  if (!el) {
    el = document.createElement('style');
    el.id = STYLE_EL_ID;
    // Appended to <head> last → wins the source-order tie against the
    // .mode-standard / .mode-basic per-mode rebinds (same specificity).
    document.head.appendChild(el);
  }
  return el;
}

function canPaintDocument(): boolean {
  return typeof document !== 'undefined' && Boolean(document.head);
}

let listeners = new Set<() => void>();
let currentHex = DEFAULT_ACCENT;
/** Currently-active palette pack id, or null when a raw custom hex is active. */
let currentPaletteId: string | null = DEFAULT_PALETTE_ID;
/** Last contrast-guard result (null while on the byte-faithful default). */
let lastContrast: ContrastResult | null = null;

function readStoredAccent(): string {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v && isValidHex(v)) return normalizeHex(v);
  } catch {
    /* localStorage unavailable — fall back to default */
  }
  return DEFAULT_ACCENT;
}

function readStoredPaletteId(): string | null {
  try {
    const v = localStorage.getItem(PALETTE_STORAGE_KEY);
    if (v && PALETTE_PACKS.some(p => p.id === v)) return v;
  } catch {
    /* localStorage unavailable */
  }
  return null;
}

function packById(id: string): PalettePack | undefined {
  return PALETTE_PACKS.find(p => p.id === id);
}

function paint(hex: string): void {
  if (!canPaintDocument()) {
    lastContrast = isDefault(hex) ? null : buildVarBlock(hex).result;
    return;
  }

  const el = styleEl();
  if (isDefault(hex)) {
    // Reset: clear the override so the shipped per-mode palettes win. The
    // byte-faithful default injects NOTHING (no contrast result either).
    el.textContent = '';
    lastContrast = null;
    return;
  }
  // A non-default accent was explicitly chosen → it "affects every mode"
  // (design spec), Hacker included. Default short-circuits above, so
  // Hacker's shipped phosphor-green is only ever overridden on opt-in.
  const { css, result } = buildVarBlock(hex);
  lastContrast = result;
  el.textContent =
    `:root, .mode-standard, .mode-basic, .mode-hacker { ${css} }`;
}

/**
 * Set + persist + repaint + notify subscribers from a raw hex.
 * Clears the active palette id (a hand-typed hex is "custom", not a pack)
 * unless it happens to match a pack — applyPalette routes that case.
 * Returns the contrast-guard result (or null for the byte-faithful default).
 */
export function applyAccent(input: string): ContrastResult | null {
  const hex = isValidHex(input) ? normalizeHex(input) : DEFAULT_ACCENT;
  currentHex = hex;
  // If the hex matches a known pack, keep that pack's identity; else custom.
  const match = PALETTE_PACKS.find(p => normalizeHex(p.hex) === hex);
  currentPaletteId = match ? match.id : null;
  paint(hex);
  try {
    localStorage.setItem(STORAGE_KEY, hex);
    if (match) localStorage.setItem(PALETTE_STORAGE_KEY, match.id);
    else localStorage.removeItem(PALETTE_STORAGE_KEY);
  } catch {
    /* non-fatal — accent just won't survive reload in this browser */
  }
  listeners.forEach(fn => fn());
  return lastContrast;
}

/**
 * Apply a curated palette pack by id (the Theme Studio gallery path).
 * Resolves the pack to its hex, drives the same accent engine as applyAccent,
 * and persists both the palette id and the resolved accent hex. Unknown ids
 * fall back to the default pack. Returns the contrast-guard result.
 */
export function applyPalette(id: string): ContrastResult | null {
  const pack = packById(id) ?? packById(DEFAULT_PALETTE_ID)!;
  currentHex = normalizeHex(pack.hex);
  currentPaletteId = pack.id;
  paint(currentHex);
  try {
    localStorage.setItem(PALETTE_STORAGE_KEY, pack.id);
    localStorage.setItem(STORAGE_KEY, currentHex);
  } catch {
    /* non-fatal */
  }
  listeners.forEach(fn => fn());
  return lastContrast;
}

export function getAccent(): string {
  return currentHex;
}

/** The active palette pack id, or null when a raw custom hex is active. */
export function getPaletteId(): string | null {
  return currentPaletteId;
}

/** Last contrast-guard result (null on the byte-faithful default). */
export function getContrastResult(): ContrastResult | null {
  return lastContrast;
}

/** useSyncExternalStore subscribe. */
export function subscribeAccent(cb: () => void): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

let initialized = false;

/** Apply the stored palette (preferred) or accent before first paint. Idempotent. */
export function initAccent(): void {
  if (initialized) return;
  initialized = true;
  // Palette id takes precedence; fall back to a raw stored accent hex; then
  // to the byte-faithful default. This keeps existing dcent_accent_v1 users
  // working while making palette packs the primary persisted identity.
  const storedPalette = readStoredPaletteId();
  if (storedPalette) {
    const pack = packById(storedPalette)!;
    currentHex = normalizeHex(pack.hex);
    currentPaletteId = pack.id;
  } else {
    currentHex = readStoredAccent();
    const match = PALETTE_PACKS.find(p => normalizeHex(p.hex) === currentHex);
    currentPaletteId = match ? match.id : null;
  }
  paint(currentHex);

  // Parity with the design handoff's documented public API (window.dcpTheme)
  // so external / MCP-driven flows can drive the theme. Extended ADDITIVELY —
  // the original presets/current/apply contract is preserved verbatim.
  if (typeof window !== 'undefined') {
    (window as unknown as { dcpTheme?: unknown }).dcpTheme = {
      presets: ACCENT_PRESETS,
      current: getAccent,
      apply: applyAccent,
      // ── additive ( Theme Studio) ──
      packs: PALETTE_PACKS,
      applyPalette,
      currentPalette: getPaletteId,
      contrast: getContrastResult,
    };
  }
}

// Run before React mounts (this module is imported synchronously in main.tsx
// ahead of ReactDOM.createRoot().render) so the accent is correct on the very
// first paint — no flash of the default palette.
initAccent();
