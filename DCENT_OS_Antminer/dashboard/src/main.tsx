import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { I18nProvider } from './i18n/i18n';
import './styles/fonts.css';
import './styles/tokens.css';
import './styles/global.css';
import './styles/basic.css';
import './styles/standard.css';
import './styles/advanced.css';
import './styles/features.css';
import './styles/design-system.css';
import './styles/canonical.css';
import './styles/current-block.css';
import './styles/common.css';
import './styles/charts.css';
import './styles/responsive.css';
import './styles/motion.css';
// Claude-Design handoff visual recreation — loaded LAST so it re-skins
// every mode (foundation VR1; per-mode structural recreation VR2–VR5).
import './styles/handoff-skin.css';
// Per-mode handoff recreation skins (VR2/VR3/VR4) — each scoped to its
// .mode-* root; load order among them is immaterial (no cross-mode
// selectors) but they MUST follow the VR1 foundation above.
import './styles/handoff-skin-standard.css';
import './styles/handoff-skin-tuning.css';
import './styles/handoff-skin-heater.css';
import './styles/handoff-skin-hacker.css';
// Shared primitive + signature-live-component fidelity layers, loaded
// after the per-mode skins (low-specificity baseline; per-mode wins).
import './styles/handoff-skin-components.css';
import './styles/handoff-skin-live.css';
// Light appearance overrides (UINAV-7) — loaded AFTER the handoff skins so the
// `[data-appearance=light]` light palette is source-order-late as well as
// specificity-winning (belt-and-suspenders). Only matches when <html> carries
// data-appearance="light"; absent/dark = byte-identical to the prior build.
import './styles/light-theme.css';
// Accent theme engine — applies the stored accent before first paint
// (module side-effect runs synchronously, ahead of createRoot().render).
import './theme/accent';
// Appearance (light/dark) pre-paint applier — stamps data-appearance on <html>
// from the persisted preference before the first paint (no FOUC). Mirrors the
// accent engine's module side-effect; imported right after it.
import './theme/appearance';
// Theme Studio chrome (ZONE C) — palette-pack gallery + live preview
// + custom builder. Loads after the accent engine; reuses common.css's
// .accent-picker grammar and the design tokens.
import './styles/theme-studio.css';
import './styles/fx.css';
// QA mock-telemetry harness — DEV-ONLY, tree-shaken out of production builds
// (import.meta.env.DEV is a compile-time false in prod, so the whole branch +
// the ./dev/* fixtures are dropped — zero bytes ship in the firmware bundle).
// To use: `npm run dev`, then add ?mock to the URL (or set localStorage
// dcent_qa_mock=1). It wraps fetch so every page renders with mock telemetry.
const boot = () =>
  ReactDOM.createRoot(document.getElementById('root')!).render(
    <React.StrictMode>
      <I18nProvider>
        <App />
      </I18nProvider>
    </React.StrictMode>,
  );

if (import.meta.env.DEV) {
  // Install the QA mock BEFORE the app mounts (so the first poll is mocked),
  // then boot. The whole branch is dropped from production builds.
  import('./dev/mockApi')
    .then(({ isMockEnabled, installMockApi }) => { if (isMockEnabled()) installMockApi(); })
    .catch(() => { /* harness optional */ })
    .finally(boot);
} else {
  boot();
}
