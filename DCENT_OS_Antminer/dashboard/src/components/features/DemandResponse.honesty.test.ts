import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';

const componentSource = readFileSync('src/components/features/DemandResponse.tsx', 'utf8');
const featureTypesSource = readFileSync('src/api/feature-types.ts', 'utf8');
const localeSources = [
  readFileSync('src/i18n/locales/en.ts', 'utf8'),
  readFileSync('src/i18n/locales/es.ts', 'utf8'),
  readFileSync('src/i18n/locales/fr.ts', 'utf8'),
  readFileSync('src/i18n/locales/zh.ts', 'utf8'),
].join('\n');

describe('DemandResponse honesty contract', () => {
  it('presents demand response as a planning surface without runtime control', () => {
    expect(componentSource).toContain('Demand response planning helps define');
    expect(componentSource).toMatch(/this screen drafts\s+the policy only/);
    expect(componentSource).toContain('planning surface only');
    expect(componentSource).toContain('does not send curtailment, sleep, wake, fan, voltage, frequency, or pool commands');
    expect(componentSource).toContain('Demand response draft updated locally. No miner state changed.');
    expect(componentSource).toContain('Acknowledge Draft');
  });

  it('reports runtime demand-response data as unavailable until a backend source exists', () => {
    expect(componentSource).toContain('Runtime Status');
    expect(componentSource).toContain('Unavailable');
    expect(componentSource).toContain('No live price source');
    expect(componentSource).toContain('READINESS ONLY');
    expect(componentSource).toContain('No curtailment command is sent from this page');
    expect(componentSource).toContain('Runtime control');
    expect(componentSource).toContain('In development');
    expect(componentSource).toContain('Price source');
    expect(componentSource).toContain('Not connected');
    expect(componentSource).toContain('Revenue impact');
    expect(componentSource).toContain('Not calculated');
  });

  it('does not expose fabricated live demand-response telemetry types or fields', () => {
    const combined = `${componentSource}\n${featureTypesSource}\n${localeSources}`;

    expect(combined).not.toMatch(/DemandResponseStatus/);
    expect(combined).not.toMatch(/currentPriceCentsKwh/);
    expect(combined).not.toMatch(/priceSignal/);
    expect(combined).not.toMatch(/revenueToday/);
    expect(combined).not.toMatch(/negativePriceHoursToday/);
    expect(combined).not.toMatch(/Simulated status/);
  });

  it('does not claim active grid-price mining control from the dashboard page', () => {
    const activeControlClaims = [
      /can curtail mining/i,
      /mine harder/i,
      /automatic curtailment/i,
      /automated power management/i,
      /real-time demand response/i,
      /live price signal/i,
    ];

    for (const claim of activeControlClaims) {
      expect(componentSource).not.toMatch(claim);
    }

    expect(componentSource).not.toMatch(/fetch\s*\(/);
    expect(componentSource).not.toMatch(/\bapi\./);
    expect(componentSource).not.toMatch(/\/api\/action\/(?:sleep|wake)/);
    expect(componentSource).not.toMatch(/\/api\/(?:fan|pools|system\/upgrade|tou\/schedule)/);
  });

  it('keeps locale subtitle copy explicit about unavailable runtime control', () => {
    expect(localeSources).toContain('Runtime DPS control is unavailable');
  });
});
