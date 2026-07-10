// DCENT_OS Design System — Glassmorphism + industrial orange
// Design tokens for the three dashboard modes
//
// ── DCENT Design Language — generated/validated mirror of styles/tokens.css ──
// This JS object MIRRORS the canonical [shared] role values authored as :root
// custom properties in styles/tokens.css (the OS emission of
// docs/design-system/DCENT_DESIGN_LANGUAGE/token-contract.md). Per contract §0,
// it is a generated/validated artifact — do NOT hand-edit values out of sync with
// tokens.css. The hand-mirror previously drifted (void #060609, green #2DD4A0,
// yellow #F0B429); re-derived here to the canonical tokens.css values so a future
// validator passes. Runtime CSS uses the var(--*) tokens, not this object.

export const theme = {
  colors: {
    bg: {
      void: '#070710',         // Deepest background level (mirrors --bg-void)
      base: '#0c0c14',         // Body background (mirrors --bg-base)
      raised: '#1c1c2a',       // Elevated panels (mirrors --bg-raised)
      overlay: '#252536',      // Modals/dropdowns (mirrors --bg-overlay)
      glass: 'rgba(26, 26, 46, 0.7)', // Glass panel (mirrors --surface-glass-panel)
    },
    accent: {
      primary: '#FAA500',      // DCENT industrial orange (mirrors --accent)
      primaryDeep: '#FA6700',  // Deep orange gradient stop (mirrors --accent-deep)
      primaryHover: '#FFC94D', // Lighter hover (mirrors --accent-hover)
      primaryDim: 'rgba(250, 165, 0, 0.15)', // Orange background tint (mirrors --accent-glow)
      green: '#22C55E',        // Success, mining active — canonical status green (mirrors --green)
      greenTeal: '#2DD4A0',    // Teal accent alias (mirrors --green-teal) — distinct from canonical --green
      red: '#EF4444',          // Error, critical (mirrors --red)
      yellow: '#EAB308',       // Warning (mirrors --yellow)
      blue: '#3B82F6',         // Info, SV2 — OS protocol-info sibling (mirrors --blue)
    },
    text: {
      primary: '#f0f0f0',      // Main text
      secondary: '#8b8b9e',    // Muted text
      dim: '#6E6E80',          // Very muted — WCAG-AA on dark bg (PR-074; matches canonical --fg-dim / --text-dim)
      accent: '#FAA500',       // Highlighted values
    },
    border: {
      subtle: 'rgba(255, 255, 255, 0.06)',
      glass: 'rgba(255, 255, 255, 0.08)',
      active: 'rgba(250, 165, 0, 0.3)',
    },
  },
  glass: {
    panel: `
      background: rgba(26, 26, 46, 0.7);
      backdrop-filter: blur(16px);
      -webkit-backdrop-filter: blur(16px);
      border: 1px solid rgba(255, 255, 255, 0.08);
      border-radius: 16px;
    `,
    card: `
      background: rgba(18, 18, 26, 0.8);
      backdrop-filter: blur(12px);
      -webkit-backdrop-filter: blur(12px);
      border: 1px solid rgba(255, 255, 255, 0.06);
      border-radius: 12px;
    `,
    sidebar: `
      background: rgba(10, 10, 15, 0.85);
      backdrop-filter: blur(20px);
      -webkit-backdrop-filter: blur(20px);
      border-right: 1px solid rgba(255, 255, 255, 0.06);
    `,
  },
  shadow: {
    card: '0 4px 24px rgba(0, 0, 0, 0.3)',
    elevated: '0 8px 32px rgba(0, 0, 0, 0.4)',
    glow: '0 0 20px rgba(250, 165, 0, 0.15)', // mirrors --shadow-glow / --accent-glow (canonical accent #FAA500, was legacy #F7931A 247,147,26)
  },
  radius: {
    sm: '8px',
    md: '12px',
    lg: '16px',
    xl: '20px',
  },
  font: {
    ui: "'Inter', 'Inter Fallback', -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, sans-serif",
    mono: "'JetBrains Mono', 'JetBrains Mono Fallback', ui-monospace, Consolas, monospace",
    heading: "'Inter', 'Inter Fallback', -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, sans-serif",
  },
  fx: {
    glowSoft: 'rgba(250, 165, 0, 0.22)',
    glowRecord: 'rgba(255, 201, 77, 0.42)',
    durationMicro: '300ms',
    durationMoment: '1800ms',
    intensity: 0,
  },
} as const;

// Legacy brand export — aliased to new tokens for backward compatibility
export const brand = {
  orange: theme.colors.accent.primary,
  orangeGlow: theme.colors.accent.primaryDim,
  orangeLight: theme.colors.accent.primaryHover,
  darkBg: theme.colors.bg.base,
  cardSurface: theme.colors.bg.raised,
  border: theme.colors.border.glass,
  textPrimary: theme.colors.text.primary,
  textSecondary: theme.colors.text.secondary,
  green: theme.colors.accent.green,
  greenDim: 'rgba(34, 197, 94, 0.15)',   // mirrors --green-dim (canonical green #22C55E)
  greenTeal: theme.colors.accent.greenTeal, // teal alias preserved (mirrors --green-teal #2DD4A0)
  yellow: theme.colors.accent.yellow,
  yellowDim: 'rgba(234, 179, 8, 0.15)',  // mirrors --yellow-dim (canonical yellow #EAB308)
  red: theme.colors.accent.red,
  redDim: 'rgba(239, 68, 68, 0.15)',     // mirrors --red-dim
  hackerGreen: '#00FF41',
  hackerBg: '#0A0A0A',
  basicBg: '#FFF8F0',
  basicCard: '#FFFFFF',
  basicText: '#1A1A1A',
  basicWarm: '#F59E0B',
} as const;

export const fonts = {
  heading: theme.font.heading,
  body: theme.font.ui,
  mono: theme.font.mono,
} as const;
