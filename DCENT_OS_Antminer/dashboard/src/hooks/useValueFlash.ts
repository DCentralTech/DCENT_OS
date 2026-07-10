import { useEffect, useRef, useState } from 'react';

/**
 * Returns a className string ("ds-value-flash" + custom) that animates once
 * whenever `value` changes to a new non-null, non-equal reading. Used for
 * KPI / telemetry numbers that update live so the operator gets a subtle
 * visual confirmation a fresh sample landed.
 *
 * The CSS animation is defined in `styles/design-system.css` (ds-valueFlash
 * keyframe). Honors prefers-reduced-motion via the global guard there.
 */
export function useValueFlash(value: number | string | null | undefined, durationMs = 700): string {
  const previous = useRef(value);
  const [flashing, setFlashing] = useState(false);
  const timer = useRef<number | null>(null);

  useEffect(() => {
    if (previous.current !== value && previous.current != null && value != null) {
      setFlashing(false);
      // Force a microtask gap so the class actually re-applies and re-triggers the animation.
      const id = window.setTimeout(() => {
        setFlashing(true);
        if (timer.current) window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => setFlashing(false), durationMs);
      }, 16);
      previous.current = value;
      return () => window.clearTimeout(id);
    }
    previous.current = value;
    return undefined;
  }, [value, durationMs]);

  useEffect(() => () => {
    if (timer.current) window.clearTimeout(timer.current);
  }, []);

  return flashing ? 'ds-value-flash' : '';
}
