import React, { useState, useEffect, useRef, useMemo, useCallback } from 'react';
import type { PaletteItem } from '../common/CommandPalette';

interface CommandBarProps {
  items: PaletteItem[];
  onOpenPalette?: () => void;
  onShowHelp?: () => void;
}

/**
 * Simple fuzzy ranker. Returns a positive integer score (higher = better
 * match) or 0 when no sequential match exists. Substring hits get a strong
 * boost; prefix hits get an extra boost; sequential-character chains
 * accumulate bonus points.
 */
function fuzzyScore(target: string, query: string): number {
  if (!query) return 1;
  const t = target.toLowerCase();
  const q = query.toLowerCase();
  if (t.includes(q)) return 100 + (t.startsWith(q) ? 50 : 0);
  let score = 0;
  let ti = 0;
  let lastMatch = -1;
  for (let qi = 0; qi < q.length; qi++) {
    const ch = q[qi];
    const found = t.indexOf(ch, ti);
    if (found === -1) return 0;
    score += found - lastMatch === 1 ? 10 : 2;
    lastMatch = found;
    ti = found + 1;
  }
  return score;
}

/**
 * Render `label` with characters that appear (in order) in `query` wrapped
 * in <mark>. Used to give a visual cue of the fuzzy match.
 */
function highlightLabel(label: string, query: string): React.ReactNode {
  const q = query.trim().toLowerCase();
  if (!q) return label;
  const ll = label.toLowerCase();
  const parts: React.ReactNode[] = [];
  let qi = 0;
  let buf = '';
  for (let i = 0; i < label.length; i++) {
    if (qi < q.length && ll[i] === q[qi]) {
      if (buf) { parts.push(buf); buf = ''; }
      parts.push(<mark key={`m-${i}`} className="hacker-command-bar-match">{label[i]}</mark>);
      qi++;
    } else {
      buf += label[i];
    }
  }
  if (buf) parts.push(buf);
  return <>{parts}</>;
}

/**
 * Sticky bottom command bar for Hacker mode. Visual + spiritual cousin of the
 * inspiration HackerMode.jsx `CommandLine` + `StatusLine`. Single input with
 * `>` prompt + cursor blink, opens a small fuzzy dropdown above on focus, and
 * exposes hints on the right edge.
 *
 * Keyboard:
 *   `:`     focus this bar's input (vim-style, matches inspiration)
 *   `Esc`   blur + close dropdown
 *   `Enter` run highlighted item
 *   `Up/Dn` move highlight in dropdown
 */
export function CommandBar({ items, onOpenPalette, onShowHelp }: CommandBarProps) {
  const [query, setQuery] = useState('');
  const [focused, setFocused] = useState(false);
  const [highlight, setHighlight] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const wrapperRef = useRef<HTMLDivElement>(null);

  // Focus on `:` keypress anywhere (not while typing in another input)
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key !== ':') return;
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName;
      const isFormField =
        tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' ||
        target?.isContentEditable;
      if (isFormField) return;
      e.preventDefault();
      inputRef.current?.focus();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  const matches = useMemo(() => {
    const q = query.trim();
    if (!q) return items.slice(0, 8);
    return items
      .map(item => {
        const hay = [
          item.label,
          item.id,
          item.category,
          item.description ?? '',
          ...(item.keywords ?? []),
        ].join(' ');
        return { item, score: fuzzyScore(hay, q), labelScore: fuzzyScore(item.label, q) };
      })
      .filter(entry => entry.score > 0 || entry.labelScore > 0)
      .sort((a, b) => {
        // Prefer label hits, fall back to combined haystack score.
        const aPrimary = a.labelScore > 0 ? a.labelScore : a.score;
        const bPrimary = b.labelScore > 0 ? b.labelScore : b.score;
        return bPrimary - aPrimary;
      })
      .slice(0, 8)
      .map(entry => entry.item);
  }, [items, query]);

  useEffect(() => { setHighlight(0); }, [query]);

  const run = useCallback((item: PaletteItem) => {
    try {
      void item.action();
    } catch (e) {
      // swallow — palette has its own error toasts
    }
    setQuery('');
    setFocused(false);
    inputRef.current?.blur();
  }, []);

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      setQuery('');
      setFocused(false);
      inputRef.current?.blur();
      return;
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setHighlight(h => Math.min(h + 1, matches.length - 1));
      return;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      setHighlight(h => Math.max(h - 1, 0));
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      const item = matches[highlight];
      if (item) run(item);
      return;
    }
    // Tab: complete to highlighted label (no action)
    if (e.key === 'Tab' && matches[highlight]) {
      e.preventDefault();
      setQuery(matches[highlight].label.toLowerCase());
    }
  };

  // Click-outside collapse
  useEffect(() => {
    if (!focused) return;
    const handler = (e: MouseEvent) => {
      if (wrapperRef.current && !wrapperRef.current.contains(e.target as Node)) {
        setFocused(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [focused]);

  const showDropdown = focused && matches.length > 0;

  return (
    <div ref={wrapperRef} className="hacker-command-bar" role="region" aria-label="Command bar">
      {showDropdown && (
        <div className="hacker-command-bar-dropdown" role="listbox">
          {matches.map((item, i) => (
            <button
              key={item.id}
              type="button"
              role="option"
              aria-selected={i === highlight}
              className={`hacker-command-bar-option ${i === highlight ? 'is-active' : ''}`}
              onMouseEnter={() => setHighlight(i)}
              onMouseDown={(e) => { e.preventDefault(); run(item); }}
            >
              <span className="hacker-command-bar-cat">{item.category}</span>
              <span className="hacker-command-bar-label">{highlightLabel(item.label, query)}</span>
              {item.description && (
                <span className="hacker-command-bar-desc">{item.description}</span>
              )}
              {item.shortcut && (
                <span className="hacker-command-bar-shortcut">{item.shortcut}</span>
              )}
            </button>
          ))}
          {matches.length === 0 && (
            <div className="hacker-command-bar-empty">no match</div>
          )}
        </div>
      )}

      <div className="hacker-command-bar-row">
        <span className="hacker-command-bar-prompt" aria-hidden="true">
          {focused || query ? '>' : ':'}
        </span>
        <input
          ref={inputRef}
          type="text"
          className="hacker-command-bar-input"
          value={query}
          spellCheck={false}
          autoComplete="off"
          placeholder="type a command  ·  press : to focus  ·  ↑↓ to navigate  ·  ↵ to run"
          aria-label="Command input"
          onChange={(e) => setQuery(e.target.value)}
          onFocus={() => setFocused(true)}
          onKeyDown={onKeyDown}
        />
        {focused && <span className="ds-cursor-blink hacker-command-bar-caret" aria-hidden="true">_</span>}

        <div className="hacker-command-bar-hints" role="group" aria-label="Command shortcuts">
          <button
            type="button"
            className="hacker-command-bar-hint"
            onClick={() => onOpenPalette?.()}
            title="Open command palette (Ctrl+K)"
          >
            <kbd>Ctrl+K</kbd> palette
          </button>
          <span className="hacker-command-bar-sep">·</span>
          <button
            type="button"
            className="hacker-command-bar-hint"
            onClick={() => onShowHelp?.()}
            title="Show keyboard shortcuts"
          >
            <kbd>?</kbd> help
          </button>
          <span className="hacker-command-bar-sep">·</span>
          <span className="hacker-command-bar-hint">
            <kbd>:</kbd> cmd
          </span>
        </div>
      </div>
    </div>
  );
}
