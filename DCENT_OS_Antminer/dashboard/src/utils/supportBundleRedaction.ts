export const SUPPORT_BUNDLE_REDACTED = '<redacted>';

// Sensitive key names, matched at the END of a key. Beyond credentials, this
// covers non-credential PII that a "safe to share" bundle must not leak:
// the pool worker string (which for Bitcoin is `<btc-payout-address>.<rig>`),
// wallet/payout addresses, MAC/BSSID, and device serials.
const SENSITIVE_KEY_RE =
  /(password|passwd|pwd|secret|authorization|apikey|api[_-]?key|privatekey|private[_-]?key|token|worker|worker[_-]?name|wallet|payout|bssid|serial|mac|mac[_-]?addr(?:ess)?)$/i;

export function redactSupportBundlePayload<T>(payload: T): T {
  const seen = new WeakSet<object>();
  return redactUnknown(payload, seen) as T;
}

function redactUnknown(value: unknown, seen: WeakSet<object>): unknown {
  if (typeof value === 'string') return redactSensitiveString(value);
  if (value === null || typeof value !== 'object') return value;
  if (seen.has(value)) return '[Circular]';
  seen.add(value);

  if (Array.isArray(value)) {
    return value.map(item => redactUnknown(item, seen));
  }

  const source = value as Record<string, unknown>;
  const redacted: Record<string, unknown> = {};
  for (const [key, item] of Object.entries(source)) {
    if (SENSITIVE_KEY_RE.test(key) && typeof item === 'string' && item.length > 0) {
      redacted[key] = SUPPORT_BUNDLE_REDACTED;
      continue;
    }
    redacted[key] = redactUnknown(item, seen);
  }
  return redacted;
}

export function redactSensitiveString(value: string): string {
  return value
    .replace(
      /\b(Authorization\s*:\s*)(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]+/gi,
      (_match, prefix: string, scheme: string) => `${prefix}${scheme} ${SUPPORT_BUNDLE_REDACTED}`,
    )
    .replace(
      /\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{8,}/gi,
      (_match, scheme: string) => `${scheme} ${SUPPORT_BUNDLE_REDACTED}`,
    )
    .replace(
      /([a-z][a-z0-9+.-]*:\/\/)([^/\s:@]+):([^@\s/]+)@/gi,
      (_match, prefix: string) => `${prefix}${SUPPORT_BUNDLE_REDACTED}@`,
    )
    .replace(
      /(["']?(?:password|passwd|pwd|token|secret|api[_-]?key|authorization)["']?\s*[:=]\s*["']?)([^"',\s;}]+)(["']?)/gi,
      (_match, prefix: string, _secret: string, suffix: string) => `${prefix}${SUPPORT_BUNDLE_REDACTED}${suffix}`,
    )
    // Bitcoin bech32 wallet/worker addresses embedded in free text (worker
    // strings or pool URLs logged as a message). Length-bounded so short tokens
    // aren't matched; real addresses are ~42-62 chars.
    .replace(/\b(?:bc1|tb1)[a-z0-9]{25,71}\b/gi, SUPPORT_BUNDLE_REDACTED)
    // MAC addresses (colon-separated) embedded in free text. Colonless MACs are
    // deliberately NOT matched in free text (too false-positive-prone vs bare
    // 12-hex tokens); the `mac`/`bssid` key redaction above catches the field
    // value regardless of format.
    .replace(/\b(?:[0-9a-f]{2}:){5}[0-9a-f]{2}\b/gi, SUPPORT_BUNDLE_REDACTED);
}
