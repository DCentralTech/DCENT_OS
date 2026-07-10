#!/usr/bin/env node
/**
 * Operator-run Chrome/CDP kiosk soak probe for the dashboard.
 *
 * This launches or attaches to Chrome, navigates to a bench miner dashboard,
 * samples JS heap and UI liveness over time, and emits a JSON report. It does
 * not contact SSH, reboot, flash, stop services, or mutate daemon config.
 */

import { spawn } from 'node:child_process';
import { mkdtemp, writeFile } from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

const DEFAULT_CDP_URL = 'http://127.0.0.1:9223';
const DEFAULT_DURATION_SECONDS = 8 * 60 * 60;
const DEFAULT_SAMPLE_SECONDS = 60 * 60;
const DEFAULT_MAX_HEAP_GROWTH_BYTES = 10 * 1024 * 1024;
const DEFAULT_MAX_ANIMATIONS = 3;

function usage() {
  return `Usage: node scripts/dashboard_kiosk_soak_check.mjs --target http://<miner-ip>/ [options]

Options:
  --target <url>                 Dashboard URL to soak.
  --duration-seconds <n>         Total soak duration, default 28800.
  --sample-seconds <n>           Sample interval, default 3600.
  --max-heap-growth-mb <n>       Heap growth budget, default 10.
  --max-animations <n>           Running animation budget, default 3.
  --require-live                 Fail samples whose transport chip is not LIVE.
  --output <path>                Write the JSON report to this file.
  --chrome-path <path>           Chrome/Chromium executable path.
  --cdp-url <url>                Existing or launched Chrome DevTools URL.
  --attach                       Attach to an already-running Chrome with CDP.
  --headless                     Launch Chrome headless.
  --manual-auth-seconds <n>      Pause after navigation so the operator can log in.
  --no-gc-before-sample          Do not request GC before sampling.
  --help                         Show this help.
`;
}

function parseArgs(argv) {
  const args = {
    target: null,
    durationSeconds: DEFAULT_DURATION_SECONDS,
    sampleSeconds: DEFAULT_SAMPLE_SECONDS,
    maxHeapGrowthBytes: DEFAULT_MAX_HEAP_GROWTH_BYTES,
    maxAnimations: DEFAULT_MAX_ANIMATIONS,
    requireLive: false,
    output: null,
    chromePath: null,
    cdpUrl: DEFAULT_CDP_URL,
    attach: false,
    headless: false,
    manualAuthSeconds: 0,
    gcBeforeSample: true,
    help: false,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => {
      if (i + 1 >= argv.length) throw new Error(`${arg} requires a value`);
      i += 1;
      return argv[i];
    };
    switch (arg) {
      case '--target':
        args.target = next();
        break;
      case '--duration-seconds':
        args.durationSeconds = positiveNumber(next(), arg);
        break;
      case '--sample-seconds':
        args.sampleSeconds = positiveNumber(next(), arg);
        break;
      case '--max-heap-growth-mb':
        args.maxHeapGrowthBytes = mbToBytes(nonNegativeNumber(next(), arg));
        break;
      case '--max-animations':
        args.maxAnimations = nonNegativeNumber(next(), arg);
        break;
      case '--require-live':
        args.requireLive = true;
        break;
      case '--output':
        args.output = next();
        break;
      case '--chrome-path':
        args.chromePath = next();
        break;
      case '--cdp-url':
        args.cdpUrl = next();
        break;
      case '--attach':
        args.attach = true;
        break;
      case '--headless':
        args.headless = true;
        break;
      case '--manual-auth-seconds':
        args.manualAuthSeconds = nonNegativeNumber(next(), arg);
        break;
      case '--no-gc-before-sample':
        args.gcBeforeSample = false;
        break;
      case '--help':
      case '-h':
        args.help = true;
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }
  return args;
}

function positiveNumber(raw, label) {
  const value = Number(raw);
  if (!Number.isFinite(value) || value <= 0) {
    throw new Error(`${label} must be a positive number`);
  }
  return value;
}

function nonNegativeNumber(raw, label) {
  const value = Number(raw);
  if (!Number.isFinite(value) || value < 0) {
    throw new Error(`${label} must be a non-negative number`);
  }
  return value;
}

export function mbToBytes(value) {
  return Math.round(value * 1024 * 1024);
}

export function normalizeTargetUrl(raw) {
  if (!raw || !String(raw).trim()) throw new Error('--target is required');
  const candidate = String(raw).includes('://') ? String(raw) : `http://${raw}`;
  const url = new URL(candidate);
  if (!['http:', 'https:'].includes(url.protocol)) {
    throw new Error('--target must use http or https');
  }
  return url.toString();
}

function defaultChromePaths() {
  const candidates = [];
  if (process.platform === 'win32') {
    const roots = [
      process.env.PROGRAMFILES,
      process.env['PROGRAMFILES(X86)'],
      process.env.LOCALAPPDATA,
    ].filter(Boolean);
    for (const root of roots) {
      candidates.push(path.join(root, 'Google', 'Chrome', 'Application', 'chrome.exe'));
      candidates.push(path.join(root, 'Microsoft', 'Edge', 'Application', 'msedge.exe'));
    }
  } else if (process.platform === 'darwin') {
    candidates.push('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome');
    candidates.push('/Applications/Chromium.app/Contents/MacOS/Chromium');
  } else {
    candidates.push('/usr/bin/google-chrome');
    candidates.push('/usr/bin/google-chrome-stable');
    candidates.push('/usr/bin/chromium');
    candidates.push('/usr/bin/chromium-browser');
  }
  return candidates;
}

async function findChrome(explicitPath) {
  if (explicitPath) return explicitPath;
  const { access } = await import('node:fs/promises');
  for (const candidate of defaultChromePaths()) {
    try {
      await access(candidate);
      return candidate;
    } catch {
      // Try next candidate.
    }
  }
  throw new Error('Chrome/Chromium not found; pass --chrome-path or use --attach with --cdp-url');
}

function httpJson(url, method = 'GET') {
  return new Promise((resolve, reject) => {
    const req = http.request(url, { method }, (res) => {
      const chunks = [];
      res.on('data', (chunk) => chunks.push(chunk));
      res.on('end', () => {
        const body = Buffer.concat(chunks).toString('utf8');
        if (res.statusCode < 200 || res.statusCode >= 300) {
          reject(new Error(`${method} ${url} returned ${res.statusCode}: ${body.slice(0, 200)}`));
          return;
        }
        try {
          resolve(JSON.parse(body));
        } catch (err) {
          reject(new Error(`${method} ${url} returned non-JSON: ${err.message}`));
        }
      });
    });
    req.on('error', reject);
    req.end();
  });
}

async function waitForCdp(cdpUrl, deadlineMs = 30_000) {
  const deadline = Date.now() + deadlineMs;
  const versionUrl = new URL('/json/version', cdpUrl);
  while (Date.now() < deadline) {
    try {
      return await httpJson(versionUrl);
    } catch {
      await delay(500);
    }
  }
  throw new Error(`timed out waiting for Chrome DevTools at ${cdpUrl}`);
}

async function createTarget(cdpUrl, targetUrl) {
  const newUrl = new URL(`/json/new?${encodeURIComponent(targetUrl)}`, cdpUrl);
  try {
    return await httpJson(newUrl, 'PUT');
  } catch {
    return await httpJson(newUrl, 'GET');
  }
}

class CdpClient {
  constructor(wsUrl) {
    this.wsUrl = wsUrl;
    this.ws = null;
    this.nextId = 1;
    this.pending = new Map();
  }

  connect() {
    return new Promise((resolve, reject) => {
      this.ws = new WebSocket(this.wsUrl);
      this.ws.addEventListener('open', resolve, { once: true });
      this.ws.addEventListener('error', reject, { once: true });
      this.ws.addEventListener('message', (event) => this.onMessage(event));
    });
  }

  onMessage(event) {
    const message = JSON.parse(event.data);
    if (!message.id) return;
    const entry = this.pending.get(message.id);
    if (!entry) return;
    this.pending.delete(message.id);
    if (message.error) {
      entry.reject(new Error(message.error.message || JSON.stringify(message.error)));
    } else {
      entry.resolve(message.result ?? {});
    }
  }

  send(method, params = {}) {
    const id = this.nextId;
    this.nextId += 1;
    const payload = JSON.stringify({ id, method, params });
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(payload);
    });
  }

  close() {
    if (this.ws) this.ws.close();
  }
}

function metricValue(metrics, name) {
  const metric = metrics.find((item) => item.name === name);
  return typeof metric?.value === 'number' ? metric.value : null;
}

async function samplePage(client, gcBeforeSample) {
  if (gcBeforeSample) {
    try {
      await client.send('HeapProfiler.collectGarbage');
    } catch {
      // Some Chrome builds deny this; Performance metrics still work.
    }
  }

  const runtime = await client.send('Runtime.evaluate', {
    returnByValue: true,
    expression: `(() => {
      const nodes = Array.from(document.querySelectorAll('[data-testid="transport-chip"], .transport-chip, .chip'));
      const transportNode = nodes.find((node) => /\\b(LIVE|POLLING|STALE)\\b/.test(node.textContent || ''));
      const animations = document.getAnimations ? document.getAnimations().filter((a) => a.playState === 'running').length : null;
      const memory = performance.memory ? {
        usedJSHeapSize: performance.memory.usedJSHeapSize,
        totalJSHeapSize: performance.memory.totalJSHeapSize,
        jsHeapSizeLimit: performance.memory.jsHeapSizeLimit
      } : null;
      return {
        title: document.title,
        href: location.href,
        pageReady: Boolean(document.querySelector('#root')) && document.body.innerText.trim().length > 0,
        transportText: transportNode ? transportNode.textContent.trim().replace(/\\s+/g, ' ') : null,
        animationCount: animations,
        memory,
        hidden: document.hidden,
        timestampMs: Date.now()
      };
    })()`,
  });
  const value = runtime.result?.value ?? {};
  const performance = await client.send('Performance.getMetrics');
  const metrics = performance.metrics ?? [];
  const jsHeapUsedSize = metricValue(metrics, 'JSHeapUsedSize') ?? value.memory?.usedJSHeapSize ?? null;
  const jsHeapTotalSize = metricValue(metrics, 'JSHeapTotalSize') ?? value.memory?.totalJSHeapSize ?? null;
  return {
    at: new Date().toISOString(),
    title: value.title ?? '',
    href: value.href ?? '',
    pageReady: Boolean(value.pageReady),
    transportText: value.transportText ?? null,
    animationCount: value.animationCount ?? null,
    hidden: Boolean(value.hidden),
    jsHeapUsedSize,
    jsHeapTotalSize,
  };
}

export function evaluateSamples(samples, options = {}) {
  const maxHeapGrowthBytes = options.maxHeapGrowthBytes ?? DEFAULT_MAX_HEAP_GROWTH_BYTES;
  const maxAnimations = options.maxAnimations ?? DEFAULT_MAX_ANIMATIONS;
  const requireLive = Boolean(options.requireLive);
  const checks = [];
  const heapSamples = samples.filter((sample) => Number.isFinite(sample.jsHeapUsedSize));
  const firstHeap = heapSamples[0]?.jsHeapUsedSize ?? null;
  const lastHeap = heapSamples[heapSamples.length - 1]?.jsHeapUsedSize ?? null;
  const heapGrowthBytes = firstHeap == null || lastHeap == null ? null : lastHeap - firstHeap;
  const notReady = samples.filter((sample) => !sample.pageReady);
  const tooManyAnimations = samples.filter(
    (sample) => Number.isFinite(sample.animationCount) && sample.animationCount > maxAnimations,
  );
  const nonLive = samples.filter(
    (sample) => requireLive && !/\bLIVE\b/.test(sample.transportText || ''),
  );

  checks.push({
    name: 'sample-count',
    ok: samples.length >= 3,
    detail: `samples=${samples.length}`,
  });
  checks.push({
    name: 'heap-samples',
    ok: heapSamples.length >= 2,
    detail: `heap_samples=${heapSamples.length}`,
  });
  checks.push({
    name: 'heap-growth-budget',
    ok: heapGrowthBytes != null && heapGrowthBytes <= maxHeapGrowthBytes,
    detail: `growth=${heapGrowthBytes} max=${maxHeapGrowthBytes}`,
  });
  checks.push({
    name: 'page-ready',
    ok: notReady.length === 0,
    detail: `not_ready_samples=${notReady.length}`,
  });
  checks.push({
    name: 'animation-cap',
    ok: tooManyAnimations.length === 0,
    detail: `over_cap_samples=${tooManyAnimations.length} max=${maxAnimations}`,
  });
  if (requireLive) {
    checks.push({
      name: 'transport-live',
      ok: nonLive.length === 0,
      detail: `non_live_samples=${nonLive.length}`,
    });
  }

  return {
    ok: checks.every((check) => check.ok),
    heapGrowthBytes,
    firstHeap,
    lastHeap,
    maxHeapGrowthBytes,
    maxAnimations,
    requireLive,
    checks,
  };
}

async function launchChrome(args, cdpUrl, userDataDir) {
  const chromePath = await findChrome(args.chromePath);
  const port = new URL(cdpUrl).port || '9223';
  const chromeArgs = [
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${userDataDir}`,
    '--no-first-run',
    '--disable-background-timer-throttling',
    '--disable-renderer-backgrounding',
    '--disable-backgrounding-occluded-windows',
  ];
  if (args.headless) chromeArgs.push('--headless=new', '--disable-gpu');
  const proc = spawn(chromePath, chromeArgs, {
    stdio: ['ignore', 'ignore', 'ignore'],
    windowsHide: true,
  });
  return proc;
}

async function run(argv) {
  const args = parseArgs(argv);
  if (args.help) {
    console.log(usage());
    return 0;
  }
  const targetUrl = normalizeTargetUrl(args.target);
  const userDataDir = await mkdtemp(path.join(os.tmpdir(), 'dcentos-kiosk-soak-'));
  let chrome = null;
  if (!args.attach) {
    chrome = await launchChrome(args, args.cdpUrl, userDataDir);
  }

  try {
    await waitForCdp(args.cdpUrl);
    const target = await createTarget(args.cdpUrl, targetUrl);
    if (!target.webSocketDebuggerUrl) {
      throw new Error('Chrome did not return a page WebSocket URL');
    }
    const client = new CdpClient(target.webSocketDebuggerUrl);
    await client.connect();
    await client.send('Page.enable');
    await client.send('Runtime.enable');
    await client.send('Performance.enable');
    await client.send('Page.navigate', { url: targetUrl });
    if (args.manualAuthSeconds > 0) {
      console.error(`Waiting ${args.manualAuthSeconds}s for manual dashboard login/setup...`);
      await delay(args.manualAuthSeconds * 1000);
    } else {
      await delay(5000);
    }

    const samples = [];
    const start = Date.now();
    const deadline = start + args.durationSeconds * 1000;
    do {
      const sample = await samplePage(client, args.gcBeforeSample);
      sample.elapsedSeconds = Math.round((Date.now() - start) / 1000);
      samples.push(sample);
      console.error(
        `[soak] t=${sample.elapsedSeconds}s heap=${sample.jsHeapUsedSize} transport=${sample.transportText ?? 'unknown'} animations=${sample.animationCount ?? 'unknown'}`,
      );
      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      await delay(Math.min(args.sampleSeconds * 1000, remaining));
    } while (Date.now() < deadline);

    const verdict = evaluateSamples(samples, {
      maxHeapGrowthBytes: args.maxHeapGrowthBytes,
      maxAnimations: args.maxAnimations,
      requireLive: args.requireLive,
    });
    const report = {
      target: targetUrl,
      startedAt: new Date(start).toISOString(),
      endedAt: new Date().toISOString(),
      durationSeconds: args.durationSeconds,
      sampleSeconds: args.sampleSeconds,
      verdict,
      samples,
    };
    const text = JSON.stringify(report, null, 2);
    if (args.output) await writeFile(args.output, text + '\n', 'utf8');
    console.log(text);
    client.close();
    return verdict.ok ? 0 : 1;
  } finally {
    if (chrome && chrome.pid) chrome.kill();
  }
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  run(process.argv.slice(2)).then((code) => {
    process.exitCode = code;
  }).catch((err) => {
    console.error(err instanceof Error ? err.message : err);
    process.exitCode = 2;
  });
}
