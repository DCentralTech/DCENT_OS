import { useState, useCallback, useRef } from 'react';
import { api } from '../api/client';
import { useMinerStore } from '../store/miner';
import type { FanMode } from '../components/common/FanControl';

/** Dashboard fan presets. The daemon applies platform/home caps and RPM proof. */
export const FAN_MODE_PWM: Record<FanMode, number | null> = {
  quiet: 10,
  balanced: null,    // Auto PID — don't send PWM, let dcentrald manage
  performance: null,
  custom: null,      // User sets via slider
};

const DEBOUNCE_MS = 300;

export function useFanControl() {
  const addToast = useMinerStore(s => s.addToast);
  const [sending, setSending] = useState(false);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const sendFanCommand = useCallback(async (mode: FanMode, targetPwm?: number) => {
    setSending(true);
    try {
      await api.setFan(mode, mode === 'custom' && targetPwm != null ? targetPwm : undefined);
      const modeLabel = mode === 'custom' ? `Custom PWM ${targetPwm}` : mode;
      addToast(`Fan mode: ${modeLabel}`, 'success');
    } catch {
      addToast('Failed to set fan speed', 'error');
    }
    setSending(false);
  }, [addToast]);

  const handleModeChange = useCallback((mode: FanMode) => {
    const targetPwm = FAN_MODE_PWM[mode];
    sendFanCommand(mode, targetPwm ?? undefined);
  }, [sendFanCommand]);

  /** Debounced PWM slider change — waits 300ms before sending */
  const handlePwmChange = useCallback((pwm: number) => {
    if (debounceRef.current) {
      clearTimeout(debounceRef.current);
    }
    debounceRef.current = setTimeout(() => {
      sendFanCommand('custom', pwm);
    }, DEBOUNCE_MS);
  }, [sendFanCommand]);

  return {
    sending,
    handleModeChange,
    handlePwmChange,
    sendFanCommand,
  };
}
