import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type React from 'react';

interface WindowedListOptions {
  count: number;
  itemHeight: number;
  overscan?: number;
  disabled?: boolean;
}

export interface WindowedListState<T extends HTMLElement = HTMLElement> {
  containerRef: React.RefObject<T>;
  start: number;
  end: number;
  padTop: number;
  padBottom: number;
  onScroll: (event: React.UIEvent<T>) => void;
}

export function useWindowedList<T extends HTMLElement = HTMLElement>({
  count,
  itemHeight,
  overscan = 8,
  disabled = false,
}: WindowedListOptions): WindowedListState<T> {
  const containerRef = useRef<T>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(0);
  const rafRef = useRef<number | null>(null);
  const latestScrollTopRef = useRef(0);

  const measure = useCallback(() => {
    setViewportHeight(containerRef.current?.clientHeight ?? 0);
  }, []);

  useLayoutEffect(() => {
    measure();
    const node = containerRef.current;
    if (!node || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [measure]);

  useEffect(() => () => {
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
    }
  }, []);

  const onScroll = useCallback((event: React.UIEvent<T>) => {
    if (disabled) return;
    latestScrollTopRef.current = event.currentTarget.scrollTop;
    if (rafRef.current !== null) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      setScrollTop(latestScrollTopRef.current);
    });
  }, [disabled]);

  return useMemo(() => {
    if (disabled || count <= 0 || itemHeight <= 0) {
      return {
        containerRef,
        start: 0,
        end: count,
        padTop: 0,
        padBottom: 0,
        onScroll,
      };
    }

    const visibleCount = Math.max(1, Math.ceil((viewportHeight || itemHeight * 12) / itemHeight));
    const rawStart = Math.floor(scrollTop / itemHeight) - overscan;
    const start = Math.max(0, Math.min(count, rawStart));
    const end = Math.max(start, Math.min(count, start + visibleCount + overscan * 2));
    return {
      containerRef,
      start,
      end,
      padTop: start * itemHeight,
      padBottom: Math.max(0, (count - end) * itemHeight),
      onScroll,
    };
  }, [count, disabled, itemHeight, onScroll, overscan, scrollTop, viewportHeight]);
}
