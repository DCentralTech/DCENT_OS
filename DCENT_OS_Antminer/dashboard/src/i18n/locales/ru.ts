// DCENTos Russian locale (Русский) — SCAFFOLD (G12).
//
// RU is a registered locale so the switcher, loader, and storage round-
// trip already work for Russian-market operators, but the strings are
// NOT yet translated: this file deliberately re-exports the English
// table so every key resolves (no missing-key fallbacks at runtime) and
// the i18n key-parity test passes. Replace `en` below key-by-key with
// Russian translations to promote RU from scaffold to a full locale —
// no other file needs to change (i18n.tsx already wires `ru`).
//
// Until then `t()` returns English for `ru`, which is the same behaviour
// as the en-fallback in i18n.tsx — explicit here so the intent is
// auditable rather than implicit.

import en from './en';

const ru: Record<string, string> = { ...en };

export default ru;
