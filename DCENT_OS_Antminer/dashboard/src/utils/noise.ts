import type { HeaterStatusResponse } from '../api/types';

export function getTachBackedNoiseDb(heater: HeaterStatusResponse | null | undefined): number | null {
  if (!heater || typeof heater.noise_db !== 'number' || heater.noise_db <= 0) {
    return null;
  }
  if (heater.noise_source === 'tach_estimate') {
    return heater.noise_db;
  }
  if (heater.fans?.rpm_ && (heater.fans.rpm ?? 0) > 0) {
    return heater.noise_db;
  }
  return null;
}

export function noiseUnavailableNote(heater: HeaterStatusResponse | null | undefined): string {
  return heater?.noise_note || 'Noise unavailable until fan RPM is reported';
}
