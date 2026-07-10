export type ApiClientRouteMethod = 'GET' | 'POST' | 'PUT' | 'DELETE';

export type ApiClientRouteOwner =
  | 'actions'
  | 'autotuner'
  | 'config'
  | 'core-client'
  | 'debug'
  | 'diagnostics'
  | 'donation'
  | 'evidence-page'
  | 'fleet-discovery'
  | 'home'
  | 'led'
  | 'mining-pipeline'
  | 'network-info'
  | 'offgrid'
  | 'pools'
  | 'restore-to-stock'
  | 'setup'
  | 'solar'
  | 'stats-history'
  | 'support-bundle'
  | 'sv2-jd'
  | 'system-upgrade';

export type ApiClientRouteManifestEntry = {
  readonly method: ApiClientRouteMethod;
  readonly path: `/api/${string}`;
  readonly owner: ApiClientRouteOwner;
};

export const API_CLIENT_ROUTE_MANIFEST = [
  { method: 'POST', path: '/api/auth/session', owner: 'core-client' },
  { method: 'DELETE', path: '/api/auth/session/current', owner: 'core-client' },
  { method: 'GET', path: '/api/status', owner: 'core-client' },
  { method: 'GET', path: '/api/dashboard/version', owner: 'core-client' },
  { method: 'GET', path: '/api/config', owner: 'config' },
  { method: 'POST', path: '/api/config', owner: 'config' },
  { method: 'POST', path: '/api/config/shared', owner: 'config' },
  { method: 'GET', path: '/api/config/donation', owner: 'config' },
  { method: 'POST', path: '/api/config/donation', owner: 'config' },
  // W9.5: public donation pool disclosure (read-only, unauthenticated).
  { method: 'GET', path: '/api/donation/info', owner: 'donation' },
  { method: 'GET', path: '/api/config/backup/manifest', owner: 'config' },
  { method: 'GET', path: '/api/config/export', owner: 'config' },
  { method: 'POST', path: '/api/config/import', owner: 'config' },
  { method: 'GET', path: '/api/config/mqtt', owner: 'config' },
  { method: 'POST', path: '/api/config/mqtt', owner: 'config' },
  { method: 'POST', path: '/api/config/mqtt/test', owner: 'config' },
  { method: 'GET', path: '/api/mqtt/status', owner: 'config' },
  { method: 'GET', path: '/api/config/power-calibration', owner: 'config' },
  { method: 'POST', path: '/api/config/power-calibration', owner: 'config' },
  { method: 'GET', path: '/api/config/psu-override', owner: 'config' },
  { method: 'POST', path: '/api/config/psu-override', owner: 'config' },
  { method: 'GET', path: '/api/config/webhook', owner: 'config' },
  { method: 'POST', path: '/api/config/webhook', owner: 'config' },
  { method: 'POST', path: '/api/config/webhook/test', owner: 'config' },

  { method: 'GET', path: '/api/setup/status', owner: 'setup' },
  { method: 'POST', path: '/api/setup/step1-safety', owner: 'setup' },
  { method: 'POST', path: '/api/setup/step2-circuit', owner: 'setup' },
  { method: 'POST', path: '/api/setup/step4-mode', owner: 'setup' },
  { method: 'POST', path: '/api/setup/step5-pool', owner: 'setup' },
  { method: 'POST', path: '/api/setup/test-pool', owner: 'setup' },
  { method: 'POST', path: '/api/setup/complete', owner: 'setup' },

  { method: 'GET', path: '/api/system/info', owner: 'core-client' },
  { method: 'GET', path: '/api/system/health', owner: 'core-client' },
  { method: 'GET', path: '/api/system/asic', owner: 'core-client' },
  { method: 'GET', path: '/api/system/stats', owner: 'core-client' },
  { method: 'GET', path: '/api/system/api-compatibility/manifest', owner: 'core-client' },
  { method: 'GET', path: '/api/v1/capabilities', owner: 'core-client' },
  { method: 'GET', path: '/api/competitive/readiness', owner: 'core-client' },
  { method: 'GET', path: '/api/network/block', owner: 'core-client' },

  { method: 'GET', path: '/api/pools', owner: 'pools' },
  { method: 'POST', path: '/api/pools', owner: 'pools' },
  { method: 'POST', path: '/api/pools/test', owner: 'pools' },

  { method: 'GET', path: '/api/stats', owner: 'stats-history' },
  { method: 'GET', path: '/api/metrics/rolling', owner: 'stats-history' },
  { method: 'GET', path: '/api/thermal/posture', owner: 'stats-history' },
  { method: 'POST', path: '/api/tou/schedule', owner: 'stats-history' },
  { method: 'GET', path: '/api/history', owner: 'stats-history' },
  { method: 'GET', path: '/api/history/shares', owner: 'stats-history' },
  { method: 'GET', path: '/api/history/audit?limit={limit}', owner: 'evidence-page' },
  { method: 'GET', path: '/api/profiles', owner: 'stats-history' },
  { method: 'POST', path: '/api/profiles', owner: 'stats-history' },

  { method: 'GET', path: '/api/mining/work/posture', owner: 'mining-pipeline' },
  { method: 'GET', path: '/api/mining/pipeline/manifest', owner: 'mining-pipeline' },
  { method: 'GET', path: '/api/mining/pipeline/snapshot', owner: 'mining-pipeline' },
  { method: 'GET', path: '/api/mining/pipeline/snapshot/schema', owner: 'mining-pipeline' },

  { method: 'GET', path: '/api/autotuner/status', owner: 'autotuner' },
  { method: 'PUT', path: '/api/autotuner/active', owner: 'autotuner' },
  { method: 'GET', path: '/api/autotuner/chip-health', owner: 'autotuner' },
  { method: 'GET', path: '/api/autotuner/telemetry', owner: 'autotuner' },
  { method: 'GET', path: '/api/autotuner/telemetry/csv', owner: 'autotuner' },
  { method: 'GET', path: '/api/autotuner/visibility', owner: 'autotuner' },

  { method: 'GET', path: '/api/home/status', owner: 'home' },
  { method: 'POST', path: '/api/home/target', owner: 'home' },
  { method: 'GET', path: '/api/home/presets', owner: 'home' },
  { method: 'POST', path: '/api/home/room-temp', owner: 'home' },
  { method: 'GET', path: '/api/home/night-mode', owner: 'home' },
  { method: 'POST', path: '/api/home/night-mode', owner: 'home' },
  { method: 'GET', path: '/api/home/history', owner: 'home' },

  { method: 'POST', path: '/api/fan', owner: 'actions' },
  { method: 'POST', path: '/api/fleet/discover', owner: 'fleet-discovery' },
  { method: 'POST', path: '/api/action/restart', owner: 'actions' },
  { method: 'POST', path: '/api/action/reboot', owner: 'actions' },
  { method: 'POST', path: '/api/action/sleep', owner: 'actions' },
  { method: 'POST', path: '/api/action/wake', owner: 'actions' },

  { method: 'GET', path: '/api/debug/registers?chain={chain}&offset={offset}', owner: 'debug' },
  { method: 'POST', path: '/api/debug/registers', owner: 'debug' },
  { method: 'GET', path: '/api/debug/log?lines={lines}', owner: 'debug' },
  { method: 'GET', path: '/api/debug/i2c?bus={bus}&addr={addr}', owner: 'debug' },
  { method: 'POST', path: '/api/debug/i2c', owner: 'debug' },
  { method: 'POST', path: '/api/debug/asic-command', owner: 'debug' },
  { method: 'GET', path: '/api/debug/pid-state', owner: 'debug' },
  { method: 'POST', path: '/api/debug/pid-params', owner: 'debug' },
  { method: 'POST', path: '/api/debug/chip/frequency', owner: 'debug' },
  { method: 'POST', path: '/api/debug/chip/voltage', owner: 'debug' },
  { method: 'POST', path: '/api/debug/psu/control', owner: 'debug' },

  { method: 'POST', path: '/api/diagnostics/hashreport/start', owner: 'diagnostics' },
  { method: 'POST', path: '/api/diagnostics/hashreport/cancel', owner: 'diagnostics' },
  { method: 'GET', path: '/api/diagnostics/hashreport/status?test_id={id}', owner: 'diagnostics' },
  { method: 'GET', path: '/api/diagnostics/hashreport/result?test_id={id}', owner: 'diagnostics' },
  { method: 'POST', path: '/api/diagnostics/chip-health/start', owner: 'diagnostics' },
  { method: 'GET', path: '/api/diagnostics/chip-health/status?test_id={id}', owner: 'diagnostics' },
  { method: 'GET', path: '/api/diagnostics/chip-health/result?test_id={id}', owner: 'diagnostics' },
  { method: 'POST', path: '/api/diagnostics/board-health/start', owner: 'diagnostics' },
  { method: 'GET', path: '/api/diagnostics/board-health/status?test_id={id}', owner: 'diagnostics' },
  { method: 'GET', path: '/api/diagnostics/board-health/result?test_id={id}', owner: 'diagnostics' },
  { method: 'GET', path: '/api/diagnostics/reports/recent?limit={limit}', owner: 'diagnostics' },
  { method: 'GET', path: '/api/diagnostics/logs/manifest', owner: 'diagnostics' },
  { method: 'GET', path: '/api/diagnostics/failure_modes', owner: 'evidence-page' },
  { method: 'GET', path: '/api/diagnostics/chain?id={id}', owner: 'evidence-page' },
  { method: 'GET', path: '/api/diagnostics/shares/local_rejects?limit={limit}', owner: 'evidence-page' },
  { method: 'GET', path: '/api/diagnostics/recovery_actions', owner: 'evidence-page' },
  { method: 'GET', path: '/api/diagnostics/state_machine', owner: 'evidence-page' },
  { method: 'GET', path: '/api/system/update_capability', owner: 'evidence-page' },
  { method: 'GET', path: '/api/diagnostics/error_vocab', owner: 'evidence-page' },
  { method: 'GET', path: '/api/mining/ramp', owner: 'evidence-page' },
  { method: 'GET', path: '/api/stratum/protocol', owner: 'evidence-page' },
  { method: 'GET', path: '/api/hardware/psu_bypass_matrix', owner: 'evidence-page' },
  { method: 'GET', path: '/api/thermal/cold_environment', owner: 'evidence-page' },
  { method: 'GET', path: '/api/pools/failover_policy', owner: 'evidence-page' },
  { method: 'GET', path: '/api/tuning/constraints', owner: 'evidence-page' },
  { method: 'GET', path: '/api/diagnostics/sensor_outlier', owner: 'evidence-page' },
  { method: 'GET', path: '/api/firmware/vnish_schema', owner: 'evidence-page' },
  { method: 'GET', path: '/api/firmware/luxos_architecture', owner: 'evidence-page' },
  { method: 'GET', path: '/api/thermal/cooling_modes', owner: 'evidence-page' },
  { method: 'GET', path: '/api/power/dps', owner: 'evidence-page' },
  { method: 'GET', path: '/api/network/config_schema', owner: 'evidence-page' },
  { method: 'GET', path: '/api/firmware/luxos_web_map', owner: 'evidence-page' },
  { method: 'GET', path: '/api/firmware/proto_wire_types', owner: 'evidence-page' },
  { method: 'GET', path: '/api/firmware/luxos_responses', owner: 'evidence-page' },
  { method: 'GET', path: '/api/firmware/luxos_status_codes', owner: 'evidence-page' },
  { method: 'GET', path: '/api/firmware/vnish_overlay', owner: 'evidence-page' },
  { method: 'GET', path: '/api/diagnostics/troubleshoot/network', owner: 'diagnostics' },
  { method: 'GET', path: '/api/diagnostics/troubleshoot/psu', owner: 'diagnostics' },
  { method: 'GET', path: '/api/diagnostics/troubleshoot/fpga', owner: 'diagnostics' },

  { method: 'GET', path: '/api/system/boot_timeline', owner: 'evidence-page' },
  { method: 'GET', path: '/api/hardware/pic_info', owner: 'evidence-page' },
  { method: 'GET', path: '/api/hardware/psu_catalog', owner: 'evidence-page' },
  { method: 'GET', path: '/api/cgminer/catalog', owner: 'evidence-page' },
  { method: 'GET', path: '/api/re/catalog/index', owner: 'evidence-page' },

  { method: 'GET', path: '/api/offgrid/config', owner: 'offgrid' },
  { method: 'POST', path: '/api/offgrid/config', owner: 'offgrid' },
  { method: 'GET', path: '/api/offgrid/status', owner: 'offgrid' },
  { method: 'GET', path: '/api/offgrid/presets', owner: 'offgrid' },
  { method: 'POST', path: '/api/offgrid/test', owner: 'offgrid' },

  { method: 'GET', path: '/api/solar/config', owner: 'solar' },
  { method: 'POST', path: '/api/solar/config', owner: 'solar' },
  { method: 'GET', path: '/api/solar/status', owner: 'solar' },
  { method: 'GET', path: '/api/solar/verification-history', owner: 'solar' },
  { method: 'POST', path: '/api/solar/test', owner: 'solar' },

  { method: 'GET', path: '/api/led/status', owner: 'led' },
  { method: 'GET', path: '/api/led/patterns', owner: 'led' },
  { method: 'POST', path: '/api/led/locate', owner: 'led' },
  { method: 'POST', path: '/api/led/locate/stop', owner: 'led' },
  { method: 'GET', path: '/api/led/config', owner: 'led' },
  { method: 'POST', path: '/api/led/config', owner: 'led' },
  // W11.12: stock-CGI parity (RE2 §15.2 + competing-firmware features).
  { method: 'GET', path: '/api/network/info', owner: 'network-info' },
  { method: 'POST', path: '/api/network/hostname', owner: 'network-info' },
  { method: 'GET', path: '/api/miner/type', owner: 'network-info' },
  { method: 'GET', path: '/api/log/backup', owner: 'support-bundle' },

  { method: 'GET', path: '/api/pool/sv2/status', owner: 'sv2-jd' },
  { method: 'GET', path: '/api/pool/sv2/handshake', owner: 'sv2-jd' },
  { method: 'GET', path: '/api/pool/sv2/messages', owner: 'sv2-jd' },
  { method: 'GET', path: '/api/jd/status', owner: 'sv2-jd' },
  { method: 'POST', path: '/api/jd/config', owner: 'sv2-jd' },
  { method: 'POST', path: '/api/jd/test-connection', owner: 'sv2-jd' },

  { method: 'POST', path: '/api/system/upgrade', owner: 'system-upgrade' },
  { method: 'GET', path: '/api/system/upgrade/status', owner: 'system-upgrade' },

  { method: 'POST', path: '/api/system/restore-to-stock/preflight', owner: 'restore-to-stock' },
  { method: 'POST', path: '/api/system/restore-to-stock', owner: 'restore-to-stock' },
  { method: 'GET', path: '/api/system/restore-to-stock/status', owner: 'restore-to-stock' },
  { method: 'GET', path: '/api/system/restore-to-stock/preflight-checks', owner: 'restore-to-stock' },

  // APIC-3 (2026-06-18): routes client.ts actually calls that had drifted out of
  // this manifest. The cypress drift-guard (api_client_routes.cy.ts) never ran
  // (no git remote → its Action never fires), so the manifest silently went
  // stale. Now also enforced in the fast vitest suite by
  // client-route-manifest.test.ts. Each verified present in the daemon router.
  { method: 'GET', path: '/api/chips', owner: 'stats-history' },
  { method: 'GET', path: '/api/audit-log', owner: 'diagnostics' },
  { method: 'GET', path: '/api/boot/phase', owner: 'stats-history' },
  { method: 'GET', path: '/api/boot/timeline', owner: 'stats-history' },
  { method: 'GET', path: '/api/miner/pvt-table', owner: 'stats-history' },
  { method: 'GET', path: '/api/thermal/supervisor', owner: 'stats-history' },
  { method: 'GET', path: '/api/perf/efficiency', owner: 'autotuner' },
  { method: 'POST', path: '/api/perf/calibrate', owner: 'autotuner' },
  { method: 'GET', path: '/api/autotuner/silicon-report', owner: 'autotuner' },
  { method: 'POST', path: '/api/auth/setup', owner: 'core-client' },
  { method: 'POST', path: '/api/setup/quiet-hours', owner: 'setup' },
  { method: 'POST', path: '/api/setup/skip-password', owner: 'setup' },
  { method: 'POST', path: '/api/setup/skip-safety', owner: 'setup' },
  { method: 'POST', path: '/api/setup/step-economics', owner: 'setup' },
] as const satisfies readonly ApiClientRouteManifestEntry[];

export function apiClientRouteKey(route: Pick<ApiClientRouteManifestEntry, 'method' | 'path'>): string {
  return `${route.method} ${route.path}`;
}

export function apiClientRoutePathname(route: Pick<ApiClientRouteManifestEntry, 'path'>): string {
  return route.path.split('?')[0];
}

export function apiClientRoutePathnameKey(route: Pick<ApiClientRouteManifestEntry, 'method' | 'path'>): string {
  return `${route.method} ${apiClientRoutePathname(route)}`;
}
