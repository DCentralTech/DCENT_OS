#!/usr/bin/env node
// Read-only DCENT_axe endpoint truth smoke.
//
// This is intentionally separate from soak-dispatcher-metrics.ps1 because
// /api/system/info and MCP status are heavier validation endpoints. Use this
// once before/after a firmware flash or at low frequency during manual checks.

import http from "node:http";
import { once } from "node:events";

const MCP_PROTOCOL = "2024-11-05";
const DEFAULT_TIMEOUT_MS = 5000;
const DEFAULT_CROSS_TOLERANCE = 4;
const DEFAULT_DISPATCHER_TOLERANCE = 64;
const DEFAULT_MAX_PENDING_AGE_MS = 65_000;

const READ_ONLY_TOOLS = [
  "get_status",
  "get_device_info",
  "get_asic_info",
  "get_network",
  "get_history",
  "get_swarm_status",
  "get_swarm",
];

const WRITE_TOOLS = [
  "set_frequency",
  "set_core_voltage",
  "set_fan_speed",
  "set_pool",
  "restart_mining",
  "identify_device",
  "run_autotune",
];

const RESOURCES = [
  "bitaxe://status",
  "bitaxe://config",
  "bitaxe://history",
  "bitaxe://swarm",
];

const REQUIRED_METRICS = [
  ["dcentaxe_stratum_shares_pending"],
  ["dcentaxe_stratum_shares_unresolved_total"],
  ["dcentaxe_stratum_oldest_pending_submit_age_ms"],
  ["dcentaxe_dispatcher_stale_nonces_total"],
  ["dcentaxe_dispatcher_slot_recoveries_total"],
  ["dcentaxe_dispatcher_filtered_nonces_total"],
  ["dcentaxe_dispatcher_ticket_difficulty"],
  ["dcentaxe_free_heap_bytes"],
  ["dcentaxe_uptime_seconds"],
  ["dcentaxe_mining_enabled"],
  ["dcentaxe_thermal_sensors_ok"],
  ["dcentaxe_fan_rpm", { fan: "1" }],
  ["dcentaxe_fan_rpm", { fan: "2" }],
];

class SmokeFailure extends Error {
  constructor(errors) {
    super(errors.join("\n"));
    this.name = "SmokeFailure";
    this.errors = errors;
  }
}

function usage() {
  return `Usage:
  node scripts/smoke-pool-truth.mjs --base-url http://203.0.113.132 [options]
  node scripts/smoke-pool-truth.mjs --self-test success

Options:
  --base-url URL                 DCENT_axe base URL
  --bearer-token TOKEN           MCP/API bearer token, or use DCENTAXE_TOKEN
  --timeout-ms N                 Request timeout per endpoint (default ${DEFAULT_TIMEOUT_MS})
  --cross-tolerance N            Counter tolerance across separate endpoint reads (default ${DEFAULT_CROSS_TOLERANCE})
  --dispatcher-tolerance N       Dispatcher counter tolerance across endpoint reads (default ${DEFAULT_DISPATCHER_TOLERANCE})
  --max-pending-age-ms N         Healthy pending submit age ceiling (default ${DEFAULT_MAX_PENDING_AGE_MS})
  --strict-cross-checks          Require exact cross-endpoint metric/API matches
  --self-test CASE               success | missing-metric | bad-json | mcp-error | recent-events-over-cap | missing-field
`;
}

function parseArgs(argv) {
  const opts = {
    baseUrl: "",
    bearerToken: process.env.DCENTAXE_TOKEN || "",
    timeoutMs: DEFAULT_TIMEOUT_MS,
    crossTolerance: DEFAULT_CROSS_TOLERANCE,
    dispatcherTolerance: DEFAULT_DISPATCHER_TOLERANCE,
    maxPendingAgeMs: DEFAULT_MAX_PENDING_AGE_MS,
    strictCrossChecks: false,
    selfTest: "",
  };

  for (let i = 2; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => {
      i += 1;
      if (i >= argv.length) throw new Error(`Missing value for ${arg}`);
      return argv[i];
    };
    if (arg === "--base-url") opts.baseUrl = next();
    else if (arg === "--bearer-token") opts.bearerToken = next();
    else if (arg === "--timeout-ms") opts.timeoutMs = positiveInt(next(), arg);
    else if (arg === "--cross-tolerance") opts.crossTolerance = nonNegativeNumber(next(), arg);
    else if (arg === "--dispatcher-tolerance") opts.dispatcherTolerance = nonNegativeNumber(next(), arg);
    else if (arg === "--max-pending-age-ms") opts.maxPendingAgeMs = nonNegativeNumber(next(), arg);
    else if (arg === "--strict-cross-checks") opts.strictCrossChecks = true;
    else if (arg === "--self-test") opts.selfTest = next();
    else if (arg === "--help" || arg === "-h") {
      console.log(usage());
      process.exit(0);
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }

  if (opts.strictCrossChecks) {
    opts.crossTolerance = 0;
    opts.dispatcherTolerance = 0;
  }
  if (!opts.selfTest && !opts.baseUrl) {
    throw new Error("Missing --base-url");
  }
  if (opts.baseUrl) opts.baseUrl = normalizeBaseUrl(opts.baseUrl);
  return opts;
}

function positiveInt(value, flag) {
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`${flag} must be a positive integer`);
  }
  return parsed;
}

function nonNegativeNumber(value, flag) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 0) {
    throw new Error(`${flag} must be a non-negative number`);
  }
  return parsed;
}

function normalizeBaseUrl(url) {
  let out = url.trim();
  if (!out.startsWith("http://") && !out.startsWith("https://")) {
    out = `http://${out}`;
  }
  return out.replace(/\/+$/, "");
}

function headers(opts, hasBody = false) {
  const out = { "X-Requested-With": "XMLHttpRequest" };
  if (opts.bearerToken) out.Authorization = `Bearer ${opts.bearerToken}`;
  if (hasBody) out["Content-Type"] = "application/json";
  return out;
}

async function requestText(opts, path, init = {}) {
  const method = init.method || "GET";
  const url = `${opts.baseUrl}${path}`;
  const response = await fetch(url, {
    method,
    headers: headers(opts, Boolean(init.body)),
    body: init.body,
    signal: AbortSignal.timeout(opts.timeoutMs),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`${method} ${path} HTTP ${response.status}: ${text.slice(0, 160)}`);
  }
  return { response, text };
}

async function requestJson(opts, path, init = {}) {
  const { response, text } = await requestText(opts, path, init);
  try {
    return { response, json: JSON.parse(text), text };
  } catch (error) {
    throw new Error(`${init.method || "GET"} ${path} returned invalid JSON: ${error.message}`);
  }
}

async function jsonRpc(opts, method, params = {}, id = method) {
  const { json } = await requestJson(opts, "/mcp", {
    method: "POST",
    body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
  });
  if (json.jsonrpc !== "2.0") {
    throw new Error(`MCP ${method} response missing jsonrpc=2.0`);
  }
  if (JSON.stringify(json.id) !== JSON.stringify(id)) {
    throw new Error(`MCP ${method} response id mismatch`);
  }
  if (json.error) {
    throw new Error(`MCP ${method} error ${json.error.code}: ${json.error.message}`);
  }
  if (!Object.hasOwn(json, "result")) {
    throw new Error(`MCP ${method} response missing result`);
  }
  return json.result;
}

function asPools(value) {
  if (Array.isArray(value)) return value;
  if (value && Array.isArray(value.pools)) return value.pools;
  throw new Error("/api/pools must return an array or an object with pools[]");
}

function get(obj, path) {
  return path.reduce((cur, key) => (cur == null ? undefined : cur[key]), obj);
}

function requireObject(ctx, value, path) {
  if (value == null || typeof value !== "object" || Array.isArray(value)) {
    ctx.errors.push(`${path} must be an object`);
    return {};
  }
  return value;
}

function requireArray(ctx, value, path, maxLen = undefined) {
  if (!Array.isArray(value)) {
    ctx.errors.push(`${path} must be an array`);
    return [];
  }
  if (maxLen !== undefined && value.length > maxLen) {
    ctx.errors.push(`${path}.length ${value.length} exceeds ${maxLen}`);
  }
  return value;
}

function requireString(ctx, value, path) {
  if (typeof value !== "string") ctx.errors.push(`${path} must be a string`);
}

function requireBool(ctx, value, path) {
  if (typeof value !== "boolean") ctx.errors.push(`${path} must be a boolean`);
}

function requireNumber(ctx, value, path) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    ctx.errors.push(`${path} must be a finite non-negative number`);
    return 0;
  }
  return value;
}

function requireIntegerish(ctx, value, path) {
  const n = requireNumber(ctx, value, path);
  if (!Number.isInteger(n)) ctx.errors.push(`${path} must be an integer`);
  return n;
}

function approxEqual(ctx, actual, expected, tolerance, path) {
  if (!Number.isFinite(actual) || !Number.isFinite(expected)) {
    ctx.errors.push(`${path} cannot compare non-finite values`);
    return;
  }
  if (Math.abs(actual - expected) > tolerance) {
    ctx.errors.push(`${path} ${actual} differs from expected ${expected} by more than ${tolerance}`);
  }
}

function assertAccounting(ctx, values, path) {
  const submitted = requireIntegerish(ctx, values.submitted, `${path}.shares_submitted`);
  const accepted = requireIntegerish(ctx, values.accepted, `${path}.shares_accepted`);
  const rejected = requireIntegerish(ctx, values.rejected, `${path}.shares_rejected`);
  const pending = requireIntegerish(ctx, values.pending, `${path}.shares_pending`);
  const unresolved = requireIntegerish(ctx, values.unresolved, `${path}.shares_unresolved`);
  const oldest = requireIntegerish(ctx, values.oldestAge, `${path}.oldest_pending_submit_age_ms`);
  const sum = accepted + rejected + pending + unresolved;

  if (submitted !== sum) {
    ctx.errors.push(`${path} accounting gap: submitted ${submitted}, accepted+rejected+pending+unresolved ${sum}`);
  }
  if (pending > 64) ctx.errors.push(`${path}.shares_pending ${pending} exceeds queue cap 64`);
  if (pending === 0 && oldest !== 0) {
    ctx.errors.push(`${path}.oldest_pending_submit_age_ms must be 0 when shares_pending is 0`);
  }
  if (pending > 0 && oldest > ctx.opts.maxPendingAgeMs) {
    ctx.errors.push(`${path}.oldest_pending_submit_age_ms ${oldest} exceeds ${ctx.opts.maxPendingAgeMs}`);
  }
}

function assertEventsNondecreasing(ctx, events, path) {
  let previous = -1;
  for (let i = 0; i < events.length; i += 1) {
    const ts = events[i]?.tsUnixMs ?? events[i]?.ts_unix_ms;
    if (typeof ts !== "number" || !Number.isFinite(ts) || ts < 0) {
      ctx.errors.push(`${path}[${i}].tsUnixMs must be a finite non-negative number`);
      continue;
    }
    if (ts < previous) ctx.errors.push(`${path} timestamps must be nondecreasing`);
    previous = ts;
  }
}

function validateSystemInfo(ctx, info) {
  const dcentaxe = requireObject(ctx, info.dcentaxe, "/api/system/info.dcentaxe");
  const poolTruth = requireObject(ctx, dcentaxe.poolTruth, "/api/system/info.dcentaxe.poolTruth");
  const dispatcher = requireObject(ctx, dcentaxe.dispatcher, "/api/system/info.dcentaxe.dispatcher");

  requireString(ctx, poolTruth.activePool, "poolTruth.activePool");
  requireBool(ctx, poolTruth.connected, "poolTruth.connected");
  requireBool(ctx, poolTruth.failoverActive, "poolTruth.failoverActive");
  requireString(ctx, poolTruth.lastRejectReason, "poolTruth.lastRejectReason");
  requireNumber(ctx, poolTruth.difficulty, "poolTruth.difficulty");
  requireNumber(ctx, poolTruth.responseTimeMs, "poolTruth.responseTimeMs");
  const recentEvents = requireArray(ctx, poolTruth.recentEvents, "poolTruth.recentEvents", 8);
  assertEventsNondecreasing(ctx, recentEvents, "poolTruth.recentEvents");
  requireArray(ctx, poolTruth.rejectReasonCounts, "poolTruth.rejectReasonCounts", 8);

  assertAccounting(ctx, {
    submitted: poolTruth.sharesSubmitted,
    accepted: poolTruth.sharesAccepted,
    rejected: poolTruth.sharesRejected,
    pending: poolTruth.sharesPending,
    unresolved: poolTruth.sharesUnresolved,
    oldestAge: poolTruth.oldestPendingSubmitAgeMs,
  }, "poolTruth");

  for (const key of ["staleNonces", "slotRecoveries", "filteredNonces", "noncesFound", "ticketDifficulty"]) {
    requireNumber(ctx, dispatcher[key], `dispatcher.${key}`);
  }

  return { poolTruth, dispatcher };
}

function validatePools(ctx, pools) {
  if (pools.length < 1) ctx.errors.push("/api/pools must include at least one pool");
  for (const pool of pools) {
    const path = `/api/pools[${pool?.index ?? "?"}]`;
    requireIntegerish(ctx, pool.index, `${path}.index`);
    requireString(ctx, pool.url, `${path}.url`);
    requireString(ctx, pool.worker, `${path}.worker`);
    requireBool(ctx, pool.connected, `${path}.connected`);
    requireBool(ctx, pool.authorized, `${path}.authorized`);
    requireBool(ctx, pool.failover_active, `${path}.failover_active`);
    requireNumber(ctx, pool.difficulty, `${path}.difficulty`);
    requireNumber(ctx, pool.response_time_ms, `${path}.response_time_ms`);
    requireString(ctx, pool.last_reject_reason, `${path}.last_reject_reason`);
    const events = requireArray(ctx, pool.recent_events, `${path}.recent_events`, 64);
    assertEventsNondecreasing(ctx, events, `${path}.recent_events`);
    requireArray(ctx, pool.reject_reason_counts, `${path}.reject_reason_counts`, 8);
    assertAccounting(ctx, {
      submitted: pool.shares_submitted,
      accepted: pool.shares_accepted,
      rejected: pool.shares_rejected,
      pending: pool.shares_pending,
      unresolved: pool.shares_unresolved,
      oldestAge: pool.oldest_pending_submit_age_ms,
    }, path);
  }
  return {
    pending: sum(pools, "shares_pending"),
    unresolved: sum(pools, "shares_unresolved"),
    oldestAge: pools.reduce((max, pool) => Math.max(max, Number(pool.oldest_pending_submit_age_ms) || 0), 0),
    accepted: sum(pools, "shares_accepted"),
    rejected: sum(pools, "shares_rejected"),
  };
}

function sum(items, key) {
  return items.reduce((acc, item) => acc + (Number(item?.[key]) || 0), 0);
}

function parseMetrics(ctx, text) {
  const samples = [];
  const errorsBefore = ctx.errors.length;
  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;
    const parts = line.split(/\s+/);
    if (parts.length < 2) continue;
    const token = parts[0];
    const value = Number(parts[1]);
    const name = token.replace(/\{.*$/, "");
    const labels = {};
    const labelText = token.match(/\{(.+)\}$/)?.[1];
    if (labelText) {
      for (const match of labelText.matchAll(/([A-Za-z0-9_]+)="([^"]*)"/g)) {
        labels[match[1]] = match[2];
      }
    }
    if (!Number.isFinite(value)) {
      ctx.errors.push(`/metrics ${token} value is not numeric`);
      continue;
    }
    samples.push({ name, labels, value, token });
  }
  if (samples.length === 0 && ctx.errors.length === errorsBefore) {
    ctx.errors.push("/metrics contained no parseable samples");
  }
  return samples;
}

function findMetric(samples, name, labels = {}) {
  return samples.find((sample) => {
    if (sample.name !== name) return false;
    return Object.entries(labels).every(([key, value]) => sample.labels[key] === value);
  });
}

function requireMetric(ctx, samples, name, labels = {}) {
  const sample = findMetric(samples, name, labels);
  const suffix = Object.keys(labels).length ? JSON.stringify(labels) : "";
  if (!sample) {
    ctx.errors.push(`/metrics missing ${name}${suffix}`);
    return 0;
  }
  return sample.value;
}

function validateMetrics(ctx, samples) {
  const values = {};
  for (const [name, labels] of REQUIRED_METRICS) {
    const key = labels ? `${name}:${Object.values(labels).join(":")}` : name;
    values[key] = requireMetric(ctx, samples, name, labels || {});
  }
  return {
    pending: values.dcentaxe_stratum_shares_pending,
    unresolved: values.dcentaxe_stratum_shares_unresolved_total,
    oldestAge: values.dcentaxe_stratum_oldest_pending_submit_age_ms,
    stale: values.dcentaxe_dispatcher_stale_nonces_total,
    recoveries: values.dcentaxe_dispatcher_slot_recoveries_total,
    filtered: values.dcentaxe_dispatcher_filtered_nonces_total,
    ticketDifficulty: values.dcentaxe_dispatcher_ticket_difficulty,
  };
}

function assertToolList(ctx, tools) {
  const names = tools.map((tool) => tool?.name).filter(Boolean);
  for (const name of READ_ONLY_TOOLS) {
    if (!names.includes(name)) ctx.errors.push(`MCP tools/list missing ${name}`);
  }
  for (const name of WRITE_TOOLS) {
    if (!names.includes(name)) ctx.errors.push(`MCP tools/list missing declared write-capable tool ${name}`);
  }
}

function assertResourceList(ctx, resources) {
  const byUri = new Map(resources.map((resource) => [resource?.uri, resource]));
  for (const uri of RESOURCES) {
    const resource = byUri.get(uri);
    if (!resource) {
      ctx.errors.push(`MCP resources/list missing ${uri}`);
      continue;
    }
    if (resource.mimeType !== "application/json") {
      ctx.errors.push(`MCP resource ${uri} mimeType must be application/json`);
    }
  }
}

function parseMcpTextJson(ctx, result, path) {
  const content = result?.content;
  if (!Array.isArray(content) || content[0]?.type !== "text" || typeof content[0]?.text !== "string") {
    ctx.errors.push(`${path} must return result.content[0].text`);
    return {};
  }
  try {
    return JSON.parse(content[0].text);
  } catch (error) {
    ctx.errors.push(`${path} text is not valid JSON: ${error.message}`);
    return {};
  }
}

function validateMcpStatus(ctx, status) {
  const poolTruth = requireObject(ctx, status.pool_truth, "MCP get_status.pool_truth");
  const pools = requireArray(ctx, status.pools, "MCP get_status.pools");

  for (const key of ["filtered_nonces", "stale_nonces", "slot_recoveries", "ticket_difficulty"]) {
    requireNumber(ctx, status[key], `MCP get_status.${key}`);
  }
  requireNumber(ctx, status.pool_shares_pending, "MCP get_status.pool_shares_pending");
  requireNumber(ctx, status.pool_shares_unresolved, "MCP get_status.pool_shares_unresolved");
  requireNumber(ctx, status.pool_oldest_pending_submit_age_ms, "MCP get_status.pool_oldest_pending_submit_age_ms");

  if (Object.keys(poolTruth).length > 0) {
    assertAccounting(ctx, {
      submitted: poolTruth.shares_submitted,
      accepted: poolTruth.shares_accepted,
      rejected: poolTruth.shares_rejected,
      pending: poolTruth.shares_pending,
      unresolved: poolTruth.shares_unresolved,
      oldestAge: poolTruth.oldest_pending_submit_age_ms,
    }, "MCP pool_truth");
    requireNumber(ctx, poolTruth.shares_accounting_gap, "MCP pool_truth.shares_accounting_gap");
    if (poolTruth.shares_accounting_gap !== 0) {
      ctx.errors.push(`MCP pool_truth.shares_accounting_gap must be 0, got ${poolTruth.shares_accounting_gap}`);
    }
    const events = requireArray(ctx, poolTruth.recent_events, "MCP pool_truth.recent_events", 8);
    assertEventsNondecreasing(ctx, events, "MCP pool_truth.recent_events");
    requireArray(ctx, poolTruth.reject_reason_counts, "MCP pool_truth.reject_reason_counts", 8);
  }

  for (const pool of pools) {
    const path = `MCP pools[${pool?.index ?? "?"}]`;
    assertAccounting(ctx, {
      submitted: pool.shares_submitted,
      accepted: pool.shares_accepted,
      rejected: pool.shares_rejected,
      pending: pool.shares_pending,
      unresolved: pool.shares_unresolved,
      oldestAge: pool.oldest_pending_submit_age_ms,
    }, path);
    requireNumber(ctx, pool.shares_accounting_gap, `${path}.shares_accounting_gap`);
    if (pool.shares_accounting_gap !== 0) {
      ctx.errors.push(`${path}.shares_accounting_gap must be 0, got ${pool.shares_accounting_gap}`);
    }
  }

  return { poolTruth, pools };
}

async function runSmoke(opts, quiet = false) {
  const ctx = { opts, errors: [], checks: [] };

  const { json: info } = await requestJson(opts, "/api/system/info");
  ctx.checks.push("/api/system/info");
  const systemTruth = validateSystemInfo(ctx, info);

  const poolsResponse = await requestJson(opts, "/api/pools");
  ctx.checks.push("/api/pools");
  const pools = asPools(poolsResponse.json);
  const poolSums = validatePools(ctx, pools);

  const { text: metricsText } = await requestText(opts, "/metrics");
  ctx.checks.push("/metrics");
  const metrics = validateMetrics(ctx, parseMetrics(ctx, metricsText));

  const mcpMeta = (await requestJson(opts, "/mcp")).json;
  ctx.checks.push("GET /mcp");
  if (mcpMeta.name !== "dcentaxe") ctx.errors.push("GET /mcp name must be dcentaxe");
  if (mcpMeta.protocol !== MCP_PROTOCOL) ctx.errors.push(`GET /mcp protocol must be ${MCP_PROTOCOL}`);
  if (mcpMeta.transport !== "http-jsonrpc") ctx.errors.push("GET /mcp transport must be http-jsonrpc");

  const init = await jsonRpc(opts, "initialize", {}, "initialize");
  ctx.checks.push("MCP initialize");
  if (init.protocolVersion !== MCP_PROTOCOL) ctx.errors.push(`MCP initialize protocolVersion must be ${MCP_PROTOCOL}`);
  requireObject(ctx, init.capabilities?.tools, "MCP initialize.capabilities.tools");
  requireObject(ctx, init.capabilities?.resources, "MCP initialize.capabilities.resources");

  const ping = await jsonRpc(opts, "ping", {}, "ping");
  ctx.checks.push("MCP ping");
  if (Object.keys(ping).length !== 0) ctx.errors.push("MCP ping result must be an empty object");

  const toolsList = await jsonRpc(opts, "tools/list", {}, "tools/list");
  ctx.checks.push("MCP tools/list");
  assertToolList(ctx, requireArray(ctx, toolsList.tools, "MCP tools/list.tools"));

  const resourcesList = await jsonRpc(opts, "resources/list", {}, "resources/list");
  ctx.checks.push("MCP resources/list");
  assertResourceList(ctx, requireArray(ctx, resourcesList.resources, "MCP resources/list.resources"));

  const mcpStatusResult = await jsonRpc(opts, "tools/call", { name: "get_status", arguments: {} }, "status");
  ctx.checks.push("MCP tools/call get_status");
  const mcpStatus = validateMcpStatus(ctx, parseMcpTextJson(ctx, mcpStatusResult, "MCP get_status"));

  const statusResource = await jsonRpc(opts, "resources/read", { uri: "bitaxe://status" }, "resource-status");
  ctx.checks.push("MCP resources/read bitaxe://status");
  const contents = requireArray(ctx, statusResource.contents, "MCP resources/read.contents");
  if (contents[0]?.uri !== "bitaxe://status") ctx.errors.push("MCP status resource URI mismatch");
  if (contents[0]?.mimeType !== "application/json") ctx.errors.push("MCP status resource mimeType must be application/json");
  if (typeof contents[0]?.text === "string") {
    try {
      JSON.parse(contents[0].text);
    } catch (error) {
      ctx.errors.push(`MCP status resource text is not valid JSON: ${error.message}`);
    }
  } else {
    ctx.errors.push("MCP status resource text must be a string");
  }

  crossCheck(ctx, systemTruth, poolSums, metrics, mcpStatus, pools);

  if (ctx.errors.length > 0) throw new SmokeFailure(ctx.errors);
  if (!quiet) {
    console.log(JSON.stringify({
      ok: true,
      baseUrl: opts.baseUrl,
      checks: ctx.checks,
      pools: pools.length,
      pending: poolSums.pending,
      unresolved: poolSums.unresolved,
      oldestPendingSubmitAgeMs: poolSums.oldestAge,
    }, null, 2));
  }
  return ctx;
}

function crossCheck(ctx, systemTruth, poolSums, metrics, mcpStatus, pools) {
  const shareTol = ctx.opts.crossTolerance;
  const dispatchTol = ctx.opts.dispatcherTolerance;
  approxEqual(ctx, poolSums.pending, metrics.pending, shareTol, "sum(/api/pools.shares_pending) vs /metrics pending");
  approxEqual(ctx, poolSums.unresolved, metrics.unresolved, shareTol, "sum(/api/pools.shares_unresolved) vs /metrics unresolved");
  approxEqual(ctx, poolSums.oldestAge, metrics.oldestAge, ctx.opts.strictCrossChecks ? 0 : ctx.opts.maxPendingAgeMs, "max(/api/pools.oldest_pending_submit_age_ms) vs /metrics oldest age");

  approxEqual(ctx, systemTruth.dispatcher.staleNonces, metrics.stale, dispatchTol, "system dispatcher.staleNonces vs /metrics");
  approxEqual(ctx, systemTruth.dispatcher.slotRecoveries, metrics.recoveries, dispatchTol, "system dispatcher.slotRecoveries vs /metrics");
  approxEqual(ctx, systemTruth.dispatcher.filteredNonces, metrics.filtered, dispatchTol, "system dispatcher.filteredNonces vs /metrics");
  approxEqual(ctx, systemTruth.dispatcher.ticketDifficulty, metrics.ticketDifficulty, 0, "system dispatcher.ticketDifficulty vs /metrics");

  const systemPool = systemTruth.poolTruth;
  if (pools.length === 1) {
    approxEqual(ctx, Number(systemPool.sharesAccepted), poolSums.accepted, shareTol, "system poolTruth.sharesAccepted vs /api/pools sum");
    approxEqual(ctx, Number(systemPool.sharesRejected), poolSums.rejected, shareTol, "system poolTruth.sharesRejected vs /api/pools sum");
  }

  if (mcpStatus.poolTruth && Object.keys(mcpStatus.poolTruth).length > 0) {
    approxEqual(ctx, Number(systemPool.sharesPending), Number(mcpStatus.poolTruth.shares_pending), shareTol, "system poolTruth.sharesPending vs MCP pool_truth");
    approxEqual(ctx, Number(systemPool.sharesUnresolved), Number(mcpStatus.poolTruth.shares_unresolved), shareTol, "system poolTruth.sharesUnresolved vs MCP pool_truth");
  }
  approxEqual(ctx, Number(mcpStatus.poolTruth?.shares_pending ?? 0), Number(mcpStatus.pools?.[0]?.shares_pending ?? 0), shareTol, "MCP pool_truth pending vs active pool");
}

async function runSelfTest(caseName) {
  const cases = new Set(["success", "missing-metric", "bad-json", "mcp-error", "recent-events-over-cap", "missing-field"]);
  if (!cases.has(caseName)) throw new Error(`Unknown self-test case: ${caseName}`);
  const server = createMockServer(caseName);
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  const opts = {
    baseUrl: `http://127.0.0.1:${address.port}`,
    bearerToken: "",
    timeoutMs: 2000,
    crossTolerance: 0,
    dispatcherTolerance: 0,
    strictCrossChecks: true,
    maxPendingAgeMs: DEFAULT_MAX_PENDING_AGE_MS,
  };
  const shouldFail = caseName !== "success";
  try {
    await runSmoke(opts, true);
    if (shouldFail) throw new Error(`self-test ${caseName} unexpectedly passed`);
    console.log(`self-test ${caseName}: passed`);
  } catch (error) {
    if (!shouldFail) throw error;
    console.log(`self-test ${caseName}: expected failure observed`);
    if (error instanceof SmokeFailure) {
      console.log(error.errors[0]);
    } else {
      console.log(error.message);
    }
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

function createMockServer(caseName) {
  return http.createServer(async (req, res) => {
    const payload = mockPayload(caseName);
    if (req.method === "GET" && req.url === "/api/system/info") {
      if (caseName === "bad-json") return sendRaw(res, 200, "application/json", "{broken");
      return sendJson(res, payload.info);
    }
    if (req.method === "GET" && req.url === "/api/pools") return sendJson(res, payload.pools);
    if (req.method === "GET" && req.url === "/metrics") return sendRaw(res, 200, "text/plain", payload.metrics);
    if (req.method === "GET" && req.url === "/mcp") {
      return sendJson(res, {
        name: "dcentaxe",
        version: "0.3.0",
        protocol: MCP_PROTOCOL,
        transport: "http-jsonrpc",
        profileId: "minimal",
        profile: {},
      });
    }
    if (req.method === "POST" && req.url === "/mcp") {
      const request = JSON.parse(await readBody(req));
      if (request.method === "tools/call" && request.params?.name === "get_status") {
        if (caseName === "mcp-error") {
          return sendJson(res, { jsonrpc: "2.0", id: request.id, error: { code: -32602, message: "mock mcp error" } });
        }
        return sendJson(res, rpcResult(request.id, {
          content: [{ type: "text", text: JSON.stringify(payload.mcpStatus) }],
        }));
      }
      if (request.method === "initialize") {
        return sendJson(res, rpcResult(request.id, {
          protocolVersion: MCP_PROTOCOL,
          capabilities: { tools: {}, resources: {} },
          serverInfo: { name: "dcentaxe", version: "0.3.0" },
        }));
      }
      if (request.method === "ping") return sendJson(res, rpcResult(request.id, {}));
      if (request.method === "tools/list") {
        return sendJson(res, rpcResult(request.id, { tools: [...READ_ONLY_TOOLS, ...WRITE_TOOLS].map((name) => ({ name })) }));
      }
      if (request.method === "resources/list") {
        return sendJson(res, rpcResult(request.id, { resources: RESOURCES.map((uri) => ({ uri, name: uri, mimeType: "application/json" })) }));
      }
      if (request.method === "resources/read" && request.params?.uri === "bitaxe://status") {
        return sendJson(res, rpcResult(request.id, { contents: [{ uri: "bitaxe://status", mimeType: "application/json", text: JSON.stringify(payload.mcpStatus) }] }));
      }
      return sendJson(res, { jsonrpc: "2.0", id: request.id, error: { code: -32601, message: "unknown mock method" } });
    }
    sendRaw(res, 404, "application/json", "{}");
  });
}

function mockPayload(caseName) {
  const events = Array.from({ length: caseName === "recent-events-over-cap" ? 9 : 2 }, (_, i) => ({
    tsUnixMs: 1_700_000_000_000 + i,
    kind: "shareAccepted",
    detail: `event ${i}`,
  }));
  const pool = {
    index: 0,
    url: "pool.example:3333",
    worker: "worker",
    target_pct: 100,
    actual_pct: 100,
    dispatched: 100,
    shares_submitted: 10,
    shares_accepted: 8,
    shares_rejected: 1,
    shares_pending: 1,
    shares_unresolved: 0,
    oldest_pending_submit_age_ms: 1200,
    connected: true,
    difficulty: 1024,
    failover_active: false,
    authorized: true,
    response_time_ms: 45,
    last_reject_reason: "",
    reject_reason_counts: [],
    recent_events: events,
  };
  const poolTruth = {
    activePool: pool.url,
    connected: true,
    difficulty: pool.difficulty,
    sharesSubmitted: pool.shares_submitted,
    sharesAccepted: pool.shares_accepted,
    sharesRejected: pool.shares_rejected,
    sharesPending: pool.shares_pending,
    sharesUnresolved: pool.shares_unresolved,
    oldestPendingSubmitAgeMs: pool.oldest_pending_submit_age_ms,
    responseTimeMs: pool.response_time_ms,
    failoverActive: false,
    lastRejectReason: "",
    rejectReasonCounts: [],
    recentEvents: events,
  };
  if (caseName === "missing-field") delete poolTruth.sharesPending;

  const info = {
    sharesAccepted: pool.shares_accepted,
    sharesRejected: pool.shares_rejected,
    dcentaxe: {
      poolTruth,
      dispatcher: {
        staleNonces: 0,
        slotRecoveries: 1,
        filteredNonces: 2,
        noncesFound: 100,
        ticketDifficulty: 256,
      },
    },
  };
  const mcpStatus = {
    filtered_nonces: 2,
    stale_nonces: 0,
    slot_recoveries: 1,
    ticket_difficulty: 256,
    pool_shares_pending: 1,
    pool_shares_unresolved: 0,
    pool_oldest_pending_submit_age_ms: 1200,
    pool_truth: {
      active_pool: pool.url,
      connected: true,
      difficulty: pool.difficulty,
      shares_submitted: pool.shares_submitted,
      shares_accepted: pool.shares_accepted,
      shares_rejected: pool.shares_rejected,
      shares_pending: pool.shares_pending,
      shares_unresolved: pool.shares_unresolved,
      oldest_pending_submit_age_ms: pool.oldest_pending_submit_age_ms,
      shares_accounting_gap: 0,
      response_time_ms: pool.response_time_ms,
      last_reject_reason: "",
      reject_reason_counts: [],
      recent_events: events,
      failover_active: false,
    },
    pools: [{
      index: 0,
      connected: true,
      authorized: true,
      active_pool: pool.url,
      difficulty: pool.difficulty,
      shares_submitted: pool.shares_submitted,
      shares_accepted: pool.shares_accepted,
      shares_rejected: pool.shares_rejected,
      shares_pending: pool.shares_pending,
      shares_unresolved: pool.shares_unresolved,
      oldest_pending_submit_age_ms: pool.oldest_pending_submit_age_ms,
      shares_accounting_gap: 0,
    }],
  };
  const metrics = [
    "# HELP dcentaxe mock",
    caseName === "missing-metric" ? "" : "dcentaxe_stratum_shares_pending 1",
    "dcentaxe_stratum_shares_unresolved_total 0",
    "dcentaxe_stratum_oldest_pending_submit_age_ms 1200 1710000000000",
    "dcentaxe_dispatcher_stale_nonces_total 0",
    "dcentaxe_dispatcher_slot_recoveries_total 1",
    "dcentaxe_dispatcher_filtered_nonces_total 2",
    "dcentaxe_dispatcher_ticket_difficulty 256",
    "dcentaxe_free_heap_bytes 180000",
    "dcentaxe_uptime_seconds 100",
    "dcentaxe_mining_enabled 1",
    "dcentaxe_thermal_sensors_ok 1",
    'dcentaxe_fan_rpm{fan="1"} 4100',
    'dcentaxe_fan_rpm{fan="2"} 4050',
  ].filter(Boolean).join("\n");
  return { info, pools: [pool], metrics, mcpStatus };
}

function rpcResult(id, result) {
  return { jsonrpc: "2.0", id, result };
}

function sendJson(res, value) {
  sendRaw(res, 200, "application/json", JSON.stringify(value));
}

function sendRaw(res, status, contentType, text) {
  const body = Buffer.from(text);
  res.writeHead(status, {
    "Content-Type": contentType,
    "Content-Length": body.length,
  });
  res.end(body);
}

async function readBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  return Buffer.concat(chunks).toString("utf8");
}

async function main() {
  const opts = parseArgs(process.argv);
  if (opts.selfTest) {
    await runSelfTest(opts.selfTest);
    return;
  }
  await runSmoke(opts);
}

main().catch((error) => {
  if (error instanceof SmokeFailure) {
    console.error("pool-truth smoke failed:");
    for (const message of error.errors) console.error(`- ${message}`);
  } else {
    console.error(`pool-truth smoke failed: ${error.message}`);
  }
  process.exit(1);
});
