import React, { createContext, useContext, useEffect, useMemo, useRef, useState } from 'react';
import type { WsMessage } from '../api/types';
import { wsManager } from '../api/websocket';
import { useFlightRecorder } from './useFlightRecorder';
import { useMinerStore } from '../store/miner';

export type PatchBayEventId =
  | 'share_accepted'
  | 'lucky_share'
  | 'share_rejected'
  | 'clean_job'
  | 'warn_log'
  | 'error_log'
  | 'hot_temp';

type PatchBayEffectId = 'toast' | 'marker' | 'pulse' | 'beep' | 'freeze';

interface PatchBayRule {
  enabled: boolean;
  effects: Record<PatchBayEffectId, boolean>;
}

interface PatchBayEventDefinition {
  id: PatchBayEventId;
  label: string;
  description: string;
  cooldownMs: number;
  defaultRule: PatchBayRule;
}

interface PatchBayContextValue {
  rules: Record<PatchBayEventId, PatchBayRule>;
  definitions: PatchBayEventDefinition[];
  updateRule: (eventId: PatchBayEventId, next: PatchBayRule) => void;
  triggerTest: (eventId: PatchBayEventId) => void;
}

const STORAGE_KEY = 'dcentos-patch-bay-rules';
const PULSE_DURATION_MS = 900;
const HOT_TEMP_TRIGGER_C = 65;
const HOT_TEMP_CLEAR_C = 62;

const DEFINITIONS: PatchBayEventDefinition[] = [
  {
    id: 'share_accepted',
    label: 'Share Accepted',
    description: 'Pool accepted work from this miner.',
    cooldownMs: 12000,
    defaultRule: {
      enabled: false,
      effects: { toast: false, marker: false, pulse: false, beep: false, freeze: false },
    },
  },
  {
    id: 'lucky_share',
    label: 'Lucky Share',
    description: 'A high-difficulty accepted share worth celebrating.',
    cooldownMs: 3000,
    defaultRule: {
      enabled: true,
      effects: { toast: true, marker: true, pulse: true, beep: true, freeze: false },
    },
  },
  {
    id: 'share_rejected',
    label: 'Share Rejected',
    description: 'Pool rejected a submitted share.',
    cooldownMs: 5000,
    defaultRule: {
      enabled: true,
      effects: { toast: true, marker: false, pulse: true, beep: false, freeze: false },
    },
  },
  {
    id: 'clean_job',
    label: 'Clean Job / New Block',
    description: 'Pool invalidated previous work and issued a new block template.',
    cooldownMs: 3000,
    defaultRule: {
      enabled: true,
      effects: { toast: false, marker: true, pulse: true, beep: false, freeze: false },
    },
  },
  {
    id: 'warn_log',
    label: 'Warning Log',
    description: 'Runtime emitted a warning log event.',
    cooldownMs: 5000,
    defaultRule: {
      enabled: false,
      effects: { toast: true, marker: false, pulse: false, beep: false, freeze: false },
    },
  },
  {
    id: 'error_log',
    label: 'Error Log',
    description: 'Runtime emitted an error log event.',
    cooldownMs: 5000,
    defaultRule: {
      enabled: true,
      effects: { toast: true, marker: true, pulse: true, beep: true, freeze: false },
    },
  },
  {
    id: 'hot_temp',
    label: 'Hot Temperature',
    description: 'Any active chain temperature crossed the hot threshold.',
    cooldownMs: 30000,
    defaultRule: {
      enabled: true,
      effects: { toast: true, marker: true, pulse: true, beep: false, freeze: false },
    },
  },
];

const PatchBayContext = createContext<PatchBayContextValue>({
  rules: DEFINITIONS.reduce((acc, definition) => ({ ...acc, [definition.id]: definition.defaultRule }), {} as Record<PatchBayEventId, PatchBayRule>),
  definitions: DEFINITIONS,
  updateRule: () => {},
  triggerTest: () => {},
});

function loadRules() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const parsed = raw ? JSON.parse(raw) as Partial<Record<PatchBayEventId, PatchBayRule>> : {};
    return DEFINITIONS.reduce((acc, definition) => {
      acc[definition.id] = {
        ...definition.defaultRule,
        ...parsed[definition.id],
        effects: {
          ...definition.defaultRule.effects,
          ...(parsed[definition.id]?.effects ?? {}),
        },
      };
      return acc;
    }, {} as Record<PatchBayEventId, PatchBayRule>);
  } catch {
    return DEFINITIONS.reduce((acc, definition) => {
      acc[definition.id] = definition.defaultRule;
      return acc;
    }, {} as Record<PatchBayEventId, PatchBayRule>);
  }
}

function getAudioContextCtor(): (new () => AudioContext) | null {
  const maybeWindow = window as Window & typeof globalThis & { webkitAudioContext?: new () => AudioContext };
  return maybeWindow.AudioContext ?? maybeWindow.webkitAudioContext ?? null;
}

async function playBeep(contextRef: React.MutableRefObject<AudioContext | null>, frequency: number) {
  try {
    const AudioContextCtor = getAudioContextCtor();
    if (!AudioContextCtor) {
      return;
    }

    if (!contextRef.current || contextRef.current.state === 'closed') {
      contextRef.current = new AudioContextCtor();
    }

    const ctx = contextRef.current;
    let ctxState = ctx.state;
    if (ctxState === 'suspended') {
      await ctx.resume().catch(() => {});
      ctxState = ctx.state;
    }
    if (ctxState !== 'running') {
      return;
    }

    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.frequency.value = frequency;
    gain.gain.value = 0.05;
    osc.connect(gain);
    gain.connect(ctx.destination);
    osc.start();
    setTimeout(() => {
      osc.stop();
    }, 160);
  } catch {
    // ignore browser audio failures
  }
}

export function PatchBayProvider({ children }: { children: React.ReactNode }) {
  const status = useMinerStore(s => s.status);
  const addToast = useMinerStore(s => s.addToast);
  const { addMarker, freeze, recordAction } = useFlightRecorder();
  const [rules, setRules] = useState<Record<PatchBayEventId, PatchBayRule>>(() => loadRules());
  const [pulseColor, setPulseColor] = useState<string | null>(null);
  const pulseTimerRef = useRef<number | null>(null);
  const lastTriggerRef = useRef<Record<PatchBayEventId, number>>({
    share_accepted: 0,
    lucky_share: 0,
    share_rejected: 0,
    clean_job: 0,
    warn_log: 0,
    error_log: 0,
    hot_temp: 0,
  });
  const hotTempActiveRef = useRef(false);
  const audioContextRef = useRef<AudioContext | null>(null);

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(rules));
  }, [rules]);

  const applyEvent = (
    eventId: PatchBayEventId,
    message: string,
    bypassCooldown = false,
    forceEnabled = false,
  ) => {
    const definition = DEFINITIONS.find(item => item.id === eventId);
    const rule = rules[eventId];
    if (!definition || (!forceEnabled && !rule?.enabled)) {
      return;
    }

    const now = Date.now();
    if (!bypassCooldown && now - lastTriggerRef.current[eventId] < definition.cooldownMs) {
      return;
    }
    lastTriggerRef.current[eventId] = now;

    recordAction('patch_bay_event', { eventId, message, bypassCooldown, forceEnabled });

    if (rule.effects.toast) {
      addToast(message, eventId === 'share_rejected' || eventId === 'error_log' ? 'warning' : 'info');
    }
    if (rule.effects.marker) {
      addMarker(`patch-bay: ${message}`);
    }
    if (rule.effects.pulse) {
      if (pulseTimerRef.current) {
        window.clearTimeout(pulseTimerRef.current);
      }
      setPulseColor(
        eventId === 'share_rejected' || eventId === 'error_log'
          ? 'rgba(239, 68, 68, 0.26)'
          : eventId === 'lucky_share'
            ? 'rgba(250, 165, 0, 0.24)'
            : 'rgba(0, 255, 65, 0.18)',
      );
      pulseTimerRef.current = window.setTimeout(() => {
        setPulseColor(null);
        pulseTimerRef.current = null;
      }, PULSE_DURATION_MS);
    }
    if (rule.effects.beep) {
      void playBeep(audioContextRef, eventId === 'lucky_share' ? 1320 : eventId === 'share_rejected' || eventId === 'error_log' ? 420 : 880);
    }
    if (rule.effects.freeze) {
      freeze();
    }
  };

  useEffect(() => {
    return wsManager.subscribe((message: WsMessage) => {
      if (message.type === 'mining_sync') {
        if (message.event === 'share_accepted') {
          applyEvent('share_accepted', `Share accepted at diff ${message.difficulty?.toFixed(0) ?? '?'}`);
        }
        if (message.event === 'lucky_share') {
          applyEvent('lucky_share', `Lucky share at diff ${message.difficulty?.toFixed(0) ?? '?'}`);
        }
        if (message.event === 'share_rejected') {
          applyEvent('share_rejected', message.error_msg || 'Share rejected by pool');
        }
        if (message.event === 'clean_job') {
          applyEvent('clean_job', `New block clean job for ${message.job_id ?? 'unknown'}`);
        }
      }

      if (message.type === 'log') {
        if (message.level === 'warn') {
          applyEvent('warn_log', message.message);
        }
        if (message.level === 'error') {
          applyEvent('error_log', message.message);
        }
      }
    });
  }, [rules]);

  useEffect(() => {
    const hottest = Math.max(...(status?.chains ?? []).map(chain => chain.temp_c), 0);
    if (!hotTempActiveRef.current && hottest >= HOT_TEMP_TRIGGER_C) {
      hotTempActiveRef.current = true;
      applyEvent('hot_temp', `Hot chain detected at ${hottest.toFixed(1)} C`);
    } else if (hotTempActiveRef.current && hottest <= HOT_TEMP_CLEAR_C) {
      hotTempActiveRef.current = false;
    }
  }, [status]);

  useEffect(() => () => {
    if (pulseTimerRef.current) {
      window.clearTimeout(pulseTimerRef.current);
    }
    void audioContextRef.current?.close().catch(() => {});
  }, []);

  const value = useMemo<PatchBayContextValue>(() => ({
    rules,
    definitions: DEFINITIONS,
    updateRule: (eventId, next) => {
      setRules(prev => ({ ...prev, [eventId]: next }));
      recordAction('patch_bay_rule_updated', { eventId, next });
    },
    triggerTest: eventId => {
      const definition = DEFINITIONS.find(item => item.id === eventId);
      applyEvent(eventId, `Test pulse for ${definition?.label ?? eventId}`, true, true);
    },
  }), [rules, recordAction]);

  return React.createElement(
    PatchBayContext.Provider,
    { value },
    <>
      {children}
      {pulseColor && (
        <div
          aria-hidden="true"
          style={{
            pointerEvents: 'none',
            position: 'fixed',
            inset: 0,
            zIndex: 9998,
            background: pulseColor,
            boxShadow: `inset 0 0 0 2px ${pulseColor}`,
            animation: 'fadeIn 0.12s ease-out',
          }}
        />
      )}
    </>,
  );
}

export function usePatchBay() {
  return useContext(PatchBayContext);
}
