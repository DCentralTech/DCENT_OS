import React, { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { useMinerStore } from '../../store/miner';
import type { Alert, Toast } from '../../store/miner';

/**
 * Hacker-mode pull-out drawer for alerts + toast history. Slides in from the
 * right edge, lives below the topbar. Reads alerts + toasts from the zustand
 * store and renders them as a unified timeline, newest first.
 *
 * Trigger: keyboard "n" (when not typing in a form field) or external state
 * via the `useNotificationDrawer` hook. The hook returns `{ open, setOpen,
 * toggle }` so the parent dashboard can wire its own bell button.
 */

interface NotificationRow {
  id: string;
  level: 'info' | 'warn' | 'error';
  message: string;
  timestamp: number;
  source: 'alert' | 'toast';
}

function alertLevelToRowLevel(level: Alert['level']): NotificationRow['level'] {
  if (level === 'critical') return 'error';
  if (level === 'warning') return 'warn';
  return 'info';
}

function toastTypeToRowLevel(type: Toast['type']): NotificationRow['level'] {
  if (type === 'error') return 'error';
  if (type === 'warning') return 'warn';
  if (type === 'success') return 'info';
  return 'info';
}

function formatTimestamp(ts: number): string {
  const d = new Date(ts);
  return d.toTimeString().slice(0, 8);
}

export function useNotificationDrawer() {
  const [open, setOpen] = useState(false);
  const toggle = useCallback(() => setOpen(o => !o), []);

  // Keyboard shortcut: "n" toggles when not typing in a form field.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key !== 'n' && e.key !== 'N') return;
      // Don't fire while modifier keys held (avoid Ctrl+N new window etc.)
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName;
      const isFormField =
        tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' ||
        target?.isContentEditable;
      if (isFormField) return;
      e.preventDefault();
      setOpen(o => !o);
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  return { open, setOpen, toggle };
}

interface NotificationCenterProps {
  open: boolean;
  onClose: () => void;
}

export function NotificationCenter({ open, onClose }: NotificationCenterProps) {
  const alerts = useMinerStore(s => s.alerts);
  const toasts = useMinerStore(s => s.toasts);
  const panelRef = useRef<HTMLDivElement>(null);

  // Unified, newest-first timeline.
  const rows = useMemo<NotificationRow[]>(() => {
    const alertRows: NotificationRow[] = alerts
      .filter(a => !a.dismissed)
      .map(a => ({
        id: a.id,
        level: alertLevelToRowLevel(a.level),
        message: a.message,
        timestamp: a.timestamp,
        source: 'alert' as const,
      }));
    const toastRows: NotificationRow[] = toasts.map(t => ({
      id: t.id,
      level: toastTypeToRowLevel(t.type),
      message: t.message,
      timestamp: t.createdAt,
      source: 'toast' as const,
    }));
    return [...alertRows, ...toastRows].sort((a, b) => b.timestamp - a.timestamp);
  }, [alerts, toasts]);

  // Close on Escape.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, onClose]);

  if (!open) {
    return null;
  }

  return (
    <aside
      ref={panelRef}
      className="hacker-notif-center is-open"
      role="complementary"
      aria-label="Notification center"
    >
      <header className="hacker-notif-head">
        <span className="hacker-notif-eyebrow">// alerts</span>
        <span className="hacker-notif-count">{rows.length} item{rows.length === 1 ? '' : 's'}</span>
        <button
          type="button"
          className="hacker-notif-close"
          onClick={onClose}
          aria-label="Close notification center"
        >
          ×
        </button>
      </header>
      <div className="hacker-notif-body">
        {rows.length === 0 && (
          <div className="hacker-notif-empty">no alerts · system quiet</div>
        )}
        {rows.map(row => (
          <div
            key={`${row.source}-${row.id}`}
            className={`hacker-notif-row is-${row.level}`}
          >
            <span className="hacker-notif-row-time">{formatTimestamp(row.timestamp)}</span>
            <span className={`hacker-notif-row-pill is-${row.level}`}>{row.level}</span>
            <span className="hacker-notif-row-msg">{row.message}</span>
          </div>
        ))}
      </div>
      <footer className="hacker-notif-foot">
        <span><kbd>n</kbd> toggle · <kbd>Esc</kbd> close</span>
      </footer>
    </aside>
  );
}
