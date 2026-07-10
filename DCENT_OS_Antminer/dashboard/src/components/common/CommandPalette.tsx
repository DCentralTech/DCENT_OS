import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { OverlayDialog } from './OverlayDialog';
import { useMinerStore } from '../../store/miner';

export interface PaletteItem {
  id: string;
  label: string;
  category: string;  // 'Navigate', 'Action', 'Mode', 'API'
  action: () => void | Promise<void>;
  shortcut?: string;
  description?: string;
  keywords?: string[];
  dangerous?: boolean;
  confirmDescription?: string;
  successMessage?: string;
  errorMessage?: string;
}

interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  items: PaletteItem[];
}

const DANGEROUS_TERMS = [
  'restart',
  'reboot',
  'shutdown',
  'sleep',
  'stop',
  'disable',
  'erase',
  'flash',
  'write',
  'voltage',
  'reset',
];

function isDangerousPaletteItem(item: PaletteItem): boolean {
  if (item.dangerous === true) return true;
  if (item.dangerous === false) return false;
  const haystack = [
    item.id,
    item.label,
    item.description ?? '',
    ...(item.keywords ?? []),
  ].join(' ').toLowerCase();

  return DANGEROUS_TERMS.some(term => haystack.includes(term));
}

function getActionErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return 'Unknown error';
}

function paletteScore(item: PaletteItem, q: string): number {
  const label = item.label.toLowerCase();
  const category = item.category.toLowerCase();
  const description = (item.description ?? '').toLowerCase();
  const keywords = (item.keywords ?? []).map(keyword => keyword.toLowerCase());
  if (label.startsWith(q)) return 0;
  if (keywords.some(keyword => keyword.startsWith(q))) return 1;
  if (label.includes(q)) return 2;
  if (category.includes(q)) return 3;
  if (keywords.some(keyword => keyword.includes(q))) return 4;
  if (description.includes(q)) return 5;
  return Number.POSITIVE_INFINITY;
}

export function CommandPalette({ open, onClose, items }: CommandPaletteProps) {
  const [query, setQuery] = useState('');
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [confirmingItem, setConfirmingItem] = useState<PaletteItem | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [runningItemId, setRunningItemId] = useState<string | null>(null);
  const addToast = useMinerStore(s => s.addToast);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const confirmButtonRef = useRef<HTMLButtonElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const reactId = React.useId();
  const listboxId = `cmdp-listbox-${reactId.replace(/[:]/g, '_')}`;
  const optionId = (i: number) => `${listboxId}-opt-${i}`;
  const confirmTitleId = `cmdp-confirm-title-${reactId.replace(/[:]/g, '_')}`;
  const confirmDescId = `cmdp-confirm-desc-${reactId.replace(/[:]/g, '_')}`;

  // Reset on open
  useEffect(() => {
    if (open) {
      previousFocusRef.current = document.activeElement as HTMLElement | null;
      setQuery('');
      setSelectedIndex(0);
      setConfirmingItem(null);
      setActionError(null);
      setRunningItemId(null);
      setTimeout(() => inputRef.current?.focus(), 50);
      return;
    }

    previousFocusRef.current?.focus();
  }, [open]);

  useEffect(() => {
    if (confirmingItem) {
      setTimeout(() => confirmButtonRef.current?.focus(), 0);
    }
  }, [confirmingItem]);

  // Fuzzy filter
  const filtered = useMemo(() => {
    if (!query.trim()) return items.slice(0, 14);
    const q = query.toLowerCase();
    return items
      .map((item, index) => ({ item, index, score: paletteScore(item, q) }))
      .filter(match => Number.isFinite(match.score))
      .sort((a, b) => a.score - b.score || a.index - b.index)
      .map(match => match.item)
      .slice(0, 14);
  }, [query, items]);

  // Clamp selected index when list shrinks
  useEffect(() => {
    if (selectedIndex >= filtered.length) {
      setSelectedIndex(Math.max(0, filtered.length - 1));
    }
  }, [filtered.length, selectedIndex]);

  // Scroll selected item into view
  useEffect(() => {
    if (!listRef.current) return;
    const buttons = listRef.current.querySelectorAll('button');
    if (buttons[selectedIndex]) {
      buttons[selectedIndex].scrollIntoView({ block: 'nearest' });
    }
  }, [selectedIndex]);

  const runItem = useCallback(async (item: PaletteItem) => {
    setRunningItemId(item.id);
    setActionError(null);
    try {
      await item.action();
      if (item.successMessage) {
        addToast(item.successMessage, 'success');
      }
      setConfirmingItem(null);
      onClose();
    } catch (error) {
      const detail = getActionErrorMessage(error);
      const message = item.errorMessage ? `${item.errorMessage}: ${detail}` : `Command failed: ${detail}`;
      setActionError(message);
      addToast(message, 'error');
    } finally {
      setRunningItemId(null);
    }
  }, [addToast, onClose]);

  const requestRunItem = useCallback((item: PaletteItem) => {
    setActionError(null);

    if (isDangerousPaletteItem(item)) {
      setConfirmingItem(item);
      return;
    }

    void runItem(item);
  }, [runItem]);

  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      if (confirmingItem) {
        setConfirmingItem(null);
        setActionError(null);
        inputRef.current?.focus();
        return;
      }
      onClose();
      return;
    }
    if (e.key === 'Enter' && (e.target as HTMLElement).tagName === 'BUTTON') {
      return;
    }
    if (e.key === 'Tab') {
      const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(
        'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
      );
      if (!focusable || focusable.length === 0) return;

      const first = focusable[0];
      const last = focusable[focusable.length - 1];

      if (e.shiftKey) {
        if (document.activeElement === first) {
          e.preventDefault();
          last.focus();
        }
      } else if (document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
      return;
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setSelectedIndex(i => Math.min(i + 1, filtered.length - 1));
      return;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      setSelectedIndex(i => Math.max(i - 1, 0));
      return;
    }
    if (e.key === 'Enter' && filtered[selectedIndex]) {
      requestRunItem(filtered[selectedIndex]);
      return;
    }
  }, [confirmingItem, filtered, selectedIndex, onClose, requestRunItem]);

  if (!open) return null;

  return (
    <OverlayDialog open={open} onClose={onClose} ariaLabel="Command palette" initialFocusRef={inputRef as React.RefObject<HTMLElement>} maxWidth={480} chrome={false}>
      <div
        ref={dialogRef}
        className="cp-palette"
        onClick={e => e.stopPropagation()}
        onKeyDown={handleKeyDown}
      >
        {/* Search input — combobox semantics so SR + tools announce
            the live listbox + active option. Keeps existing behavior;
            only adds aria attributes. */}
        <div
          className="cp-palette-search"
          role="combobox"
          aria-expanded={filtered.length > 0}
          aria-haspopup="listbox"
          aria-controls={listboxId}
          aria-owns={listboxId}
        >
          <span className="cp-palette-prompt" aria-hidden="true">&gt;</span>
          <input
            ref={inputRef}
            type="text"
            className="cp-palette-input"
            value={query}
            onChange={e => {
              setQuery(e.target.value);
              setSelectedIndex(0);
              setConfirmingItem(null);
              setActionError(null);
            }}
            placeholder="Type a command..."
            aria-autocomplete="list"
            aria-controls={listboxId}
            aria-activedescendant={filtered[selectedIndex] ? optionId(selectedIndex) : undefined}
            aria-label="Command palette search"
          />
        </div>

        {/* Results list */}
        <div
          ref={listRef}
          id={listboxId}
          className="cp-palette-list"
          role="listbox"
          aria-label="Command results"
        >
          {filtered.length === 0 ? (
            <div className="cp-palette-empty">
              No results for &ldquo;{query}&rdquo;
            </div>
          ) : (
            filtered.map((item, i) => (
              <button
                key={item.id}
                id={optionId(i)}
                className="cp-palette-opt"
                role="option"
                aria-selected={i === selectedIndex}
                data-selected={i === selectedIndex ? 'true' : undefined}
                data-dangerous={isDangerousPaletteItem(item) ? 'true' : undefined}
                onClick={() => requestRunItem(item)}
                onMouseEnter={() => setSelectedIndex(i)}
              >
                <div className="cp-palette-opt-main">
                  <span className="cp-palette-opt-cat">
                    {item.category}
                  </span>
                  <span
                    className="cp-palette-opt-body"
                    style={{ gap: item.description ? 2 : 0 }}
                  >
                    <span>{item.label}</span>
                    {item.description && (
                      <span className="cp-palette-opt-desc">
                        {item.description}
                      </span>
                    )}
                  </span>
                </div>
                <span className="cp-palette-opt-meta">
                  {isDangerousPaletteItem(item) && (
                    <span className="cp-palette-tag cp-confirm">
                      confirm
                    </span>
                  )}
                  {item.shortcut && (
                    <span className="cp-palette-tag cp-kbd">
                      {item.shortcut}
                    </span>
                  )}
                </span>
              </button>
            ))
          )}
        </div>

        {confirmingItem && (
          <div
            className="cp-palette-confirm"
            role="alertdialog"
            aria-modal="false"
            aria-labelledby={confirmTitleId}
            aria-describedby={confirmDescId}
          >
            <div className="cp-palette-confirm-title" id={confirmTitleId}>
              Confirm {confirmingItem.label}
            </div>
            <div className="cp-palette-confirm-desc" id={confirmDescId}>
              {confirmingItem.confirmDescription ?? 'This command can interrupt mining or change live miner state.'}
            </div>
            {actionError && (
              <div className="cp-palette-confirm-err">
                {actionError}
              </div>
            )}
            <div className="cp-palette-confirm-actions">
              <button
                type="button"
                className="cp-palette-btn cp-cancel"
                onClick={() => {
                  setConfirmingItem(null);
                  setActionError(null);
                  inputRef.current?.focus();
                }}
                disabled={runningItemId === confirmingItem.id}
              >
                Cancel
              </button>
              <button
                ref={confirmButtonRef}
                type="button"
                className="cp-palette-btn cp-danger"
                onClick={() => { void runItem(confirmingItem); }}
                disabled={runningItemId === confirmingItem.id}
              >
                {runningItemId === confirmingItem.id ? 'Running...' : 'Confirm'}
              </button>
            </div>
          </div>
        )}

        {/* Footer hint */}
        <div className="cp-palette-footer">
          <span>&#8593;&#8595; navigate</span>
          <span>&#8629; select</span>
          <span>esc close</span>
        </div>
      </div>
    </OverlayDialog>
  );
}
