import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import {
  API_CLIENT_ROUTE_MANIFEST,
  apiClientRoutePathnameKey,
} from './client-route-manifest';

// APIC-3 / TEST-CI-1: enforce the dashboard API-client route manifest in the FAST
// vitest suite. The equivalent cypress spec (cypress/e2e/api_client_routes.cy.ts)
// only runs under a browser e2e harness that never fires (the repo has no git
// remote, so its GitHub Action never runs), which let the manifest silently drift
// stale. This is the same extraction logic, run in node against the real source,
// so the manifest stays an honest drift-guarded contract — not documentation.
//
// Keep this in sync with the extraction in api_client_routes.cy.ts.

function routePathname(raw: string): string {
  const withoutBase = raw.replace(/^\$\{BASE\}/, '');
  const q = withoutBase.indexOf('?');
  return q >= 0 ? withoutBase.slice(0, q) : withoutBase;
}
function routeKey(method: string, rawPath: string): string {
  return `${method.toUpperCase()} ${routePathname(rawPath)}`;
}
function inferMethodFromFollowingSource(source: string, index: number, fallback = 'GET'): string {
  const lookahead = source.slice(index, index + 240);
  const match = lookahead.match(/method:\s*['"]([A-Z]+)['"]/);
  return match?.[1] ?? fallback;
}
function extractClientRoutes(source: string): string[] {
  const routes = new Set<string>();
  // Path char class excludes `$` so a template-literal route like
  // `/api/chips${chain != null ? '?id=' + chain : ''}` captures the static
  // prefix `/api/chips` (the dynamic `${…}` query is not part of the contract).
  const wrapperCall = /\b(get|post|put|delete)(?:<[^'"`]*>)?\(\s*['"`](\/api\/[^'"`$]*)/g;
  for (const m of source.matchAll(wrapperCall)) routes.add(routeKey(m[1], m[2]));
  const baseFetch = /\bfetch\(\s*`\$\{BASE\}(\/api\/[^`$]+)`/g;
  for (const m of source.matchAll(baseFetch)) {
    routes.add(routeKey(inferMethodFromFollowingSource(source, m.index ?? 0), m[1]));
  }
  const xhrOpen = /xhr\.open\(\s*['"]([A-Z]+)['"]\s*,\s*`\$\{BASE\}(\/api\/[^`$]+)`/g;
  for (const m of source.matchAll(xhrOpen)) routes.add(routeKey(m[1], m[2]));
  return [...routes].sort();
}
function extractApiFetchRoutes(source: string): string[] {
  const routes = new Set<string>();
  const apiFetchCall = /\bapiFetch\(\s*['"`](\/api\/[^'"`$]*)/g;
  for (const m of source.matchAll(apiFetchCall)) {
    routes.add(routeKey(inferMethodFromFollowingSource(source, m.index ?? 0), m[1]));
  }
  return [...routes].sort();
}

describe('dashboard API client route manifest', () => {
  it('covers every dashboard API client route by method and pathname', () => {
    const clientSource = readFileSync('src/api/client.ts', 'utf8');
    const restoreSource = readFileSync('src/api/restore-to-stock.ts', 'utf8');
    const manifestKeys = new Set(API_CLIENT_ROUTE_MANIFEST.map(apiClientRoutePathnameKey));
    const clientKeys = new Set([
      ...extractClientRoutes(clientSource),
      ...extractApiFetchRoutes(restoreSource),
    ]);
    const missing = [...clientKeys].filter((k) => !manifestKeys.has(k)).sort();
    expect(missing, 'client routes missing from API_CLIENT_ROUTE_MANIFEST').toEqual([]);
  });
});
