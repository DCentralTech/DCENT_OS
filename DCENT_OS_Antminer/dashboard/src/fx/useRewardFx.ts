import { useCallback, useEffect, useRef, useState } from 'react';
import { rewardBus, type FxEvent } from './rewardBus';

export function useRewardFx(handler: (event: FxEvent) => void): void {
  useEffect(() => rewardBus.subscribe(handler), [handler]);
}

export function useFxPulse(durationMs: number): readonly [boolean, () => void] {
  const [active, setActive] = useState(false);
  const timersRef = useRef<number[]>([]);

  useEffect(() => () => {
    for (const timer of timersRef.current) {
      window.clearTimeout(timer);
    }
    timersRef.current = [];
  }, []);

  const pulse = useCallback(() => {
    for (const timer of timersRef.current) {
      window.clearTimeout(timer);
    }
    timersRef.current = [];
    setActive(false);

    const start = window.setTimeout(() => setActive(true), 0);
    const stop = window.setTimeout(() => setActive(false), durationMs);
    timersRef.current = [start, stop];
  }, [durationMs]);

  return [active, pulse] as const;
}
