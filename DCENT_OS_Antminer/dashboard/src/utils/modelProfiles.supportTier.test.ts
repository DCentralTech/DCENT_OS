import { describe, expect, it } from 'vitest';
import { SUPPORT_TIER_VALUES, type SupportTier } from '../api/generated/capability';
import {
  MODEL_PROFILES,
  getModelProfile,
  isModelProfileProven,
} from './modelProfiles';

//  honest-claims contract, upgraded to the shared support-tier
// vocabulary. The dashboard renders full operating profiles for registered
// models, including models outside the public-beta install set. `supportTier`
// is what lets the UI surface an honest development note so a rendered profile
// is never read as a support/tuning guarantee.

const EXPECTED_SUPPORT_TIER: Record<string, SupportTier> = {
  s9: 'beta',
  s19jproam2: 'beta',
  s17: 'experimental',
  t17: 'experimental',
  s19pro: 'experimental',
  t19: 'experimental',
  s19xp: 'experimental',
  s21: 'experimental',
  s19k: 'experimental',
};

describe('modelProfiles.supportTier (shared honesty tier)', () => {
  it('sets supportTier exactly per the public support roster', () => {
    for (const [key, expected] of Object.entries(EXPECTED_SUPPORT_TIER)) {
      const profile = MODEL_PROFILES[key];
      expect(profile, `profile "${key}" should exist`).toBeDefined();
      expect(profile.supportTier, `supportTier for "${key}"`).toBe(expected);
    }
  });

  it('marks ONLY S9 and S19j Pro am2 as beta across the fallback table', () => {
    const betaKeys = Object.entries(MODEL_PROFILES)
      .filter(([, p]) => p.supportTier === 'beta')
      .map(([k]) => k)
      .sort();
    expect(betaKeys).toEqual(['s19jproam2', 's9']);
  });

  it('uses only generated shared support-tier vocabulary values', () => {
    const allowed = new Set<SupportTier>(SUPPORT_TIER_VALUES);
    for (const [key, profile] of Object.entries(MODEL_PROFILES)) {
      expect(allowed.has(profile.supportTier), `supportTier for "${key}"`).toBe(true);
      expect('validated' in profile, `legacy validated flag for "${key}"`).toBe(false);
    }
  });

  it('resolves supportTier through getModelProfile() alias lookups too', () => {
    expect(getModelProfile('Antminer S9')?.supportTier).toBe('beta');
    expect(getModelProfile('s19jpro')?.supportTier).toBe('beta');
    expect(getModelProfile('Antminer S17')?.supportTier).toBe('experimental');
    expect(getModelProfile('Antminer T17')?.supportTier).toBe('experimental');
    expect(getModelProfile('Antminer T19')?.supportTier).toBe('experimental');
    expect(getModelProfile('Antminer S19 XP')?.supportTier).toBe('experimental');
    expect(getModelProfile('S21')?.supportTier).toBe('experimental');
  });

  it('keeps the proven helper pinned to stable/beta only', () => {
    expect(isModelProfileProven(MODEL_PROFILES.s9)).toBe(true);
    expect(isModelProfileProven(MODEL_PROFILES.s19jproam2)).toBe(true);
    expect(isModelProfileProven(MODEL_PROFILES.s17)).toBe(false);
    expect(isModelProfileProven(MODEL_PROFILES.s21)).toBe(false);
  });

  it('registers expanded Antminer matrix models instead of falling back to unknown', () => {
    expect(getModelProfile('Antminer S17')?.platformKey).toBe('am2-s17p');
    expect(getModelProfile('Antminer T17')?.displayName).toBe('Antminer T17');
    expect(getModelProfile('Antminer T19')?.displayName).toBe('Antminer T19');
    expect(getModelProfile('Antminer S19 XP')?.displayName).toBe('Antminer S19 XP');
  });
});
