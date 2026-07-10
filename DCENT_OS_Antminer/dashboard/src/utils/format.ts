// Formatting utilities for hashrate, uptime, temp, BTU, sats, difficulty

export function formatHashrate(ghs: number): string {
  // Decimals per the Terminology Lexicon hashrate ladder (TERM-3 §3.1):
  // PH/s & TH/s = .2, GH/s = .1 (canonical — matches formatHashrateShort and
  // axe's fHR; a single-GH/s resolution beyond one decimal is noise), MH/s = .0.
  if (ghs >= 1000000) return `${(ghs / 1000000).toFixed(2)} PH/s`;
  if (ghs >= 1000) return `${(ghs / 1000).toFixed(2)} TH/s`;
  if (ghs >= 1) return `${ghs.toFixed(1)} GH/s`;
  return `${(ghs * 1000).toFixed(0)} MH/s`;
}

export function formatHashrateShort(ghs: number): { value: string; unit: string } {
  if (ghs >= 1000000) return { value: (ghs / 1000000).toFixed(2), unit: 'PH/s' };
  if (ghs >= 1000) return { value: (ghs / 1000).toFixed(2), unit: 'TH/s' };
  if (ghs >= 1) return { value: ghs.toFixed(1), unit: 'GH/s' };
  return { value: (ghs * 1000).toFixed(0), unit: 'MH/s' };
}

export function formatUptime(seconds: number): string {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d}d ${h}h ${m}m`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

export function formatTemp(c: number): string {
  return `${c.toFixed(1)}°C`;
}

export function formatTempF(c: number): string {
  return `${((c * 9) / 5 + 32).toFixed(1)}°F`;
}

export function formatWatts(w: number): string {
  if (w >= 1000) return `${(w / 1000).toFixed(2)} kW`;
  return `${w} W`;
}

export function formatBtu(btu: number): string {
  return `${btu.toLocaleString()} BTU/h`;
}

export function formatSats(sats: number): string {
  if (sats >= 100000000) return `${(sats / 100000000).toFixed(8)} BTC`;
  if (sats >= 1000000) return `${(sats / 1000000).toFixed(2)}M sats`;
  if (sats >= 1000) return `${(sats / 1000).toFixed(1)}K sats`;
  return `${sats} sats`;
}

export function formatEfficiency(jth: number): string {
  // jth <= 0 / non-finite means there is no real hashrate to divide by yet —
  // render an honest placeholder, never a fabricated number. (Truth contract.)
  if (!Number.isFinite(jth) || jth <= 0) return '—';
  // A value this high is not a real operating efficiency — it means hashrate
  // has collapsed (a degraded / near-dead unit). ALARM with the honest bound
  // instead of blanking it to "—", which would silently hide a broken miner.
  // (Omega P3-3.)
  if (jth > 10000) return '>10,000 J/TH (degraded)';
  return `${jth.toFixed(1)} J/TH`;
}

export function formatNoise(db: number): string {
  if (db <= 35) return 'Whisper';
  if (db <= 45) return 'Quiet';
  if (db <= 55) return 'Moderate';
  if (db <= 65) return 'Loud';
  return 'Very Loud';
}

export function formatNoiseDb(db: number): string {
  return `${db} dB`;
}

export function formatPercent(v: number): string {
  return `${v.toFixed(1)}%`;
}

export function formatHex(v: number, digits = 8): string {
  return `0x${v.toString(16).padStart(digits, '0').toUpperCase()}`;
}

export function formatVoltage(mv: number): string {
  return `${(mv / 1000).toFixed(3)} V`;
}

export function formatFrequency(mhz: number): string {
  return `${mhz} MHz`;
}

// PIC DAC ↔ voltage conversion — S9 / BM1387 ONLY (P3-4).
//
// `pic_val = round(1608.420446 - 170.423497 * voltage_V)` is the PIC16F1704 DAC
// transfer function on the S9 (am1 / BM1387) control board. It is NOT valid on
// any other platform: S17 (dsPIC33EP), S19/S19j Pro Zynq (dsPIC), Amlogic
// S19j Pro / S21 (NoPic — TAS5782M DACs / register-based PSU). This util ships
// in the single fleet-wide bundle, so blindly applying it to a non-S9 board
// fabricates a voltage that looks real but is wrong.
//
// Truth-contract: when the caller knows the board and it is NOT an S9/BM1387
// target, return `null` ("n/a") instead of a wrong number. Callers that omit
// `board` are the S9-only tools (e.g. the BM1387 PIC Voltage Programmer) and
// keep the S9 math — the single-arg overload preserves the `number` return.

/**
 * True when `board` is an S9 / BM1387 / PIC16F1704 target the DAC formula
 * applies to. Matches the chip id `bm1387`, model `s9`/`s9i`, board target
 * `am1-s9`, and the platform fingerprint `zynq-am1-bm1387`. Excludes the
 * dsPIC/NoPic platforms (s17 / s19 / s21 / amlogic / bm139x / bm136x). Returns
 * false for null/empty so an unknown board never gets the S9 conversion.
 */
export function isS9PicDacBoard(board: string | null | undefined): boolean {
  if (!board) return false;
  const b = board.toLowerCase();
  return b.includes('bm1387') || b.includes('s9');
}

export function picToVoltage(picVal: number): number;
export function picToVoltage(picVal: number, board: string | null | undefined): number | null;
export function picToVoltage(picVal: number, board?: string | null): number | null {
  // Only S9/BM1387 boards use the PIC16F1704 DAC. A named non-S9 board (or an
  // explicitly-unknown null board) gets an honest null, never a wrong S9 value.
  if (board !== undefined && !isS9PicDacBoard(board)) return null;
  return (1608.420446 - picVal) / 170.423497;
}

export function voltageToPic(voltage: number): number;
export function voltageToPic(voltage: number, board: string | null | undefined): number | null;
export function voltageToPic(voltage: number, board?: string | null): number | null {
  if (board !== undefined && !isS9PicDacBoard(board)) return null;
  return Math.round(1608.420446 - voltage * 170.423497);
}

// Unix ms for 2020-01-01T00:00:00Z. A timestamp earlier than this almost always
// means the miner had no RTC and SNTP had not yet stepped the clock when the
// value was recorded — an Antminer control board has no battery-backed RTC, so
// it boots at the 1970 epoch. Such a value must NEVER be rendered as a real
// 1970 wall-clock date (truth contract: don't dress an unsynced clock up as a
// measured time). Guard every `new Date(ts)` and client-vs-daemon epoch
// comparison with `isRtcSyncedMs()` first. (P2-1 / D-18.)
export const RTC_SANE_EPOCH_MS = 1_577_836_800_000;

/**
 * True when `ms` is a plausible post-2020 epoch-millis wall-clock value (the
 * RTC was synced when it was recorded). Returns false for null/undefined/NaN
 * and for any pre-2020 value, so callers can render "—"/"since boot" instead of
 * a misleading 1970 date.
 */
export function isRtcSyncedMs(ms: number | null | undefined): boolean {
  return typeof ms === 'number' && Number.isFinite(ms) && ms >= RTC_SANE_EPOCH_MS;
}

// ── Unsupported-metric honesty (Omega P3-8) ───────────────────────────────
// The daemon emits certain AxeOS/pyasic-compatibility fields as a literal 0
// purely so the ecosystem clients don't choke — they are NOT real telemetry.
// The REST `/api/system/info` lists them in `unsupported_metrics`; the
// CGMiner shim (port 4028) lists them per-section in `_DCENTUnsupported`. The
// UI must never present those 0/0.0 values as measured data: render "n/a", or
// at minimum surface which fields are placeholders. These helpers read the
// flag so consumers (ApiExplorer / SystemDebug / HardwareInfo) can honor it.

/** Normalize an unsupported-metric flag (REST or CGMiner) to a clean list. */
export function unsupportedMetricList(
  unsupported?: readonly string[] | null,
): string[] {
  return Array.isArray(unsupported)
    ? unsupported.filter((k): k is string => typeof k === 'string' && k.length > 0)
    : [];
}

/** True when `key` is flagged as an unsupported/compatibility-only metric. */
export function isUnsupportedMetric(
  key: string,
  unsupported?: readonly string[] | null,
): boolean {
  return unsupportedMetricList(unsupported).includes(key);
}

// P3-35(c): one consistent, human-facing chain number across panels.
//
// The daemon exposes two different chain identifiers: the status/stats
// `ChainState.id` is the hardware/FPGA chain index (e.g. 6/7/8 on an S9), while
// `/api/mining/chain/presence` reports a 0-based array `idx` (0/1/2). Showing
// both raw schemes side by side implies "chain 6" and "chain 0" are different
// boards. User-facing panels therefore display a single 1-based ordinal derived
// from array position; the real hardware id is kept in the element title for
// traceability. (The autotuner panel intentionally keeps the raw hardware
// `chain_id` — there it is the API-addressable identifier, not cosmetic.)
export function chainOrdinal(position: number): number {
  const p = Number.isFinite(position) ? Math.trunc(position) : 0;
  return Math.max(0, p) + 1;
}

export function chainLabel(position: number): string {
  return `Chain ${chainOrdinal(position)}`;
}

export function chainShortLabel(position: number): string {
  return `CH${chainOrdinal(position)}`;
}
