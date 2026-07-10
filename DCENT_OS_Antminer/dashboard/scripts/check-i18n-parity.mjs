// G12 — i18n key-parity guard.
//
// Asserts every locale in src/i18n/locales/ exposes EXACTLY the same set
// of translation keys as the canonical English table (en.ts). A drifted
// or missing key silently degrades the UI to an en-fallback (or a raw
// key string) in production, so this runs as part of `npm run build`
// (same wiring as check-build-size.mjs) and fails the build on drift.
//
// Dependency-free on purpose (matches the dashboard's "no heavy
// libraries" i18n philosophy): the compact locale files use one shared
// `LOCALE_KEYS` table plus per-locale `defineLocale([...])` value arrays,
// so line-oriented extraction is sufficient and avoids needing a TS loader
// in the build pipeline. The legacy flat object shape is still accepted.
//
// `ru.ts` is the documented scaffold (`{ ...en }`) — it has no literal
// key lines of its own, so it is parity-checked structurally instead of
// by regex.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const localesDir = path.resolve(__dirname, '..', 'src', 'i18n', 'locales');

// Locales that are intentionally derived from en (scaffold via spread).
const SPREAD_SCAFFOLDS = new Set(['ru']);

const keyLineRe = /^\s*'([^']+)'\s*:/;
const quotedLineRe = /^\s*(["'])(.*)\1,?\s*$/;

function extractKeys(file) {
  const text = fs.readFileSync(file, 'utf8');
  const keys = [];
  for (const line of text.split(/\r?\n/)) {
    const m = keyLineRe.exec(line);
    if (m) keys.push(m[1]);
  }
  return keys;
}

function extractQuotedArray(file, marker) {
  const text = fs.readFileSync(file, 'utf8');
  const markerIndex = text.indexOf(marker);
  if (markerIndex < 0) return [];
  const start = text.indexOf('[', markerIndex);
  const end = text.indexOf(']', start);
  if (start < 0 || end < 0) return [];
  const values = [];
  for (const line of text.slice(start + 1, end).split(/\r?\n/)) {
    const m = quotedLineRe.exec(line.trim());
    if (m) {
      try {
        values.push(JSON.parse(`${m[1]}${m[2]}${m[1]}`));
      } catch {
        values.push(m[2]);
      }
    }
  }
  return values;
}

function fail(msg) {
  console.error(`[i18n:parity] ${msg}`);
  process.exit(1);
}

if (!fs.existsSync(localesDir)) {
  fail(`locales dir missing: ${localesDir}`);
}

const localeFiles = fs
  .readdirSync(localesDir)
  .filter((f) => f.endsWith('.ts') && f !== 'table.ts')
  .sort();
const coverageMode = process.argv.includes('--coverage');

if (!localeFiles.includes('en.ts')) {
  fail('en.ts (canonical) not found');
}

const tableFile = path.join(localesDir, 'table.ts');
const tableKeys = fs.existsSync(tableFile) ? extractQuotedArray(tableFile, 'LOCALE_KEYS') : [];
const enKeys = tableKeys.length > 0 ? tableKeys : extractKeys(path.join(localesDir, 'en.ts'));
const enKeySet = new Set(enKeys);

// Duplicate-key guard on the canonical locale.
if (enKeySet.size !== enKeys.length) {
  const seen = new Set();
  const dupes = enKeys.filter((k) => (seen.has(k) ? true : (seen.add(k), false)));
  fail(`en.ts has duplicate keys: ${[...new Set(dupes)].join(', ')}`);
}

let problems = 0;
const summary = [];
const coverageRows = [];

for (const file of localeFiles) {
  const name = path.basename(file, '.ts');
  if (name === 'en') {
    const enValues = extractQuotedArray(path.join(localesDir, file), 'defineLocale');
    if (tableKeys.length > 0 && enValues.length !== enKeys.length) {
      console.error(`[i18n:parity] en.ts: value count ${enValues.length} does not match LOCALE_KEYS ${enKeys.length}`);
      problems++;
    }
    summary.push(`en: ${enKeys.length} keys (canonical)`);
    coverageRows.push({
      locale: name,
      kind: 'source',
      keys: enKeys.length,
      translatedKeys: enKeys.length,
    });
    continue;
  }

  if (SPREAD_SCAFFOLDS.has(name)) {
    // Scaffold: must spread `en` so it is parity-correct by construction.
    const text = fs.readFileSync(path.join(localesDir, file), 'utf8');
    const ownKeys = extractKeys(path.join(localesDir, file));
    if (!/\.\.\.\s*en\b/.test(text)) {
      console.error(
        `[i18n:parity] ${file}: scaffold locale must spread \`{ ...en }\` ` +
          `to guarantee key parity (or be promoted to a full locale).`,
      );
      problems++;
    }
    // A scaffold may also override individual keys; any literal key it
    // does declare must still exist in en.
    const strayInScaffold = ownKeys.filter((k) => !enKeySet.has(k));
    if (strayInScaffold.length) {
      console.error(`[i18n:parity] ${file}: keys not present in en: ${strayInScaffold.join(', ')}`);
      problems++;
    }
    summary.push(`${name}: scaffold ({ ...en } + ${ownKeys.length} override(s))`);
    coverageRows.push({
      locale: name,
      kind: 'scaffold',
      keys: enKeys.length,
      translatedKeys: ownKeys.filter((k) => enKeySet.has(k)).length,
    });
    continue;
  }

  const localePath = path.join(localesDir, file);
  const compactValues = extractQuotedArray(localePath, 'defineLocale');
  const keys = tableKeys.length > 0 && compactValues.length > 0
    ? (compactValues.length === enKeys.length ? enKeys : [])
    : extractKeys(localePath);
  const keySet = new Set(keys);

  if (tableKeys.length > 0 && compactValues.length > 0 && compactValues.length !== enKeys.length) {
    console.error(`[i18n:parity] ${file}: value count ${compactValues.length} does not match LOCALE_KEYS ${enKeys.length}`);
    problems++;
  }

  if (keySet.size !== keys.length) {
    const seen = new Set();
    const dupes = keys.filter((k) => (seen.has(k) ? true : (seen.add(k), false)));
    console.error(`[i18n:parity] ${file}: duplicate keys: ${[...new Set(dupes)].join(', ')}`);
    problems++;
  }

  const missing = enKeys.filter((k) => !keySet.has(k));
  const extra = keys.filter((k) => !enKeySet.has(k));

  if (missing.length) {
    console.error(`[i18n:parity] ${file}: MISSING ${missing.length} key(s): ${missing.join(', ')}`);
    problems++;
  }
  if (extra.length) {
    console.error(`[i18n:parity] ${file}: EXTRA ${extra.length} key(s) not in en: ${extra.join(', ')}`);
    problems++;
  }
  if (!missing.length && !extra.length && keySet.size === keys.length) {
    summary.push(`${name}: ${keys.length} keys OK`);
  } else {
    summary.push(`${name}: DRIFT`);
  }
  coverageRows.push({
    locale: name,
    kind: 'locale',
    keys: enKeys.length,
    translatedKeys: keys.filter((k) => enKeySet.has(k)).length,
  });
}

console.log(`[i18n:parity] ${localeFiles.length} locale file(s) checked`);
for (const s of summary) console.log(`  - ${s}`);

if (coverageMode) {
  console.log('[i18n:coverage] Key coverage only; UI coverage scope is Settings and Tools');
  for (const row of coverageRows) {
    const percent = row.keys > 0 ? (row.translatedKeys / row.keys) * 100 : 0;
    console.log(
      `  - ${row.locale}: ${row.translatedKeys}/${row.keys} keys ` +
        `(${percent.toFixed(1)}%, ${row.kind})`,
    );
  }
}

if (problems > 0) {
  fail(`${problems} parity problem(s) — locales must have identical key sets to en.ts`);
}
console.log('[i18n:parity] OK — all locales key-parity-clean');
