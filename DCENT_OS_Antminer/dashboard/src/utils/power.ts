type StatsPower = {
  watts?: number | null;
  wall_watts?: number | null;
  efficiency_jth?: number | null;
  source?: string | null;
  source_detail?: string | null;
  live_power_available?: boolean | null;
  modeled?: boolean | null;
  note?: string | null;
  calibrated?: boolean;
  calibration_multiplier?: number | null;
  targeting?: {
    active: boolean;
    source?: string | null;
    mode?: string | null;
    preset?: string | null;
    schedule_label?: string | null;
    target_watts?: number | null;
    current_wall_watts: number;
    current_wall_watts_measured?: boolean | null;
    current_wall_watts_source_detail?: string | null;
    delta_watts?: number | null;
    comparison?: 'under' | 'near' | 'over' | null;
  } | null;
} | null | undefined;

type PowerMetadata = {
  [key: string]: unknown;
  watts?: number | null;
  wall_watts?: number | null;
  power_watts?: number | null;
  efficiency_jth?: number | null;
  source?: string | null;
  source_detail?: string | null;
  power_source_detail?: string | null;
  live_power_available?: boolean | null;
  modeled?: boolean | null;
  power_modeled?: boolean | null;
  note?: string | null;
  power_note?: string | null;
  calibrated?: boolean;
  calibration_multiplier?: number | null;
  targeting?: {
    active: boolean;
    source?: string | null;
    mode?: string | null;
    preset?: string | null;
    schedule_label?: string | null;
    target_watts?: number | null;
    current_wall_watts: number;
    current_wall_watts_measured?: boolean | null;
    current_wall_watts_source_detail?: string | null;
    delta_watts?: number | null;
    comparison?: 'under' | 'near' | 'over' | null;
  } | null;
} | null | undefined;

type HeaterPower = {
  power_watts?: number | null;
  wall_watts?: number | null;
  source?: string | null;
  power_source_detail?: string | null;
  live_power_available?: boolean | null;
  power_modeled?: boolean | null;
  power_note?: string | null;
  calibrated?: boolean;
  calibration_multiplier?: number | null;
  targeting?: {
    active: boolean;
    source?: string | null;
    mode?: string | null;
    preset?: string | null;
    schedule_label?: string | null;
    target_watts?: number | null;
    current_wall_watts: number;
    current_wall_watts_measured?: boolean | null;
    current_wall_watts_source_detail?: string | null;
    delta_watts?: number | null;
    comparison?: 'under' | 'near' | 'over' | null;
  } | null;
} | null | undefined;

type HistoryPowerPoint = {
  power_watts?: number | null;
  power_source?: string | null;
  power_source_detail?: string | null;
  live_power_available?: boolean | null;
  power_modeled?: boolean | null;
  power_calibrated?: boolean;
  power_calibration_multiplier?: number | null;
  power_note?: string | null;
} | null | undefined;

export function getWallWatts(power: StatsPower): number {
  const wallWatts = power?.wall_watts ?? 0;
  if (wallWatts > 0) {
    return wallWatts;
  }
  return power?.watts ?? 0;
}

export function hasLiveWallPower(power: StatsPower): boolean {
  const watts = getWallWatts(power);
  if (watts <= 0) {
    return false;
  }

  const source = power?.source ?? null;
  const sourceDetail = power?.source_detail ?? null;
  if (
    source === 'static_model_fallback' ||
    source === 'unavailable' ||
    sourceDetail === 'static_power_fallback_from_miner_state' ||
    sourceDetail === 'live_power_unavailable'
  ) {
    return false;
  }
  if (power?.live_power_available === false) {
    return false;
  }
  if (power?.live_power_available === true) {
    return true;
  }

  return (
    sourceDetail === 'pmbus_measured' ||
    sourceDetail === 'adc_measured' ||
    sourceDetail === 'wall_calibrated_estimate' ||
    sourceDetail === 'live_runtime_model' ||
    source === 'pmbus' ||
    source === 'adc'
  );
}

export function getLiveWallWatts(power: StatsPower): number {
  return hasLiveWallPower(power) ? getWallWatts(power) : 0;
}

export function getLivePowerEfficiencyJth(power: StatsPower): number {
  if (!hasLiveWallPower(power)) {
    return 0;
  }

  const efficiency = power?.efficiency_jth ?? 0;
  return Number.isFinite(efficiency) && efficiency > 0 ? efficiency : 0;
}

export function getLiveDisplayWallWatts(heater: HeaterPower, power: StatsPower): number {
  const heaterLiveWatts = getLiveWallWatts({
    wall_watts: heater?.wall_watts ?? null,
    source: heater?.source ?? null,
    source_detail: heater?.power_source_detail ?? null,
    live_power_available: heater?.live_power_available ?? null,
    modeled: heater?.power_modeled ?? null,
    note: heater?.power_note ?? null,
    calibrated: heater?.calibrated,
    calibration_multiplier: heater?.calibration_multiplier ?? null,
    targeting: heater?.targeting ?? null,
  });
  return heaterLiveWatts > 0 ? heaterLiveWatts : getLiveWallWatts(power);
}

export function getLiveHistoryPointWallWatts(point: HistoryPowerPoint): number {
  return getLiveWallWatts({
    wall_watts: point?.power_watts ?? null,
    source: point?.power_source ?? null,
    source_detail: point?.power_source_detail ?? null,
    live_power_available: point?.live_power_available ?? null,
    modeled: point?.power_modeled ?? null,
    note: point?.power_note ?? null,
    calibrated: point?.power_calibrated,
    calibration_multiplier: point?.power_calibration_multiplier ?? null,
  });
}

export function getDisplayPowerWatts(heater: HeaterPower, power: StatsPower): number {
  const heaterWallWatts = heater?.wall_watts ?? 0;
  if (heaterWallWatts > 0) {
    return heaterWallWatts;
  }

  const statsWallWatts = getWallWatts(power);
  if (statsWallWatts > 0) {
    return statsWallWatts;
  }

  const heaterBoardWatts = heater?.power_watts ?? 0;
  if (heaterBoardWatts > 0) {
    return heaterBoardWatts;
  }

  return power?.watts ?? 0;
}

export function getPowerTelemetryLabel(power: unknown): string | null {
  const data = power as PowerMetadata;
  if (!data) {
    return null;
  }

  const source = data.source ?? null;
  const sourceDetail = data.source_detail ?? data.power_source_detail ?? null;

  if (sourceDetail === 'pmbus_measured' || source === 'pmbus') {
    return 'PMBus measured power';
  }

  if (sourceDetail === 'adc_measured' || source === 'adc') {
    return 'ADC measured power';
  }

  if (sourceDetail === 'live_power_unavailable' || source === 'unavailable') {
    return 'Power telemetry unavailable';
  }

  if (data.live_power_available === false || source === 'static_model_fallback') {
    return 'Modeled fallback estimate';
  }

  if (data.calibrated) {
    if (data.calibration_multiplier) {
      return `Wall-meter calibrated estimate x${data.calibration_multiplier.toFixed(3)}`;
    }
    return 'Wall-meter calibrated estimate';
  }

  if (source === 'live' || source === 'live_power_watch' || sourceDetail === 'live_runtime_model') {
    return 'Live modeled wall estimate';
  }

  return 'Modeled wall estimate';
}

function getTargetModeLabel(mode?: string | null): string {
  switch (mode) {
    case 'power':
      return 'Power mode';
    case 'efficiency':
      return 'Efficiency mode';
    case 'hashrate_target':
      return 'Hashrate target mode';
    case 'hashrate':
      return 'Hashrate mode';
    default:
      return 'Target mode';
  }
}

export function getPowerTargetingLabel(power: unknown): string | null {
  const data = power as PowerMetadata;
  const targeting = data?.targeting;
  if (!targeting?.active) {
    return null;
  }

  const targetWatts = targeting.target_watts ?? null;
  const sourceLabel = targeting.source === 'schedule'
    ? targeting.schedule_label ? `Schedule ${targeting.schedule_label}` : 'Scheduled power'
    : targeting.source === 'home'
      ? targeting.preset ? `Home preset ${targeting.preset}` : 'Home target'
      : targeting.preset
        ? `Preset ${targeting.preset}`
        : getTargetModeLabel(targeting.mode);

  const measuredWallPower = targeting.current_wall_watts_measured !== false;
  if (!targetWatts) {
    return `${sourceLabel} active`;
  }
  if (!measuredWallPower || targeting.delta_watts == null || !targeting.comparison) {
    return `${sourceLabel}: ${targetWatts.toLocaleString()} W target active`;
  }

  const delta = Math.abs(targeting.delta_watts);
  if (targeting.comparison === 'near') {
    return `${sourceLabel}: ${targetWatts.toLocaleString()} W target, within ${delta} W`;
  }

  return `${sourceLabel}: ${targetWatts.toLocaleString()} W target, ${delta} W ${targeting.comparison}`;
}
