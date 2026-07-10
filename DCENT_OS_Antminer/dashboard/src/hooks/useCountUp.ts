import { useEffect, useRef, useState } from 'react';

/**
 * Animated number-ticker hook.
 *
 * Animates from the previous `target` value to the new `target` over
 * `durationMs` (default 700ms) using easeOutQuad and requestAnimationFrame.
 * Returns the live, formatted display string.
 *
 * - Honors `prefers-reduced-motion`: jumps directly to the target with no
 *   intermediate frames. The first render always shows the target value
 *   (no animation from 0).
 * - Handles null/undefined/NaN inputs by returning the formatted fallback
 *   ("—" by default for missing data, "0" for legitimate zero numbers).
 *
 * @param target    Current numeric target (the value we are animating to).
 *                  Pass `null` / `undefined` for "no sample".
 * @param durationMs Animation duration in milliseconds. Default 700.
 * @param format    Optional formatter. Receives the live animated value
 *                  (a number) and returns a display string. Defaults to
 *                  `toLocaleString` on the rounded integer.
 *
 * @example
 *   const hr = useMinerStore(s => s.status?.hashrate_ghs ?? 0);
 *   const hrDisplay = useCountUp(hr, 700, (n) => n.toFixed(2));
 */
export function useCountUp(
  target: number | null | undefined,
  durationMs = 700,
  format?: (n: number) => string,
): string {
  // Resolve formatter (stable default).
  const fmt = format ?? ((n: number) => Math.round(n).toLocaleString());

  // Track the *displayed* numeric value. Animation tweens this from the
  // previous target to the new target whenever `target` changes.
  const safeTarget = (target == null || Number.isNaN(target as number))
    ? null
    : (target as number);

  const [display, setDisplay] = useState<number | null>(safeTarget);

  // Refs that persist across renders without triggering re-render churn.
  const fromRef = useRef<number>(safeTarget ?? 0);
  const toRef = useRef<number>(safeTarget ?? 0);
  const startRef = useRef<number>(0);
  const rafRef = useRef<number | null>(null);

  // Detect prefers-reduced-motion at hook level. We re-read it on every
  // animation start so OS-level changes are picked up between transitions.
  const prefersReducedMotion = (): boolean => {
    if (typeof window === 'undefined' || !window.matchMedia) return false;
    try {
      return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    } catch {
      return false;
    }
  };

  useEffect(() => {
    // Null / undefined target → freeze display at null (renders fallback).
    if (safeTarget == null) {
      if (rafRef.current != null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
      setDisplay(null);
      return;
    }

    // If we have no previous value, or the user prefers reduced motion,
    // jump straight to the target (no tween).
    const current = display ?? safeTarget;
    if (
      display == null ||
      prefersReducedMotion() ||
      durationMs <= 0 ||
      Math.abs(current - safeTarget) < 0.0001
    ) {
      if (rafRef.current != null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
      setDisplay(safeTarget);
      return;
    }

    fromRef.current = current;
    toRef.current = safeTarget;
    startRef.current = (typeof performance !== 'undefined' && performance.now)
      ? performance.now()
      : Date.now();

    const tick = (now: number) => {
      const elapsed = now - startRef.current;
      const t = Math.max(0, Math.min(1, elapsed / durationMs));
      // easeOutQuad: t * (2 - t)
      const eased = t * (2 - t);
      const value = fromRef.current + (toRef.current - fromRef.current) * eased;

      if (t >= 1) {
        setDisplay(toRef.current);
        rafRef.current = null;
        return;
      }
      setDisplay(value);
      rafRef.current = requestAnimationFrame(tick);
    };

    if (rafRef.current != null) cancelAnimationFrame(rafRef.current);
    rafRef.current = requestAnimationFrame(tick);

    return () => {
      if (rafRef.current != null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
    };
    // We intentionally exclude `display` from deps — we only want a new
    // animation when the *target* changes, not when the tween updates the
    // displayed value (that would re-arm endlessly).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [safeTarget, durationMs]);

  // Cleanup on unmount.
  useEffect(() => () => {
    if (rafRef.current != null) cancelAnimationFrame(rafRef.current);
  }, []);

  if (display == null) return '—';
  return fmt(display);
}
