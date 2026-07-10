import type { ChainState, SetupStatusResponse, StatusResponse } from '../api/types';
import { selectIsMining } from './miningStatus';

export type HealthTone = 'success' | 'info' | 'warning' | 'danger' | 'neutral';
export type FaviconState = 'mining' | 'warning' | 'error' | 'standby';

export interface HealthChip {
  label: string;
  tone: HealthTone;
}

export interface HealthIssue {
  key: string;
  level: 'info' | 'warning' | 'critical';
  message: string;
  /** Freedom-first: derived advisories (e.g. "no owner password") are
   *  dismissible. Telemetry/hardware issues are NOT (omit / false). */
  dismissible?: boolean;
  /** Optional in-app navigation target for a "Fix it" affordance, e.g.
   *  `{ page: 'settings', anchor: 'security' }`. */
  action?: { page: string; anchor?: string };
}

/** localStorage key prefix for persisted health-issue dismissals. */
export const DISMISSED_HEALTH_KEY_PREFIX = 'dcentos-dismissed:';

export function isHealthIssueDismissed(key: string): boolean {
  try {
    return localStorage.getItem(`${DISMISSED_HEALTH_KEY_PREFIX}${key}`) === '1';
  } catch {
    return false;
  }
}

export function setHealthIssueDismissed(key: string, dismissed: boolean): void {
  try {
    if (dismissed) {
      localStorage.setItem(`${DISMISSED_HEALTH_KEY_PREFIX}${key}`, '1');
    } else {
      localStorage.removeItem(`${DISMISSED_HEALTH_KEY_PREFIX}${key}`);
    }
  } catch {
    /* localStorage unavailable — non-fatal, warning just re-shows */
  }
}

// ─── Board (hash-board / chain) health derivation ──────────────────────────
// P0-3 (C-3/D-3): the daemon reports a 0-GH/s dead board as `status:"Active"`,
// so any health logic keyed off the `status` string never flags it. Board
// health is therefore derived from *hashrate-while-powered* — a board that is
// clocked (freq>0) with enumerated chips (chips>0) but produces ~no hashrate
// while the unit is mining is degraded/faulted regardless of what the status
// string says. This is consumed by both the issue list here and the per-chain
// tone in LiveAsicVisual so the two never disagree.
export type BoardHealthVerdict = 'healthy' | 'idle' | 'degraded' | 'fault';

export interface BoardHealthSummary {
  /** Real (non-placeholder) boards considered. */
  total: number;
  /** Boards that are powered + clocked but producing ~no hashrate while mining. */
  notHashing: number;
  /** Of `notHashing`, those with a high hardware-error signal. */
  faulted: number;
  /** Of `notHashing`, those without a high hardware-error signal. */
  degraded: number;
  /** Worst verdict across all boards. */
  worst: BoardHealthVerdict;
  /** Per-board verdicts, index-aligned with the input chains. */
  perBoard: BoardHealthVerdict[];
}

/** A board below this hashrate is treated as "not hashing" (a dead chain reads
 *  exactly 0.0; real Zynq/Amlogic chains are hundreds–thousands of GH/s). */
export const BOARD_HASHING_MIN_GHS = 1;

/** Hardware-error count that promotes a non-hashing board from degraded→fault. */
export const BOARD_FAULT_ERROR_THRESHOLD = 16;

type BoardHealthInput = Pick<
  ChainState,
  'chips' | 'frequency_mhz' | 'hashrate_ghs' | 'errors' | 'status'
>;

/**
 * Classify a single board from telemetry, NOT from the `status` string.
 * `unitIsMining` is the whole-unit "is any hashrate flowing" context so we
 * don't cry fault on every powered board during boot/standby.
 */
export function classifyBoardHealth(
  chain: BoardHealthInput,
  unitIsMining: boolean,
): BoardHealthVerdict {
  const status = (chain.status ?? '').toLowerCase();
  const hashing = (chain.hashrate_ghs ?? 0) > BOARD_HASHING_MIN_GHS;
  // A board actually producing hashrate is healthy regardless of a stale or
  // mislabeled status string.
  if (hashing) {
    return 'healthy';
  }
  // An explicit dead/fault/error status with no hashrate is a fault.
  if (status.includes('dead') || status.includes('fault') || status.includes('error')) {
    return 'fault';
  }
  const powered = (chain.chips ?? 0) > 0 && (chain.frequency_mhz ?? 0) > 0;
  if (!powered) {
    // No chips / not clocked — board is absent or asleep, not a fault.
    return 'idle';
  }
  if (!unitIsMining) {
    // Whole unit isn't mining yet (boot/standby) — powered idle, not a fault.
    return 'idle';
  }
  // Powered + clocked + the unit IS mining, but this board produces ~zero
  // hashrate: that is a real fault — even with the status string saying "Active".
  return (chain.errors ?? 0) >= BOARD_FAULT_ERROR_THRESHOLD ? 'fault' : 'degraded';
}

/** Summarize per-board verdicts across the chain list. */
export function summarizeBoardHealth(
  chains: ReadonlyArray<BoardHealthInput>,
  unitIsMining: boolean,
): BoardHealthSummary {
  const perBoard = chains.map(chain => classifyBoardHealth(chain, unitIsMining));
  const faulted = perBoard.filter(v => v === 'fault').length;
  const degraded = perBoard.filter(v => v === 'degraded').length;
  const worst: BoardHealthVerdict = faulted > 0
    ? 'fault'
    : degraded > 0
      ? 'degraded'
      : perBoard.some(v => v === 'healthy')
        ? 'healthy'
        : 'idle';
  return {
    total: perBoard.length,
    notHashing: faulted + degraded,
    faulted,
    degraded,
    worst,
    perBoard,
  };
}

export interface DashboardHealth {
  ageMs: number | null;
  hasFreshTelemetry: boolean;
  hasRecentTelemetry: boolean;
  minerChip: HealthChip;
  transportChip: HealthChip;
  issues: HealthIssue[];
  faviconState: FaviconState;
  /** Per-board health derived from hashrate-while-powered (P0-3). */
  boardHealth: BoardHealthSummary;
}

export const FRESH_TELEMETRY_MS = 15000;
export const RECENT_TELEMETRY_MS = 30000;

function telemetryAge(lastUpdate: number): number | null {
  if (!lastUpdate) {
    return null;
  }

  return Math.max(0, Date.now() - lastUpdate);
}

export function getDashboardHealth({
  status,
  wsConnected,
  lastUpdate,
  setupStatus,
}: {
  status: StatusResponse | null;
  wsConnected: boolean;
  lastUpdate: number;
  setupStatus?: SetupStatusResponse | null;
}): DashboardHealth {
  const ageMs = telemetryAge(lastUpdate);
  const hasFreshTelemetry = ageMs !== null && ageMs < FRESH_TELEMETRY_MS;
  const hasRecentTelemetry = ageMs !== null && ageMs < RECENT_TELEMETRY_MS;
  // Canonical whole-miner mining state (Omega P0-7 / C-8). Single source of
  // truth shared with the per-chain grid + every other surface so the topbar
  // chip + favicon never disagree with them off the same sample.
  const isMining = selectIsMining(status);
  const poolStatus = status?.pool?.status?.toLowerCase() ?? '';
  const poolDisconnected = poolStatus === 'disconnected' || poolStatus === 'dead';
  // FWT-3 / FWT-2: the daemon now surfaces these as distinct, actionable pool
  // states (the hybrid path previously collapsed everything to mining/connecting,
  // so a wrong-worker or all-rejecting pool produced no health signal at all).
  const poolAuthFailed = poolStatus === 'auth_failed';
  const poolRejecting = poolStatus === 'rejecting';
  const chains = Array.isArray(status?.chains) ? status.chains : [];
  const hotChain = chains.find(chain => chain.temp_c >= 70);
  const missingChain = chains.find(chain => chain.chips === 0 || chain.status?.toLowerCase().includes('dead'));
  const fanFailure = Boolean(status?.fans && status.fans.pwm > 20 && status.fans.rpm === 0);

  const issues: HealthIssue[] = [];

  if (!hasRecentTelemetry) {
    issues.push({
      key: 'telemetry-offline',
      level: 'critical',
      message: 'Miner telemetry is offline. Dashboard is waiting to reconnect.',
    });
  } else if (!hasFreshTelemetry) {
    issues.push({
      key: 'telemetry-stale',
      level: 'warning',
      message: 'Telemetry is stale. Dashboard values may be out of date.',
    });
  }

  if (poolDisconnected) {
    issues.push({
      key: 'pool-disconnected',
      level: isMining ? 'critical' : 'warning',
      message: isMining
        ? 'Pool is disconnected while the miner is still hashing.'
        : 'Pool connection is down. Shares will not submit until it reconnects.',
    });
  }

  if (poolAuthFailed) {
    issues.push({
      key: 'pool-auth-failed',
      level: 'critical',
      message:
        'Pool REJECTED your credentials. For solo pools the worker name must be a valid Bitcoin address; for regular pools check your account / worker settings. No shares can count until this is fixed.',
    });
  }

  if (poolRejecting) {
    issues.push({
      key: 'pool-rejecting',
      level: 'critical',
      message:
        'Pool is rejecting every submitted share. Check pool difficulty, worker configuration, or system clock — the miner is hashing but no shares are counting.',
    });
  }

  if (fanFailure) {
    issues.push({
      key: 'fan-failure',
      level: 'critical',
      message: 'Fan tachometer is zero while cooling is active. Check fans immediately.',
    });
  }

  if (hotChain) {
    issues.push({
      key: `hot-chain-${hotChain.id}`,
      level: 'critical',
      message: `Chain ${hotChain.id} temperature is critical.`,
    });
  }

  if (isMining && missingChain) {
    issues.push({
      key: `chain-missing-${missingChain.id}`,
      level: 'warning',
      message: `Chain ${missingChain.id} appears missing or unpowered while the miner is active.`,
    });
  }

  // P0-3: derive board health from hashrate-while-powered, NOT the status
  // string. Surfaces a 0-GH/s board the daemon still reports as "Active".
  const boardHealth = summarizeBoardHealth(chains, isMining);
  if (boardHealth.notHashing > 0) {
    const n = boardHealth.notHashing;
    const total = boardHealth.total;
    issues.push({
      key: 'board-not-hashing',
      level: boardHealth.faulted > 0 ? 'critical' : 'warning',
      message: `${n} of ${total} board${total === 1 ? '' : 's'} not hashing`
        + ` while the miner is active. Check the affected hashboard${n === 1 ? '' : 's'}.`,
    });
  }

  // Freedom-first: the operator deliberately chose to run without an owner
  // password. We honor that — but surface a dismissible, friendly
  // (warning, NOT critical) reminder so the choice stays visible and is
  // one click from being fixed. Self-clears the moment a password is set
  // (password_set true ⇒ condition false ⇒ issue not emitted ⇒ the bridge
  // clears the alert). Only emitted on an explicit opt-out, never on a
  // unit that simply hasn't finished setup.
  const passwordOptedOut = setupStatus?.password_opt_out === true
    || setupStatus?.auth?.password_opt_out === true;
  const passwordSet = setupStatus?.auth?.password_set === true;
  if (passwordOptedOut && !passwordSet) {
    issues.push({
      key: 'security:no-owner-password',
      level: 'warning',
      message:
        'No owner password is set. The dashboard is open to anyone on your network and write/control actions stay locked. Recommended: set one in Settings — you can keep it open if you prefer.',
      dismissible: true,
      action: { page: 'settings', anchor: 'security' },
    });
  }

  // Freedom-first (the EXACT parallel of the password advisory above):
  // the operator deliberately chose to run without completing the
  // circuit/breaker/safety check. We honor that — but surface a
  // dismissible, friendly (warning, NEVER critical — amber, not red)
  // reminder so the choice stays visible and is one click from being
  // fixed. Self-clears the moment the safety check is acknowledged
  // (firmware reconciliation clears safety_opt_out ⇒ condition false ⇒
  // issue not emitted ⇒ the bridge clears the alert). Only emitted on an
  // explicit opt-out, never on a unit that simply hasn't finished setup.
  // Coexists independently with the no-password advisory above — both can
  // show at once, each dismissed/self-cleared on its own.
  const safetyOptedOut = setupStatus?.safety_opt_out === true;
  const safetyAcked = setupStatus?.safety_opt_out === false
    && setupStatus?.safety_decision_made === true;
  if (safetyOptedOut && !safetyAcked) {
    issues.push({
      key: 'safety:circuit-check-not-done',
      level: 'warning',
      message:
        'The circuit/breaker check has not been completed. Recommended: verify your circuit can handle the load — the autotuner won’t cap power to your breaker until you do. Complete it in Settings; the dashboard and logs stay fully viewable in the meantime.',
      dismissible: true,
      action: { page: 'settings', anchor: 'circuit-safety' },
    });
  }

  const transportChip: HealthChip = !status && !wsConnected
    ? { label: 'No telemetry', tone: 'danger' }
    : !hasRecentTelemetry
      ? { label: 'No telemetry', tone: 'danger' }
      : !hasFreshTelemetry
        ? { label: 'Telemetry stale', tone: 'warning' }
        : wsConnected
          ? { label: 'WebSocket live', tone: 'info' }
          : { label: 'REST polling', tone: 'warning' };

  const minerChip: HealthChip = !status && wsConnected
    ? { label: 'Connecting', tone: 'warning' }
    : !hasRecentTelemetry
      ? { label: 'Offline', tone: 'danger' }
      : !hasFreshTelemetry
        ? { label: 'Stale data', tone: 'warning' }
        : isMining
          ? { label: 'Mining', tone: 'success' }
          : { label: 'Standby', tone: 'neutral' };

  const faviconState: FaviconState = !hasRecentTelemetry
    ? 'error'
    : issues.some(issue => issue.level === 'critical')
      ? 'error'
      : issues.some(issue => issue.level === 'warning')
        ? 'warning'
        : isMining
          ? 'mining'
          : 'standby';

  return {
    ageMs,
    hasFreshTelemetry,
    hasRecentTelemetry,
    minerChip,
    transportChip,
    issues: issues.slice(0, 4),
    faviconState,
    boardHealth,
  };
}
