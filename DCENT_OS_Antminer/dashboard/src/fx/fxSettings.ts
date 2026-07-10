export type VitalityPreference = 'full' | 'calm';

export interface FxSettings {
  enabled: boolean;
  vitality: VitalityPreference;
  titleTicker: boolean;
}
export interface FxSettingsStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export const FX_SETTINGS_KEY = 'dcentos-fx-settings';

export const DEFAULT_FX_SETTINGS: FxSettings = {
  enabled: true,
  vitality: 'full',
  titleTicker: true,
};

function getDefaultStorage(): FxSettingsStorage | null {
  try {
    return typeof window !== 'undefined' ? window.localStorage : null;
  } catch {
    return null;
  }
}

export function readFxSettings(storage: FxSettingsStorage | null = getDefaultStorage()): FxSettings {
  if (!storage) return DEFAULT_FX_SETTINGS;
  try {
    const raw = storage.getItem(FX_SETTINGS_KEY);
    if (!raw) return DEFAULT_FX_SETTINGS;
    const parsed = JSON.parse(raw) as Partial<FxSettings>;
    return {
      enabled: parsed.enabled !== false,
      vitality: parsed.vitality === 'calm' ? 'calm' : 'full',
      titleTicker: parsed.titleTicker !== false,
    };
  } catch {
    return DEFAULT_FX_SETTINGS;
  }
}

export function writeFxSettings(
  settings: Partial<FxSettings>,
  storage: FxSettingsStorage | null = getDefaultStorage(),
): FxSettings {
  const next = { ...readFxSettings(storage), ...settings };
  if (storage) {
    try {
      storage.setItem(FX_SETTINGS_KEY, JSON.stringify(next));
    } catch {
      // Storage failure keeps the in-memory value usable for this call.
    }
  }
  applyVitalityAttribute(next);
  return next;
}

export function applyVitalityAttribute(
  settings: FxSettings = readFxSettings(),
  doc: Document | undefined = globalThis.document,
): void {
  if (!doc) return;
  const root = doc.documentElement;
  if (settings.vitality === 'calm') {
    root.setAttribute('data-vitality', 'calm');
  } else {
    root.removeAttribute('data-vitality');
  }
}

export function initVitalityAttribute(
  doc: Document | undefined = globalThis.document,
  storage: FxSettingsStorage | null = getDefaultStorage(),
): () => void {
  if (!doc) return () => {};
  applyVitalityAttribute(readFxSettings(storage), doc);
  return () => {
    doc.documentElement.removeAttribute('data-vitality');
  };
}

export function initPageVisibilityAttribute(doc: Document | undefined = globalThis.document): () => void {
  if (!doc) return () => {};
  const root = doc.documentElement;
  const apply = () => {
    if (doc.hidden) {
      root.setAttribute('data-page-hidden', 'true');
    } else {
      root.removeAttribute('data-page-hidden');
    }
  };
  apply();
  doc.addEventListener('visibilitychange', apply);
  return () => {
    doc.removeEventListener('visibilitychange', apply);
    root.removeAttribute('data-page-hidden');
  };
}
