import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const basicDashboard = readFileSync('src/components/basic/BasicDashboard.tsx', 'utf8');
const bigReadouts = readFileSync('src/components/basic/HeaterBigReadouts.tsx', 'utf8');
const earningCard = readFileSync('src/components/basic/HeaterEarningCard.tsx', 'utf8');
const enginePanel = readFileSync('src/components/basic/HeaterEnginePanel.tsx', 'utf8');
const heaterStatus = readFileSync('src/components/basic/HeaterStatus.tsx', 'utf8');
const heatingValue = readFileSync('src/components/basic/HeatingValueSummary.tsx', 'utf8');
const historyView = readFileSync('src/components/basic/HistoryView.tsx', 'utf8');
const powerUtils = readFileSync('src/utils/power.ts', 'utf8');
const thermostat = readFileSync('src/components/basic/Thermostat.tsx', 'utf8');
const apiTypes = readFileSync('src/api/types.ts', 'utf8');

describe('basic heater live power honesty', () => {
  it('keeps glance wall power and cost labels live-only', () => {
    expect(powerUtils).toContain('export function getLiveDisplayWallWatts');
    expect(bigReadouts).toContain('getDisplayPowerWatts, getLiveDisplayWallWatts, getPowerTelemetryLabel');
    expect(bigReadouts).toContain('const displayPower = getDisplayPowerWatts(heater, stats?.power)');
    expect(bigReadouts).toContain('const liveWallPower = getLiveDisplayWallWatts(heater, stats?.power)');
    expect(bigReadouts).toContain('Live wall power');
    expect(bigReadouts).toContain('liveWallPower > 0 ? Math.round(liveWallPower).toLocaleString()');
    expect(bigReadouts).toContain("powerTelemetryLabel ?? 'Waiting for live wall telemetry'");
    expect(bigReadouts).not.toContain('No draw at the wall');
    expect(bigReadouts).not.toContain('power > 0 ? Math.round(power).toLocaleString()');
  });

  it('does not compute heat-credit electricity cost from fallback watts', () => {
    expect(heatingValue).toContain("import { getLiveDisplayWallWatts } from '../../utils/power'");
    expect(heatingValue).toContain('const powerWatts = getLiveDisplayWallWatts(heater, stats?.power)');
    expect(heatingValue).toContain('const electricityCost = powerWatts > 0');
    expect(heatingValue).toContain('Live wall-power unavailable; net excludes electricity cost.');
    expect(heatingValue).not.toContain('getWallWatts(stats?.power)');
    expect(heatingValue).not.toContain('heater?.wall_watts && heater.wall_watts > 0');
  });

  it('does not compute heater earning electricity cost from fallback watts', () => {
    expect(earningCard).toContain("import { getDisplayPowerWatts, getLiveDisplayWallWatts } from '../../utils/power'");
    expect(earningCard).toContain('const displayPower = getDisplayPowerWatts(heater, statsPower)');
    expect(earningCard).toContain('const liveWallPower = getLiveDisplayWallWatts(heater, statsPower)');
    expect(earningCard).toContain('const dailyCost = liveWallPower > 0');
    expect(earningCard).toContain('Heating electricity (live power unavailable)');
    expect(earningCard).toContain('cost pending live wall power');
    expect(earningCard).not.toContain('const dailyCost = power > 0');
  });

  it('does not compute heater history cost/net cards from fallback watts', () => {
    expect(heaterStatus).toContain('getDisplayPowerWatts, getLiveDisplayWallWatts, getPowerTargetingLabel');
    expect(heaterStatus).toContain('const displayPower = getDisplayPowerWatts(heater, statsPower)');
    expect(heaterStatus).toContain('const liveWallPower = getLiveDisplayWallWatts(heater, statsPower)');
    expect(heaterStatus).toContain('const dailyCost = liveWallPower > 0');
    expect(heaterStatus).toContain('const netCost = dailyCost != null');
    expect(heaterStatus).toContain('Live wall power unavailable for cost');
    expect(heaterStatus).toContain('Live wall power unavailable for net cost');
    expect(heaterStatus).not.toContain('const dailyCost = power > 0');
  });

  it('keeps session activity right-now power live-only', () => {
    expect(historyView).toContain("import { getLiveHistoryPointWallWatts, getLiveWallWatts } from '../../utils/power'");
    expect(historyView).toContain('const watts = getLiveWallWatts(stats?.power)');
    expect(historyView).toContain('Live wall power being drawn and turned into useful room heat right now.');
    expect(historyView).not.toContain('const watts = getWallWatts(stats?.power)');
  });

  it('keeps persisted heat-history value live-power-only', () => {
    expect(apiTypes).toContain('power_source?: string;');
    expect(apiTypes).toContain('power_source_detail?: string;');
    expect(apiTypes).toContain('live_power_available?: boolean;');
    expect(powerUtils).toContain('export function getLiveHistoryPointWallWatts');
    expect(historyView).toContain('const livePowerWatts = points.map(getLiveHistoryPointWallWatts).filter(watts => watts > 0);');
    expect(historyView).toContain('const livePowerHours = (livePowerWatts.length * intervalS) / 3600;');
    expect(historyView).toContain('const heatingValue = avgPower > 0');
    expect(historyView).not.toContain('points.reduce((s, p) => s + p.power_watts, 0) / points.length');
  });

  it('labels remaining Basic display-power surfaces as estimates unless live', () => {
    expect(basicDashboard).toContain('getDisplayPowerWatts, getLiveDisplayWallWatts, getPowerTargetingLabel');
    expect(basicDashboard).toContain('const liveDisplayPowerWatts = getLiveDisplayWallWatts(heaterStatus, stats?.power)');
    expect(basicDashboard).toContain('const stateLinePower = liveDisplayPowerWatts > 0');
    expect(basicDashboard).toContain('W est.');

    expect(enginePanel).toContain("import { getDisplayPowerWatts, getLiveDisplayWallWatts } from '../../utils/power'");
    expect(enginePanel).toContain('const drawPower = liveWallPower > 0 ? liveWallPower : displayPower');
    expect(enginePanel).toContain("liveWallPower > 0 ? 'kW' : 'kW est.'");
    expect(enginePanel).toContain('liveWallPower > 0 && maxWatts > 0');

    expect(thermostat).toContain("import { getDisplayPowerWatts, getLiveDisplayWallWatts } from '../../utils/power'");
    expect(thermostat).toContain('const powerText = liveWallPower > 0');
    expect(thermostat).toContain('W est.');
  });

  it('labels Basic BTU output as estimated unless it is backed by live wall power', () => {
    expect(bigReadouts).toContain('const btuIsLive = liveWallPower > 0');
    expect(bigReadouts).toContain("btuIsLive ? ' BTU/h' : ' BTU/h est.'");
    expect(bigReadouts).toContain("btuIsLive ? 'Heat output' : 'Heat output estimate'");

    expect(heaterStatus).toContain('const btuIsLive = liveWallPower > 0');
    expect(heaterStatus).toContain("btuIsLive ? 'HEAT OUTPUT' : 'HEAT OUTPUT EST.'");
    expect(heaterStatus).toContain("btuIsLive ? 'BTU per hour' : 'estimated BTU per hour'");
    expect(heaterStatus).toContain("btuIsLive ? 'BTU/h' : 'BTU/h est.'");

    expect(earningCard).toContain("const btuUnit = btuIsLive ? 'BTU/h' : 'BTU/h est.'");
    expect(enginePanel).toContain("btuIsLive ? 'BTU/h' : 'BTU/h est.'");
  });
});
