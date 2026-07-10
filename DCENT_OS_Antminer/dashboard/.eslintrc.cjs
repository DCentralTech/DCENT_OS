/*
 * ESLint config — NON-build-blocking static gate (SLOP-TOOL-01).
 *
 * `tsc` is the only thing wired into `npm run build`, and tsconfig.json disables
 * noUnusedLocals/noUnusedParameters, so dead code + `any` + hook-dependency bugs
 * currently ship silently. This lint surfaces them WITHOUT being able to break
 * the beta build — it is deliberately NOT part of `npm run build`; run it on its
 * own via `npm run lint`.
 *
 * Rules are tuned so the existing `src/` tree passes today: the high-churn,
 * pre-existing categories (`any`, exhaustive-deps, unused vars) are `warn`, not
 * `error`, so the gate informs without flooding. Genuine correctness rules
 * (rules-of-hooks) stay `error`.
 *
 * Type-aware linting is intentionally OFF (no `parserOptions.project`) to keep
 * the run fast and avoid a second TS program build.
 */
module.exports = {
  root: true,
  env: {
    browser: true,
    es2021: true,
    node: true,
  },
  parser: '@typescript-eslint/parser',
  parserOptions: {
    ecmaVersion: 2021,
    sourceType: 'module',
    ecmaFeatures: { jsx: true },
  },
  plugins: ['@typescript-eslint', 'react-hooks'],
  extends: [
    'eslint:recommended',
    'plugin:@typescript-eslint/recommended',
  ],
  ignorePatterns: [
    'dist/',
    'node_modules/',
    'cypress/',
    'scripts/',
    'vite.config.ts',
    'vitest.config.ts',
    'cypress.config.ts',
  ],
  rules: {
    // ── React Hooks ──────────────────────────────────────────────────────
    'react-hooks/rules-of-hooks': 'error',     // real correctness bug if violated
    'react-hooks/exhaustive-deps': 'warn',     // advisory — codebase opts out inline

    // ── Surface, don't block ─────────────────────────────────────────────
    '@typescript-eslint/no-explicit-any': 'warn',

    // Dead-code visibility (tsconfig turns noUnusedLocals/Parameters off).
    'no-unused-vars': 'off',
    '@typescript-eslint/no-unused-vars': [
      'warn',
      { argsIgnorePattern: '^_', varsIgnorePattern: '^_', ignoreRestSiblings: true },
    ],

    // TypeScript already checks for undefined identifiers; `no-undef` only
    // adds false positives on a TS codebase (per typescript-eslint guidance).
    'no-undef': 'off',

    // The following recommended-set rules fire on pre-existing, intentional
    // patterns across src/ (empty catch blocks, control-char regexes in the
    // ANSI-strip path, switch-case lexical decls, deliberate non-null
    // assertions, ts-directive comments). Downgrade to warn so the gate stays
    // green on the existing tree while still surfacing them.
    'no-empty': ['warn', { allowEmptyCatch: true }],
    'no-control-regex': 'warn',
    'no-case-declarations': 'warn',
    'no-useless-escape': 'warn',
    'no-prototype-builtins': 'warn',
    '@typescript-eslint/no-empty-function': 'warn',
    '@typescript-eslint/no-non-null-assertion': 'off',
    '@typescript-eslint/ban-ts-comment': 'warn',
    '@typescript-eslint/no-inferrable-types': 'off',
    '@typescript-eslint/no-empty-interface': 'warn',
    '@typescript-eslint/ban-types': 'warn',   // pre-existing `{}`/`Function` shapes
    'prefer-const': 'warn',                   // one pre-existing `let` never reassigned
  },
};
