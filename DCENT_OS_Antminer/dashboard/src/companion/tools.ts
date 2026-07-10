// Companion tools — the actions the chat companion can take on the miner.
//
// Each tool maps to an EXISTING dashboard API call (which hits the daemon REST
// API), so the companion controls the miner exactly the way the UI does — no
// new privileged surface. Hardware-affecting tools are flagged `hardware:true`
// and the chat UI requires an explicit user confirmation before running them
// (safety: the firmware's "cut hash before noise" + no-surprise-actions ethos).

import { api } from '../api/client';
import { useMinerStore } from '../store/miner';
import { formatHashrate } from '../utils/format';
import { getLiveWallWatts, getPowerTelemetryLabel } from '../utils/power';
import type { ToolSpec } from './llm';

export interface ToolDef {
  spec: ToolSpec;
  hardware: boolean;
  /** Human one-line summary of what running this will do (shown in the confirm prompt). */
  preview: (args: Record<string, unknown>) => string;
  run: (args: Record<string, unknown>) => Promise<string>;
}

function num(v: unknown, fallback: number): number {
  const n = typeof v === 'string' ? parseFloat(v) : (v as number);
  return Number.isFinite(n) ? n : fallback;
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

function stringField(record: unknown, key: string): string | null {
  if (!record || typeof record !== 'object' || !(key in record)) {
    return null;
  }
  const value = (record as Record<string, unknown>)[key];
  return typeof value === 'string' && value.length > 0 ? value : null;
}

export const TOOLS: ToolDef[] = [
  {
    spec: {
      name: 'get_miner_status',
      description: 'Read the current miner state: hashrate, chip temperature, fan speed, pool connection, accepted/rejected shares, and live wall power when available. Use this before answering questions about how the miner is doing; if power_w is null, live power is unavailable or only modeled.',
      parameters: { type: 'object', properties: {}, additionalProperties: false },
    },
    hardware: false,
    preview: () => 'Read the current miner status.',
    run: async () => {
      const s = useMinerStore.getState();
      const st = s.status;
      const chains = st?.chains ?? [];
      const temps = chains.map(c => c.temp_c).filter(t => t > 0);
      const maxTemp = temps.length ? Math.max(...temps) : null;
      const accepted = st?.accepted ?? 0;
      const rejected = st?.rejected ?? 0;
      const power = s.stats?.power ?? s.status?.power;
      const watts = getLiveWallWatts(power);
      const powerNote = getPowerTelemetryLabel(power) ?? 'Power telemetry unavailable';
      return JSON.stringify({
        mining: (st?.hashrate_ghs ?? 0) > 0,
        hashrate: formatHashrate(st?.hashrate_ghs ?? 0),
        max_chip_temp_c: maxTemp,
        pool: st?.pool?.status ?? 'unknown',
        accepted_shares: accepted,
        rejected_shares: rejected,
        power_w: watts > 0 ? Math.round(watts) : null,
        power_live: watts > 0,
        power_source: stringField(power, 'source'),
        power_source_detail: stringField(power, 'source_detail') ?? stringField(power, 'power_source_detail'),
        power_note: powerNote,
      });
    },
  },
  {
    spec: {
      name: 'lower_noise',
      description: 'Make the miner quieter by enabling quiet/night mode with a low fan cap. Use this for requests like "lower the noise", "make it quiet", "too loud", "night mode". Optionally pass a fan PWM cap (0-30, lower = quieter).',
      parameters: {
        type: 'object',
        properties: { max_fan_pwm: { type: 'number', description: 'Fan PWM cap 0-30 (default 15). Lower is quieter.' } },
        additionalProperties: false,
      },
    },
    hardware: true,
    preview: (a) => {
      const cap = clamp(Math.round(num(a.max_fan_pwm, 15)), 0, 30);
      return `Enable quiet/night mode with a fan cap of PWM ${cap}.`;
    },
    run: async (a) => {
      const cap = clamp(Math.round(num(a.max_fan_pwm, 15)), 0, 30);
      await api.setNightMode({ enabled: true, max_fan_pwm: cap });
      return `Quiet mode ON, fan capped at PWM ${cap}. The miner cuts hash before raising fan noise, so it should get noticeably quieter.`;
    },
  },
  {
    spec: {
      name: 'set_quiet_mode',
      description: 'Turn quiet/night mode on or off explicitly.',
      parameters: {
        type: 'object',
        properties: { enabled: { type: 'boolean', description: 'true = quiet on, false = quiet off' } },
        required: ['enabled'], additionalProperties: false,
      },
    },
    hardware: true,
    preview: (a) => `Turn quiet/night mode ${a.enabled ? 'ON' : 'OFF'}.`,
    run: async (a) => {
      const on = Boolean(a.enabled);
      await api.setNightMode({ enabled: on });
      return `Quiet/night mode is now ${on ? 'ON' : 'OFF'}.`;
    },
  },
  {
    spec: {
      name: 'set_room_temperature',
      description: 'Set the target ROOM temperature (Celsius) for space-heater mode. The miner modulates heat output toward this.',
      parameters: {
        type: 'object',
        properties: { temp_c: { type: 'number', description: 'Target room temperature in Celsius (15-30).' } },
        required: ['temp_c'], additionalProperties: false,
      },
    },
    hardware: true,
    preview: (a) => {
      const t = clamp(num(a.temp_c, 21), 15, 30);
      return `Set the target room temperature to ${t.toFixed(0)}°C.`;
    },
    run: async (a) => {
      const t = clamp(num(a.temp_c, 21), 15, 30);
      await api.setRoomTemp({ temp_c: t });
      return `Target room temperature set to ${t.toFixed(0)}°C.`;
    },
  },
  {
    spec: {
      name: 'set_heat_output',
      description: 'Set the heater power target in watts (controls how much heat/hash the miner produces).',
      parameters: {
        type: 'object',
        properties: { watts: { type: 'number', description: 'Target wall power in watts.' } },
        required: ['watts'], additionalProperties: false,
      },
    },
    hardware: true,
    preview: (a) => {
      const w = Math.max(0, Math.round(num(a.watts, 1000)));
      return `Set the heat output target to ${w} W.`;
    },
    run: async (a) => {
      const w = Math.max(0, Math.round(num(a.watts, 1000)));
      await api.setHeaterTarget({ watts: w });
      return `Heat output target set to ${w} W.`;
    },
  },
];

export const TOOL_SPECS: ToolSpec[] = TOOLS.map(t => t.spec);
export function findTool(name: string): ToolDef | undefined {
  return TOOLS.find(t => t.spec.name === name);
}

export const COMPANION_SYSTEM_PROMPT =
  "You are the DCENT_OS miner companion — a friendly, concise assistant living inside D-Central's " +
  "open-source mining firmware dashboard. You help the operator understand and control their Bitcoin " +
  "miner (a home space heater that mines Bitcoin). You can read the miner's status and take a few " +
  "actions via tools (make it quieter, set the room temperature, set heat output). " +
  "Rules: (1) Call get_miner_status before stating facts about the miner — never invent numbers. " +
  "(2) For any request that changes the hardware (noise, temperature, heat), call the matching tool; " +
  "the dashboard will ask the operator to confirm before it runs. (3) If get_miner_status returns " +
  "power_w:null, say live power telemetry is unavailable and do not estimate current watts from " +
  "modeled/source fields. (4) Keep replies short and clear. (5) Prioritize safety and quiet home " +
  "operation — the firmware cuts hash before raising fan noise. Never claim an action succeeded unless the tool " +
  "result says so.";
