import type { DeviceCapabilityDescriptor } from '../../api/generated/capability';
import { InfoBanner } from './InfoBanner';
import { SupportTierBadge, supportTierLabel } from './SupportTierBadge';

interface HardwareDetectionStateProps {
  descriptor: DeviceCapabilityDescriptor | null;
  loading?: boolean;
  error?: string | null;
  testId?: string;
}

function isFailClosed(descriptor: DeviceCapabilityDescriptor): boolean {
  return (
    descriptor.support === 'unknown' ||
    descriptor.support === 'unsupported' ||
    descriptor.identity.confidence === 'unknown' ||
    descriptor.failSafe.readOnly ||
    descriptor.failSafe.mutatingRoutesAllowed === false
  );
}

function titleForDescriptor(descriptor: DeviceCapabilityDescriptor): string {
  if (descriptor.support === 'unknown') return 'Unknown hardware';
  if (descriptor.support === 'unsupported') return 'Unsupported hardware';
  if (descriptor.family !== 'antminer') return `${supportTierLabel(descriptor.support)} ${descriptor.family} hardware`;
  return 'Read-only hardware state';
}

function bodyForDescriptor(descriptor: DeviceCapabilityDescriptor): string {
  const support = supportTierLabel(descriptor.support);
  if (descriptor.support === 'unknown' || descriptor.identity.confidence === 'unknown') {
    return 'Hardware tools are hidden or read-only until the daemon reports a known capability descriptor.';
  }
  if (descriptor.support === 'unsupported') {
    return 'This SKU is not customer-installable. Mutating hardware tools stay disabled by the capability descriptor.';
  }
  if (descriptor.family !== 'antminer') {
    return `${support} support is surfaced from the shared descriptor. This Antminer dashboard keeps family-specific hardware tools disabled.`;
  }
  return descriptor.failSafe.reason || 'Mutating hardware tools are disabled by the capability descriptor.';
}

export function HardwareDetectionState({
  descriptor,
  loading = false,
  error = null,
  testId = 'hardware-detection-state',
}: HardwareDetectionStateProps) {
  if (loading) {
    return (
      <InfoBanner
        className="cp-hardware-detection-state"
        dense
        tone="neutral"
        title="Detecting hardware"
      >
        <span data-testid={testId}>Loading the shared capability descriptor.</span>
      </InfoBanner>
    );
  }

  if (error && !descriptor) {
    return (
      <InfoBanner
        className="cp-hardware-detection-state"
        dense
        tone="warn"
        title="Capability descriptor unavailable"
      >
        <span data-testid={testId}>
          Using legacy platform detection where available. Hardware tools still
          fail closed on unknown platforms.
        </span>
      </InfoBanner>
    );
  }

  if (!descriptor || !isFailClosed(descriptor)) {
    return null;
  }

  const alertTone =
    descriptor.support === 'unknown' || descriptor.support === 'unsupported'
      ? 'danger'
      : 'warn';

  return (
    <InfoBanner
      className="cp-hardware-detection-state"
      dense
      tone={alertTone}
      title={titleForDescriptor(descriptor)}
    >
      <span data-testid={testId}>
        <SupportTierBadge tier={descriptor.support} testId={`${testId}-tier`} />{' '}
        {bodyForDescriptor(descriptor)}
      </span>
    </InfoBanner>
  );
}
