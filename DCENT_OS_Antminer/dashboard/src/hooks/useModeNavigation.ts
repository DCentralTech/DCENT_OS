import { useCallback, useState } from 'react';
import { api } from '../api/client';
import type { OperatingMode } from '../api/types';
import { useMinerStore } from '../store/miner';

export function useModeNavigation() {
  const mode = useMinerStore(s => s.mode);
  const currentPage = useMinerStore(s => s.currentPage);
  const taskHandoff = useMinerStore(s => s.taskHandoff);
  const [switchingMode, setSwitchingMode] = useState<OperatingMode | null>(null);

  const switchMode = useCallback(async (nextMode: OperatingMode, targetPage?: string) => {
    if (switchingMode) {
      return false;
    }

    if (mode === nextMode) {
      if (!targetPage) {
        useMinerStore.getState().clearTaskHandoff();
      }
      if (targetPage) {
        useMinerStore.getState().setCurrentPage(targetPage);
      }
      return true;
    }

    setSwitchingMode(nextMode);
    try {
      await api.updateConfig({ mode: { active: nextMode } });
      useMinerStore.getState().setMode(nextMode);
      if (!targetPage) {
        useMinerStore.getState().clearTaskHandoff();
      }
      if (targetPage) {
        useMinerStore.getState().setCurrentPage(targetPage);
      }
      return true;
    } catch {
      useMinerStore.getState().addToast('Failed to switch dashboard mode', 'error');
      return false;
    } finally {
      setSwitchingMode(null);
    }
  }, [mode, switchingMode]);

  const startTaskHandoff = useCallback(async (
    nextMode: OperatingMode,
    targetPage: string,
    options?: { returnMode?: OperatingMode; returnPage?: string; returnLabel?: string },
  ) => {
    useMinerStore.getState().setTaskHandoff({
      fromMode: options?.returnMode ?? mode,
      fromPage: options?.returnPage ?? currentPage,
      toMode: nextMode,
      toPage: targetPage,
      returnLabel: options?.returnLabel,
    });

    const success = await switchMode(nextMode, targetPage);
    if (!success) {
      useMinerStore.getState().clearTaskHandoff();
    }
    return success;
  }, [currentPage, mode, switchMode]);

  const returnFromTaskHandoff = useCallback(async () => {
    if (!taskHandoff) {
      return false;
    }

    const success = await switchMode(taskHandoff.fromMode, taskHandoff.fromPage);
    if (success) {
      useMinerStore.getState().clearTaskHandoff();
    }
    return success;
  }, [switchMode, taskHandoff]);

  return { switchMode, startTaskHandoff, returnFromTaskHandoff, switchingMode, taskHandoff };
}
