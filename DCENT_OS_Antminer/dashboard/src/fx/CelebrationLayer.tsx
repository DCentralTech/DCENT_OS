import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { BestDifficultyStore, isFiniteDifficulty } from './bestDifficulty';
import { useRewardFx } from './useRewardFx';
import type { FxEvent } from './rewardBus';
import { useMinerStore } from '../store/miner';

type MomentKind = 'lucky' | 'first' | 'record';

interface Moment {
  id: string;
  kind: MomentKind;
  title: string;
  detail: string | null;
}

const MOMENT_MS = 2200;
const FIRST_SHARE_MS = 6500;
const PARTICLES = Array.from({ length: 12 }, (_, index) => index);

function formatDifficulty(value: number | undefined): string | null {
  if (!isFiniteDifficulty(value)) return null;
  return new Intl.NumberFormat('en-US', {
    maximumFractionDigits: value >= 100 ? 0 : 2,
  }).format(value);
}

function difficultyDetail(event: FxEvent): string | null {
  const achieved = formatDifficulty(event.difficulty);
  if (!achieved) return null;
  const target = formatDifficulty(event.targetDifficulty);
  return target ? `${achieved} achieved / ${target} target` : `${achieved} achieved`;
}

function makeId(kind: MomentKind, at: number): string {
  return `${kind}-${Math.round(at)}-${Math.random().toString(36).slice(2)}`;
}

export function CelebrationLayer() {
  const bestDifficulty = useMemo(() => new BestDifficultyStore(), []);
  const mode = useMinerStore(s => s.mode);
  const [mounted, setMounted] = useState(false);
  const [moments, setMoments] = useState<Moment[]>([]);
  const [recordDifficulty, setRecordDifficulty] = useState(() => bestDifficulty.read()?.value ?? null);
  const timersRef = useRef<number[]>([]);

  useEffect(() => {
    setMounted(true);
    return () => {
      for (const timer of timersRef.current) {
        window.clearTimeout(timer);
      }
      timersRef.current = [];
    };
  }, []);

  const pushMoment = useCallback((moment: Moment, ttlMs: number) => {
    setMoments(current => [...current.slice(-1), moment]);
    const timer = window.setTimeout(() => {
      setMoments(current => current.filter(item => item.id !== moment.id));
    }, ttlMs);
    timersRef.current.push(timer);
  }, []);

  const handleEvent = useCallback((event: FxEvent) => {
    if (event.kind === 'best-difficulty' && isFiniteDifficulty(event.difficulty)) {
      setRecordDifficulty(event.difficulty);
      if (event.intensity > 0) {
        pushMoment({
          id: makeId('record', event.at),
          kind: 'record',
          title: 'New session best',
          detail: difficultyDetail(event),
        }, MOMENT_MS);
      }
      return;
    }

    if (event.intensity <= 0) return;

    if (event.kind === 'lucky-share') {
      pushMoment({
        id: makeId('lucky', event.at),
        kind: 'lucky',
        title: 'Lucky share',
        detail: difficultyDetail(event),
      }, MOMENT_MS);
      return;
    }

    if (event.kind === 'first-share') {
      pushMoment({
        id: makeId('first', event.at),
        kind: 'first',
        title: 'First share accepted this session',
        detail: difficultyDetail(event),
      }, FIRST_SHARE_MS);
    }
  }, [pushMoment]);

  useRewardFx(handleEvent);

  if (!mounted) return null;

  const recordText = formatDifficulty(recordDifficulty ?? undefined);
  const modeClass = mode === 'heater' ? 'mode-basic' : mode === 'hacker' ? 'mode-hacker' : 'mode-standard';

  return createPortal(
    <>
      <div className={`dcfx-layer ${modeClass}`} data-testid="dcfx-layer">
        {moments.map(moment => (
          <div
            key={moment.id}
            className={`dcfx-moment dcfx-moment-${moment.kind}`}
            role="status"
            aria-live="polite"
          >
            <div className="dcfx-moment-title">{moment.title}</div>
            {moment.detail && <div className="dcfx-moment-detail">{moment.detail}</div>}
            {moment.kind === 'lucky' && (
              <div className="dcfx-particles" aria-hidden="true">
                {PARTICLES.map(index => <span key={index} className="dcfx-dot" />)}
              </div>
            )}
          </div>
        ))}
      </div>
      {recordText && (
        <div className={`dcfx-record-caption ${modeClass}`} role="status" aria-live="polite">
          Session best: {recordText}
        </div>
      )}
    </>,
    document.body,
  );
}
