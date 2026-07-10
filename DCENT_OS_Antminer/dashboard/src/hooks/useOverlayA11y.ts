import { useEffect, useRef } from 'react';
import type { RefObject } from 'react';

function getFocusable(container: HTMLElement | null): HTMLElement[] {
  if (!container) {
    return [];
  }

  return Array.from(container.querySelectorAll<HTMLElement>(
    'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
  )).filter(el => !el.hasAttribute('disabled') && el.tabIndex !== -1);
}

export function useOverlayA11y({
  open,
  onClose,
  dismissible = true,
  initialFocusRef,
  lockScroll = true,
  closeOnInteractOutside = false,
}: {
  open: boolean;
  onClose: () => void;
  dismissible?: boolean;
  initialFocusRef?: RefObject<HTMLElement>;
  lockScroll?: boolean;
  closeOnInteractOutside?: boolean;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!open) {
      return;
    }

    previousFocusRef.current = document.activeElement as HTMLElement | null;
    const previousOverflow = document.body.style.overflow;
    if (lockScroll) {
      document.body.style.overflow = 'hidden';
    }

    const timer = setTimeout(() => {
      const target = initialFocusRef?.current ?? getFocusable(containerRef.current)[0] ?? containerRef.current;
      target?.focus();
    }, 0);

    const handleKeyDown = (event: KeyboardEvent) => {
      if (dismissible && event.key === 'Escape') {
        event.preventDefault();
        onClose();
        return;
      }

      if (event.key !== 'Tab') {
        return;
      }

      const focusable = getFocusable(containerRef.current);
      if (focusable.length === 0) {
        event.preventDefault();
        containerRef.current?.focus();
        return;
      }

      const first = focusable[0];
      const last = focusable[focusable.length - 1];

      if (event.shiftKey) {
        if (document.activeElement === first || document.activeElement === containerRef.current) {
          event.preventDefault();
          last.focus();
        }
      } else if (document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    const handlePointerDown = (event: PointerEvent) => {
      if (!dismissible || !closeOnInteractOutside) {
        return;
      }

      if (!containerRef.current?.contains(event.target as Node)) {
        onClose();
      }
    };

    document.addEventListener('keydown', handleKeyDown);
    document.addEventListener('pointerdown', handlePointerDown);

    return () => {
      clearTimeout(timer);
      document.removeEventListener('keydown', handleKeyDown);
      document.removeEventListener('pointerdown', handlePointerDown);
      if (lockScroll) {
        document.body.style.overflow = previousOverflow;
      }
      previousFocusRef.current?.focus();
    };
  }, [closeOnInteractOutside, dismissible, initialFocusRef, lockScroll, onClose, open]);

  return { containerRef };
}
