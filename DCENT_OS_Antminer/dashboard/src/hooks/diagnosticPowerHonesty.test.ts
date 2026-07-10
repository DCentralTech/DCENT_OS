import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const protocolTrace = readFileSync('src/hooks/useProtocolTrace.tsx', 'utf8');
const flightRecorder = readFileSync('src/hooks/useFlightRecorder.tsx', 'utf8');
const protocolTimeline = readFileSync('src/components/advanced/ProtocolTimeline.tsx', 'utf8');
const pipelineScope = readFileSync('src/components/advanced/PipelineScope.tsx', 'utf8');

describe('diagnostic live power honesty', () => {
  it('keeps protocol trace and flight recorder power live-only', () => {
    expect(protocolTrace).toContain("import { getLiveWallWatts } from '../utils/power'");
    expect(protocolTrace).toContain('wallWatts: number | null');
    expect(protocolTrace).toContain('wallWatts: null');
    expect(protocolTrace).toContain('const liveWallWatts = getLiveWallWatts(power)');
    expect(protocolTrace).toContain('wallWatts: liveWallWatts > 0 ? liveWallWatts : null');
    expect(protocolTrace).not.toContain('status?.power?.wall_watts ?? status?.power?.watts ?? getWallWatts');

    expect(flightRecorder).toContain("import { getLiveWallWatts, getPowerTelemetryLabel } from '../utils/power'");
    expect(flightRecorder).toContain("import { wattsToBtu } from '../utils/thermal'");
    expect(flightRecorder).toContain('type LivePowerTelemetry = Parameters<typeof getLiveWallWatts>[0]');
    expect(flightRecorder).toContain('function livePowerRecorderDetail(power: LivePowerTelemetry)');
    expect(flightRecorder).toContain('wallWatts: wallWatts > 0 ? wallWatts : null');
    expect(flightRecorder).toContain('wallPowerLive: wallWatts > 0');
    expect(flightRecorder).toContain('wallPowerNote: getPowerTelemetryLabel(power)');
    expect(flightRecorder).toContain('function heaterPowerRecorderDetail(message: HeaterWsMessage)');
    expect(flightRecorder).toContain('reportedBtuH: message.btu_h');
    expect(flightRecorder).toContain('btuH: liveBtuH');
    expect(flightRecorder).toContain('btuLive: liveBtuH !== null');
    expect(flightRecorder).not.toContain('wallWatts: message.wall_watts ?? null');
    expect(flightRecorder).not.toContain('btuH: message.btu_h');
    expect(flightRecorder).not.toContain('state.status?.power?.wall_watts ?? getWallWatts');
  });

  it('renders unavailable diagnostic wall power instead of formatting null or fallback watts', () => {
    expect(protocolTimeline).toContain('function formatLiveWallPower(watts: number | null)');
    expect(protocolTimeline).toContain("watts != null ? `${watts.toFixed(0)} W` : 'Unavailable'");
    expect(protocolTimeline).not.toContain('snapshot.wallWatts.toFixed(0)');

    expect(pipelineScope).toContain('function formatLiveWallPower(watts: number | null)');
    expect(pipelineScope).toContain("watts != null ? `${watts.toFixed(0)} W` : 'Unavailable'");
    expect(pipelineScope).not.toContain('snapshot.wallWatts.toFixed(0)');
  });
});
