// Step 2 — operator types miner serial verbatim. Submit only
// enabled when typed string matches the live serial.
//
// W8-F backend re-checks at the wire so direct curl bypasses don't
// work either; this is a usability gate, not a security gate.

import React from 'react';
import { useMinerStore } from '../../store/miner';
import { BreakerWarningBanner } from './BreakerWarningBanner';

interface Props {
  typedSerial: string;
  setTypedSerial: (s: string) => void;
}

export function TypeSerial({ typedSerial, setTypedSerial }: Props) {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const expected = systemInfo?.hardware?.miner_serial ?? systemInfo?.hostname ?? '';
  const trimmed = typedSerial.trim();
  const expectedTrimmed = expected.trim();
  const match = !!expectedTrimmed && trimmed === expectedTrimmed;
  // Real-time match progress — show only when typing has started AND the
  // user is clearly still typing (no partial-match auto-accept).
  const hasInput = trimmed.length > 0;
  const isPrefix = hasInput && !!expectedTrimmed && expectedTrimmed.startsWith(trimmed) && !match;

  return (
    <div>
      <BreakerWarningBanner />
      <h3 style={{ marginTop: 0, fontSize: '1.1rem' }}>Type the miner serial</h3>
      <div style={{ fontSize: '0.82rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 14 }}>
        Type the serial below verbatim. This protects against the wrong-tab disaster — when
        you have multiple miners' dashboards open, typing the serial proves you're flashing
        the unit you think you're flashing.
      </div>

      <div style={{
        padding: 12,
        borderRadius: 8,
        background: 'rgba(18,18,26,0.6)',
        border: '1px solid var(--border, rgba(255,255,255,0.08))',
        marginBottom: 12,
      }}>
        <div style={{ fontSize: '0.7rem', color: 'var(--text-secondary, #8b8b9e)', textTransform: 'uppercase', letterSpacing: '0.04em', fontWeight: 700 }}>
          Live serial / hostname
        </div>
        <div style={{ fontSize: '1.15rem', color: 'var(--text)', fontFamily: 'JetBrains Mono, monospace', fontWeight: 700, marginTop: 4, letterSpacing: '0.02em', userSelect: 'all' }}>
          {expected || '(unknown — backend did not report a serial; type your hostname)'}
        </div>
      </div>

      <label htmlFor="restore-serial-input" style={{ display: 'block', fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 6 }}>
        Type it here (case-sensitive, exact match required)
      </label>
      <input
        id="restore-serial-input"
        type="text"
        value={typedSerial}
        onChange={(e) => setTypedSerial(e.target.value)}
        autoComplete="off"
        spellCheck={false}
        autoCapitalize="off"
        placeholder="serial / hostname"
        aria-invalid={hasInput && !match && !isPrefix}
        style={{
          width: '100%',
          padding: '12px 14px',
          background: 'rgba(10,10,15,0.6)',
          border: `1px solid ${match ? 'var(--green, #2DD4A0)' : isPrefix ? 'var(--accent, #FAA500)' : 'var(--border, rgba(255,255,255,0.12))'}`,
          borderRadius: 8,
          color: 'var(--text)',
          fontFamily: 'JetBrains Mono, monospace',
          fontSize: '1.05rem',
          letterSpacing: '0.02em',
        }}
      />
      <div
        style={{
          marginTop: 8,
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'center',
          fontSize: '0.75rem',
          color: match ? 'var(--green, #2DD4A0)' : isPrefix ? 'var(--accent, #FAA500)' : 'var(--text-dim, #6E6E80)',
        }}
        aria-live="polite"
      >
        <span>
          {match
            ? '✓ Exact match — you can advance to NAND backup.'
            : isPrefix
              ? 'Prefix matches; keep typing for an exact match.'
              : hasInput
                ? '✗ No match — check capitalization.'
                : 'Waiting for an exact match...'}
        </span>
        {expectedTrimmed && (
          <span style={{ fontFamily: 'JetBrains Mono, monospace', color: 'var(--text-dim, #6E6E80)' }}>
            {trimmed.length}/{expectedTrimmed.length}
          </span>
        )}
      </div>
    </div>
  );
}

export function isSerialMatch(typed: string, expected: string | null | undefined): boolean {
  return !!expected && typed.trim() === expected.trim();
}
