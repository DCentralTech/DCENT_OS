import { useCallback, useEffect, useRef, useState } from 'react';
import { useDashboardHealth } from './useDashboardHealth';
import type { FaviconState } from '../utils/health';
import { useRewardFx } from '../fx/useRewardFx';

type FaviconRenderState = FaviconState | 'lucky';

const faviconCache = new Map<FaviconRenderState, string>();

function generateFaviconSvg(state: FaviconRenderState): string {
  const colors: Record<FaviconRenderState, { sphere: string; glow: string; ring?: string }> = {
    mining:  { sphere: '#22C55E', glow: '#22C55E' },
    warning: { sphere: '#EAB308', glow: '#EAB308' },
    error:   { sphere: '#EF4444', glow: '#EF4444' },
    standby: { sphere: '#f59e0b', glow: '#f59e0b' },
    lucky:   { sphere: '#FAA500', glow: '#FA6700', ring: '#FFC94D' },
  };
  const { sphere, glow, ring } = colors[state];

  // DCENT_OS chip icon favicon — matches the logo
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
    <defs>
      <radialGradient id="s" cx="40%" cy="32%" r="50%">
        <stop offset="0%" stop-color="${sphere}" stop-opacity="0.6"/>
        <stop offset="45%" stop-color="${sphere}"/>
        <stop offset="100%" stop-color="${glow}"/>
      </radialGradient>
    </defs>
    ${ring ? `<circle cx="16" cy="16" r="15" fill="none" stroke="${ring}" stroke-width="1.6"/>` : ''}
    <rect x="5" y="5" width="22" height="22" rx="1.5" fill="#161b22" stroke="#30363d" stroke-width="0.8"/>
    <circle cx="8" cy="8" r="1" fill="none" stroke="#374151" stroke-width="0.5" opacity="0.6"/>
    <g stroke="#4b5563" stroke-width="1.2" stroke-linecap="round">
      <line x1="1" y1="11" x2="5" y2="11"/>
      <line x1="1" y1="16" x2="5" y2="16"/>
      <line x1="1" y1="21" x2="5" y2="21"/>
      <line x1="27" y1="11" x2="31" y2="11"/>
      <line x1="27" y1="16" x2="31" y2="16"/>
      <line x1="27" y1="21" x2="31" y2="21"/>
      <line x1="11" y1="1" x2="11" y2="5"/>
      <line x1="16" y1="1" x2="16" y2="5"/>
      <line x1="21" y1="1" x2="21" y2="5"/>
      <line x1="11" y1="27" x2="11" y2="31"/>
      <line x1="16" y1="27" x2="16" y2="31"/>
      <line x1="21" y1="27" x2="21" y2="31"/>
    </g>
    <path d="M11,22 L11,16 L16,16" stroke="#1c2533" stroke-width="1" fill="none" stroke-linejoin="round"/>
    <path d="M16,16 L16,11 L21,11" stroke="#1c2533" stroke-width="1" fill="none" stroke-linejoin="round"/>
    <circle cx="10.5" cy="22.5" r="4" fill="url(#s)"/>
    <circle cx="16" cy="15.5" r="3.2" fill="url(#s)"/>
    <circle cx="21" cy="10.5" r="2.5" fill="url(#s)"/>
  </svg>`;

  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

function faviconDataUri(state: FaviconRenderState): string {
  const cached = faviconCache.get(state);
  if (cached) return cached;
  const next = generateFaviconSvg(state);
  faviconCache.set(state, next);
  return next;
}

export function useFavicon() {
  const health = useDashboardHealth();
  const [luckyActive, setLuckyActive] = useState(false);
  const luckyTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => () => {
    if (luckyTimerRef.current !== null) {
      globalThis.clearTimeout(luckyTimerRef.current);
    }
  }, []);

  useRewardFx(useCallback((event) => {
    if (event.kind !== 'lucky-share' || event.intensity <= 0) return;
    if (luckyTimerRef.current !== null) {
      globalThis.clearTimeout(luckyTimerRef.current);
    }
    setLuckyActive(true);
    luckyTimerRef.current = globalThis.setTimeout(() => {
      setLuckyActive(false);
      luckyTimerRef.current = null;
    }, 4000);
  }, []));

  useEffect(() => {
    const state: FaviconRenderState = luckyActive ? 'lucky' : health.faviconState;
    const dataUri = faviconDataUri(state);

    let link = document.querySelector<HTMLLinkElement>('link[rel="icon"]');
    if (!link) {
      link = document.createElement('link');
      link.rel = 'icon';
      document.head.appendChild(link);
    }
    link.type = 'image/svg+xml';
    link.href = dataUri;
  }, [health.faviconState, luckyActive]);
}
