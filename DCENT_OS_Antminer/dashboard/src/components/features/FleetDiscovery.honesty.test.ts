import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';

const componentSource = readFileSync('src/components/features/FleetDiscovery.tsx', 'utf8');
const featureTypesSource = readFileSync('src/api/feature-types.ts', 'utf8');
const localeSources = [
  readFileSync('src/i18n/locales/en.ts', 'utf8'),
  readFileSync('src/i18n/locales/es.ts', 'utf8'),
  readFileSync('src/i18n/locales/fr.ts', 'utf8'),
  readFileSync('src/i18n/locales/zh.ts', 'utf8'),
].join('\n');

describe('FleetDiscovery honesty contract', () => {
  it('describes firmware fleet data as local snapshot plus manual probes', () => {
    expect(componentSource).toContain('read-only local state');
    expect(componentSource).toMatch(/does not\s+scan subnets or contact peer miners/);
    expect(componentSource).toContain('Use DCENT_Toolbox for broader');
    expect(componentSource).toContain('Snapshot scope');
    expect(componentSource).toContain('fleet-discovery-limitations');
    expect(componentSource).toContain('does not scan the LAN or proxy manual probes');
    expect(featureTypesSource).toContain('limitations?: string[];');
  });

  it('does not claim daemon-backed LAN scanning or competitor superiority', () => {
    const combined = `${componentSource}\n${localeSources}`;
    expect(combined).not.toMatch(/scans? your local network/i);
    expect(combined).not.toMatch(/scanning local network/i);
    expect(combined).not.toMatch(/discover DCENT_OS miners on your local network/i);
    expect(combined).not.toMatch(/network scan above/i);
    expect(combined).not.toMatch(/no competitor/i);
    expect(combined).not.toMatch(/feature no competitor/i);
    expect(combined).not.toMatch(/Descubra mineros DCENT_OS en su red local/i);
    expect(combined).not.toMatch(/D\\u00E9couvrez les mineurs DCENT_OS sur votre r\\u00E9seau local/i);
    expect(combined).not.toMatch(/在本地网络上发现 DCENT_OS 矿机/);
  });

  it('keeps locale copy on the local-snapshot contract', () => {
    expect(localeSources).toContain('Fleet Snapshot');
    expect(localeSources).toContain('LAN discovery is not linked yet');
    expect(localeSources).toContain('Actualizar Estado Local');
    expect(localeSources).toContain('d\\u00E9couverte LAN');
    expect(localeSources).toContain('LAN 发现尚未接入');
  });
  it('does not derive fleet power from hashrate', () => {
    expect(componentSource).not.toContain('hashrateThs * 80');
    expect(componentSource).not.toMatch(/80\s*W\/TH/i);
    expect(componentSource).not.toContain('Estimated at 80 W/TH');
    expect(featureTypesSource).toContain('powerWatts?: number | null;');
    expect(featureTypesSource).toContain('totalPowerWatts: number | null;');
    expect(componentSource).toContain('Power not reported by current sources; live wall-power provenance required');
    expect(componentSource).toContain('Reported by');
    expect(componentSource).toContain('live wall-power source');
    expect(localeSources).toContain('Reported Power');
  });

  it('accepts manual probe power only through the live wall-power provenance gate', () => {
    expect(componentSource).toContain("import { getLiveWallWatts } from '../../utils/power'");
    expect(componentSource).toContain('const powerWatts = probeLivePowerWatts(data)');
    expect(componentSource).toContain('function liveWallWattsFromRecord(record: Record<string, unknown>): number | null');
    expect(componentSource).not.toContain('const powerWatts = finitePositiveNumber(data.wall_watts)');
    expect(componentSource).not.toContain('?? finitePositiveNumber(data.power_watts)');
    expect(componentSource).not.toContain('?? finitePositiveNumber(data.powerWatts)');
  });
});
