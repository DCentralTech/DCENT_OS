/* ────────────────────────────────────────────────────────────────────────────
 * DCENT_OS — Tooltip primitive   ( keystone / Agent F1, 2026-05-17)
 *
 * The  mandate is "meaningful hover explanations / tooltips EVERYWHERE".
 * This file is the ONLY tooltip infrastructure. No external deps (project
 * allows react / react-dom / zustand only).
 *
 * ── TWO USAGE PATTERNS ──────────────────────────────────────────────────────
 *
 *  (1) CHEAP CSS PATH — for the thousands of short inline labels.
 *      Add `data-tooltip="..."` (+ optional `data-tooltip-pos="top|bottom|
 *      left|right"`) to ANY element. Zero JS, zero React, zero bundle cost
 *      per use. Styled by the `[data-tooltip]` rules in design-system.css.
 *      Pull canonical text from the glossary so the truth-contracts hold:
 *
 *        import { glossaryText } from '../../utils/glossary';
 *        <span data-tooltip={glossaryText('efficiency_jth')}>J/TH</span>
 *
 *      Use this for terse, single-paragraph hovers. It is NOT focus-trapping,
 *      shows on :hover AND :focus-within (keyboard), and is reduced-motion
 *      safe. (Pure CSS tooltips can't auto-flip — keep them short; use the
 *      React component near viewport edges or for rich content.)
 *
 *  (2) RICH REACT PATH — for multi-part / interactive / glossary content.
 *
 *        <Tooltip content="Plain explanation">  <button>?</button> </Tooltip>
 *        <Tooltip term="efficiency_jth"><span className="kpi-label">J/TH</span></Tooltip>
 *        <InfoDot term="pool_target_difficulty" />        // standalone glyph
 *        <InfoDot content={<>custom <b>JSX</b></>} label="More info" />
 *
 *      Accessible: trigger is focusable, `aria-describedby` wires the panel,
 *      ESC dismisses, opens on hover + focus, tap-toggles on touch, never
 *      traps focus, auto-flips to stay in viewport, no layout shift
 *      (portaled, fixed-positioned), respects prefers-reduced-motion.
 *
 * Phase-3 agents: do NOT invent another tooltip. Use one of the two paths.
 * ──────────────────────────────────────────────────────────────────────────── */

import {
  cloneElement,
  isValidElement,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactElement,
  type ReactNode,
} from 'react';
import { createPortal } from 'react-dom';
import { glossary, type GlossaryKey } from '../../utils/glossary';

type Placement = 'top' | 'bottom' | 'left' | 'right';

interface TooltipBaseProps {
  /** Rich content. Mutually-exclusive-ish with `term`; `content` wins. */
  content?: ReactNode;
  /** Pull canonical copy from the glossary (encodes the truth-contracts). */
  term?: GlossaryKey | string;
  /** Preferred side. Auto-flips if it would overflow the viewport. */
  placement?: Placement;
  /** Hover open delay (ms). Default 140. Focus/tap open immediately. */
  delay?: number;
  /** Disable the tooltip entirely (renders the child untouched). */
  disabled?: boolean;
  children: ReactNode;
}

const GAP = 8; // px between trigger and panel
const VIEWPORT_PAD = 8;
const HIDE_DELAY = 90; // ms grace before hide so the panel doesn't flicker out

function resolveContent(
  content: ReactNode | undefined,
  term: string | undefined,
): { node: ReactNode; heading?: string } {
  if (content != null && content !== false) return { node: content };
  if (term) {
    const e = glossary(term);
    if (e) return { node: e.note ? `${e.body} — ${e.note}` : e.body, heading: e.term };
  }
  return { node: null };
}

/**
 * Wrap any element. The child becomes the accessible trigger.
 * If the child is a single element it is cloned with the required a11y
 * props; otherwise it is wrapped in a focusable inline span.
 */
export function Tooltip({
  content,
  term,
  placement = 'top',
  delay = 140,
  disabled,
  children,
}: TooltipBaseProps) {
  const [open, setOpen] = useState(false);
  const [coords, setCoords] = useState<{ top: number; left: number; place: Placement } | null>(null);
  const triggerRef = useRef<HTMLElement | null>(null);
  const panelRef = useRef<HTMLDivElement | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const id = useId();
  const tipId = `tt-${id}`;

  const { node, heading } = resolveContent(content, term);
  const inert = disabled || node == null || node === '';

  const clearTimer = () => {
    if (timer.current) {
      clearTimeout(timer.current);
      timer.current = null;
    }
  };

  const show = useCallback((immediate = false) => {
    clearTimer();
    if (immediate || delay <= 0) {
      setOpen(true);
      return;
    }
    timer.current = setTimeout(() => setOpen(true), delay);
  }, [delay]);

  const hide = useCallback((immediate = false) => {
    clearTimer();
    if (immediate) {
      setOpen(false);
      return;
    }
    // Small grace period so moving the pointer across the tiny trigger→panel
    // gap (or a brief blur) doesn't flicker the tooltip closed.
    timer.current = setTimeout(() => setOpen(false), HIDE_DELAY);
  }, []);

  // ESC dismiss + outside-tap dismiss (touch) while open — both immediate.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') hide(true);
    };
    const onDocPointer = (e: PointerEvent) => {
      const t = e.target as Node;
      if (
        triggerRef.current && !triggerRef.current.contains(t) &&
        panelRef.current && !panelRef.current.contains(t)
      ) {
        hide(true);
      }
    };
    document.addEventListener('keydown', onKey);
    document.addEventListener('pointerdown', onDocPointer, true);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('pointerdown', onDocPointer, true);
    };
  }, [open, hide]);

  useEffect(() => () => clearTimer(), []);

  // Position + auto-flip. Measured after paint so the panel has real size.
  useLayoutEffect(() => {
    if (!open || !triggerRef.current || !panelRef.current) return;
    const tr = triggerRef.current.getBoundingClientRect();
    const pr = panelRef.current.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;

    const fits = (p: Placement): boolean => {
      if (p === 'top') return tr.top - pr.height - GAP >= VIEWPORT_PAD;
      if (p === 'bottom') return tr.bottom + pr.height + GAP <= vh - VIEWPORT_PAD;
      if (p === 'left') return tr.left - pr.width - GAP >= VIEWPORT_PAD;
      return tr.right + pr.width + GAP <= vw - VIEWPORT_PAD;
    };
    const opposite: Record<Placement, Placement> = {
      top: 'bottom', bottom: 'top', left: 'right', right: 'left',
    };
    let place: Placement = placement;
    if (!fits(place)) {
      if (fits(opposite[place])) place = opposite[place];
      else place = (['top', 'bottom', 'right', 'left'] as Placement[]).find(fits) ?? place;
    }

    let top: number;
    let left: number;
    if (place === 'top') {
      top = tr.top - pr.height - GAP;
      left = tr.left + tr.width / 2 - pr.width / 2;
    } else if (place === 'bottom') {
      top = tr.bottom + GAP;
      left = tr.left + tr.width / 2 - pr.width / 2;
    } else if (place === 'left') {
      top = tr.top + tr.height / 2 - pr.height / 2;
      left = tr.left - pr.width - GAP;
    } else {
      top = tr.top + tr.height / 2 - pr.height / 2;
      left = tr.right + GAP;
    }
    // Clamp into viewport.
    left = Math.max(VIEWPORT_PAD, Math.min(left, vw - pr.width - VIEWPORT_PAD));
    top = Math.max(VIEWPORT_PAD, Math.min(top, vh - pr.height - VIEWPORT_PAD));
    setCoords({ top, left, place });
  }, [open, placement, node]);

  if (inert || !isValidElement(children)) {
    // Nothing to explain, or children isn't a single element we can clone —
    // render children untouched (no wrapper div ⇒ no layout shift).
    return <>{children}</>;
  }

  const child = children as ReactElement<Record<string, unknown>>;
  const triggerProps: Record<string, unknown> = {
    ref: (el: HTMLElement | null) => {
      triggerRef.current = el;
      const r = (child as { ref?: unknown }).ref;
      if (typeof r === 'function') (r as (n: HTMLElement | null) => void)(el);
      else if (r && typeof r === 'object') (r as { current: unknown }).current = el;
    },
    'aria-describedby': open ? tipId : undefined,
    onMouseEnter: (e: React.MouseEvent) => {
      (child.props.onMouseEnter as ((e: React.MouseEvent) => void) | undefined)?.(e);
      show();
    },
    onMouseLeave: (e: React.MouseEvent) => {
      (child.props.onMouseLeave as ((e: React.MouseEvent) => void) | undefined)?.(e);
      hide();
    },
    onFocus: (e: React.FocusEvent) => {
      (child.props.onFocus as ((e: React.FocusEvent) => void) | undefined)?.(e);
      show(true);
    },
    onBlur: (e: React.FocusEvent) => {
      (child.props.onBlur as ((e: React.FocusEvent) => void) | undefined)?.(e);
      hide();
    },
    onClick: (e: React.MouseEvent) => {
      (child.props.onClick as ((e: React.MouseEvent) => void) | undefined)?.(e);
      // Touch / click toggles (so coarse pointers can open it).
      setOpen(o => !o);
    },
  };

  return (
    <>
      {cloneElement(child, triggerProps)}
      {open && coords != null && createPortal(
        <div
          ref={panelRef}
          id={tipId}
          role="tooltip"
          className={`dcent-tooltip dcm-tooltip-in dcent-tooltip--${coords.place}`}
          style={{ top: coords.top, left: coords.left } as CSSProperties}
        >
          {heading && <div className="dcent-tooltip__title">{heading}</div>}
          <div className="dcent-tooltip__body">{node}</div>
        </div>,
        document.body,
      )}
    </>
  );
}

interface InfoDotProps {
  term?: GlossaryKey | string;
  content?: ReactNode;
  /** Accessible label for the standalone glyph. Defaults to the glossary term. */
  label?: string;
  placement?: Placement;
  /** Visual size in px. Default 14. */
  size?: number;
}

/**
 * Standalone "ⓘ" affordance that pulls canonical glossary text. Use next to a
 * label/value that needs explaining when there's no obvious element to wrap.
 *
 *   <span className="kpi-label">Efficiency <InfoDot term="efficiency_jth" /></span>
 */
export function InfoDot({ term, content, label, placement = 'top', size = 14 }: InfoDotProps) {
  const entry = term ? glossary(term) : undefined;
  const a11y = label ?? entry?.term ?? 'More information';
  return (
    <Tooltip term={term} content={content} placement={placement}>
      <button
        type="button"
        className="dcent-infodot"
        aria-label={a11y}
        style={{ width: size, height: size, fontSize: Math.round(size * 0.72) }}
      >
        <span aria-hidden="true">i</span>
      </button>
    </Tooltip>
  );
}

export default Tooltip;
