import { formatHashrate } from '../utils/format';

const DEFAULT_TITLE = 'DCENT_OS';
const TELEMETRY_RENDER_INTERVAL_MS = 10_000;
const ALERT_FLASH_MS = 10_000;

let baseTitle = DEFAULT_TITLE;
let alertMessage: string | null = null;
let alertTimer: ReturnType<typeof setTimeout> | null = null;
let lastTelemetryRenderAt = Number.NEGATIVE_INFINITY;

function getDocument(doc: Document | undefined = globalThis.document): Document | null {
  return doc ?? null;
}

function renderTitle(doc: Document | undefined = globalThis.document): void {
  const target = getDocument(doc);
  if (!target) return;
  target.title = alertMessage ? `[!] ${alertMessage} - ${DEFAULT_TITLE}` : baseTitle;
}

function titleFromTelemetry(hashrateGhs: number, hasRecentTelemetry: boolean, enabled: boolean): string {
  if (!enabled || !hasRecentTelemetry || !Number.isFinite(hashrateGhs) || hashrateGhs <= 0) {
    return DEFAULT_TITLE;
  }
  return `${DEFAULT_TITLE} - ${formatHashrate(hashrateGhs)}`;
}

export function updateTitleTicker({
  hashrateGhs,
  hasRecentTelemetry,
  enabled,
  now = Date.now(),
  doc,
}: {
  hashrateGhs: number;
  hasRecentTelemetry: boolean;
  enabled: boolean;
  now?: number;
  doc?: Document;
}): void {
  const nextTitle = titleFromTelemetry(hashrateGhs, hasRecentTelemetry, enabled);
  const staleOrDisabled = nextTitle === DEFAULT_TITLE;
  if (!staleOrDisabled && now - lastTelemetryRenderAt < TELEMETRY_RENDER_INTERVAL_MS) {
    return;
  }
  lastTelemetryRenderAt = now;
  if (baseTitle === nextTitle && !alertMessage) return;
  baseTitle = nextTitle;
  renderTitle(doc);
}

export function flashAlert(message: string, durationMs = ALERT_FLASH_MS, doc: Document | undefined = globalThis.document): void {
  alertMessage = message;
  renderTitle(doc);

  if (alertTimer !== null) {
    globalThis.clearTimeout(alertTimer);
  }
  alertTimer = globalThis.setTimeout(() => {
    alertMessage = null;
    alertTimer = null;
    renderTitle(doc);
  }, durationMs);
}

export function resetTitleTicker(doc: Document | undefined = globalThis.document): void {
  if (alertTimer !== null) {
    globalThis.clearTimeout(alertTimer);
    alertTimer = null;
  }
  alertMessage = null;
  baseTitle = DEFAULT_TITLE;
  lastTelemetryRenderAt = Number.NEGATIVE_INFINITY;
  renderTitle(doc);
}
