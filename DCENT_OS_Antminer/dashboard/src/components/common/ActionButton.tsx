import React, { useEffect, useRef, useState } from 'react';
import { OverlayDialog } from './OverlayDialog';

interface ActionButtonProps {
  label: string;
  onClick: () => Promise<void> | void;
  variant?: 'primary' | 'secondary' | 'danger';
  confirm?: string;  // Confirmation message — shows dialog before action
  disabled?: boolean;
  loading?: boolean;
  icon?: string;
  className?: string;
}

// One canonical class builder. `.ds-btn` is the source of truth (it carries
// the disabled-opacity, focus ring, hover-lift); the legacy `btn`/`btn-<v>`
// hooks are kept because mode CSS (standard/advanced/hacker skins) still
// targets them. The canonical variant modifier maps secondary → no modifier
// (default glass), so we no longer emit a redundant `.ds-btn secondary`.
function dsBtnClass(variant: 'primary' | 'secondary' | 'danger', extra?: string): string {
  const mod = variant === 'primary' ? 'primary' : variant === 'danger' ? 'danger' : '';
  return `btn btn-${variant} ds-btn ${mod} ${extra || ''}`.replace(/\s+/g, ' ').trim();
}

export function ActionButton({
  label, onClick, variant = 'primary', confirm,
  disabled, loading: externalLoading, icon, className,
}: ActionButtonProps) {
  const [loading, setLoading] = useState(false);
  const [showConfirm, setShowConfirm] = useState(false);
  const isLoading = externalLoading || loading;
  const dialogRef = useRef<HTMLDivElement>(null);
  const cancelButtonRef = useRef<HTMLButtonElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);

  const execute = async () => {
    setLoading(true);
    try { await onClick(); }
    finally { setLoading(false); setShowConfirm(false); }
  };

  const handleClick = () => {
    if (confirm) setShowConfirm(true);
    else execute();
  };

  useEffect(() => {
    if (!showConfirm) {
      return;
    }

    previousFocusRef.current = document.activeElement as HTMLElement | null;
    const timer = setTimeout(() => {
      cancelButtonRef.current?.focus();
    }, 0);

    return () => {
      clearTimeout(timer);
      previousFocusRef.current?.focus();
    };
  }, [showConfirm]);

  const handleDialogKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Escape') {
      setShowConfirm(false);
      return;
    }

    if (e.key !== 'Tab') {
      return;
    }

    const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(
      'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
    );

    if (!focusable || focusable.length === 0) {
      return;
    }

    const first = focusable[0];
    const last = focusable[focusable.length - 1];

    if (e.shiftKey) {
      if (document.activeElement === first || document.activeElement === dialogRef.current) {
        e.preventDefault();
        last.focus();
      }
    } else if (document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <>
      <button
        type="button"
        className={dsBtnClass(variant, className)}
        onClick={handleClick}
        disabled={disabled || isLoading}
        aria-busy={isLoading ? true : undefined}
        aria-label={isLoading ? `${label} in progress` : label}
      >
        {isLoading ? (
          <span className="ab-spin" aria-hidden="true">
            {'\u21BB'}
          </span>
        ) : icon ? (
          <span aria-hidden="true">{icon}</span>
        ) : null}
        {label}
      </button>

      {showConfirm && (
        <OverlayDialog
          open={showConfirm}
          onClose={() => setShowConfirm(false)}
          ariaLabel="Confirm action"
          initialFocusRef={cancelButtonRef as React.RefObject<HTMLElement>}
          maxWidth={400}
        >
          <div ref={dialogRef} onKeyDown={handleDialogKeyDown} className="ab-confirm">
            <div className="ab-confirm-title">Confirm Action</div>
            <div className="ab-confirm-body">
              {confirm}
            </div>
            <div className="ab-confirm-actions">
              <button
                ref={cancelButtonRef}
                type="button"
                className="btn btn-secondary ds-btn ghost"
                onClick={() => setShowConfirm(false)}
              >
                Cancel
              </button>
              <button
                type="button"
                className={dsBtnClass(variant)}
                onClick={execute}
                disabled={isLoading}
                aria-busy={isLoading ? true : undefined}
              >
                {isLoading ? 'Processing...' : 'Confirm'}
              </button>
            </div>
          </div>
        </OverlayDialog>
      )}
    </>
  );
}
