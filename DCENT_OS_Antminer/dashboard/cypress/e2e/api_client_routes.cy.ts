/// <reference types="cypress" />

import {
  API_CLIENT_ROUTE_MANIFEST,
  apiClientRouteKey,
  apiClientRoutePathnameKey,
} from '../../src/api/client-route-manifest';

const REQUIRED_ROUTES = [
  'GET /api/system/api-compatibility/manifest',
  'GET /api/config/donation',
  'POST /api/config/donation',
  'PUT /api/autotuner/active',
  'GET /api/network/block',
  'GET /api/metrics/rolling',
  'GET /api/mining/pipeline/manifest',
  'GET /api/mining/pipeline/snapshot',
  'GET /api/mining/pipeline/snapshot/schema',
  'GET /api/diagnostics/failure_modes',
  'GET /api/diagnostics/chain?id={id}',
  'GET /api/diagnostics/shares/local_rejects?limit={limit}',
  'GET /api/system/boot_timeline',
  'GET /api/hardware/pic_info',
  'GET /api/hardware/psu_catalog',
  'GET /api/cgminer/catalog',
  'GET /api/re/catalog/index',
  'GET /api/diagnostics/recovery_actions',
  'GET /api/history/audit?limit={limit}',
  'POST /api/fleet/discover',
  'POST /api/system/upgrade',
  'POST /api/system/restore-to-stock/preflight',
  'POST /api/system/restore-to-stock',
  'GET /api/system/restore-to-stock/status',
] as const;

describe('dashboard API client route manifest', () => {
  it('tracks critical typed client routes without duplicates or non-api paths', () => {
    const keys = API_CLIENT_ROUTE_MANIFEST.map(apiClientRouteKey);

    expect(new Set(keys).size, 'unique route keys').to.equal(keys.length);
    expect(keys).to.include.members([...REQUIRED_ROUTES]);

    for (const route of API_CLIENT_ROUTE_MANIFEST) {
      expect(route.path, apiClientRouteKey(route)).to.match(/^\/api\//);
      expect(route.method, apiClientRouteKey(route)).to.match(/^(GET|POST|PUT|DELETE)$/);
      expect(route.owner, apiClientRouteKey(route)).to.not.equal('');
    }
  });

  it('covers every dashboard API client route by method and pathname', () => {
    cy.readFile('src/api/client.ts', 'utf8').then((clientSource: string) => {
      cy.readFile('src/api/restore-to-stock.ts', 'utf8').then((restoreSource: string) => {
        const manifestKeys = new Set(API_CLIENT_ROUTE_MANIFEST.map(apiClientRoutePathnameKey));
        const clientKeys = new Set([
          ...extractClientRoutes(clientSource),
          ...extractApiFetchRoutes(restoreSource),
        ]);
        const missing = [...clientKeys].filter((key) => !manifestKeys.has(key)).sort();

        expect(missing, 'client routes missing from API_CLIENT_ROUTE_MANIFEST').to.deep.equal([]);
      });
    });
  });

  it('keeps the mining pipeline snapshot route contract mounted but default-off', () => {
    cy.readFile('cypress/support/e2e.ts', 'utf8').then((fixtureSource: string) => {
      expect(fixtureSource).to.include('live_snapshot_endpoint: "/api/mining/pipeline/snapshot"');
      expect(fixtureSource).to.include('snapshot_available: false');
      expect(fixtureSource).to.include('live_route_mounted: true');
    });

    cy.readFile('src/components/standard/MiningPipelineManifestCard.tsx', 'utf8').then((cardSource: string) => {
      expect(cardSource).to.include('live snapshot route is mounted as a read-only clone endpoint');
      expect(cardSource).to.include('publisher remains default-off and unavailable until validated');
      expect(cardSource).not.to.include('live route remains absent');
    });
  });
});

function routePathname(raw: string): string {
  const withoutBase = raw.replace(/^\$\{BASE\}/, '');
  const queryIndex = withoutBase.indexOf('?');
  return queryIndex >= 0 ? withoutBase.slice(0, queryIndex) : withoutBase;
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
  // Path char class excludes `$` and drops the closing-delimiter backreference
  // so a template-literal route (`/api/chips${…}`) captures its static prefix
  // `/api/chips`. Kept in sync with src/api/client-route-manifest.test.ts (the
  // vitest enforcer that actually runs — this cypress spec needs a browser
  // harness that never fires without a git remote).
  const wrapperCall = /\b(get|post|put|delete)(?:<[^'"`]*>)?\(\s*['"`](\/api\/[^'"`$]*)/g;
  for (const match of source.matchAll(wrapperCall)) {
    routes.add(routeKey(match[1], match[2]));
  }

  const baseFetch = /\bfetch\(\s*`\$\{BASE\}(\/api\/[^`$]+)`/g;
  for (const match of source.matchAll(baseFetch)) {
    routes.add(routeKey(inferMethodFromFollowingSource(source, match.index ?? 0), match[1]));
  }

  const xhrOpen = /xhr\.open\(\s*['"]([A-Z]+)['"]\s*,\s*`\$\{BASE\}(\/api\/[^`$]+)`/g;
  for (const match of source.matchAll(xhrOpen)) {
    routes.add(routeKey(match[1], match[2]));
  }

  return [...routes].sort();
}

function extractApiFetchRoutes(source: string): string[] {
  const routes = new Set<string>();
  const apiFetchCall = /\bapiFetch\(\s*['"`](\/api\/[^'"`$]*)/g;
  for (const match of source.matchAll(apiFetchCall)) {
    routes.add(routeKey(inferMethodFromFollowingSource(source, match.index ?? 0), match[1]));
  }
  return [...routes].sort();
}
