//  — Am3BbInfoCard.
//
// Renders a status card for the AM335x BeagleBone Black platform (am3-bb)
// showing the current port progress. The actual mining UI is unchanged
// across platforms; this card just calls out that the BB platform port is
// in progress and links to the relevant  doc.
//
// Visible only when `platform_key` collapses to "am3-bb"; gated by
// `platformCapabilities(tier).bbSdCardRecovery` from
// utils/platformCapabilities.ts.

import React from 'react';
import { PlatformTier } from '../../utils/platformCapabilities';

export interface Am3BbInfoCardProps {
  /** Resolved platform tier from system info. */
  tier: PlatformTier;
  /** Whether `uart_trans` userspace daemon is running. */
  uartTransRunning?: boolean;
  /** Number of populated chains discovered (0..=4 on BB). */
  chainsDiscovered?: number;
}

export function Am3BbInfoCard({
  tier,
  uartTransRunning,
  chainsDiscovered,
}: Am3BbInfoCardProps) {
  if (tier !== 'am3-bb') {
    return null;
  }

  const uartStatus = uartTransRunning === undefined
    ? 'unknown'
    : (uartTransRunning ? 'running' : 'stopped');

  return (
    <div
      style={{
        background: 'var(--surface-1, #1b1b1f)',
        border: '1px solid var(--border-1, #2a2a30)',
        borderRadius: 8,
        padding: '1rem 1.25rem',
        margin: '0.75rem 0',
      }}
    >
      <div style={{ fontSize: '0.75rem', opacity: 0.65, letterSpacing: '0.06em' }}>
        AM335X BEAGLEBONE BLACK PLATFORM
      </div>
      <div style={{ fontSize: '1.1rem', fontWeight: 600, marginTop: 4 }}>
        Stock Bitmain BB port — in progress
      </div>
      <div style={{ fontSize: '0.85rem', marginTop: '0.5rem', opacity: 0.85 }}>
        Clean-room <code>uart_trans</code> userspace daemon mediates direct
        ASIC UART chains via <code>/dev/ttyO1</code>, <code>/dev/ttyO2</code>,{' '}
        <code>/dev/ttyO4</code>, <code>/dev/ttyO5</code>. Default route
        gated by BB PIC/power integration; live validation is expanding.
      </div>
      <dl
        style={{
          display: 'grid',
          gridTemplateColumns: 'auto 1fr',
          gap: '0.25rem 0.75rem',
          margin: '0.75rem 0 0',
          fontSize: '0.8rem',
        }}
      >
        <dt style={{ opacity: 0.7 }}>uart_trans:</dt>
        <dd style={{ margin: 0 }}>{uartStatus}</dd>
        <dt style={{ opacity: 0.7 }}>chains discovered:</dt>
        <dd style={{ margin: 0 }}>
          {chainsDiscovered ?? '—'} / 3 populated
        </dd>
      </dl>
    </div>
  );
}

export default Am3BbInfoCard;
