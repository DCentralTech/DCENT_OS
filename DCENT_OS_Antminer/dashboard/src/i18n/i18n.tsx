// DCENTos i18n — Lightweight internationalization (no heavy libraries)
// Simple key-value lookup with React context

import React, { createContext, useContext, useState, useCallback, type ReactNode } from 'react';
import en from './locales/en';
import fr from './locales/fr';
import es from './locales/es';
import zh from './locales/zh';
import ru from './locales/ru';

// English is the source locale. FR/ES/ZH expose the current Settings and
// Tools translation surface; the rest of the dashboard remains English.
// RU is registered as an English-fallthrough scaffold; see locales/ru.ts.
export type Locale = 'en' | 'fr' | 'es' | 'zh' | 'ru';

// All locales registered in the codebase (loadable + key-parity-checked by
// scripts/check-i18n-parity.mjs). `ru` stays registered so a previously
// stored 'ru' value still round-trips harmlessly to the English-fallthrough
// scaffold rather than crashing.
export const SUPPORTED_LOCALES: readonly Locale[] = ['en', 'fr', 'es', 'zh', 'ru'] as const;

// Omega P3-5: locales actually OFFERED to the operator (the language picker +
// browser auto-detect). `ru` is a 832-byte English-fallthrough scaffold (not a
// real translation yet), so it is intentionally HIDDEN here until it is
// translated — surfacing an untranslated language as selectable reads as
// unfinished. Promote `ru` by moving it into this list once locales/ru.ts
// carries real Russian strings.
export const AVAILABLE_LOCALES: readonly Locale[] = ['en', 'fr', 'es', 'zh'] as const;

// Native language labels for the switcher. These are intentionally NOT
// translation keys — a language name is shown in its own language, so it
// must not change with the active locale (and keeps the locale key sets
// identical, which the parity test enforces).
export const LOCALE_LABELS: Record<Locale, string> = {
  en: 'English',
  fr: 'Français',
  es: 'Español',
  zh: '简体中文',
  ru: 'Русский',
};

// F002 honesty: the picker offers en/fr/es/zh, but only a subset of the UI
// (the Settings + Tools strings that flow through t()) is actually translated
// today — the primary dashboard renders hardcoded English. English is the
// source language, so it is the only fully-covered locale; fr/es/zh are
// partial and ru is an English-fallthrough scaffold. These drive the
// LanguageSelector's "partial translation" note so the UI never implies that
// switching locale fully localizes the app.
export const FULLY_TRANSLATED_LOCALES: readonly Locale[] = ['en'];

// Human-readable scope of what is translated today (shown in the partial note).
export const TRANSLATION_COVERAGE_SCOPE = 'Settings and Tools';

// True only when the active locale needs no "partial translation" note.
export function isFullyTranslated(locale: Locale): boolean {
  return FULLY_TRANSLATED_LOCALES.includes(locale);
}

const locales: Record<Locale, Record<string, string>> = { en, fr, es, zh, ru };

function isLocale(value: unknown): value is Locale {
  return typeof value === 'string' && (SUPPORTED_LOCALES as readonly string[]).includes(value);
}

interface I18nContextValue {
  locale: Locale;
  setLocale: (locale: Locale) => void;
  t: (key: string, fallback?: string) => string;
}

const I18nContext = createContext<I18nContextValue>({
  locale: 'en',
  setLocale: () => {},
  t: (key: string) => key,
});

const STORAGE_KEY = 'dcentos-locale';

function loadLocale(): Locale {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (isLocale(stored)) return stored;
  } catch { /* ignore */ }
  // Auto-detect from browser (longest-prefix match against the OFFERED set —
  // never auto-select an untranslated scaffold like `ru`).
  try {
    const browserLang = navigator.language.toLowerCase();
    for (const loc of AVAILABLE_LOCALES) {
      if (browserLang === loc || browserLang.startsWith(loc + '-')) return loc;
    }
    // zh-CN / zh-Hans / zh-TW etc. all map to our zh
    if (browserLang.startsWith('zh')) return 'zh';
  } catch { /* ignore */ }
  return 'en';
}

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>(loadLocale);

  const setLocale = useCallback((newLocale: Locale) => {
    setLocaleState(newLocale);
    try {
      localStorage.setItem(STORAGE_KEY, newLocale);
    } catch { /* ignore */ }
  }, []);

  const t = useCallback((key: string, fallback?: string): string => {
    return locales[locale]?.[key] ?? locales.en[key] ?? fallback ?? key;
  }, [locale]);

  return (
    <I18nContext.Provider value={{ locale, setLocale, t }}>
      {children}
    </I18nContext.Provider>
  );
}

export function useTranslation() {
  return useContext(I18nContext);
}

// Language selector component
export function LanguageSelector() {
  const { locale, setLocale, t } = useTranslation();

  return (
    <div className="feat-language-selector">
      <label className="feat-label">{t('lang.title')}</label>
      <div className="feat-lang-buttons">
        {AVAILABLE_LOCALES.map((loc) => (
          <button
            key={loc}
            className={`feat-lang-btn ${locale === loc ? 'active' : ''}`}
            onClick={() => setLocale(loc)}
            lang={loc}
            aria-pressed={locale === loc}
          >
            {LOCALE_LABELS[loc]}
          </button>
        ))}
      </div>
      {!isFullyTranslated(locale) && (
        <span
          className="feat-hint feat-lang-coverage"
          role="note"
          title={t(
            'lang.coverage',
            `Translation currently covers ${TRANSLATION_COVERAGE_SCOPE} only — the main dashboard stays in English for now.`,
          )}
        >
          {t(
            'lang.coverage.short',
            `Partial — ${TRANSLATION_COVERAGE_SCOPE} only; dashboard is English for now`,
          )}
        </span>
      )}
    </div>
  );
}
