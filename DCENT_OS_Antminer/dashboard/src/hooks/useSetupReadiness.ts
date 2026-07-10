import { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../api/client';
import type { PowerCalibrationResponse, SetupStatusResponse } from '../api/types';
import { useModeNavigation } from './useModeNavigation';
import { useMinerStore } from '../store/miner';

const DISMISSED_KEY = 'dcentos-readiness-dismissed-v1';
const DEFAULT_MINER_NAME = 'My Miner';
const MAX_READINESS_TASKS = 4;

export interface ReadinessTask {
  id: string;
  label: string;
  detail: string;
  actionLabel: string;
  onAction: () => void;
}

export interface ReadinessTaskSeed {
  id: string;
  label: string;
  detail: string;
  actionLabel: string;
  page: string;
  settingsTab?: 'general' | 'security' | 'network' | 'backup' | 'appearance';
}

function readDismissedTaskIds(): string[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(DISMISSED_KEY) || '[]') as unknown;
    return Array.isArray(parsed) ? parsed.filter((id): id is string => typeof id === 'string') : [];
  } catch {
    return [];
  }
}

function saveDismissedTaskIds(ids: string[]) {
  try {
    localStorage.setItem(DISMISSED_KEY, JSON.stringify(ids));
  } catch {
    /* non-fatal */
  }
}

export function deriveSetupReadinessTaskSeeds({
  setupStatus,
  minerName,
  calibration,
}: {
  setupStatus: SetupStatusResponse | null;
  minerName: string;
  calibration: PowerCalibrationResponse | null;
}): ReadinessTaskSeed[] {
  if (!setupStatus || setupStatus.needs_setup) {
    return [];
  }

  const progress = setupStatus.progress;
  const powerSource = setupStatus.current?.power_source || setupStatus.power_source || '';
  const dcCommissioning = powerSource === 'direct_dc' || powerSource === 'solar_battery';
  const powerUndeclared = powerSource.trim().length === 0;
  const tasks: ReadinessTaskSeed[] = [];

  if (!progress?.pool) {
    tasks.push({
      id: 'pool',
      label: 'Configure your payout pool',
      detail: 'The miner is set up, but it still needs a pool URL and worker identity before it can submit shares.',
      actionLabel: 'Open Pool Setup',
      page: 'pools',
    });
  }

  if (!progress?.circuit || powerUndeclared) {
    tasks.push({
      id: powerUndeclared ? 'power_source' : 'circuit',
      label: powerUndeclared
        ? 'Declare power source and safe limit'
        : dcCommissioning ? 'Finish DC commissioning' : 'Review your safe power limit',
      detail: powerUndeclared
        ? 'Quick Start deferred power-source and circuit details. Declare them before trusting power limits or unattended operation.'
        : dcCommissioning
          ? 'This install still needs battery/DC commissioning so DCENT_OS can protect your source and wiring before mining.'
          : 'DCENT_OS still needs circuit or power-budget confirmation so it can operate safely on your installation.',
      actionLabel: powerUndeclared
        ? 'Open Power Setup'
        : dcCommissioning ? 'Open Off-Grid Setup' : 'Open Circuit Check',
      page: powerUndeclared ? 'energy/circuit' : dcCommissioning ? 'offgrid' : 'energy/circuit',
    });
  }

  if ((minerName || '').trim() === '' || minerName.trim() === DEFAULT_MINER_NAME) {
    tasks.push({
      id: 'miner_name',
      label: 'Name this miner',
      detail: 'Quick Start kept the default local name. Give the unit a clear label before managing multiple miners or heater zones.',
      actionLabel: 'Open Name Setting',
      page: 'settings/general',
      settingsTab: 'general',
    });
  }

  if (calibration && !calibration.enabled && !calibration.calibrated) {
    tasks.push({
      id: 'power_calibration',
      label: 'Calibrate wall power',
      detail: 'Power calibration is not enabled. Add a wall-meter reading so cost, heat, and efficiency labels can cite a measured reference.',
      actionLabel: 'Open Calibration',
      page: 'settings/general',
      settingsTab: 'general',
    });
  }

  if (powerSource === 'solar_battery' && !progress?.solar_provider) {
    tasks.push({
      id: 'solar_provider',
      label: 'Finish solar provider commissioning',
      detail: setupStatus.commissioning?.solar_provider_saved
        ? 'The solar provider config is saved, but the running daemon has not adopted it yet. Restart and validate the provider before trusting solar+battery automation.'
        : 'Solar+battery installs also need a separate solar-provider commissioning step. Configure Green Mining so DCENT_OS can tell the truth about the solar side of the system.',
      actionLabel: 'Open Green Mining',
      page: 'energy/green',
    });
  }

  const safetyDecisionMade = Boolean(setupStatus.safety_decision_made || setupStatus.safety_opt_out);
  if (!progress?.safety && !safetyDecisionMade) {
    tasks.push({
      id: 'safety',
      label: 'Confirm deployment safety details',
      detail: 'Older or incomplete installs may still be missing an explicit safety acknowledgement.',
      actionLabel: 'Open Safety Settings',
      page: 'settings/security',
      settingsTab: 'security',
    });
  }

  if (tasks.length === 0 && !setupStatus.mining_ready) {
    tasks.push({
      id: 'verify',
      label: 'Verify live mining readiness',
      detail: 'Setup is saved, but DCENT_OS is still waiting on live mining conditions like pool connectivity or share flow.',
      actionLabel: 'Open Pools',
      page: 'pools',
    });
  }

  return tasks;
}

export function selectVisibleReadinessTaskSeeds(
  input: Parameters<typeof deriveSetupReadinessTaskSeeds>[0],
  dismissedIds: string[] = [],
): ReadinessTaskSeed[] {
  return deriveSetupReadinessTaskSeeds(input)
    .filter(task => !dismissedIds.includes(task.id))
    .slice(0, MAX_READINESS_TASKS);
}

export function useSetupReadiness(mode: 'heater' | 'standard') {
  const setupComplete = useMinerStore(s => s.settings.setupComplete);
  const minerName = useMinerStore(s => s.settings.minerName);
  const setCurrentPage = useMinerStore(s => s.setCurrentPage);
  const { startTaskHandoff } = useModeNavigation();
  const [setupStatus, setSetupStatus] = useState<SetupStatusResponse | null>(null);
  const [calibration, setCalibration] = useState<PowerCalibrationResponse | null>(null);
  const [dismissedIds, setDismissedIds] = useState<string[]>(readDismissedTaskIds);

  useEffect(() => {
    if (!setupComplete) {
      setSetupStatus(null);
      setCalibration(null);
      return;
    }

    let cancelled = false;
    const load = async () => {
      const [statusResult, calibrationResult] = await Promise.allSettled([
        api.getSetupStatus(),
        api.getPowerCalibration(),
      ]);
      if (cancelled) {
        return;
      }
      setSetupStatus(statusResult.status === 'fulfilled' ? statusResult.value : null);
      setCalibration(calibrationResult.status === 'fulfilled' ? calibrationResult.value : null);
    };

    void load();
    const timer = setInterval(() => {
      void load();
    }, 30000);

    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [setupComplete]);

  const dismissTask = useCallback((id: string) => {
    setDismissedIds(prev => {
      if (prev.includes(id)) {
        return prev;
      }
      const next = [...prev, id];
      saveDismissedTaskIds(next);
      return next;
    });
  }, []);

  const tasks = useMemo<ReadinessTask[]>(() => {
    const seeds = selectVisibleReadinessTaskSeeds(
      { setupStatus, minerName, calibration },
      dismissedIds,
    );

    const openTask = (seed: ReadinessTaskSeed) => {
      if (seed.settingsTab) {
        try {
          localStorage.setItem('dcentos_system_tab', seed.settingsTab);
        } catch {
          /* non-fatal */
        }
      }
      if (mode === 'standard') {
        setCurrentPage(seed.page);
      } else {
        void startTaskHandoff('standard', seed.page, { returnLabel: 'Back to Heat view' });
      }
    };

    return seeds.map(seed => ({
      id: seed.id,
      label: seed.label,
      detail: seed.detail,
      actionLabel: seed.actionLabel,
      onAction: () => openTask(seed),
    }));
  }, [calibration, dismissedIds, minerName, mode, setCurrentPage, setupStatus, startTaskHandoff]);

  useEffect(() => {
    setDismissedIds(prev => {
      const activeIds = new Set(deriveSetupReadinessTaskSeeds({ setupStatus, minerName, calibration }).map(task => task.id));
      const next = prev.filter(id => activeIds.has(id));
      if (next.length !== prev.length) {
        saveDismissedTaskIds(next);
        return next;
      }
      return prev;
    });
  }, [calibration, minerName, setupStatus]);

  const summary = !setupStatus?.device_ready
    ? 'Setup saved, miner still needs commissioning'
    : setupStatus.mining_ready
      ? 'Mining ready'
      : 'Device ready, mining steps still missing';

  return {
    setupComplete,
    setupStatus,
    tasks,
    summary,
    primaryTask: tasks[0] ?? null,
    remainingTasks: tasks.length,
    showReadinessCta: Boolean(setupComplete && setupStatus && !setupStatus.mining_ready && tasks.length > 0),
    dismissTask,
  };
}
