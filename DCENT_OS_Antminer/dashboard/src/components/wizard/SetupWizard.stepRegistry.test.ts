import { describe, expect, it } from 'vitest';

import {
  setupFamilyFromBoardTarget,
  stepRegistryForDeviceFamily,
} from './SetupWizard';

function ids(family: Parameters<typeof stepRegistryForDeviceFamily>[0]): string[] {
  return stepRegistryForDeviceFamily(family).map(step => step.id);
}

describe('SetupWizard family step registry', () => {
  it('classifies board targets into shared descriptor families', () => {
    expect(setupFamilyFromBoardTarget('am2-s19jpro-zynq')).toBe('antminer');
    expect(setupFamilyFromBoardTarget('amlogic-s21')).toBe('antminer');
    expect(setupFamilyFromBoardTarget('bitaxe-gamma')).toBe('esp');
    expect(setupFamilyFromBoardTarget('dcent-axe-hex-bm1397')).toBe('esp');
    expect(setupFamilyFromBoardTarget('whatsminer-m60s')).toBe('whatsminer');
    expect(setupFamilyFromBoardTarget('avalon-q-k230')).toBe('avalon');
    expect(setupFamilyFromBoardTarget('innosilicon-t2tz')).toBe('innosilicon');
    expect(setupFamilyFromBoardTarget('')).toBe('unknown');
  });

  it('keeps Antminer-specific PSU and circuit steps out of non-Antminer registries', () => {
    expect(ids('antminer')).toEqual(expect.arrayContaining(['circuit', 'power', 'psu_override']));

    for (const family of ['esp', 'whatsminer', 'avalon', 'innosilicon', 'unknown'] as const) {
      expect(ids(family)).not.toContain('circuit');
      expect(ids(family)).not.toContain('psu_override');
    }
  });

  it('keeps scaffold families to existing non-destructive wizard components', () => {
    expect(ids('whatsminer')).toEqual(['welcome', 'pool', 'name', 'review']);
    expect(ids('avalon')).toEqual(['welcome', 'pool', 'name', 'review']);
    expect(ids('innosilicon')).toEqual(['welcome', 'pool', 'name', 'review']);
  });
});
