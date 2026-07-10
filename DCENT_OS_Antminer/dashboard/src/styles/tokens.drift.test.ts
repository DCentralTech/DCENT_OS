import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { theme } from './theme';

// ─────────────────────────────────────────────────────────────────────────────
// DCENT Design Language — OS TOKEN DRIFT VALIDATOR (token-contract §0 / UIVIS-RENDER-1)
//
// SOURCE OF TRUTH: docs/design-system/DCENT_DESIGN_LANGUAGE/token-contract.md §2/§3/§5/§8.
//
// This is the OS half of the contract's "author-once / emit-twice / VALIDATE"
// durability mechanism (§0). It parses the OS emission (styles/tokens.css) and
// the JS mirror (styles/theme.ts) and asserts every [shared] canonical role
// resolves to the canonical VALUE in the contract. The build FAILS if either
// emission drifts — this is the permanent CI gate that closes UIVIS-ACCENT-1,
// UIVIS-SURFACE-1, and the historically-stale theme.ts mirror (D5) at the root.
//
// SCOPING (token-contract §0 point 4): ONLY [shared] rows are cross-validated.
// [OS-only] roles (greenTeal #2DD4A0 teal alias, 5th --bg-subtle surface tier,
// glass/elevation/blur scales, 8 accent packs) are the project's legitimate
// extension surface and are deliberately NOT asserted here. The asymmetry vs the
// axe validator (which does NOT value-check status/sphere-mid because those are
// per-project on axe, §5/§9) is itself contract-faithful: OS is the canonical
// VALUE owner for status hues + sphere-mid, so OS validates them.
//
// This file is excluded from tsconfig.json ("src/**/*.test.ts") so it NEVER
// affects `npm run build` (tsc && vite build). It runs only under `npm run test`
// (vitest, node environment — fs available). Pure additive test code; no runtime
// CSS/TS touched.
// ─────────────────────────────────────────────────────────────────────────────

// Co-located read of tokens.css — robust to cwd (works whether vitest runs from
// the dashboard dir or repo root) because it resolves relative to this module.
const TOKENS_CSS = readFileSync(
  fileURLToPath(new URL('./tokens.css', import.meta.url)),
  'utf8',
);
const COMMON_CSS = readFileSync(
  fileURLToPath(new URL('./common.css', import.meta.url)),
  'utf8',
);

/**
 * Extract a CSS custom-property value from a :root block, whitespace/case-robust,
 * resolving EXACTLY ONE level of `var(--other)` indirection (the only depth that
 * exists in either project's emission today — e.g. OS `--term-green: var(--fg-hacker-green)`).
 *
 * Returns the value UPPERCASED with internal whitespace stripped, so callers can
 * compare against an uppercase canonical hex (`#FAA500`) regardless of source
 * casing/spacing. Returns `undefined` if the role is absent.
 *
 * 1-LEVEL LIMIT (fail-closed): if a future emission introduces a 2-level alias,
 * the resolved value will still be a `VAR(--X)` literal and the equality assert
 * fails loudly — an acceptable fail-closed signal, not a silent pass.
 */
function cssRoleValue(css: string, role: string): string | undefined {
  const raw = readRole(css, role);
  if (raw === undefined) return undefined;
  const aliasMatch = /^VAR\(\s*(--[A-Z0-9-]+)\s*\)$/.exec(raw);
  if (aliasMatch) {
    const inner = readRole(css, aliasMatch[1].toLowerCase());
    if (inner !== undefined) return inner;
  }
  return raw;
}

/** Read one `--role: value;` declaration → UPPERCASE, whitespace-stripped value. */
function readRole(css: string, role: string): string | undefined {
  // role is lowercase (e.g. "--accent"); match case-insensitively.
  const re = new RegExp(`${escapeRe(role)}\\s*:\\s*([^;]+);`, 'i');
  const m = re.exec(css);
  if (!m) return undefined;
  return m[1].replace(/\s+/g, '').toUpperCase();
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\-]/g, '\\$&');
}

/** Case-insensitive hex compare (normalize both to uppercase, strip whitespace). */
function norm(hex: string): string {
  return hex.replace(/\s+/g, '').toUpperCase();
}

const CONTRACT = 'token-contract.md §2/§3/§5/§8';

describe('OS token emission matches the DCENT Design Language contract (UIVIS-RENDER-1)', () => {
  // ── (a) styles/tokens.css :root — the OS CSS emission ───────────────────────
  describe('styles/tokens.css [shared] role values', () => {
    // §2 ACCENT trio (+ opt-in BTC alias)
    it('--accent = #FAA500 (canonical brand accent, §2)', () => {
      expect(cssRoleValue(TOKENS_CSS, '--accent')).toBe(norm('#FAA500'));
    });
    it('--accent-deep = #FA6700 (ember companion, §2)', () => {
      expect(cssRoleValue(TOKENS_CSS, '--accent-deep')).toBe(norm('#FA6700'));
    });
    it('--accent-hover = #FFC94D (amber lift, §2)', () => {
      expect(cssRoleValue(TOKENS_CSS, '--accent-hover')).toBe(norm('#FFC94D'));
    });
    it('--orange-bitcoin = #F7931A (opt-in legacy BTC alias — never default, §2)', () => {
      expect(cssRoleValue(TOKENS_CSS, '--orange-bitcoin')).toBe(norm('#F7931A'));
    });

    // §3 SURFACE floor (byte-identical family glue — the lock)
    it('--bg-void = #070710 (deepest floor, family glue, §3)', () => {
      expect(cssRoleValue(TOKENS_CSS, '--bg-void')).toBe(norm('#070710'));
    });

    // §2 sphere-mid (RESOLVED to brand amber 2026-06-14, §9 closure)
    it('--sphere-mid = #FAA500 (brand amber, resolved 2026-06-14, §9)', () => {
      expect(cssRoleValue(TOKENS_CSS, '--sphere-mid')).toBe(norm('#FAA500'));
    });

    // §5/§8 reserved terminal-green (byte-identical; via --fg-hacker-green alias)
    it('--term-green resolves to #00FF41 (reserved terminal/log/hacker, §5/§8)', () => {
      expect(cssRoleValue(TOKENS_CSS, '--term-green')).toBe(norm('#00FF41'));
    });

    // §5 STATUS hues — OS owns the canonical VALUES (saturated, table-tuned)
    it('--green = #22C55E (ok/active, §5)', () => {
      expect(cssRoleValue(TOKENS_CSS, '--green')).toBe(norm('#22C55E'));
    });
    it('--yellow = #EAB308 (warn, §5)', () => {
      expect(cssRoleValue(TOKENS_CSS, '--yellow')).toBe(norm('#EAB308'));
    });
    it('--red = #EF4444 (error, §5)', () => {
      expect(cssRoleValue(TOKENS_CSS, '--red')).toBe(norm('#EF4444'));
    });
    it('--blue = #3B82F6 (OS protocol/SV2-info sibling, §5)', () => {
      expect(cssRoleValue(TOKENS_CSS, '--blue')).toBe(norm('#3B82F6'));
    });
  });

  // ── (b) styles/theme.ts JS mirror — closes the historical stale-mirror (D5) ──
  // Per §0 point 2, theme.ts MUST be a generated/validated artifact, not a
  // hand-edited copy that drifts. This block makes that a permanent CI gate.
  describe('styles/theme.ts mirror equals the same canonical [shared] values (D5)', () => {
    it('accent trio mirrors §2', () => {
      expect(norm(theme.colors.accent.primary)).toBe(norm('#FAA500'));
      expect(norm(theme.colors.accent.primaryDeep)).toBe(norm('#FA6700'));
      expect(norm(theme.colors.accent.primaryHover)).toBe(norm('#FFC94D'));
    });
    it('surface void floor mirrors §3', () => {
      expect(norm(theme.colors.bg.void)).toBe(norm('#070710'));
    });
    it('status hues mirror §5', () => {
      expect(norm(theme.colors.accent.green)).toBe(norm('#22C55E'));
      expect(norm(theme.colors.accent.yellow)).toBe(norm('#EAB308'));
      expect(norm(theme.colors.accent.red)).toBe(norm('#EF4444'));
      expect(norm(theme.colors.accent.blue)).toBe(norm('#3B82F6'));
    });
    // greenTeal (#2DD4A0) is explicitly NOT asserted against --green: it is a
    // distinct [OS-only] teal alias (mirrors --green-teal), per the tokens.css /
    // theme.ts comments. Asserting it would falsely fail.
    it('greenTeal stays the [OS-only] teal alias (NOT cross-validated as --green)', () => {
      expect(norm(theme.colors.accent.greenTeal)).toBe(norm('#2DD4A0'));
      expect(norm(theme.colors.accent.greenTeal)).not.toBe(norm(theme.colors.accent.green));
    });
  });

  // ── (c) the CSS emission and the JS mirror agree on every cross-validated role
  // (the whole point of the mechanism — both halves are derived from the contract)
  it(`tokens.css and theme.ts agree on every [shared] role (${CONTRACT})`, () => {
    const pairs: Array<[string, string]> = [
      ['--accent', theme.colors.accent.primary],
      ['--accent-deep', theme.colors.accent.primaryDeep],
      ['--accent-hover', theme.colors.accent.primaryHover],
      ['--bg-void', theme.colors.bg.void],
      ['--green', theme.colors.accent.green],
      ['--yellow', theme.colors.accent.yellow],
      ['--red', theme.colors.accent.red],
      ['--blue', theme.colors.accent.blue],
    ];
    for (const [role, mirror] of pairs) {
      expect(
        cssRoleValue(TOKENS_CSS, role),
        `${role}: tokens.css must equal theme.ts mirror (${CONTRACT})`,
      ).toBe(norm(mirror));
    }
  });
});

describe('support-tier shared chrome uses brand tokens', () => {
  it('uses --accent / --accent-glow and never the opt-in Bitcoin-orange alias', () => {
    expect(COMMON_CSS).toContain('.cp-support-tier-badge--experimental');
    expect(COMMON_CSS).toContain('var(--accent)');
    expect(COMMON_CSS).toContain('var(--accent-glow)');
    expect(COMMON_CSS).not.toMatch(/cp-support-tier[\s\S]*#F7931A/i);
    expect(COMMON_CSS).not.toMatch(/cp-support-tier[\s\S]*--orange-bitcoin/i);
  });
});

// ─────────────────────────────────────────────────────────────────────────────
// Inline accent-fallback gate (token-contract §2): a `var(--accent, …)` inline
// style must fall back to the canonical accent #FAA500, never the legacy
// Bitcoin-orange #F7931A (which is the opt-in `--orange-bitcoin` alias ONLY).
// The role-value validator above covers tokens.css/theme.ts definitions; this
// gate covers the scattered inline fallbacks across components/styles so the
// whole fallback-drift class self-enforces.
// ─────────────────────────────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────
// Light-theme (UINAV-7) integrity gate. Two regressions are pinned here, both
// found live in the S6 Chrome visual QA after the S4 toggle shipped dark-only-
// in-practice:
//   (1) COMMENT-BRACE HAZARD — the esbuild CSS minifier (Vite's transformer)
//       mis-parses a `{`/`}`/backtick INSIDE a CSS comment and silently DROPS
//       the rule that follows. The original S4 tier-1 token rebind followed a
//       comment that quoted a braced selector, so it never reached the cascade
//       and the dashboard stayed dark under light mode. Keep light-theme.css
//       comments brace-free and backtick-free.
//   (2) SURFACE-TOKEN COVERAGE — the light rebind must set the glass surface
//       tokens (sidebar/topbar) + the floor under a [data-appearance="light"]
//       scope, or the chrome stays dark even when the toggle flips. Pin that the
//       light palette actually recolors those roles.
// ─────────────────────────────────────────────────────────────────────────────
describe('light-theme.css build-transform + coverage integrity (UINAV-7)', () => {
  const LIGHT_CSS = readFileSync(
    fileURLToPath(new URL('./light-theme.css', import.meta.url)),
    'utf8',
  );
  // Extract every /* ... */ comment span.
  const comments = LIGHT_CSS.match(/\/\*[\s\S]*?\*\//g) ?? [];

  it('contains no CSS braces or backticks inside comments (esbuild drops the next rule)', () => {
    const offenders = comments
      .filter((c) => /[{}`]/.test(c))
      .map((c) => c.replace(/\s+/g, ' ').slice(0, 80));
    expect(
      offenders,
      `light-theme.css comments must not contain { } or backtick — the esbuild ` +
        `CSS minifier mis-parses them and silently drops the FOLLOWING rule ` +
        `(this is the bug that made the S4 light theme a no-op). Offending comments:\n` +
        offenders.join('\n'),
    ).toEqual([]);
  });

  it('light scope actually recolors the glass surfaces + floor (not just .mode-standard aliases)', () => {
    // strip comments so we assert against real declarations only
    const css = LIGHT_CSS.replace(/\/\*[\s\S]*?\*\//g, '');
    expect(/data-appearance="light"/.test(css)).toBe(true);
    for (const role of ['--surface-glass-sidebar', '--surface-glass-topbar', '--bg-void']) {
      expect(
        new RegExp(`${role}\\s*:`).test(css),
        `light-theme.css must rebind ${role} under the light scope, else that ` +
          `surface stays dark when the toggle flips (S6 regression).`,
      ).toBe(true);
    }
    // the rebind must ride a descendant-combinator selector (.mode-* or body),
    // never a bare :root[data-appearance="light"]{…} custom-prop-only rule — that
    // form is the one esbuild drops.
    expect(
      /data-appearance="light"\]\s+\.mode-standard/.test(css),
      'the light token rebind must be scoped to the .mode-standard app-shell ' +
        'wrapper (a surviving descendant selector), not a bare root rule.',
    ).toBe(true);
  });

  // The completeness critic (S6) showed that recoloring the TOKENS is not enough:
  // surfaces painted with a HARDCODED dark literal (inputs, the Standard inner
  // panels, the portaled modals) read no token, so they stayed dark-on-dark while
  // the text token flipped to dark ink — the toggle's own Settings form was
  // unreadable. Pin that the light theme carries the literal-surface overrides so
  // that defect cannot silently regress.
  it('light theme repoints the hardcoded-dark surfaces (inputs / panels / modals)', () => {
    const css = LIGHT_CSS.replace(/\/\*[\s\S]*?\*\//g, '');
    // (B1) inputs: must be light-backgrounded under the light scope (else dark ink
    // on a near-black field — unreadable).
    expect(
      /textarea/.test(css) && /background:\s*#fff(fff)?/i.test(css),
      'light-theme.css must repoint the hardcoded input background (B1: ' +
        'dark-on-dark unreadable inputs).',
    ).toBe(true);
    // (H1) Standard inner panels: pin a representative sample is covered.
    for (const panel of ['.kpi-card', '.chain-card', '.chart-wrap']) {
      expect(
        css.includes(panel),
        `light-theme.css must repoint the hardcoded ${panel} background (H1: ` +
          `muddy dark panels under light).`,
      ).toBe(true);
    }
    // (M1) portaled overlays: pin the canonical modal panel is recolored.
    expect(
      /\.ds-overlay-panel/.test(css),
      'light-theme.css must recolor .ds-overlay-panel (M1: portaled modals stay ' +
        'dark over a light page).',
    ).toBe(true);
  });
});

describe('inline accent fallbacks must be canonical (token-contract §2)', () => {
  it('no `var(--accent, #F7931A)` wrong-fallback remains under src/', async () => {
    const { readdirSync } = await import('node:fs');
    const path = await import('node:path');
    const SRC = fileURLToPath(new URL('..', import.meta.url)); // styles/.. = src/
    const wrong = /var\(--accent,\s*#f7931a\)/i;
    const offenders: string[] = [];
    const walk = (dir: string) => {
      for (const ent of readdirSync(dir, { withFileTypes: true })) {
        const p = path.join(dir, ent.name);
        if (ent.isDirectory()) {
          if (ent.name === 'node_modules' || ent.name === 'dist') continue;
          walk(p);
        } else if (/\.(tsx?|css)$/.test(ent.name) && !/\.(test|spec)\.tsx?$/.test(ent.name)) {
          // scan production source only — test/spec files legitimately reference
          // the forbidden pattern in their assertions.
          if (wrong.test(readFileSync(p, 'utf8'))) offenders.push(p);
        }
      }
    };
    walk(SRC);
    expect(
      offenders,
      `These files use the legacy #F7931A as the --accent fallback; use #FAA500 ` +
        `(token-contract §2). Offenders:\n${offenders.join('\n')}`,
    ).toEqual([]);
  });
});
