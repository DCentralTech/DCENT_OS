import React from 'react';
import type { OperatingMode } from '../../api/types';
import { MODE_DESCRIPTIONS } from '../../utils/constants';

interface ModeSwitchProps {
  currentMode: OperatingMode;
  onSelect: (mode: OperatingMode) => void;
  compact?: boolean;
}

const MODES: OperatingMode[] = ['heater', 'standard', 'hacker'];

export function ModeSwitch({ currentMode, onSelect, compact }: ModeSwitchProps) {
  return (
    <div
      role="group"
      aria-label="Operating mode"
      style={{
        display: 'grid',
        gridTemplateColumns: compact ? 'repeat(3, minmax(0, 1fr))' : '1fr',
        gap: compact ? 8 : 16,
      }}
    >
      {MODES.map(mode => {
        const info = MODE_DESCRIPTIONS[mode];
        const isActive = currentMode === mode;
        return (
          <button
            key={mode}
            type="button"
            aria-pressed={isActive}
            data-mode={mode}
            data-active={isActive ? 'true' : 'false'}
            onClick={() => onSelect(mode)}
            style={{
              background: isActive
                ? 'linear-gradient(135deg, rgba(250, 165, 0, 0.10) 0%, rgba(250, 103, 0, 0.04) 100%)'
                : 'var(--card-bg, #242432)',
              border: `1px solid ${isActive ? 'var(--accent-border, rgba(250,165,0,0.45))' : 'var(--border, #333)'}`,
              boxShadow: isActive
                ? 'var(--elevation-glow, 0 0 0 1px rgba(250,165,0,0.32), 0 4px 14px rgba(0,0,0,0.32), 0 0 22px rgba(250,165,0,0.18))'
                : 'var(--elevation-base, 0 1px 2px rgba(0,0,0,0.35), 0 2px 6px rgba(0,0,0,0.22))',
              borderRadius: 'var(--radius-md, 12px)',
              padding: compact ? '10px 6px' : 24,
              minWidth: 0,
              overflow: 'hidden',
              cursor: 'pointer',
              textAlign: compact ? 'center' : 'left',
              transition: 'background var(--dur-med, 250ms) var(--ease-standard, cubic-bezier(.2,0,0,1)),'
                + ' border-color var(--dur-med, 250ms) var(--ease-standard, cubic-bezier(.2,0,0,1)),'
                + ' box-shadow var(--dur-med, 250ms) var(--ease-standard, cubic-bezier(.2,0,0,1)),'
                + ' transform var(--dur-fast, 150ms) var(--ease-standard, cubic-bezier(.2,0,0,1))',
              transform: isActive ? 'translateY(-1px)' : 'none',
            }}
          >
            <div style={{ fontSize: compact ? '1.5rem' : '2rem', marginBottom: 8 }}>
              {info.icon}
            </div>
            <div style={{
              fontFamily: "var(--font-heading)",
              fontWeight: 700, fontSize: compact ? '1rem' : '1.3rem',
              color: isActive ? 'var(--accent, #FAA500)' : 'var(--text, #E8E8E8)',
            }}>
              {info.title}
            </div>
            {!compact && (
              <>
                <div style={{ color: 'var(--text-secondary, #9CA3AF)', fontSize: '0.85rem', marginTop: 4 }}>
                  {info.subtitle}
                </div>
                <div style={{ color: 'var(--text-dim, #6B7280)', fontSize: '0.8rem', marginTop: 8 }}>
                  {info.description}
                </div>
              </>
            )}
          </button>
        );
      })}
    </div>
  );
}
