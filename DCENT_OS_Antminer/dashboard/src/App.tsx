import { useEffect, useState } from 'react';
import { useMinerStore, useBetaView } from './store/miner';
import { useMinerData } from './hooks/useMinerData';
import { useFavicon } from './hooks/useFavicon';
import { useTitleTicker } from './fx/useTitleTicker';
import { useHealthAlertBridge } from './hooks/useHealthAlertBridge';
import { useDashboardVersion } from './hooks/useDashboardVersion';
import { api } from './api/client';
import type { SetupStatusResponse } from './api/types';
import { SetupWizard } from './components/wizard/SetupWizard';
import { AppShell } from './components/AppShell';
import { AlertBanner } from './components/common/AlertBanner';
import { DaemonStatusBanner } from './components/common/DaemonStatusBanner';
import { BootPhaseBanner } from './components/common/BootPhaseBanner';
import { InfoBanner } from './components/common/InfoBanner';
import { ToastContainer } from './components/common/Toast';
import { ErrorBoundary } from './components/common/ErrorBoundary';
import { HonestModeBanner, SystemHealthProvider } from './components/common/proxy/HonestModeStatus';
import { EfficiencyMigrationPrompt } from './components/onboarding/EfficiencyMigrationPrompt';
//  fix: these three context providers were defined but never mounted, so
// the Flight Recorder, Protocol Timeline/Pipeline Scope, Patchbay and Command
// Journal tools rendered permanently-empty/no-op (a misleading "RECORDING"
// badge, dead CLEAR/EXPORT buttons, all-zero pipeline) even while mining.
// FlightRecorderProvider must wrap the other two (both call useFlightRecorder).
import { FlightRecorderProvider } from './hooks/useFlightRecorder';
import { ProtocolTraceProvider } from './hooks/useProtocolTrace';
import { PatchBayProvider } from './hooks/usePatchBay';
import { randomQuote } from './utils/constants';
import { DcentOsLogo } from './components/common/DcentOsLogo';
import { applyAppearance } from './theme/appearance';
import { initPageVisibilityAttribute, initVitalityAttribute } from './fx/fxSettings';
import { initRewardBus } from './fx/rewardBus';
import { CelebrationLayer } from './fx/CelebrationLayer';
import { armFirstShareWatch } from './components/common/FirstShareWatchCard';

export default function App() {
  const [quote] = useState(randomQuote);
  const settings = useMinerStore(s => s.settings);
  const updateSettings = useMinerStore(s => s.updateSettings);
  const currentPage = useMinerStore(s => s.currentPage);
  const betaView = useBetaView();
  const dashboardVersion = useDashboardVersion();
  const [loading, setLoading] = useState(true);
  const [setupStatus, setSetupStatus] = useState<SetupStatusResponse | null>(null);

  // Connect WebSocket + REST polling
  useMinerData();

  useEffect(() => {
    initRewardBus();
    const cleanupVisibility = initPageVisibilityAttribute();
    const cleanupVitality = initVitalityAttribute();
    return () => {
      cleanupVisibility();
      cleanupVitality();
    };
  }, []);

  useEffect(() => {
    document.getElementById('boot-splash-style')?.remove();
  }, []);

  // UINAV-7: keep the <html data-appearance> attribute in sync with the
  // user's light/dark preference for runtime toggles. The pre-paint applier
  // (theme/appearance.ts) already set the initial value with no FOUC; this
  // covers post-load changes from the Settings → Appearance control.
  useEffect(() => {
    applyAppearance(settings.appearance === 'light' ? 'light' : 'dark');
  }, [settings.appearance]);

  // Dynamic favicon based on miner state
  useFavicon();
  useTitleTicker();

  // Promote derived health issues into the alert/notification pipeline
  useHealthAlertBridge();

  // Initial load — use backend setup state as the source of truth for first boot.
  useEffect(() => {
    let cancelled = false;

    (async () => {
      const [, setupStatus] = await Promise.allSettled([
        api.getConfig(),
        api.getSetupStatus(),
      ]);

      if (cancelled) {
        return;
      }

      if (setupStatus.status === 'fulfilled') {
        setSetupStatus(setupStatus.value);
        // Mirror into the store so the health pipeline (security advisory)
        // and any other consumers see the backend onboarding state.
        useMinerStore.getState().setSetupStatus(setupStatus.value);
        updateSettings({ setupComplete: !setupStatus.value.needs_setup });
      }

      setLoading(false);
    })().catch(() => {
      if (!cancelled) {
        setLoading(false);
      }
    });

    return () => {
      cancelled = true;
    };
  }, [updateSettings]);

  if (loading) {
    /* F1 wave5 splash — premium first-paint identity (glass plinth, warm
       halo breathe, dual-ring spinner). Styled by §F1-SPLASH in
       design-system.css. JSX limited to this block per F1 ownership. */
    return (
      <div className="ds-splash" role="status" aria-live="polite" aria-label="Loading DCENT_OS">
        <div className="ds-splash__logo">
          <DcentOsLogo width={240} />
        </div>
        <div className="ds-splash__quote">{quote}</div>
        <div className="ds-splash__spinner" aria-hidden="true" />
      </div>
    );
    /* end F1 wave5 splash */
  }

  // First-boot wizard.
  //
  // Pre-existing bug fix: the backend is the source of truth for "is setup
  // done". A defaulted/opted-out miner reports needs_setup === false even
  // on a fresh browser whose localStorage has no setupComplete flag yet —
  // previously that re-prompted the whole wizard. Backend
  // needs_setup === false is now authoritative; localStorage is only the
  // fallback when the status probe failed (offline / mid-reboot).
  const backendSaysSetupDone = setupStatus ? setupStatus.needs_setup === false : null;
  const showWizard =
    backendSaysSetupDone === null
      ? !settings.setupComplete
      : !backendSaysSetupDone;

  if (showWizard) {
    return (
      <SetupWizard onComplete={(cfg) => {
        const completedStatus: SetupStatusResponse = {
          ...(setupStatus ?? {
            steps: ['safety', 'circuit', 'password', 'mode', 'pool', 'complete'],
          }),
          needs_setup: false,
          device_ready: true,
          password_decision_made: true,
          safety_decision_made: true,
          password_opt_out: cfg.password.length === 0,
          progress: {
            safety: true,
            circuit: setupStatus?.progress?.circuit ?? false,
            password: true,
            mode: true,
            pool: Boolean(cfg.pool.url && cfg.pool.worker),
            complete: true,
            ...(setupStatus?.progress?.solar_provider !== undefined
              ? { solar_provider: setupStatus.progress.solar_provider }
              : {}),
          },
          auth: {
            ...(setupStatus?.auth ?? { password_set: false, token_issued: false }),
            password_set: cfg.password.length > 0 || Boolean(setupStatus?.auth?.password_set),
            token_issued: Boolean(cfg.apiToken) || Boolean(setupStatus?.auth?.token_issued),
            password_opt_out: cfg.password.length === 0,
          },
        };
        setSetupStatus(completedStatus);
        useMinerStore.getState().setSetupStatus(completedStatus);
        armFirstShareWatch();
        updateSettings({
          minerName: cfg.minerName,
          password: cfg.password || null,
          apiToken: cfg.apiToken ?? null,
          mode: cfg.mode,
          setupComplete: true,
        });
      }} setupStatus={setupStatus} />
    );
  }

  return (
    <SystemHealthProvider>
      <FlightRecorderProvider>
      <ProtocolTraceProvider>
      <PatchBayProvider>
      {/* F1 wave5 splash — D-05: skip-link styling moved entirely to the
          canonical `.skip-to-content` CSS (canonical.css). The fragile
          imperative inline focus/blur handlers are removed; the CSS rule
          handles hide / visible-on-focus + the canonical accent focus
          ring + the a11y-correct dark-on-orange contrast. */}
      <a href="#main-content" className="skip-to-content">
        Skip to main content
      </a>
      <DaemonStatusBanner />
      <BootPhaseBanner />
      {/*
        HonestModeBanner reports proxy/native chain-driver state — useful for
        firmware developers, noisy for beta operators. Gated behind the
        "beta view" preference so power users opting out still see it.
      */}
      {!betaView && <HonestModeBanner />}
      <AlertBanner />
      {dashboardVersion.showReloadBanner && (
        <InfoBanner
          tone="info"
          title="A newer dashboard is installed on this miner"
          dismissible
          onDismiss={dashboardVersion.dismiss}
          action={(
            <button type="button" className="ds-btn sm" onClick={dashboardVersion.reload}>
              Reload
            </button>
          )}
        >
          Reload to use the installed dashboard.
        </InfoBanner>
      )}
      {/* Route-change live region — announces page changes to screen
          readers. Polite so it doesn't interrupt other speech. */}
      <div role="status" aria-live="polite" aria-atomic="true" className="sr-only">
        {currentPage ? `${currentPage} page` : ''}
      </div>
      <ErrorBoundary resetKey={currentPage}>
        <main id="main-content" tabIndex={-1}>
          <AppShell />
        </main>
      </ErrorBoundary>
      <EfficiencyMigrationPrompt />
      <CelebrationLayer />
      <ToastContainer />
      </PatchBayProvider>
      </ProtocolTraceProvider>
      </FlightRecorderProvider>
    </SystemHealthProvider>
  );
}
