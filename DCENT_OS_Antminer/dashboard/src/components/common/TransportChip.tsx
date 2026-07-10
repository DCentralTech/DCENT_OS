import React from 'react';
import { useTransportState } from '../../hooks/useTransportState';

function toneClass(tone: string): string {
  if (tone === 'success') return 'success';
  if (tone === 'warning') return 'warning';
  if (tone === 'danger') return 'danger';
  return 'neutral';
}

interface TransportChipProps {
  className?: string;
  dotClassName?: string;
  showDot?: boolean;
}

export function TransportChip({
  className = 'transport-chip',
  dotClassName = 'dot',
  showDot = true,
}: TransportChipProps) {
  const state = useTransportState();
  return (
    <span
      className={`${className} ${toneClass(state.tone)}`}
      title={state.title}
      data-transport={state.transport}
      role="status"
      aria-label={`Telemetry transport ${state.label}`}
    >
      {showDot && <span className={dotClassName} aria-hidden="true" />}
      {state.label}
    </span>
  );
}
