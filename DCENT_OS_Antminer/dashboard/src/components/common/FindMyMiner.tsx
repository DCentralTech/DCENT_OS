import React, { useEffect, useId, useRef, useState } from 'react';
import api from '../../api/client';
import type { LedPatternInfo } from '../../api/types';
import { useOverlayA11y } from '../../hooks/useOverlayA11y';

/**
 * "Find My Miner" button with pattern selector dropdown.
 * Triggers LED blink patterns on the miner's front panel.
 * Available in all dashboard modes.
 */
export function FindMyMiner() {
  const [patterns, setPatterns] = useState<LedPatternInfo[]>([]);
  const [selected, setSelected] = useState('imperial_march');
  const [locating, setLocating] = useState(false);
  const [remaining, setRemaining] = useState<number | null>(null);
  const [showDropdown, setShowDropdown] = useState(false);
  const dropdownId = useId();
  const titleId = `${dropdownId}-title`;
  const descriptionId = `${dropdownId}-description`;
  const blinkNowRef = useRef<HTMLButtonElement>(null);
  const timerRef = useRef<number>();
  const { containerRef } = useOverlayA11y({
    open: showDropdown && !locating,
    onClose: () => setShowDropdown(false),
    initialFocusRef: blinkNowRef as React.RefObject<HTMLElement>,
    lockScroll: false,
    closeOnInteractOutside: true,
  });

  // Fetch available patterns on mount
  useEffect(() => {
    api.getLedPatterns().then(res => {
      // API returns locate_patterns (array) and background_patterns
      const pats = res.locate_patterns ?? res.patterns ?? [];
      setPatterns(pats);
      if (res.selected) setSelected(res.selected);
    }).catch(() => {});
  }, []);

  // Countdown timer while locating
  useEffect(() => {
    if (locating && remaining !== null && remaining > 0) {
      timerRef.current = window.setTimeout(() => {
        setRemaining(r => r !== null ? r - 1 : null);
      }, 1000);
    }
    if (remaining === 0) {
      setLocating(false);
      setRemaining(null);
    }
    return () => { if (timerRef.current) clearTimeout(timerRef.current); };
  }, [locating, remaining]);

  const handleLocate = async (patternId?: string) => {
    const id = patternId || selected;
    try {
      await api.triggerLocate({ pattern_id: id });
      setLocating(true);
      setRemaining(30);
      setShowDropdown(false);
      if (patternId) setSelected(patternId);
    } catch {
      // Silently fail — miner might not support LEDs
    }
  };

  const handleStop = async () => {
    try {
      await api.stopLocate();
    } catch {}
    setLocating(false);
    setRemaining(null);
  };

  const selectedPattern = patterns.find(p => p.id === selected);

  return (
    <div style={{ position: 'relative', display: 'inline-block' }}>
      {/* Main button */}
      {locating ? (
        <button
          type="button"
          className="btn btn-danger"
          onClick={handleStop}
          aria-live="polite"
          style={{
            display: 'flex', alignItems: 'center', gap: 6,
            animation: 'pulse-glow 1s ease-in-out infinite',
            fontSize: '0.85rem', padding: '6px 12px',
          }}
        >
          <span style={{ fontSize: '1.1rem' }}>{'\u{1F6A8}'}</span>
          Stop {remaining !== null && `(${remaining}s)`}
        </button>
      ) : (
        <button
          type="button"
          className="btn btn-secondary"
          onClick={() => setShowDropdown(!showDropdown)}
          aria-haspopup="dialog"
          aria-expanded={showDropdown}
          aria-controls={dropdownId}
          style={{
            display: 'flex', alignItems: 'center', gap: 6,
            fontSize: '0.85rem', padding: '6px 12px',
          }}
          title="Find My Miner — blink the LEDs to identify this unit"
        >
          <span style={{ fontSize: '1.1rem' }}>{'\u{1F4E1}'}</span>
          Find Miner
        </button>
      )}

      {/* Pattern dropdown */}
      {showDropdown && !locating && (
        <div
          ref={containerRef}
          id={dropdownId}
          role="dialog"
          aria-modal="false"
          aria-labelledby={titleId}
          aria-describedby={descriptionId}
          tabIndex={-1}
          style={{
            position: 'absolute', top: '100%', right: 0, marginTop: 4,
            background: 'var(--card-bg, #1a1a2e)', border: '1px solid var(--border, #333)',
            borderRadius: 8, padding: 8, minWidth: 280, zIndex: 1000,
            boxShadow: '0 8px 32px rgba(0,0,0,0.5)',
          }}
        >
          <div
            id={titleId}
            style={{
            fontSize: '0.75rem', color: 'var(--text-secondary, #888)',
            padding: '4px 8px', marginBottom: 4, fontWeight: 600,
            textTransform: 'uppercase', letterSpacing: '0.05em',
          }}>
            Choose Blink Pattern
          </div>
          <div
            id={descriptionId}
            style={{
              fontSize: '0.78rem', color: 'var(--text-secondary, #888)',
              padding: '0 8px 8px', lineHeight: 1.4,
            }}
          >
            Pick an LED pattern, then trigger the miner identification blink.
          </div>

          {patterns.map(p => (
            <button
              type="button"
              key={p.id}
              onClick={() => handleLocate(p.id)}
              aria-pressed={p.id === selected}
              style={{
                display: 'block', width: '100%', textAlign: 'left',
                background: p.id === selected ? 'var(--accent-glow, var(--accent-dim, rgba(250,165,0,0.12)))' : 'transparent',
                border: '1px solid transparent', borderRadius: 6, padding: '8px 12px',
                color: 'var(--text-primary, var(--text, #eee))', cursor: 'pointer',
                marginBottom: 2,
                transition: 'background var(--dur-fast, 150ms), border-color var(--dur-fast, 150ms)',
              }}
              onMouseEnter={e => (e.currentTarget.style.background = 'var(--hover-bg, rgba(255,255,255,0.05))')}
              onMouseLeave={e => (e.currentTarget.style.background = p.id === selected ? 'var(--accent-glow, var(--accent-dim, rgba(250,165,0,0.12)))' : 'transparent')}
            >
              <div style={{ fontWeight: 600, fontSize: '0.9rem' }}>
                {p.name}
                {p.id === selected && (
                  <span style={{ marginLeft: 8, fontSize: '0.75rem', color: 'var(--accent, #0f8)' }}>
                    DEFAULT
                  </span>
                )}
              </div>
              <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary, #888)', marginTop: 2 }}>
                {p.description}
              </div>
            </button>
          ))}

          {/* Quick trigger with default */}
          <div style={{ borderTop: '1px solid var(--border, #333)', marginTop: 4, paddingTop: 8 }}>
            <button
              type="button"
              ref={blinkNowRef}
              className="btn btn-primary"
              onClick={() => handleLocate()}
              style={{ width: '100%', fontSize: '0.85rem' }}
            >
              {'\u{1F4E1}'} Blink Now ({selectedPattern?.name || 'Imperial March'})
            </button>
          </div>
        </div>
      )}

      {/* Pulse animation for active locate */}
      <style>{`
        @keyframes pulse-glow {
          0%, 100% { box-shadow: 0 0 4px rgba(255,50,50,0.3); }
          50% { box-shadow: 0 0 16px rgba(255,50,50,0.6); }
        }
      `}</style>
    </div>
  );
}
