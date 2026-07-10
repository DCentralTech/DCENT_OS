#!/usr/bin/env node

import assert from 'node:assert/strict';

import {
  evaluateSamples,
  mbToBytes,
  normalizeTargetUrl,
} from './dashboard_kiosk_soak_check.mjs';

function sample(overrides = {}) {
  return {
    pageReady: true,
    transportText: 'LIVE',
    animationCount: 1,
    jsHeapUsedSize: 10 * 1024 * 1024,
    ...overrides,
  };
}

function check(verdict, name) {
  return verdict.checks.find((item) => item.name === name);
}

assert.equal(mbToBytes(10), 10 * 1024 * 1024);
assert.equal(normalizeTargetUrl('192.0.2.10'), 'http://192.0.2.10/');
assert.equal(normalizeTargetUrl('https://example.test/dashboard'), 'https://example.test/dashboard');

{
  const verdict = evaluateSamples([
    sample({ jsHeapUsedSize: mbToBytes(20) }),
    sample({ jsHeapUsedSize: mbToBytes(24) }),
    sample({ jsHeapUsedSize: mbToBytes(28), animationCount: 3 }),
  ], {
    maxHeapGrowthBytes: mbToBytes(10),
    maxAnimations: 3,
    requireLive: true,
  });
  assert.equal(verdict.ok, true);
  assert.equal(verdict.heapGrowthBytes, mbToBytes(8));
  assert.equal(check(verdict, 'transport-live').ok, true);
}

{
  const verdict = evaluateSamples([
    sample({ jsHeapUsedSize: mbToBytes(20) }),
    sample({ jsHeapUsedSize: mbToBytes(25) }),
    sample({ jsHeapUsedSize: mbToBytes(31) }),
  ], {
    maxHeapGrowthBytes: mbToBytes(10),
  });
  assert.equal(verdict.ok, false);
  assert.equal(check(verdict, 'heap-growth-budget').ok, false);
}

{
  const verdict = evaluateSamples([
    sample({ transportText: 'POLLING' }),
    sample({ transportText: 'LIVE' }),
    sample({ transportText: 'LIVE' }),
  ], {
    requireLive: true,
  });
  assert.equal(verdict.ok, false);
  assert.equal(check(verdict, 'transport-live').ok, false);
}

{
  const verdict = evaluateSamples([
    sample({ animationCount: 1 }),
    sample({ animationCount: 4 }),
    sample({ animationCount: 2 }),
  ], {
    maxAnimations: 3,
  });
  assert.equal(verdict.ok, false);
  assert.equal(check(verdict, 'animation-cap').ok, false);
}

console.log('dashboard_kiosk_soak_check tests passed');
