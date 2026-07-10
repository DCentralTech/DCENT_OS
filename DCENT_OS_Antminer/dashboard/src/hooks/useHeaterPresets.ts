// Heater preset data hook

import { useMinerStore } from '../store/miner';
import type { HeaterPreset } from '../api/types';

// Simplified 3-preset fallback model. Noise is not inferred from preset/PWM;
// the backend must report RPM-backed acoustic telemetry before dB is shown.
const DEFAULT_PRESETS: HeaterPreset[] = [
  { name: 'quiet', watts: 300, btu_h: 1024, noise_db: null, description: 'Low power; noise requires live RPM proof' },
  { name: 'balanced', watts: 800, btu_h: 2730, noise_db: null, description: 'Balanced heat; verify fan RPM for noise' },
  { name: 'max', watts: 1400, btu_h: 4777, noise_db: null, description: 'Maximum heat output; noise depends on live fan RPM' },
];

export function useHeaterPresets() {
  const apiPresets = useMinerStore(s => s.heaterPresets);
  return apiPresets.length > 0 ? apiPresets : DEFAULT_PRESETS;
}
