# DCENT_OS Dashboard

## Translations

English is the source language and the only locale currently marked as fully translated. The non-English picker locales currently cover the Settings and Tools strings that flow through `t`; the main dashboard still renders English copy by design, and the picker shows a coverage note for those locales.

Locale files live in `src/i18n/locales/`. Keep every locale key-parity-clean with `en.ts`; run `npm.cmd run i18n:parity` before committing translation edits. For a key-count report, run `npm.cmd run i18n:coverage`. That report is key coverage only, not a claim that the whole UI is localized.

To promote a locale to fully translated:

1. Translate every operator-facing dashboard surface, not only existing `t` keys.
2. Move the locale into `FULLY_TRANSLATED_LOCALES` in `src/i18n/i18n.tsx`.
3. Update `src/i18n/i18n.coverage.test.ts` expectations and keep the picker note absent only for fully translated locales.

`ru` is registered as a hidden English-fallthrough scaffold so existing stored preferences do not crash. Promote it only after real Russian strings are provided.
