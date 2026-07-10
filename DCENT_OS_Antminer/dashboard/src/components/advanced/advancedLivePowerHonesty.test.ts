import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const statusStrip = readFileSync('src/components/advanced/StatusStrip.tsx', 'utf8');
const advancedDashboard = readFileSync('src/components/advanced/AdvancedDashboard.tsx', 'utf8');
const beatLab = readFileSync('src/components/advanced/BeatLab.tsx', 'utf8');
const circuitWarning = readFileSync('src/components/heater/CircuitWarning.tsx', 'utf8');
const kitDashboardPage = readFileSync('src/components/standard/KitDashboardPage.tsx', 'utf8');
const tempFansPage = readFileSync('src/components/standard/TempFansPage.tsx', 'utf8');

describe('advanced live power display honesty', () => {
  it('does not show status-strip or BeatLab fallback watts as live wall draw', () => {
    expect(statusStrip).toContain("import { getLiveWallWatts } from '../../utils/power'");
    expect(statusStrip).toContain('const watts = getLiveWallWatts(stats?.power)');
    expect(statusStrip).toContain('W live wall power');
    expect(statusStrip).not.toContain('getWallWatts(stats?.power)');

    expect(beatLab).toContain("import { getLiveWallWatts } from '../../utils/power'");
    expect(beatLab).toContain('wallWatts: number | null');
    expect(beatLab).toContain('const liveWallWatts = getLiveWallWatts(stats?.power ?? status?.power)');
    expect(beatLab).toContain('const wallWatts = liveWallWatts > 0 ? liveWallWatts : null');
    expect(beatLab).toContain('function formatLiveWallPower(watts: number | null)');
    expect(beatLab).toContain("formatLiveWallPower(metrics.wallWatts)");
    expect(beatLab).not.toContain('function getWallWatts(status: StatusResponse | null, stats: StatsResponse | null)');
    expect(beatLab).not.toContain('metrics.wallWatts.toFixed(0)');
  });

  it('keeps Hacker topbar BTU and Standard circuit warnings live-power-only', () => {
    expect(advancedDashboard).toContain("import { getLiveWallWatts } from '../../utils/power'");
    expect(advancedDashboard).toContain('const liveTopbarWallWatts = getLiveWallWatts(status?.power)');
    expect(advancedDashboard).toContain('wallWatts={liveTopbarWallWatts > 0 ? liveTopbarWallWatts : null}');
    expect(advancedDashboard).not.toContain('BtuTopbarPill wallWatts={status?.power?.wall_watts}');

    expect(kitDashboardPage).toContain("import { getLiveWallWatts } from '../../utils/power'");
    expect(kitDashboardPage).toContain('const liveCircuitWatts = getLiveWallWatts(status?.power)');
    expect(kitDashboardPage).toContain('currentWatts={liveCircuitWatts > 0 ? liveCircuitWatts : null}');
    expect(kitDashboardPage).not.toContain('currentWatts={status?.power?.wall_watts ?? null}');
    expect(circuitWarning).toContain('Live wall power telemetry (watts).');
  });

  it('keeps Temp/Fans PSU wall power and heat live-only', () => {
    expect(tempFansPage).toContain("import { getLiveWallWatts } from '../../utils/power'");
    expect(tempFansPage).toContain('const psuWallWatts = getLiveWallWatts(power)');
    expect(tempFansPage).toContain("['Live wall power', psuWallWatts > 0 ? `${psuWallWatts.toFixed(0)} W`");
    expect(tempFansPage).toContain('const psuBtuH = psuWallWatts > 0 ? wattsToBtu(psuWallWatts) : 0');
    expect(tempFansPage).not.toContain("import { getWallWatts } from '../../utils/power'");
    expect(tempFansPage).not.toContain("'btu_h' in power");
  });
});
