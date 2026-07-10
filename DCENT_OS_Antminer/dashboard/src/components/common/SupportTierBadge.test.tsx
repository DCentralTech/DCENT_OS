// @vitest-environment jsdom

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';

import { SUPPORT_TIER_VALUES, type SupportTier } from '../../api/generated/capability';
import {
  ExperimentalWarningBanner,
  isProvenSupportTier,
  SupportTierBadge,
  supportTierLabel,
} from './SupportTierBadge';

afterEach(() => {
  cleanup();
});

describe('SupportTierBadge', () => {
  it.each([
    ['stable', 'Stable'],
    ['beta', 'Beta'],
    ['experimental', 'Experimental'],
    ['unsupported', 'Unsupported'],
    ['unknown', 'Unknown'],
  ] satisfies Array<[SupportTier, string]>)(
    'renders the generated %s tier label',
    (tier, label) => {
      render(<SupportTierBadge tier={tier} />);

      const badge = screen.getByTestId('support-tier-badge');
      expect(badge.textContent).toBe(label);
      expect(badge.getAttribute('aria-label')).toBe(`${label} support tier`);
      expect(badge.className).toContain(`cp-support-tier-badge--${tier}`);
    },
  );

  it('keeps helper labels exhaustive against generated tier values', () => {
    for (const tier of SUPPORT_TIER_VALUES) {
      expect(supportTierLabel(tier)).toMatch(/\S/);
    }
  });

  it('treats only stable and beta as proven tiers', () => {
    expect(isProvenSupportTier('stable')).toBe(true);
    expect(isProvenSupportTier('beta')).toBe(true);
    expect(isProvenSupportTier('experimental')).toBe(false);
    expect(isProvenSupportTier('unsupported')).toBe(false);
    expect(isProvenSupportTier('unknown')).toBe(false);
  });
});

describe('ExperimentalWarningBanner', () => {
  it('does not render for stable or beta tiers', () => {
    const { rerender } = render(<ExperimentalWarningBanner tier="stable" />);
    expect(screen.queryByTestId('experimental-warning-banner')).toBeNull();

    rerender(<ExperimentalWarningBanner tier="beta" />);
    expect(screen.queryByTestId('experimental-warning-banner')).toBeNull();
  });

  it('renders a status banner for experimental tiers', () => {
    render(<ExperimentalWarningBanner tier="experimental" />);

    const banner = screen.getByTestId('experimental-warning-banner');
    expect(banner.getAttribute('role')).toBe('status');
    expect(banner.textContent).toContain('Experimental profile');
    expect(banner.className).toContain('cp-support-tier-warning--experimental');
  });

  it('renders fail-closed alert banners for unsupported or unknown tiers', () => {
    const { rerender } = render(<ExperimentalWarningBanner tier="unsupported" />);
    expect(screen.getByTestId('experimental-warning-banner').getAttribute('role')).toBe('alert');

    rerender(<ExperimentalWarningBanner tier="unknown" />);
    expect(screen.getByTestId('experimental-warning-banner').getAttribute('role')).toBe('alert');
  });

  it('does not carry the legacy Bitcoin-orange inline warning tint', () => {
    const source = readFileSync('src/components/common/SupportTierBadge.tsx', 'utf8');
    expect(source).not.toContain('#F7931A');
    expect(source).not.toContain('247,147,26');
    expect(source).not.toContain('style=');
  });
});
