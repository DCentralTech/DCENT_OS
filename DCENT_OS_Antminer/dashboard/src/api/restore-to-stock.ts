// Restore-to-Stock API client (W8-F backend, wave 8 + wave 9 hardening)
//
// Endpoints:
//   POST /api/system/restore-to-stock/preflight    dry-run safety preflight
//   POST /api/system/restore-to-stock              full flow (default dry-run; confirm:true triggers flash)
//   GET  /api/system/restore-to-stock/status       last preflight + flash attempt
//
// Body shape (from W8-F.md):
//   {
//     stock_firmware_staged_path,
//     stock_firmware_sha256?: string | null,
//     operator_serial_typed,
//     acknowledge_breaker_warning: boolean,
//     hashboard_count_to_use: number,   // default 1 server-side (W9-G hashboard count alignment for breaker safety on 15A residential circuits)
//     confirm_string_typed,             // must equal "RESTORE TO STOCK"
//     confirm: boolean                  // default false (dry-run)
//   }
//
// Wire-shape canonical source: dcentrald-api/src/routes/restore_to_stock.rs
//   - RestoreToStockResponse   (lines 654-679)
//   - SafetyFinding            (lines 682-695)
//   - SlotPlan                 (lines 708-716)
//   - RestoreToStockStatus     (status endpoint payload; lines 346-366)
//   - RestoreState             (W9-C structured phase machine; lines 374-418)
//
//  W9-E (R4-C2): aligned with backend wire names. The previous
// drift renamed backend fields silently and produced `undefined` in
// every JSX read. Closure source:
//

import { apiFetch } from './client';

// ---------------------------------------------------------------------------
// Severity vocabulary
// ---------------------------------------------------------------------------

export type SafetyFindingSeverity = 'critical' | 'high' | 'medium' | 'low' | 'info';

// ---------------------------------------------------------------------------
// SafetyFinding — wire shape from restore_to_stock.rs:683
// ---------------------------------------------------------------------------
//
// W9-E (R4-C2): backend wins. Renamed:
//   detector    → title         (the detector's human-readable label)
//   description → remediation   (text explaining how to clear it)
//   evidence    → matched_path  (path inside extracted tarball)

export interface SafetyFinding {
  id: string;
  severity: SafetyFindingSeverity;
  /** Human-readable detector label (e.g. "atlas@anthill.farm needle"). */
  title: string;
  /** Path inside the extracted tarball that triggered the match. May be null for negative findings. */
  matched_path?: string | null;
  /** Operator-facing remediation text. */
  remediation: string;
  /**
   * `true` when the operator cannot override this finding even with
   * `confirm:true`. Currently SECURE_BOOT_SET and the daemons:22322 IOC.
   */
  no_override: boolean;
}

// ---------------------------------------------------------------------------
// SlotPlan — wire shape from restore_to_stock.rs:709
// ---------------------------------------------------------------------------
//
// W9-E (R4-C2): replaced UI-invented `target_slot`/`source_sha256`/
// `unknown` with the real wire fields. The previous `unknown` boolean
// was always `undefined` and pretended slot detection had a tri-state.
// Backend ships only `active_slot`, `inactive_slot`, `inactive_mtd`;
// any of which may be null when `fw_printenv` is unavailable. The UI
// can render `inactive_slot == null` itself if it wants to surface
// "unknown" to the operator.

export interface SlotPlan {
  /** Currently booted slot ("a" / "b" / "1" / "2" — varies per platform). */
  active_slot?: string | null;
  /** Slot the daemon would write into. */
  inactive_slot?: string | null;
  /** MTD partition path (e.g. "/dev/mtd7") the daemon would `nandwrite` into. */
  inactive_mtd?: string | null;
}

// ---------------------------------------------------------------------------
// RestoreToStockResponse — wire shape from restore_to_stock.rs:655
// ---------------------------------------------------------------------------
//
// `status` is a fixed-vocab string. Known values:
//   "preflight_ok" | "dry_run" | "scheduled" | "rejected_<reason>"
// Future variants are accepted via the open string union escape hatch.

export type RestoreResponseStatus =
  | 'preflight_ok'
  | 'dry_run'
  | 'scheduled'
  | (string & {}); // rejected_* + future variants

export interface RestoreToStockResponse {
  status: RestoreResponseStatus;
  /** Human-readable reason for `rejected_*` statuses; null otherwise. */
  reason?: string | null;
  /** Path of the NAND backup tarball, if one was created. */
  backup_path?: string | null;
  /** Unix epoch ms when the reboot is scheduled, if scheduled. */
  reboot_at_ms?: number | null;
  /** Per-detector findings from the safety preflight. */
  safety_findings: SafetyFinding[];
  /** Computed SHA-256 of the staged tarball (always returned when staged). */
  staged_sha256?: string | null;
  /** Resolved active/inactive slot pair the daemon would write into. */
  slot_plan: SlotPlan;
  /** Operator-typed hashboard count, echoed back. */
  hashboard_count_to_use: number;
  /** Was this a dry-run? */
  dry_run: boolean;
  /**
   * Structured phase machine snapshot, when the backend captured one
   * before responding (W9-C). Optional because the dry-run /
   * preflight responses don't carry it; populated on synchronous
   * pre-schedule failure (e.g. backup couldn't allocate space) and
   * on the polled status endpoint.  W10-C: rendered by
   * RebootScheduled to surface flash_failed.reason in the UI.
   */
  state_detail?: RestoreState;
}

// ---------------------------------------------------------------------------
// RestoreState — W9-C structured phase machine
// (restore_to_stock.rs:374-418)
// ---------------------------------------------------------------------------
//
// Tagged-union with `phase` discriminator (snake_case). Currently the
// dashboard only renders `phase` for now; the per-variant payloads
// (reason, backup_path, completed_at_ms, reboot_at_ms) are available
// for richer rendering in W9-G.

export type RestoreStatePhase =
  | 'idle'
  | 'preflight_running'
  | 'preflight_failed'
  | 'preflight_ok'
  | 'nand_backup_running'
  | 'nand_backup_failed'
  | 'staging'
  | 'staging_failed'
  | 'scheduled'
  | 'flash_running'
  | 'flash_succeeded'
  | 'flash_failed';

export type RestoreState =
  | { phase: 'idle' }
  | { phase: 'preflight_running' }
  | { phase: 'preflight_failed'; reason: string }
  | { phase: 'preflight_ok' }
  | { phase: 'nand_backup_running' }
  | { phase: 'nand_backup_failed'; reason: string; backup_path?: string | null }
  | { phase: 'staging'; backup_path: string }
  | { phase: 'staging_failed'; reason: string; backup_path?: string | null }
  | { phase: 'scheduled'; reboot_at_ms: number; backup_path: string }
  | { phase: 'flash_running'; backup_path: string }
  | { phase: 'flash_succeeded'; completed_at_ms: number; backup_path: string }
  | { phase: 'flash_failed'; reason: string; backup_path?: string | null };

// ---------------------------------------------------------------------------
// RestoreToStockStatus — GET /api/system/restore-to-stock/status
// wire shape from restore_to_stock.rs:347-366
// ---------------------------------------------------------------------------
//
// W9-E (R4-C2): the previous nested `last_preflight` /
// `last_flash_attempt` / `in_progress` shape was a UI invention. The
// backend ships a flat envelope. The active phase lives in
// `state_detail` (W9-C); the legacy flat `state` string is kept as a
// stable label for dashboard rollups.

export interface RestoreToStockStatus {
  /** Stable string label matching the response-status vocabulary. */
  state: string;
  /** Structured phase machine (W9-C). Present once any work has started. */
  state_detail?: RestoreState;
  last_preflight_at_ms?: number | null;
  last_preflight_verdict?: string | null;
  last_backup_path?: string | null;
  last_scheduled_reboot_at_ms?: number | null;
  last_safety_findings: SafetyFinding[];
  last_active_slot?: string | null;
  last_inactive_slot?: string | null;
  /** Monotonically-increasing transition counter (W9-C). */
  transitions: number;
  /** Epoch-ms of the most recent state transition (W9-C). */
  last_transition_at_ms?: number | null;
  /**
   * -prep R1''-Q24: did the most recent NAND backup
   * successfully include `fw_setenv` for operator Option-A recovery?
   * `true` = present; `false` = copy attempted and failed (operator
   * MUST use Option B serial console); `null/undefined` = no backup
   * has run yet on this daemon lifetime. The dashboard surfaces this
   * BEFORE the operator pulls the destructive trigger so they can
   * decide whether to proceed without working in-stock recovery.
   */
  last_backup_fw_setenv_present?: boolean | null;
  /**
   *  W13-D (A2'-#1): VNish-style polled progress streaming.
   * Rolling buffer of the last ~100 stderr/stdout lines emitted by
   * the spawned writer (`revert_to_stock_*.sh`). Streamed line-by-line
   * so the dashboard can render live progress while phase is
   * `flash_running` (1-2 minute window). Stderr lines are prefixed
   * `[err] ` so the operator can distinguish them in the live pane.
   * `undefined` before the writer starts producing output (the
   * backend `skip_serializing_if`'s the empty case so old responses
   * pre-W13-D don't accidentally leak `[]`).
   */
  recent_log_lines?: string[];
}

// ---------------------------------------------------------------------------
// Request body shape — restore_to_stock.rs:597
// ---------------------------------------------------------------------------

export interface RestoreToStockRequest {
  stock_firmware_staged_path: string;
  stock_firmware_sha256?: string | null;
  operator_serial_typed: string;
  acknowledge_breaker_warning: boolean;
  hashboard_count_to_use: number;
  confirm_string_typed: string;
  confirm: boolean;
  /**
   * Operator acknowledgement that they have reviewed any HIGH-severity
   * safety findings (atlas SSH key, hotelfee.json, Hashcore root hash,
   * etc.) and accept them for this restore.  W9-G (R5-MEDIUM):
   * the dashboard now rounds the modal's `highAcknowledged` checkbox
   * state to the wire so the backend can refuse `confirm:true` when
   * HIGH findings exist but the operator never acknowledged them.
   * Default `false` server-side.
   */
  acknowledge_high_findings?: boolean;
}

async function unpackError(res: Response): Promise<never> {
  let body: unknown = null;
  try {
    body = await res.json();
  } catch {
    // ignore
  }
  const err = new Error(`HTTP ${res.status}`);
  (err as Error & { status?: number }).status = res.status;
  (err as Error & { body?: unknown }).body = body;
  throw err;
}

// ---------------------------------------------------------------------------
// PreflightChecks — wave-12 W12-C dynamic pre-flight checklist
// wire shape from restore_to_stock.rs:4936
// ---------------------------------------------------------------------------
//
// `GET /api/system/restore-to-stock/preflight-checks` — live probes
// for the operator-facing pre-flight checklist. Replaces (with static
// fallback) the wave-11 hardcoded `PREFLIGHT_ITEMS` in RestoreStatus.
//
// All `*_path` fields are `Option<String>` on the wire — `null` means
// the probe couldn't resolve the path on $PATH. `data_free_mib` is
// always reported (0 on probe failure). `platform_signature` is `null`
// only when /proc/cpuinfo isn't readable (Windows host tests).

export interface PreflightChecks {
  /** Resolved path to `setsid` (util-linux). */
  setsid_path: string | null;
  /** Resolved path to the per-platform revert script. */
  revert_script_path: string | null;
  /** Resolved path to `fw_setenv` (libubootenv-tools). */
  fw_setenv_path: string | null;
  /** Free MiB at `/data`. Gate is `>= 250 MiB`. */
  data_free_mib: number;
  /** Resolved path to `tar` (NAND-backup tarball step). */
  tar_path: string | null;
  /** Resolved path to `nandwrite` (per-platform revert script). */
  nandwrite_path: string | null;
  /** Resolved path to `flash_erase` (UBI revert scripts). */
  flash_erase_path: string | null;
  /** Platform fingerprint (e.g. `zynq-am1-bm1387`). */
  platform_signature: string | null;
  /** `true` when running platform has a PROFILE_TABLE entry (Layer 1). */
  platform_supported: boolean;
  /** `true` when supported AND `verified_revertable` (Layer 2). */
  platform_verified_revertable: boolean;
  /** All paths resolved + disk OK + supported + verified. */
  all_present: boolean;
}

export const restoreToStockApi = {
  preflight: async (req: Partial<RestoreToStockRequest>): Promise<RestoreToStockResponse> => {
    const res = await apiFetch('/api/system/restore-to-stock/preflight', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ...req, confirm: false }),
    });
    if (!res.ok) {
      // Preflight returns the safety_findings even when status begins
      // with "rejected_*" — try to surface that body to callers.
      let body: unknown = null;
      try { body = await res.json(); } catch { /* ignore */ }
      if (body && typeof body === 'object' && 'safety_findings' in body) {
        return body as RestoreToStockResponse;
      }
      await unpackError(res);
    }
    return res.json();
  },

  // confirm:false -> dry-run echo. confirm:true -> backend will NAND
  // backup, sysupgrade -f, and schedule reboot in 30s.
  submit: async (req: RestoreToStockRequest): Promise<RestoreToStockResponse> => {
    const res = await apiFetch('/api/system/restore-to-stock', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(req),
    });
    if (!res.ok) await unpackError(res);
    return res.json();
  },

  status: async (): Promise<RestoreToStockStatus> => {
    const res = await apiFetch('/api/system/restore-to-stock/status');
    if (!res.ok) await unpackError(res);
    return res.json();
  },

  /**
   *  W12-C: live pre-flight probe results. Always 200 from the
   * backend on a daemon-up environment; the dashboard falls back to a
   * static checklist on 404/503/network error per
   * .
   */
  preflightChecks: async (): Promise<PreflightChecks> => {
    const res = await apiFetch('/api/system/restore-to-stock/preflight-checks');
    if (!res.ok) await unpackError(res);
    return res.json();
  },
};

export const REQUIRED_CONFIRM_PHRASE = 'RESTORE TO STOCK';

// ---------------------------------------------------------------------------
// Status helpers (W9-E)
// ---------------------------------------------------------------------------
//
// Compatibility shim for callers that previously read
// `last_flash_attempt.reboot_at_ms`. The new flat shape stores the
// scheduled reboot in `last_scheduled_reboot_at_ms`. Keep this helper
// instead of re-introducing the nested shape.

export function statusRebootAtMs(status: RestoreToStockStatus | null | undefined): number | null {
  if (!status) return null;
  if (status.last_scheduled_reboot_at_ms != null) return status.last_scheduled_reboot_at_ms;
  // The structured phase machine also carries the reboot stamp during
  // the `scheduled` phase.
  if (status.state_detail && status.state_detail.phase === 'scheduled') {
    return status.state_detail.reboot_at_ms;
  }
  return null;
}

/**
 * `true` while a destructive flash is mid-flight. Replaces the
 * pre-W9 `RestoreToStockStatusResponse.in_progress` boolean (which
 * was always `undefined` against the real backend payload).
 */
export function statusInProgress(status: RestoreToStockStatus | null | undefined): boolean {
  if (!status || !status.state_detail) return false;
  switch (status.state_detail.phase) {
    case 'preflight_running':
    case 'nand_backup_running':
    case 'staging':
    case 'scheduled':
    case 'flash_running':
      return true;
    default:
      return false;
  }
}

// ---------------------------------------------------------------------------
//  W11-C — phase rendering + recovery guidance helpers
// ---------------------------------------------------------------------------

/**
 *  W11-C (A5''-OPS-MED-3): phase set the dashboard should keep
 * polling /status for. When `state_detail.phase` is in this set the
 * destructive flash is non-terminal and the operator should see live
 * progress. When it leaves this set (idle / preflight_failed /
 * preflight_ok / nand_backup_failed / staging_failed /
 * flash_succeeded / flash_failed) polling stops.
 */
export function isNonTerminalPhase(phase: RestoreStatePhase | undefined | null): boolean {
  if (!phase) return false;
  switch (phase) {
    case 'preflight_running':
    case 'nand_backup_running':
    case 'staging':
    case 'scheduled':
    case 'flash_running':
      return true;
    default:
      return false;
  }
}

/**
 * Operator-facing label for a phase. Used by RebootScheduled while
 * polling /status during the 1-2 min flash window.
 */
export function phaseLabel(phase: RestoreStatePhase | undefined | null): string {
  switch (phase) {
    case 'idle': return 'Idle';
    case 'preflight_running': return 'Running safety preflight...';
    case 'preflight_failed': return 'Preflight failed';
    case 'preflight_ok': return 'Preflight OK';
    case 'nand_backup_running': return 'Backing up NAND (mtd0/1/2)...';
    case 'nand_backup_failed': return 'NAND backup failed';
    case 'staging': return 'Staging stock firmware to inactive slot...';
    case 'staging_failed': return 'Staging failed';
    case 'scheduled': return 'Reboot scheduled — flash will run on next boot';
    case 'flash_running': return 'Flashing inactive slot (do NOT power-cycle)...';
    case 'flash_succeeded': return 'Flash succeeded';
    case 'flash_failed': return 'Flash failed';
    default: return 'Status unknown';
  }
}

export type RecoverySeverity = 'retry' | 'serial' | 'sd-card' | 'generic';

export interface RecoveryGuidance {
  text: string;
  severity: RecoverySeverity;
}

/**
 *  W11-C (A5''-OPS-MED-2): map common `flash_failed.reason`
 * substrings to a specific recovery action. Pattern-matches on
 * substrings the backend writers/preflight emit. Returns a generic
 * fallback when nothing matches.
 */
export function recoveryGuidanceFor(reason: string | undefined | null): RecoveryGuidance {
  const r = (reason ?? '').toLowerCase();
  if (!r) {
    return {
      severity: 'generic',
      text: 'See STOCK_BOOT_HARVEST_PROCEDURE.md §10. Active slot was not flipped, so a power-cycle returns to DCENT_OS.',
    };
  }
  if (r.includes('post-write magic readback') || r.includes('readback mismatch') || r.includes('readback')) {
    return {
      severity: 'retry',
      text: 'Try: SSH to miner, re-run script. Active slot is still bootable; a power-cycle returns to DCENT_OS.',
    };
  }
  if (r.includes('fw_setenv') || r.includes('ubootenv') || r.includes('libubootenv')) {
    return {
      severity: 'serial',
      text: 'fw_setenv failed. Use Option B: serial console U-Boot env edit (procedure §10). Wire your USB-TTL cable now.',
    };
  }
  if (r.includes('nandwrite') || r.includes('flash_erase')) {
    return {
      severity: 'retry',
      text: 'NAND write failed. Active slot is still bootable; power-cycle returns to DCENT_OS, then SSH back in and retry.',
    };
  }
  if (r.includes('toctou') || r.includes('fingerprint') || r.includes('staged_sha256') || r.includes('drift')) {
    return {
      severity: 'retry',
      text: 'Staged tarball drifted between preflight and flash. Re-stage from the source file and retry.',
    };
  }
  return {
    severity: 'sd-card',
    text: 'Unmapped failure mode. See STOCK_BOOT_HARVEST_PROCEDURE.md §10. Worst case: SD-card recovery via JP4 jumper (§11; wave-11 backlog: §11 needs writing).',
  };
}
