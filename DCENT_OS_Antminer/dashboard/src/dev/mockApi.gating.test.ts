import { describe, it, expect } from 'vitest';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

// ─────────────────────────────────────────────────────────────────────────────
// FE-3 — production build must NOT contain the dev mock-telemetry harness.
//
// `src/dev/mockApi.ts` (+ `mockFixtures.ts`) is a DEV/QA-only fetch shim that
// serves fake "miner is mining" telemetry. It must never ship in a production
// firmware bundle — both because it bloats the ~2 MiB ramdisk budget and,
// far worse, because un-gating it would make the dashboard render fabricated
// telemetry, violating the truth contracts (connecting ≠ connected,
// scheduled ≠ flashed, etc.).
//
// Vite/esbuild only drops the harness if it is reached EXCLUSIVELY through a
// dynamic `import()` guarded by `import.meta.env.DEV` (a compile-time `false`
// in prod → the whole branch, and the lazily-imported chunk, are tree-shaken).
// A *static* `import … from './dev/mock…'` anywhere in production source would
// pull it into the prod graph unconditionally. These source-structure asserts
// are the cheapest robust check that fails the moment someone un-gates it —
// no full `vite build` + dist scan needed.
// ─────────────────────────────────────────────────────────────────────────────

const MAIN = readFileSync(fileURLToPath(new URL('../main.tsx', import.meta.url)), 'utf8');
const DIST_INDEX = fileURLToPath(new URL('../../dist/index.html', import.meta.url));

const DEV_GUARD = 'if (import.meta.env.DEV)';
const DYNAMIC_MOCK_IMPORT = /import\(\s*['"]\.\/dev\/mockApi['"]\s*\)/;

describe('FE-3 — dev mock harness stays gated out of the production build', () => {
  it('main.tsx loads the mock only via a dynamic import inside the import.meta.env.DEV branch', () => {
    const guardIdx = MAIN.indexOf(DEV_GUARD);
    expect(
      guardIdx,
      'main.tsx must production-strip the mock behind `if (import.meta.env.DEV)`',
    ).toBeGreaterThanOrEqual(0);

    // The guard must have an `else` branch that boots WITHOUT the mock, so prod
    // never touches the harness.
    const elseIdx = MAIN.indexOf('} else {', guardIdx);
    expect(
      elseIdx,
      'the DEV guard must have an `else` branch that boots without the mock',
    ).toBeGreaterThan(guardIdx);

    const mockMatch = DYNAMIC_MOCK_IMPORT.exec(MAIN);
    expect(
      mockMatch,
      "the mock harness must be loaded via a dynamic `import('./dev/mockApi')`",
    ).not.toBeNull();

    // The dynamic import must sit strictly inside the DEV-only block
    // (between the guard and its else), never in the prod boot path.
    const mockIdx = mockMatch!.index;
    expect(mockIdx).toBeGreaterThan(guardIdx);
    expect(mockIdx).toBeLessThan(elseIdx);
  });

  it('no production module statically imports the dev mock harness (it must stay code-split)', () => {
    // A static `import …/export … from '…/dev/…'` would defeat the dynamic-import
    // tree-shaking and bundle the harness into prod. The dynamic `import()` call
    // in main.tsx is a CallExpression, not an import statement, so it is NOT
    // matched here (by design).
    const SRC = fileURLToPath(new URL('..', import.meta.url)); // src/
    const staticDevImport =
      /(?:^|\n)\s*(?:import|export)\b[^\n;]*\bfrom\s*['"][^'"]*\/dev\/[^'"]*['"]/;
    const offenders: string[] = [];
    const walk = (dir: string) => {
      for (const ent of readdirSync(dir, { withFileTypes: true })) {
        const p = join(dir, ent.name);
        if (ent.isDirectory()) {
          // Skip build output, deps, and src/dev itself (intra-harness imports
          // like mockApi → mockFixtures are fine; the harness is never reached
          // in prod because nothing outside dev/ statically imports it).
          if (ent.name === 'node_modules' || ent.name === 'dist' || ent.name === 'dev') continue;
          walk(p);
        } else if (/\.tsx?$/.test(ent.name) && !/\.(test|spec)\.tsx?$/.test(ent.name)) {
          if (staticDevImport.test(readFileSync(p, 'utf8'))) offenders.push(p);
        }
      }
    };
    walk(SRC);
    expect(
      offenders,
      `These production modules statically import from src/dev/ — that bundles the ` +
        `dev mock harness into the production build. Load it via a DEV-gated dynamic ` +
        `import() instead. Offenders:\n${offenders.join('\n')}`,
    ).toEqual([]);
  });

  it('production dist does not contain the mock sync injection seam', () => {
    expect(existsSync(DIST_INDEX), 'dist/index.html must exist before this dist-gating check').toBe(true);
    const dist = readFileSync(DIST_INDEX, 'utf8');
    expect(dist).not.toContain('__dcentMockSync');
    expect(dist).not.toContain('devInject');
  });
});
