// DCENT_OS Setup Wizard — shell.
//
// STRUCTURAL recreation of the kit `SetupWizard` (ui_kits/wizard/Wizard.jsx)
// the operator specifically loves. Production now IS the kit's composition:
//   • kit header  — 3-sphere DCentralMolecule + "DCENT_OS" wordmark + a
//                    "SETUP" pill + the signature 11-step NUMBERED RAIL with
//                    done(✓)/active(gradient)/connector-line states;
//   • kit main    — centered .wiz-step-body column, soft fade-in;
//   • per-step    — each step is the kit's exact layout (welcome value-cards,
//                    mode cards w/ mini-previews, password strength bars,
//                    pool template chips, circuit NEC-derate summary, power
//                    source cards, donation slider w/ live split, calibrate,
//                    name, review summary);
//   • kit footer  — sticky Back / Skip / Continue;
//   • kit reboot  — breathing-orb + twin counter-rotating rings overlay.
//
// The kit's `.wiz-*` visual system is delivered self-contained via
// ./kit/kitStyles (we own only src/components/wizard/* and may not touch
// src/styles/*; the loaded handoff-skin-wizard.css ALSO recognises the
// retained production `.wizard-shell`/`.ds-btn` hooks). ZERO src/styles edit.
//
// EVERY real behaviour is preserved byte-for-behaviour:
//   - api.getSetupStatus / setupSafety / skipSafety / setupCircuit /
//     setupMode / setupPool / updateDonationConfig / skipPassword /
//     completeSetup / reboot — same call order, same payloads;
//   - the /api/auth/setup + api.createSession owner-credential flow;
//   - localStorage step persistence (STORAGE_KEY) + dcentos-settings auth;
//   - the reconnect poll (disconnect-observed / >=5 attempts gate);
//   - canProceed() gating incl. the strict hacker/resume password gate;
//   - the freedom-first inline skip-confirm + real terminal-skip path;
//   - setupStatus-driven resume/jump-to-step logic;
//   - keyboard Enter-advance / Esc-blur.
// Truth-contracts preserved: connecting≠connected, "recommended not
// required", honest reboot/reconnect (no fabricated progress).

import React, { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import type { OperatingMode, SetupStatusResponse } from '../../api/types';
import type { DeviceFamily } from '../../api/generated/capability';
import { api } from '../../api/client';
import {
  getSessionToken,
  setSessionToken,
  getVolatilePassword,
  setVolatilePassword,
} from '../../api/credentials';
import { ensureKitStyles } from './kit/kitStyles';
import {
  DCentralMolecule,
  StepRail,
  StepFooter,
  RebootReconnectOverlay,
  type KitStep,
  type RebootPhase,
} from './kit/KitParts';
import { WelcomeStep } from './WelcomeStep';
import { NetworkStep } from './NetworkStep';
import { ModeStep } from './ModeStep';
import { NameStep } from './NameStep';
import { PoolStep } from './PoolStep';
import type { PoolConfig } from './PoolStep';
import { DonationStep } from './DonationStep';
import type { DonationStepValue } from './DonationStep';
import { PasswordStep } from './PasswordStep';
import { PowerSourceStep } from './PowerSourceStep';
import { CircuitConfigStep, CIRCUIT_DEFAULT_DERATE } from './CircuitConfigStep';
import { CalibrationStep, type CalibrationStepValue, DEFAULT_CALIBRATION_STEP_VALUE } from './CalibrationStep';
import { HomeComfortStep, type HomeComfortStepValue, DEFAULT_HOME_COMFORT_STEP_VALUE } from './HomeComfortStep';
import { PsuOverrideStep, type PsuHardwareVariant } from './PsuOverrideStep';
import { ReviewStep } from './ReviewStep';
import { OverlayDialog } from '../common/OverlayDialog';
import { InfoBanner } from '../common/InfoBanner';

// ─── Wizard State ──────────────────────────────────────────
type SetupPath = 'quick' | 'guided';

interface WizardState {
  currentStep: number;
  currentStepId: StepId;
  setupPath: SetupPath;
  network: string;
  minerName: string;
  mode: OperatingMode | null;
  powerSource: string | null;
  circuitVoltage: number | null;
  circuitAmperage: number | null;
  circuitDerate: number;
  calibration: CalibrationStepValue;
  // P2-4 (§4.E): heater/home-only economics + quiet-hours captured at setup.
  homeComfort: HomeComfortStepValue;
  pool: PoolConfig;
  donation: DonationStepValue;
  password: string;
  confirmPassword: string;
  safetyConfirmed: boolean;
  safetyOptedOut: boolean;
  //  (2026-05-22): Loki / Bare-APW3 PSU hardware declaration. `null` =
  // operator hasn't yet declared (default, skip-friendly). When advancing the
  // wizard, `psuOverrideEnabled !== null` triggers an api.updatePsuOverride
  // call BEFORE api.setupCircuit, so the daemon picks up the declaration on
  // the same boot cycle. Migration: persisted  state lacks these
  // fields → load defaults to `null` (see loadWizardState).
  psuOverrideEnabled: boolean | null;
  psuHardwareVariant: PsuHardwareVariant | null;
}

interface CompletedSetupConfig {
  minerName: string;
  mode: OperatingMode;
  pool: PoolConfig;
  donation: DonationStepValue;
  password: string;
  apiToken?: string | null;
}

const STORAGE_KEY = 'dcentos-wizard-state';
const DEFAULT_DONATION_POOL_URL = 'stratum+tcp://pool.d-central.tech:3333';
const DEFAULT_DONATION_WORKER = 'DungeonMaster';
const DEFAULT_DONATION_PASSWORD = 'x';
const DEFAULT_DONATION_FALLBACK_POOL_URL = 'stratum+tcp://stratum.braiins.com:3333';
const DEFAULT_DONATION_FALLBACK_WORKER = 'DungeonMaster';
const DEFAULT_DONATION_CYCLE_S = 3600;
const DEFAULT_DONATION_PERCENT = 2;

const DEFAULT_STATE: WizardState = {
  currentStep: 0,
  currentStepId: 'welcome',
  setupPath: 'guided',
  network: 'eth',
  minerName: 'My Miner',
  mode: null,
  powerSource: null,
  circuitVoltage: null,
  circuitAmperage: null,
  circuitDerate: CIRCUIT_DEFAULT_DERATE,
  calibration: DEFAULT_CALIBRATION_STEP_VALUE,
  homeComfort: DEFAULT_HOME_COMFORT_STEP_VALUE,
  pool: { url: '', worker: '', password: 'x' },
  donation: { enabled: true, percent: 2 },
  password: '',
  confirmPassword: '',
  safetyConfirmed: false,
  safetyOptedOut: false,
  psuOverrideEnabled: null,
  psuHardwareVariant: null,
};

function clampDonationPercent(value: unknown): number {
  const n = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(n) ? Math.max(0, Math.min(5, n)) : DEFAULT_DONATION_PERCENT;
}

function normalizeDonationStepValue(value: Partial<DonationStepValue> | undefined): DonationStepValue {
  return {
    enabled: value?.enabled ?? true,
    percent: clampDonationPercent(value?.percent),
  };
}

function loadWizardState(): WizardState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<WizardState>;
      const setupPath: SetupPath = parsed.setupPath === 'quick' ? 'quick' : 'guided';
      const parsedStepId =
        typeof parsed.currentStepId === 'string' && ANTMINER_STEPS.some(s => s.id === parsed.currentStepId)
          ? parsed.currentStepId as StepId
          : ANTMINER_STEPS[parsed.currentStep ?? DEFAULT_STATE.currentStep]?.id ?? DEFAULT_STATE.currentStepId;
      const parsedStepIndex = stepIndex(parsedStepId);
      // :  persisted state lacks the PSU-override fields. Spread
      // DEFAULT_STATE first so missing keys (psuOverrideEnabled,
      // psuHardwareVariant) default to `null` and never block wizard advance.
      // Per silly-churning-hopper.md §Risk register row 1 (LOW-MEDIUM
      // regression mitigation).
      return {
        ...DEFAULT_STATE,
        ...parsed,
        setupPath,
        currentStep: parsedStepIndex >= 0 ? parsedStepIndex : DEFAULT_STATE.currentStep,
        currentStepId: parsedStepId,
        donation: normalizeDonationStepValue(parsed.donation),
        calibration: parsed.calibration ?? DEFAULT_CALIBRATION_STEP_VALUE,
        homeComfort: { ...DEFAULT_HOME_COMFORT_STEP_VALUE, ...(parsed.homeComfort ?? {}) },
        psuOverrideEnabled: parsed.psuOverrideEnabled ?? null,
        psuHardwareVariant: parsed.psuHardwareVariant ?? null,
      };
    }
  } catch { /* ignore */ }
  return DEFAULT_STATE;
}

function saveWizardState(state: WizardState) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
}

function clearWizardState() {
  localStorage.removeItem(STORAGE_KEY);
}

// ─── Step Definitions ──────────────────────────────────────
// Mapped 1:1 onto the kit's 11-node rail (KIT_STEPS). `skippable` controls
// the footer Skip button; `optional` steps (network, calibration) have NO
// real backend endpoint — they are honest informational/local steps and
// NEVER fabricate a call.
const ANTMINER_STEPS = [
  { id: 'welcome',      label: 'Welcome',      skippable: false },
  { id: 'network',      label: 'Network',      skippable: true,  optional: true },
  { id: 'password',     label: 'Password',     skippable: true },
  { id: 'mode',         label: 'Mode',         skippable: false },
  { id: 'pool',         label: 'Pool',         skippable: true },
  { id: 'circuit',      label: 'Circuit',      skippable: true },
  { id: 'power',        label: 'Power',        skippable: true },
  //  (2026-05-22): PSU override declaration step. Only meaningful for
  // grid/hybrid power sources; otherwise auto-skipped via the goNext()
  // visibility gate in handleSkip / canProceed. Skippable in any case.
  { id: 'psu_override', label: 'PSU Override', skippable: true },
  { id: 'donation',     label: 'Donation',     skippable: true },
  // P2-4 (§4.E): heater/home-only economics (electricity rate + currency) +
  // quiet-hours capture. Auto-skipped in Standard/Hacker via the goNext()
  // visibility gate (same mechanism as psu_override). Skippable in any case.
  { id: 'home',         label: 'Home',         skippable: true },
  { id: 'calibration',  label: 'Calibrate',    skippable: true,  optional: true },
  { id: 'name',         label: 'Name',         skippable: false },
  { id: 'review',       label: 'Review',       skippable: false },
] as const;

type SetupStep = (typeof ANTMINER_STEPS)[number];
type StepId = SetupStep['id'];

const QUICK_STEP_IDS: StepId[] = ['welcome', 'pool', 'password', 'review'];

const STEP_BY_ID = new Map<StepId, SetupStep>(
  ANTMINER_STEPS.map(step => [step.id, step]),
);

function stepsById(ids: readonly StepId[]): SetupStep[] {
  return ids
    .map(id => STEP_BY_ID.get(id))
    .filter((step): step is SetupStep => Boolean(step));
}

const STEP_REGISTRY: Record<DeviceFamily, readonly SetupStep[]> = {
  antminer: ANTMINER_STEPS,
  esp: stepsById(['welcome', 'network', 'password', 'pool', 'name', 'review']),
  whatsminer: stepsById(['welcome', 'pool', 'name', 'review']),
  avalon: stepsById(['welcome', 'pool', 'name', 'review']),
  innosilicon: stepsById(['welcome', 'pool', 'name', 'review']),
  unknown: stepsById(['welcome', 'password', 'pool', 'review']),
};

export function setupFamilyFromBoardTarget(boardTarget: string | null | undefined): DeviceFamily {
  const target = (boardTarget ?? '').trim().toLowerCase();
  if (!target) return 'unknown';
  if (target.startsWith('bitaxe-') || target.startsWith('dcent-axe-')) return 'esp';
  if (target.startsWith('whatsminer-')) return 'whatsminer';
  if (target.startsWith('avalon-')) return 'avalon';
  if (target.startsWith('innosilicon-')) return 'innosilicon';
  if (
    target.startsWith('am1') ||
    target.startsWith('am2') ||
    target.startsWith('am3') ||
    target.startsWith('amlogic') ||
    target.startsWith('cv1835') ||
    target.startsWith('bcb100') ||
    target.startsWith('xil-')
  ) {
    return 'antminer';
  }
  return 'unknown';
}

export function stepRegistryForDeviceFamily(family: DeviceFamily): readonly SetupStep[] {
  return STEP_REGISTRY[family] ?? STEP_REGISTRY.unknown;
}

function getActiveSteps(setupPath: SetupPath, family: DeviceFamily): SetupStep[] {
  const familySteps = stepRegistryForDeviceFamily(family);
  if (setupPath === 'quick') {
    return QUICK_STEP_IDS
      .map(id => familySteps.find(step => step.id === id))
      .filter((step): step is SetupStep => Boolean(step));
  }
  return [...familySteps];
}

function applyStepIndex(state: WizardState, nextStep: number): WizardState {
  const bounded = Math.min(Math.max(nextStep, 0), ANTMINER_STEPS.length - 1);
  return {
    ...state,
    currentStep: bounded,
    currentStepId: ANTMINER_STEPS[bounded]?.id ?? 'welcome',
  };
}

// ─── Component ─────────────────────────────────────────────
interface SetupWizardProps {
  onComplete: (config: {
    minerName: string;
    mode: OperatingMode;
    pool: PoolConfig;
    donation: DonationStepValue;
    password: string;
    apiToken?: string | null;
  }) => void;
  setupStatus: SetupStatusResponse | null;
}

function loadSavedDashboardAuth(): { apiToken: string | null; password: string | null } {
  // Credentials are owned by api/credentials (session token + in-memory
  // password), never the persisted dcentos-settings blob.
  return { apiToken: getSessionToken(), password: getVolatilePassword() };
}

function fromSetupMode(mode: string): OperatingMode | null {
  if (mode === 'home') return 'heater';
  if (mode === 'standard' || mode === 'hacker') return mode;
  return null;
}

function stepIndex(id: StepId): number {
  return ANTMINER_STEPS.findIndex(s => s.id === id);
}

// ─── Board gating (BUG 5) ──────────────────────────────────
// The PSU Override step (Loki / bare-APW3 / stock-APW12) is an am2/am3 Zynq
// concept: it declares whether the unit has a smart-APW12 SMBus PSU, a Loki
// spoof board, or a bare APW3 12.8 V rail. NONE of that applies to the
// Antminer S9 (am1-s9), which controls chip voltage through a PIC16F1704 DAC
// and has no smart SMBus PSU and no Loki board. Showing an S9 operator an
// APW12-SMBus / Loki PSU declaration is wrong, so we auto-skip the step on
// am1-s9 (the same auto-advance mechanism already used for DC/solar power
// sources — keeps the kit StepRail 1:1 with the active registry and never desyncs the rail
// index accounting).
//
// Detection: /api/system/info's `board` field is the canonical board id the
// daemon derives from control-board detection (am1-s9 / am2-* / AML*). The
// S9's `antminer_board_version()` returns the literal "am1-s9". We treat any
// board id that starts with "am1" (the Zynq-7010 S9/am1 family) OR contains
// "s9" as a no-smart-PSU board for PSU-override purposes.
function boardHasSmartPsuOverride(boardTarget: string | null): boolean {
  if (!boardTarget) {
    // Unknown board (info fetch failed / older daemon): DON'T hide the step.
    // The PSU-override choice is optional fleet-inventory metadata and the
    // step is skippable, so leaving it visible on an unknown board is the
    // safe default (it never blocks advance). We only AFFIRMATIVELY skip it
    // when we positively know the board is an S9-class am1 unit.
    return true;
  }
  const b = boardTarget.trim().toLowerCase();
  // am1 = Zynq-7010 S9-class control board: PIC16F1704 voltage, no APW12
  // SMBus, no Loki. The PSU-override declaration is meaningless there.
  if (b.startsWith('am1') || b === 's9' || b.includes('-s9') || b.endsWith('s9')) {
    return false;
  }
  return true;
}

export function SetupWizard({ onComplete, setupStatus }: SetupWizardProps) {
  // Self-contained kit visual system (we cannot touch src/styles/*).
  ensureKitStyles();

  const [state, setState] = useState<WizardState>(loadWizardState);
  const [phase, setPhase] = useState<'editing' | 'rebooting'>('editing');
  // BUG 5: the daemon-reported board id (e.g. "am1-s9" / "am2-s19jpro"). Used
  // to board-gate the PSU Override step. `null` until /api/system/info
  // answers; an unknown board never hides the step (see
  // boardHasSmartPsuOverride). Kept in a ref too so the psu_override
  // auto-skip effect can read the latest value without re-subscribing.
  const [boardTarget, setBoardTarget] = useState<string | null>(null);
  const boardTargetRef = useRef<string | null>(null);
  const [deviceFamily, setDeviceFamily] = useState<DeviceFamily>('antminer');
  // Direction of the last navigation (+1 = forward via goNext, -1 = back via
  // goBack) so the psu_override auto-skip effect skips the SAME way the user was
  // moving. Without this, Back into the auto-skipped step re-fires the effect
  // and bounces the user forward again — they can never step back past it.
  const navDirRef = useRef(1);
  const [pendingCompletion, setPendingCompletion] = useState<CompletedSetupConfig | null>(null);
  const [skipConfirmOpen, setSkipConfirmOpen] = useState(false);
  const [skipBusy, setSkipBusy] = useState(false);
  const [skipError, setSkipError] = useState<string | null>(null);
  const [reconnectAttempts, setReconnectAttempts] = useState(0);
  const [rebootPhase, setRebootPhase] = useState<RebootPhase>('writing');
  const reconnectAttemptsRef = useRef(0);
  const disconnectObservedRef = useRef(false);
  const stepHeadingRef = useRef<HTMLDivElement>(null);
  const skipContinueBtnRef = useRef<HTMLButtonElement>(null);
  const savedAuth = loadSavedDashboardAuth();
  const hasSavedAuth = Boolean(savedAuth.apiToken || savedAuth.password);
  const resumeRequiresPassword = Boolean(setupStatus?.resume_requires_auth) && !hasSavedAuth;
  const passwordAlreadyVerified = Boolean(setupStatus?.auth?.password_set) && hasSavedAuth;

  // Persist on every change.
  useEffect(() => {
    saveWizardState(state);
  }, [state]);

  // BUG 5: fetch the daemon-reported board id once so the PSU Override step
  // can be board-gated. Best-effort — if /api/system/info fails (offline,
  // older daemon) we leave boardTarget null and the step stays visible
  // (skippable, never blocking). The S9 (am1-s9) returns board="am1-s9".
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const info = await api.getSystemInfo();
        if (cancelled) return;
        const board = (info.board || '').trim() || null;
        boardTargetRef.current = board;
        setBoardTarget(board);
        setDeviceFamily(board ? setupFamilyFromBoardTarget(board) : 'antminer');
      } catch {
        // leave boardTarget null — unknown board never hides the step.
      }
    })();
    return () => { cancelled = true; };
  }, []);

  // A11y: focus the step container on navigation.
  useEffect(() => {
    if (phase !== 'editing') return;
    stepHeadingRef.current?.focus();
  }, [state.currentStep, phase]);

  //  (2026-05-22): visibility gate for the new PSU Override step.
  // PSU override declaration is only meaningful for grid/hybrid power sources;
  // for DC/solar setups the daemon's PSU path is not on the bring-up critical
  // path. Auto-advance past psu_override when the operator chose direct_dc /
  // solar_battery — keeps the step out of the way without burying it behind a
  // render-time conditional (which would break the kit's StepRail position
  // accounting).
  useEffect(() => {
    if (phase !== 'editing') return;
    const cur = ANTMINER_STEPS[state.currentStep];
    if (cur?.id !== 'psu_override') return;
    // Auto-skip the PSU Override step when it is meaningless:
    //   (1) BUG 5 — the board is an S9-class am1 unit (PIC16F1704 voltage,
    //       no smart-APW12 SMBus PSU, no Loki board); OR
    //   (2) the operator chose a DC/solar power source (the existing
    //       grid/hybrid gate).
    const boardSkips = !boardHasSmartPsuOverride(boardTarget ?? boardTargetRef.current);
    const powerSkips =
      Boolean(state.powerSource) &&
      state.powerSource !== 'grid' &&
      state.powerSource !== 'hybrid';
    if (boardSkips || powerSkips) {
      // Skip in the direction the user was navigating: forward (goNext) lands on
      // donation, Back (goBack) lands on power — so Back is not bounced forward.
      // setState's functional form prevents the double-render dance from racing
      // the persist-on-change effect.
      const dir = navDirRef.current < 0 ? -1 : 1;
      setState(prev => applyStepIndex(prev, prev.currentStep + dir));
    }
  }, [state.currentStep, state.powerSource, phase, boardTarget]);

  // P2-4 (§4.E): visibility gate for the Home step (electricity rate + currency
  // + quiet-hours). These are space-heater/home concepts, so the step is only
  // meaningful in heater mode. Auto-advance past it in Standard/Hacker — same
  // direction-aware auto-skip mechanism as psu_override so Back is not bounced
  // forward and the kit StepRail position accounting never desyncs.
  useEffect(() => {
    if (phase !== 'editing') return;
    const cur = ANTMINER_STEPS[state.currentStep];
    if (cur?.id !== 'home') return;
    if (state.mode !== 'heater') {
      const dir = navDirRef.current < 0 ? -1 : 1;
      setState(prev => applyStepIndex(prev, prev.currentStep + dir));
    }
  }, [state.currentStep, state.mode, phase]);

  // Honest reconnect poll (BUG 6 — never infinite-loop).
  //
  // The config is ALREADY persisted (applyConfig() ran completeSetup() before
  // api.reboot()), so the only question this poll answers is "can we go to the
  // dashboard yet?". Phases advance only on real signals — we never fabricate
  // "Online" before the daemon truthfully answers.
  //
  // The original gate exited only on
  //   `!needs_setup && (disconnectObserved || attempts >= 5)`.
  // That dead-ends in two real scenarios seen on the .138 live install:
  //   (a) The operator opted out of a password → api.reboot() is 403'd (no
  //       token) → the daemon never restarts → never disconnects, so a
  //       successful poll that returns needs_setup=false still had to wait for
  //       the attempts>=5 fallback (it did eventually fire, but the UX was a
  //       long opaque "reconnecting").
  //   (b) Restarting the daemon to engage mining CRASHED it and :8080 never
  //       came back → getSetupStatus() throws on EVERY poll → the success
  //       branch is never reached → `disconnectObserved` is true but
  //       `!needs_setup` can never be observed → the overlay polls FOREVER and
  //       the operator is stranded.
  //
  // Fix: two bounded, honest exits.
  //   1. SETUP-COMPLETE: a successful poll reporting setup-done
  //      (needs_setup === false, or completed_at set, or progress.complete)
  //      redirects to the dashboard — promptly, regardless of whether a
  //      disconnect was ever observed. This is the truthful "daemon answered,
  //      we're good" path → phase 'done' ('Online').
  //   2. HARD BOUND: after MAX_RECONNECT_ATTEMPTS, redirect to the dashboard
  //      EVEN IF the daemon never answered again. The config is saved; the
  //      dashboard's own DaemonStatusBanner / BootPhaseBanner then truthfully
  //      surface the daemon-down / booting state. We do NOT claim 'Online' on
  //      this path (phase stays 'reconnecting') — connecting ≠ connected.
  const MAX_RECONNECT_ATTEMPTS = 12; // ~60 s under failed-fetch browser cadence.
  const RECONNECT_STATUS_TIMEOUT_MS = 1000;
  const SETUP_COMPLETE_MIN_ATTEMPTS = 1;
  useEffect(() => {
    if (phase !== 'rebooting' || !pendingCompletion) return;

    let cancelled = false;
    let timer: ReturnType<typeof setInterval> | null = null;

    const finish = (donePhase: RebootPhase) => {
      if (cancelled) return;
      cancelled = true; // latch — never call onComplete twice
      if (timer) clearInterval(timer);
      setRebootPhase(donePhase);
      onComplete(pendingCompletion);
    };

    const setupReportsDone = (status: SetupStatusResponse): boolean =>
      status.needs_setup === false ||
      Boolean(status.completed_at) ||
      Boolean(status.progress?.complete);

    const pollReconnect = async () => {
      const attempt = reconnectAttemptsRef.current + 1;
      reconnectAttemptsRef.current = attempt;
      setReconnectAttempts(attempt);

      try {
        const status = await api.getSetupStatus(RECONNECT_STATUS_TIMEOUT_MS);
        if (cancelled) return;
        // The daemon answered — surface the honest 'reconnecting' phase
        // (unless a disconnect was observed, in which case the kit already
        // shows the reboot timeline).
        if (!disconnectObservedRef.current) {
          setRebootPhase('reconnecting');
        }
        // EXIT 1 — daemon truthfully reports setup is complete.
        if (setupReportsDone(status) && attempt >= SETUP_COMPLETE_MIN_ATTEMPTS) {
          finish('done');
          return;
        }
      } catch {
        if (cancelled) return;
        // Daemon is unreachable — that's an honest disconnect signal, NOT a
        // completion signal. Show 'reconnecting'; do not fabricate progress.
        disconnectObservedRef.current = true;
        setRebootPhase('reconnecting');
      }

      // EXIT 2 — HARD BOUND. Never strand the operator. The config is saved;
      // hand off to the dashboard, which truthfully reports daemon health.
      // Stay on 'reconnecting' (we did NOT confirm Online) so we never lie.
      if (!cancelled && reconnectAttemptsRef.current >= MAX_RECONNECT_ATTEMPTS) {
        finish('reconnecting');
      }
    };

    setRebootPhase('rebooting');
    const initialTimer = setTimeout(() => {
      void pollReconnect();
      timer = setInterval(() => { void pollReconnect(); }, 3000);
    }, 2000);

    return () => {
      cancelled = true;
      clearTimeout(initialTimer);
      if (timer) clearInterval(timer);
    };
  }, [onComplete, pendingCompletion, phase]);

  // setupStatus-driven hydrate + resume jump — UNCHANGED logic, remapped
  // onto the new step ids (mode lives in its own step now).
  useEffect(() => {
    if (!setupStatus) return;

    setState(prev => {
      const current = setupStatus.current;
      const next: WizardState = {
        ...prev,
        minerName: current?.hostname && prev.minerName === DEFAULT_STATE.minerName ? current.hostname : prev.minerName,
        mode: prev.mode ?? fromSetupMode(current?.mode ?? ''),
        powerSource: prev.powerSource ?? current?.power_source ?? setupStatus.power_source ?? null,
        circuitVoltage: prev.circuitVoltage ?? current?.circuit_voltage_v ?? null,
        circuitAmperage: prev.circuitAmperage ?? current?.circuit_amperage_a ?? null,
        pool: {
          url: prev.pool.url || current?.pool?.url || '',
          worker: prev.pool.worker || current?.pool?.worker || '',
          password: prev.pool.password || 'x',
        },
        // P2-4 (§4.E): prefill the Home step's rate/currency draft once, from a
        // daemon-confirmed rate, when the operator hasn't drafted one yet.
        homeComfort:
          prev.homeComfort.electricityRate === ''
          && typeof current?.electricity_rate === 'number'
          && current?.electricity_rate_calibrated === true
            ? {
                ...prev.homeComfort,
                electricityRate: String(current.electricity_rate),
                currency: current?.currency || prev.homeComfort.currency,
              }
            : prev.homeComfort,
      };

      if (setupStatus.resume_requires_auth && !hasSavedAuth) {
        next.currentStep = stepIndex('password');
      } else if (prev.currentStep === DEFAULT_STATE.currentStep) {
        if (!next.mode) {
          next.currentStep = stepIndex('welcome');
        } else if ((next.powerSource === 'grid' || next.powerSource === 'hybrid') && (!next.circuitVoltage || !next.circuitAmperage)) {
          next.currentStep = stepIndex('power');
        } else if (!next.minerName.trim()) {
          next.currentStep = stepIndex('name');
        } else if (!setupStatus.auth?.password_set && !next.password) {
          next.currentStep = stepIndex('password');
        }
      }

      return applyStepIndex(next, next.currentStep);
    });
  }, [hasSavedAuth, setupStatus]);

  const update = useCallback((patch: Partial<WizardState>) => {
    setState(prev => {
      const next = { ...prev, ...patch };
      if (patch.currentStepId) {
        const idx = stepIndex(patch.currentStepId);
        if (idx >= 0) next.currentStep = idx;
      } else if (patch.currentStep !== undefined) {
        next.currentStepId = ANTMINER_STEPS[next.currentStep]?.id ?? prev.currentStepId;
      }
      return next;
    });
  }, []);

  const currentStep = state.currentStep;
  const step = ANTMINER_STEPS[currentStep] ?? ANTMINER_STEPS[0];
  const activeSteps = useMemo(
    () => getActiveSteps(state.setupPath, deviceFamily),
    [deviceFamily, state.setupPath],
  );
  const activeStepIndex = Math.max(0, activeSteps.findIndex(s => s.id === step.id));
  const isFirst = activeStepIndex === 0;

  // KIT_STEPS (rail) is 1:1 with the active registry by id — completed set is keyed by id.
  const completed = new Set<string>(
    activeSteps.slice(0, activeStepIndex).map(s => s.id),
  );

  useEffect(() => {
    if (activeSteps.some(s => s.id === step.id)) return;
    update({ currentStepId: activeSteps[0]?.id ?? 'welcome' });
  }, [activeSteps, step.id, update]);

  const goNext = useCallback(() => {
    if (activeStepIndex < activeSteps.length - 1) {
      navDirRef.current = 1;
      update({ currentStepId: activeSteps[activeStepIndex + 1].id });
    }
  }, [activeStepIndex, activeSteps, update]);
  const goBack = useCallback(() => {
    if (activeStepIndex > 0) {
      navDirRef.current = -1;
      update({ currentStepId: activeSteps[activeStepIndex - 1].id });
    }
  }, [activeStepIndex, activeSteps, update]);
  const goToStep = useCallback((idx: number) => {
    if (idx < 0 || idx >= activeSteps.length || idx === activeStepIndex) return;
    navDirRef.current = idx > activeStepIndex ? 1 : -1;
    update({ currentStepId: activeSteps[idx].id });
  }, [activeStepIndex, activeSteps, update]);
  const goToStepId = useCallback((id: StepId) => {
    const idx = activeSteps.findIndex(s => s.id === id);
    if (idx >= 0) {
      goToStep(idx);
      return;
    }
    const canonicalIndex = stepIndex(id);
    if (canonicalIndex < 0) return;
    navDirRef.current = canonicalIndex > currentStep ? 1 : -1;
    update({ setupPath: 'guided', currentStepId: id });
  }, [activeSteps, currentStep, goToStep, update]);
  const handleSkip = useCallback(() => {
    goNext();
  }, [goNext]);
  const startSetupPath = useCallback((setupPath: SetupPath) => {
    navDirRef.current = 1;
    if (setupPath === 'quick') {
      update({
        setupPath: 'quick',
        mode: state.mode ?? 'standard',
        currentStepId: 'pool',
      });
      return;
    }
    update({ setupPath: 'guided', currentStepId: 'network' });
  }, [state.mode, update]);

  async function applyConfig(config: {
    setupPath: SetupPath;
    minerName: string;
    mode: OperatingMode;
    powerSource: string | null;
    circuitVoltage: number | null;
    circuitAmperage: number | null;
    pool: PoolConfig;
    donation: DonationStepValue;
    password: string;
    safetyOptedOut: boolean;
    psuOverrideEnabled: boolean | null;
    psuHardwareVariant: PsuHardwareVariant | null;
    homeComfort: HomeComfortStepValue;
  }): Promise<CompletedSetupConfig> {
    let apiToken: string | null = null;
    let existingToken: string | null = null;
    const shouldPersistHostname =
      config.setupPath !== 'quick' || config.minerName.trim() !== DEFAULT_STATE.minerName;
    const hostname = shouldPersistHostname ? config.minerName
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-|-$/g, '') : '';

    // Credentials are owned by api/credentials, never the durable
    // dcentos-settings blob: the bearer token lives in sessionStorage, the
    // plaintext password is held in memory only (never persisted at rest).
    existingToken = getSessionToken();
    if (config.password && config.password.length >= 8) {
      setVolatilePassword(config.password);
    }

    const backendPasswordSet = Boolean(setupStatus?.resume_requires_auth);

    if (config.password && config.password.length >= 8 && !existingToken && !backendPasswordSet) {
      const res = await fetch('/api/auth/setup', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ password: config.password }),
      });
      if (!res.ok && res.status !== 409) {
        throw new Error(await res.text() || 'Failed to configure authentication');
      }
      if (res.ok) {
        const data = await res.json();
        apiToken = data.session_token || data.api_token || data.session?.session_token || null;
      }
      if (apiToken) {
        // Token → sessionStorage; password → in-memory only. Never the
        // durable dcentos-settings blob.
        setSessionToken(apiToken);
        setVolatilePassword(config.password);
      }
    } else {
      apiToken = existingToken;
    }

    if (!apiToken && backendPasswordSet && config.password && config.password.length >= 8) {
      apiToken = await api.createSession(config.password);
    }

    // Freedom-first honest opt-out (UNCHANGED): real ack vs api.skipSafety().
    const safetyEngaged =
      !config.safetyOptedOut
      && (Boolean(config.powerSource) || (config.circuitVoltage !== null && config.circuitAmperage !== null));
    if (safetyEngaged) {
      await api.setupSafety();
    } else {
      try {
        await api.skipSafety();
      } catch (err) {
        const status = (err as { status?: number } | null)?.status;
        if (status !== 409) throw err;
      }
    }

    //  (2026-05-22): persist the operator's PSU hardware declaration
    // BEFORE setupCircuit so the daemon picks up [power.psu_override] on the
    // same reboot that finalizes the wizard. The Rust block is byte-identical
    // for `loki` and `bare-apw3` (enabled=true, model="APW3", voltage_v=12.8);
    // stock-apw12 sets enabled=false so the daemon runs the canonical
    // smart-APW12 SMBus handshake instead. The variant tag is a fleet-
    // inventory record, included as a non-standard request field that older
    // daemons ignore. Failures here are logged but non-fatal (a daemon that
    // doesn't accept the request should not block setup completion; the
    // operator can re-issue via dashboard PSU settings post-boot).
    // Agent 1F enablement (2026-05-22): stock-apw12 is now selectable.
    if (config.psuOverrideEnabled !== null) {
      try {
        await api.updatePsuOverride({
          enabled: Boolean(config.psuOverrideEnabled),
          // model/voltage_v are required by the PsuOverrideRequest type. For
          // stock-apw12 (enabled=false) the daemon ignores them and uses the
          // canonical smart-APW12 path; passing them keeps the request shape
          // stable across all three scenarios.
          model: 'APW3',
          voltage_v: 12.8,
          // psu_hardware_variant is an optional fleet-inventory tag that
          // Agent 1B () extends the /api/config/psu-override request
          // shape with.  daemons that don't understand the field
          // serde-ignore it; backward-compatible by construction.
          ...(config.psuHardwareVariant
            ? { psu_hardware_variant: config.psuHardwareVariant }
            : {}),
        } as Parameters<typeof api.updatePsuOverride>[0]);
      } catch (err) {
        console.warn('PSU override declaration was not persisted during setup', err);
      }
    }

    if (config.powerSource || (config.circuitVoltage && config.circuitAmperage)) {
      const circuitResult = await api.setupCircuit({
        source: config.powerSource,
        voltage: config.circuitVoltage,
        amperage: config.circuitAmperage,
      });
      if (typeof circuitResult === 'object' && circuitResult && 'persisted' in circuitResult && circuitResult.persisted === false) {
        throw new Error('Failed to save power commissioning settings');
      }
    }

    const modeResult = await api.setupMode(config.mode, hostname || undefined);
    if (typeof modeResult === 'object' && modeResult && 'persisted' in modeResult && modeResult.persisted === false) {
      throw new Error('Failed to save operating mode settings');
    }

    if (config.pool.url && config.pool.worker) {
      const poolResult = await api.setupPool({
        url: config.pool.url,
        worker: config.pool.worker,
        password: config.pool.password || 'x',
      });
      if (typeof poolResult === 'object' && poolResult && 'persisted' in poolResult && poolResult.persisted === false) {
        throw new Error('Failed to save pool configuration');
      }
    }

    if (config.setupPath !== 'quick') {
      try {
        await api.updateDonationConfig({
          enabled: Boolean(config.donation?.enabled),
          percent: Math.max(0, Math.min(5, config.donation?.percent ?? DEFAULT_DONATION_PERCENT)),
          pool_url: DEFAULT_DONATION_POOL_URL,
          worker: DEFAULT_DONATION_WORKER,
          password: DEFAULT_DONATION_PASSWORD,
          fallback_enabled: true,
          fallback_pool_url: DEFAULT_DONATION_FALLBACK_POOL_URL,
          fallback_worker: DEFAULT_DONATION_FALLBACK_WORKER,
          fallback_password: DEFAULT_DONATION_PASSWORD,
          cycle_duration_s: DEFAULT_DONATION_CYCLE_S,
        });
      } catch (err) {
        console.warn('Donation settings were not saved during setup', err);
      }
    }

    // P2-4 (§4.E): persist the operator-confirmed electricity rate + currency to
    // the daemon [home] config (the SINGLE SOURCE OF TRUTH — api.setupEconomics
    // also flips electricity_rate_calibrated so cost/earnings stop reading as an
    // uncalibrated estimate) and the quiet-hours night-mode schedule. Best-effort
    // + non-fatal: a daemon that rejects them must not block setup completion
    // (the operator can re-issue from Settings / the Heater page). A blank rate
    // is intentionally left unset — the daemon default stays "uncalibrated".
    if (config.setupPath !== 'quick') {
      const economicsRate = Number(config.homeComfort.electricityRate);
      if (config.homeComfort.electricityRate.trim() !== ''
          && Number.isFinite(economicsRate) && economicsRate >= 0 && economicsRate <= 10) {
        try {
          await api.setupEconomics({
            electricity_rate: economicsRate,
            currency: config.homeComfort.currency || 'USD',
          });
        } catch (err) {
          console.warn('Electricity economics were not saved during setup', err);
        }
      }
      if (config.homeComfort.quietHoursEnabled) {
        try {
          // Routes through /api/setup/quiet-hours (the setup-namespaced alias of
          // /api/home/night-mode) so it is allowed by the pre-device-ready auth
          // gate during the wizard.
          await api.setupQuietHours({
            enabled: true,
            start_hour: config.homeComfort.quietStartHour,
            end_hour: config.homeComfort.quietEndHour,
            // Safety (load-bearing): the home fan PWM ceiling is 30. The daemon
            // night-mode writer clamps to the safety max regardless; we send 30.
            max_fan_pwm: 30,
            power_reduction_pct: config.homeComfort.quietPowerReductionPct,
          });
        } catch (err) {
          console.warn('Quiet hours were not saved during setup', err);
        }
      }
    }

    const hasOwnerPassword = Boolean(config.password && config.password.length >= 8) || backendPasswordSet;
    if (!hasOwnerPassword) {
      try {
        await api.skipPassword();
      } catch (err) {
        const status = (err as { status?: number } | null)?.status;
        if (status !== 409) throw err;
      }
    }

    await api.completeSetup();

    clearWizardState();
    return {
      minerName: config.minerName,
      mode: config.mode,
      pool: config.pool,
      donation: normalizeDonationStepValue(config.donation),
      password: config.password,
      apiToken,
    };
  }

  function handleSkipAll() {
    setSkipError(null);
    setSkipConfirmOpen(true);
  }

  async function runTerminalSkip() {
    if (skipBusy) return;
    setSkipBusy(true);
    setSkipError(null);

    const chosenMode: OperatingMode = state.mode ?? 'standard';
    const minerName = state.minerName.trim() || 'My Miner';
    const hostname = minerName
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-|-$/g, '');

    try {
      try {
        await api.skipSafety();
      } catch (err) {
        const status = (err as { status?: number } | null)?.status;
        if (status !== 409) throw err;
      }
      const modeResult = await api.setupMode(chosenMode, hostname || undefined);
      if (typeof modeResult === 'object' && modeResult && 'persisted' in modeResult && modeResult.persisted === false) {
        throw new Error('Failed to save operating mode settings');
      }
      try {
        await api.skipPassword();
      } catch (err) {
        const status = (err as { status?: number } | null)?.status;
        if (status !== 409) throw err;
      }
      await api.completeSetup();
      clearWizardState();
      onComplete({
        minerName,
        mode: chosenMode,
        pool: state.pool,
        donation: normalizeDonationStepValue(state.donation),
        password: '',
        apiToken: null,
      });
    } catch (err) {
      setSkipBusy(false);
      setSkipError(
        err instanceof Error
          ? err.message
          : 'Could not skip setup. You can continue with the wizard instead.',
      );
    }
  }

  const canProceed = useCallback((): boolean => {
    switch (step.id) {
      case 'welcome': return true; // Welcome owns its own CTA
      case 'network': return true; // optional/informational
      case 'mode': return state.mode !== null;
      case 'power':
        if (state.powerSource === 'grid' || state.powerSource === 'hybrid') {
          return state.circuitVoltage !== null && state.circuitAmperage !== null;
        }
        return true;
      case 'circuit':
        if (state.circuitVoltage === null && state.circuitAmperage === null) return true;
        return state.circuitVoltage !== null && state.circuitAmperage !== null;
      case 'pool': return true;
      case 'psu_override': return true; // step is skippable; choice is optional fleet-inventory metadata
      case 'donation': return true;
      case 'home': return true; // skippable; rate/quiet-hours are optional home comfort settings
      case 'calibration': return true; // optional
      case 'name': return state.minerName.trim().length > 0;
      case 'password':
        if (passwordAlreadyVerified) return true;
        if (resumeRequiresPassword) return state.password.length >= 8;
        if (state.mode === 'hacker') {
          return state.password.length >= 8
            && state.confirmPassword.length > 0
            && state.password === state.confirmPassword;
        }
        if (state.password.length === 0) return true;
        return state.password.length >= 8
          && state.confirmPassword.length > 0
          && state.password === state.confirmPassword;
      case 'review': return true;
      default: return true;
    }
  }, [
    passwordAlreadyVerified,
    resumeRequiresPassword,
    state.circuitAmperage,
    state.circuitVoltage,
    state.confirmPassword,
    state.minerName,
    state.mode,
    state.password,
    state.powerSource,
    step.id,
  ]);

  async function handleApply() {
    const mode = state.mode || 'standard';
    const config = {
      setupPath: state.setupPath,
      minerName: state.minerName.trim() || 'My Miner',
      mode,
      powerSource: state.powerSource,
      circuitVoltage: state.circuitVoltage,
      circuitAmperage: state.circuitAmperage,
      pool: state.pool,
      donation: normalizeDonationStepValue(state.donation),
      password: state.password,
      safetyOptedOut: state.safetyOptedOut,
      psuOverrideEnabled: state.psuOverrideEnabled,
      psuHardwareVariant: state.psuHardwareVariant,
      homeComfort: state.homeComfort,
    };

    setRebootPhase('writing');
    const completion = await applyConfig(config);
    setPendingCompletion(completion);
    reconnectAttemptsRef.current = 0;
    setReconnectAttempts(0);
    disconnectObservedRef.current = false;
    setPhase('rebooting');

    try {
      await api.reboot();
    } catch {
      // Expected once the miner begins rebooting.
    }
  }

  // Keyboard: Enter advances when valid; Esc blurs. UNCHANGED semantics.
  useEffect(() => {
    if (phase !== 'editing') return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Enter' && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
        const t = e.target as HTMLElement | null;
        const tag = t?.tagName;
        const isFormControl = tag === 'TEXTAREA'
          || (tag === 'INPUT' && (t as HTMLInputElement).type === 'range')
          || tag === 'BUTTON'
          || tag === 'A';
        if (isFormControl) return;
        if (step.id === 'welcome' || step.id === 'review') return;
        if (canProceed()) {
          e.preventDefault();
          goNext();
        }
      } else if (e.key === 'Escape') {
        const t = e.target as HTMLElement | null;
        if (t && typeof (t as HTMLInputElement).blur === 'function') {
          (t as HTMLInputElement).blur();
        }
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [canProceed, goNext, phase, step.id]);

  // ─── Reboot / reconnect overlay (kit breathing-orb) ──────
  if (phase === 'rebooting' && pendingCompletion) {
    const todo: string[] = [];
    if (!state.pool.url || !state.pool.worker) {
      todo.push('Open Pool Setup in Mining mode and add your payout worker.');
    }
    if ((state.powerSource === 'grid' || state.powerSource === 'hybrid') && (!state.circuitVoltage || !state.circuitAmperage)) {
      todo.push('Review Circuit Check so DCENT_OS has a safe power ceiling for this install.');
    }
    return (
      <div className="wiz-shell mode-standard wizard-shell">
        <RebootReconnectOverlay
          phase={rebootPhase}
          reconnectAttempts={reconnectAttempts}
          modeLabel={pendingCompletion.mode}
          todo={todo}
        />
      </div>
    );
  }

  // ─── Render the current step ─────────────────────────────
  function renderStep() {
    switch (step.id) {
      case 'welcome':
        return (
          <WelcomeStep
            onQuickStart={() => startSetupPath('quick')}
            onGuidedStart={() => startSetupPath('guided')}
            onSkipAll={handleSkipAll}
          />
        );
      case 'network':
        return <NetworkStep value={state.network} onChange={network => update({ network })} />;
      case 'mode':
        return <ModeStep value={state.mode} onChange={mode => update({ mode })} />;
      case 'power':
        return (
          <PowerSourceStep
            currentSource={state.powerSource}
            circuitVoltage={state.circuitVoltage}
            circuitAmperage={state.circuitAmperage}
            onSourceChange={powerSource => update({
              powerSource,
              ...(powerSource === 'grid' || powerSource === 'hybrid'
                ? {}
                : { circuitVoltage: null, circuitAmperage: null }),
            })}
            onCircuitVoltageChange={circuitVoltage => update({ circuitVoltage })}
            onCircuitAmperageChange={circuitAmperage => update({ circuitAmperage })}
          />
        );
      case 'circuit':
        return (
          <CircuitConfigStep
            voltage={state.circuitVoltage}
            amperage={state.circuitAmperage}
            derate={state.circuitDerate}
            onVoltageChange={circuitVoltage => update({ circuitVoltage, safetyOptedOut: false })}
            onAmperageChange={circuitAmperage => update({ circuitAmperage, safetyOptedOut: false })}
            onDerateChange={circuitDerate => update({ circuitDerate })}
            onSkip={() => {
              update({ circuitVoltage: null, circuitAmperage: null, safetyOptedOut: true });
              goNext();
            }}
          />
        );
      case 'pool':
        return (
          <PoolStep
            value={state.pool}
            mode={state.mode}
            minerName={state.minerName}
            onChange={pool => update({ pool })}
          />
        );
      case 'psu_override':
        //  (2026-05-22): visibility gate handled by the auto-advance
        // useEffect above; we render the step only when grid/hybrid was chosen.
        // Agent 1F (2026-05-22): stock-apw12 is now a real selectable option
        // that sets psu_override.enabled=FALSE so the daemon runs the canonical
        // smart-APW12 SMBus handshake. Loki / bare-apw3 stay on
        // psu_override.enabled=TRUE per the EE-LOKI three-scenario contract.
        return (
          <PsuOverrideStep
            value={{ psuHardwareVariant: state.psuHardwareVariant }}
            onChange={next =>
              update({
                psuHardwareVariant: next.psuHardwareVariant,
                // loki / bare-apw3  → psu_override.enabled = true
                // stock-apw12      → psu_override.enabled = false
                // null (cleared)   → leave previous value
                psuOverrideEnabled:
                  next.psuHardwareVariant === 'loki' ||
                  next.psuHardwareVariant === 'bare-apw3'
                    ? true
                    : next.psuHardwareVariant === 'stock-apw12'
                      ? false
                      : state.psuOverrideEnabled,
              })
            }
          />
        );
      case 'donation':
        return (
          <DonationStep
            value={state.donation}
            mode={state.mode}
            onChange={donation => update({ donation: normalizeDonationStepValue(donation) })}
          />
        );
      case 'home':
        // P2-4 (§4.E): heater/home-only economics + quiet-hours. Visibility
        // gated by the auto-advance effect above (rendered only in heater mode).
        return (
          <HomeComfortStep
            value={state.homeComfort}
            defaultRate={0.12}
            alreadyCalibrated={Boolean(setupStatus?.current?.electricity_rate_calibrated)}
            onChange={homeComfort => update({ homeComfort })}
          />
        );
      case 'calibration':
        return (
          <CalibrationStep
            value={state.calibration}
            onChange={calibration => update({ calibration })}
            onSkip={goNext}
          />
        );
      case 'name':
        return (
          <NameStep
            value={state.minerName}
            mode={state.mode}
            onChange={name => update({ minerName: name })}
          />
        );
      case 'password':
        return (
          <PasswordStep
            value={state.password}
            confirmValue={state.confirmPassword}
            mode={state.mode}
            resumeExistingPassword={resumeRequiresPassword}
            alreadyAuthenticated={passwordAlreadyVerified}
            onChange={password => update({ password })}
            onConfirmChange={confirmPassword => update({ confirmPassword })}
            onSkip={
              resumeRequiresPassword || state.mode === 'hacker'
                ? undefined
                : () => { update({ password: '', confirmPassword: '' }); goNext(); }
            }
          />
        );
      case 'review':
        return (
          <ReviewStep
            setupPath={state.setupPath}
            minerName={state.minerName}
            mode={state.mode || 'standard'}
            network={state.network}
            powerSource={state.powerSource}
            circuitVoltage={state.circuitVoltage}
            circuitAmperage={state.circuitAmperage}
            pool={state.pool}
            donationPercent={state.donation.percent}
            donationEnabled={state.donation.enabled}
            password={state.password}
            safetyConfirmed={state.safetyConfirmed}
            onSafetyConfirmedChange={safetyConfirmed => update({ safetyConfirmed })}
            onApply={handleApply}
            onEditStep={(stepId) => goToStepId(stepId as StepId)}
          />
        );
      default:
        return null;
    }
  }

  const ownsCta = step.id === 'welcome' || step.id === 'review';

  return (
    <div className="wiz-shell mode-standard wizard-shell">
      {/* Freedom-first inline skip-confirm — real terminal-skip path. */}
      {skipConfirmOpen && (
        <OverlayDialog
          open={skipConfirmOpen}
          onClose={() => { if (!skipBusy) { setSkipConfirmOpen(false); setSkipError(null); } }}
          ariaLabelledBy="wizard-skip-confirm-title"
          ariaLabel="Open the dashboard now?"
          dismissible={!skipBusy}
          initialFocusRef={skipContinueBtnRef as React.RefObject<HTMLElement>}
          maxWidth={460}
          width="calc(100% - 40px)"
          chrome={false}
        >
          <div className="wiz-skip-card wizard-skip-confirm-card">
            <h2 id="wizard-skip-confirm-title" className="wizard-skip-confirm-title">
              Open the dashboard now?
            </h2>
            <p id="wizard-skip-confirm-body" className="wizard-skip-confirm-body">
              We&apos;ll finish setup with safe defaults
              ({state.mode ? state.mode : 'Standard'} mode) and take you straight to
              the dashboard. No owner password and no circuit check will be set —
              that&apos;s your call.
            </p>
            <InfoBanner tone="warn" dense className="wizard-skip-confirm-rec">
              Recommended: a password protects write &amp; control actions, and the
              circuit check keeps the autotuner from tripping your breaker. Without
              them, the dashboard and logs stay viewable and changes stay locked
              until you add a password. You can do both anytime in Settings.
            </InfoBanner>
            {skipError && (
              <div role="alert" className="wiz-skip-err wizard-skip-confirm-error">
                {skipError}
              </div>
            )}
            <div className="wiz-skip-actions wizard-skip-confirm-actions">
              <button
                type="button"
                className="wiz-btn primary"
                disabled={skipBusy}
                onClick={() => { void runTerminalSkip(); }}
              >
                {skipBusy ? 'Opening…' : 'Skip — open dashboard'}
              </button>
              <button
                ref={skipContinueBtnRef}
                type="button"
                className="wiz-btn"
                disabled={skipBusy}
                onClick={() => { setSkipConfirmOpen(false); setSkipError(null); }}
              >
                Continue with setup
              </button>
            </div>
          </div>
        </OverlayDialog>
      )}

      {/* Kit header: 3-sphere molecule + DCENT_OS wordmark + SETUP pill +
          the signature 11-step numbered rail. */}
      <header className="wiz-header">
        <div className="wiz-header-brand wizard-brand">
          <DCentralMolecule size={26} />
          <span>
            DCENT<span style={{ color: 'var(--wz-accent)' }}>_</span>OS
          </span>
          <span className="wiz-header-tag">Setup</span>
        </div>
        <StepRail
          steps={activeSteps.map(s => ({
            id: s.id,
            l: s.label,
            optional: 'optional' in s ? s.optional : undefined,
          } satisfies KitStep))}
          activeIndex={activeStepIndex}
          completed={completed}
          onJump={goToStep}
        />
      </header>

      {/* Kit main: centered step body */}
      <main className="wiz-main">
        <div
          key={step.id}
          ref={stepHeadingRef}
          tabIndex={-1}
          className="wizard-step-content"
          style={{ outline: 'none', width: '100%', display: 'flex', justifyContent: 'center' }}
        >
          {renderStep()}
        </div>
      </main>

      {/* Kit footer: Welcome + Review own their own CTAs. */}
      {!ownsCta && (
        <StepFooter
          onBack={isFirst ? undefined : goBack}
          onNext={goNext}
          nextLabel={
            activeSteps[activeStepIndex + 1] && activeSteps[activeStepIndex + 1].id === 'review'
              ? 'Review →'
              : 'Continue →'
          }
          nextDisabled={!canProceed()}
          onSkip={step.skippable ? handleSkip : undefined}
          skipLabel={('optional' in step && step.optional) ? 'Skip (optional)' : 'Skip'}
        />
      )}
    </div>
  );
}
