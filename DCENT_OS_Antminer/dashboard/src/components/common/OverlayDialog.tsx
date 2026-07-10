import React from 'react';
import { createPortal } from 'react-dom';
import type { RefObject } from 'react';
import { useOverlayA11y } from '../../hooks/useOverlayA11y';

export function OverlayDialog({
  open,
  onClose,
  ariaLabel,
  ariaLabelledBy,
  dismissible = true,
  initialFocusRef,
  variant = 'modal',
  children,
  maxWidth,
  width = '90%',
  chrome = true,
}: {
  open: boolean;
  onClose: () => void;
  ariaLabel: string;
  ariaLabelledBy?: string;
  dismissible?: boolean;
  initialFocusRef?: RefObject<HTMLElement>;
  variant?: 'modal' | 'sheet';
  children: React.ReactNode;
  maxWidth?: number;
  width?: string;
  chrome?: boolean;
}) {
  const { containerRef } = useOverlayA11y({ open, onClose, dismissible, initialFocusRef });

  if (!open) {
    return null;
  }

  const isSheet = variant === 'sheet';

  // Chrome (scrim blur, glass panel, elevation, radius, entrance animation) is
  // the canonical F6 `.ds-overlay-*` class set (design-system.css §F6-OVERLAY)
  // — byte-faithful to the prior inline rgba(0,0,0,0.6)+blur(8px) scrim, with
  // the panel upgraded from --shadow-elevated-only to the canonical
  // --elevation-overlay + ds-glass-strong-grade glass. Only the per-instance
  // dynamic sizing (`width`/`maxWidth`) stays inline. When `chrome={false}`
  // the caller supplies its own shell (CommandPalette / SetupWizard /
  // CurrentBlockCard / EfficiencyMigrationPrompt) so the panel stays a bare
  // transparent positioned container — no glass class applied.
  const panelClass = chrome
    ? (isSheet ? 'ds-overlay-panel is-sheet' : 'ds-overlay-panel')
    : undefined;

  const content = (
    <div
      data-testid="overlay-backdrop"
      className={isSheet ? 'ds-overlay-backdrop is-sheet' : 'ds-overlay-backdrop'}
      onClick={() => { if (dismissible) onClose(); }}
    >
      <div
        ref={containerRef}
        role="dialog"
        aria-modal="true"
        aria-label={ariaLabel}
        aria-labelledby={ariaLabelledBy}
        tabIndex={-1}
        onClick={e => e.stopPropagation()}
        className={panelClass}
        style={chrome
          ? { width, maxWidth }
          : isSheet
            ? { width, maxWidth, minHeight: '100%', background: 'transparent', outline: 'none' }
            : { width, maxWidth, background: 'transparent', outline: 'none' }}
      >
        {children}
      </div>
    </div>
  );

  return createPortal(content, document.body);
}
