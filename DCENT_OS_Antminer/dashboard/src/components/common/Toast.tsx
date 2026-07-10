import React, { useEffect, useCallback, useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { noticeGlyph } from '../../utils/noticeGlyphs';

export function ToastContainer() {
  const toasts = useMinerStore(s => s.toasts);
  const removeToast = useMinerStore(s => s.removeToast);

  // Container is a positioning shell only — each ToastItem is its own
  // live region (role="alert" for errors, role="status" otherwise) so
  // error toasts interrupt SR users and non-critical toasts don't.
  return (
    <div
      style={{
        position: 'fixed',
        bottom: 24,
        right: 24,
        zIndex: 10000,
        display: 'flex',
        flexDirection: 'column',
        gap: 8,
        maxWidth: 400,
        pointerEvents: 'none',
      }}
    >
      {toasts.map(toast => (
        <ToastItem key={toast.id} toast={toast} onDismiss={removeToast} />
      ))}
    </div>
  );
}

interface ToastData {
  id: string;
  message: string;
  type: 'success' | 'error' | 'warning' | 'info';
  createdAt: number;
}

function ToastItem({ toast, onDismiss }: { toast: ToastData; onDismiss: (id: string) => void }) {
  const dismiss = useCallback(() => onDismiss(toast.id), [toast.id, onDismiss]);
  const [paused, setPaused] = useState(false);

  // Auto-dismiss after 4s, but PAUSE the timer while the toast is hovered or
  // keyboard-focused so a user reading/about-to-act on it isn't raced by the
  // timeout. Re-arms on leave/blur.
  useEffect(() => {
    if (paused) return;
    const timer = setTimeout(dismiss, 4000);
    return () => clearTimeout(timer);
  }, [dismiss, paused]);

  // Error toasts interrupt SR users (role="alert" + aria-live="assertive").
  // Non-critical toasts use role="status" + aria-live="polite".
  const isError = toast.type === 'error';
  const liveRole = isError ? 'alert' : 'status';
  const liveLevel = isError ? 'assertive' : 'polite';

  const pause = useCallback(() => setPaused(true), []);
  const resume = useCallback(() => setPaused(false), []);

  return (
    <div
      role={liveRole}
      aria-live={liveLevel}
      aria-atomic="true"
    >
      {/* Wave-13: removed per-item `pointerEvents:none` — the toast STACK
          container already sets it (intentional click-through), and the
          per-item one risked interfering with the .ds-toast focus/hover
          pause-on-interaction handlers below. */}
      <div
        className={`ds-toast ds-toast--${toast.type}`}
        role="button"
        tabIndex={0}
        aria-label={`${toast.type} notification. Press Enter to dismiss.`}
        onKeyDown={e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); dismiss(); } }}
        onClick={dismiss}
        onMouseEnter={pause}
        onMouseLeave={resume}
        onFocus={pause}
        onBlur={resume}
      >
        <span className="ds-toast__glyph" aria-hidden="true">
          {/* Glyph from the shared canonical map (utils/noticeGlyphs) — same
              tone glyph as InfoBanner/AlertBanner/StatePanel. error -> danger
              glyph; aria-hidden decoration only. */}
          {noticeGlyph(toast.type)}
        </span>
        <span className="ds-toast__msg">{toast.message}</span>
      </div>
    </div>
  );
}
