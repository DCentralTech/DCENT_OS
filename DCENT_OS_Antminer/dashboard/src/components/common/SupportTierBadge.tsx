import type { ReactNode } from 'react';

import type { SupportTier } from '../../api/generated/capability';

export function supportTierLabel(tier: SupportTier): string {
  switch (tier) {
    case 'stable':
      return 'Stable';
    case 'beta':
      return 'Beta';
    case 'experimental':
      return 'Experimental';
    case 'unsupported':
      return 'Unsupported';
    case 'unknown':
      return 'Unknown';
  }
}

export function isProvenSupportTier(tier: SupportTier): boolean {
  return tier === 'stable' || tier === 'beta';
}

function supportTierTitle(tier: SupportTier): string {
  switch (tier) {
    case 'stable':
      return 'Stable support tier';
    case 'beta':
      return 'Beta support tier';
    case 'experimental':
      return 'Experimental support tier; not part of the public-beta install set';
    case 'unsupported':
      return 'Unsupported support tier; not customer-installable';
    case 'unknown':
      return 'Unknown support tier; runtime capabilities must fail closed';
  }
}

interface SupportTierBadgeProps {
  tier: SupportTier;
  testId?: string;
  title?: string;
}

export function SupportTierBadge({
  tier,
  testId = 'support-tier-badge',
  title,
}: SupportTierBadgeProps) {
  const label = supportTierLabel(tier);
  return (
    <span
      data-testid={testId}
      title={title ?? supportTierTitle(tier)}
      aria-label={`${label} support tier`}
      className={`cp-support-tier-badge cp-support-tier-badge--${tier}`}
    >
      {label}
    </span>
  );
}

interface ExperimentalWarningBannerProps {
  tier: SupportTier;
  children?: ReactNode;
  testId?: string;
}

export function ExperimentalWarningBanner({
  tier,
  children,
  testId = 'experimental-warning-banner',
}: ExperimentalWarningBannerProps) {
  if (isProvenSupportTier(tier)) {
    return null;
  }

  const label = supportTierLabel(tier);
  return (
    <div
      data-testid={testId}
      role={tier === 'unsupported' || tier === 'unknown' ? 'alert' : 'status'}
      className={`cp-support-tier-warning cp-support-tier-warning--${tier}`}
    >
      {children ?? (
        <>
          {label} profile. This support tier is outside the public-beta install
          set until its promotion gate is completed.
        </>
      )}
    </div>
  );
}
