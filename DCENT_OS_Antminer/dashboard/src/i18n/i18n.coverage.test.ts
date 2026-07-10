// @vitest-environment jsdom

import { afterEach, describe, it, expect } from 'vitest';
import React from 'react';
import { cleanup, render, screen } from '@testing-library/react';
import {
  AVAILABLE_LOCALES,
  FULLY_TRANSLATED_LOCALES,
  I18nProvider,
  LanguageSelector,
  TRANSLATION_COVERAGE_SCOPE,
  isFullyTranslated,
} from './i18n';

// F002: the LanguageSelector offers en/fr/es/zh, but only the Settings + Tools
// strings flow through t() today. These pure helpers drive the honest "partial
// translation" note so non-English locales never imply full localization.

describe('translation coverage (F002)', () => {
  afterEach(() => {
    cleanup();
    localStorage.clear();
  });

  it('treats English as the only fully-translated locale', () => {
    expect(isFullyTranslated('en')).toBe(true);
    expect(FULLY_TRANSLATED_LOCALES).toEqual(['en']);
  });

  it('only marks locales that are offered in the picker as fully translated', () => {
    for (const loc of FULLY_TRANSLATED_LOCALES) {
      expect(AVAILABLE_LOCALES).toContain(loc);
    }
  });

  it('flags every other offered locale as partial (note must show)', () => {
    for (const loc of AVAILABLE_LOCALES) {
      if (loc === 'en') continue;
      expect(isFullyTranslated(loc)).toBe(false);
    }
    // fr/es/zh are the partial locales offered in the picker today.
    expect(isFullyTranslated('fr')).toBe(false);
    expect(isFullyTranslated('es')).toBe(false);
    expect(isFullyTranslated('zh')).toBe(false);
  });

  it('renders the coverage note for every offered non-full locale', () => {
    for (const loc of AVAILABLE_LOCALES) {
      cleanup();
      localStorage.setItem('dcentos-locale', loc);
      render(React.createElement(
        I18nProvider,
        null,
        React.createElement(LanguageSelector),
      ));

      if (FULLY_TRANSLATED_LOCALES.includes(loc)) {
        expect(screen.queryByRole('note')).toBeNull();
      } else {
        const note = screen.getByRole('note');
        expect(note.textContent?.trim().length).toBeGreaterThan(0);
      }
    }
  });

  it('exposes a human-readable coverage scope for the note', () => {
    expect(TRANSLATION_COVERAGE_SCOPE).toBe('Settings and Tools');
  });
});
