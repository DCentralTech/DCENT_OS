import React from 'react';
import type { InverterBrand } from '../../api/feature-types';
import type { ProviderQualityMeta } from './SolarProviderQualityCard';

type SolarProviderSetupGuideProps = {
  providerId: InverterBrand;
  providerLabel: string;
  providerMeta: ProviderQualityMeta;
  validationMessages: string[];
};

type ProviderChecklist = {
  heading: string;
  steps: string[];
  pivotMessage: string;
};

function toneColor(tone: ProviderQualityMeta['trustTone']): string {
  if (tone === 'good') return 'var(--feat-green)';
  if (tone === 'warn') return 'var(--yellow)';
  return 'var(--text-dim)';
}

function stageLabel(stage: ProviderQualityMeta['stage']): string {
  if (stage === 'live') return 'Live path';
  if (stage === 'limited') return 'Limited live path';
  if (stage === 'unsupported') return 'Unsupported path';
  return 'Staged path';
}

export function SolarProviderSetupGuide({
  providerId,
  providerLabel,
  providerMeta,
  validationMessages,
}: SolarProviderSetupGuideProps) {
  const stageColor = providerMeta.stage === 'live' ? 'var(--feat-green)' : 'var(--yellow)';
  const providerChecklist: Record<InverterBrand, ProviderChecklist> = {
    manual: {
      heading: 'Commissioning checklist',
      steps: [
        'Enter realistic solar production and site load, then compare the resulting surplus with the miner wall-power number.',
        'Use manual battery SoC only when an operator can keep it updated during the commissioning window.',
        'Move to a live provider before relying on unattended battery-floor protection.',
      ],
      pivotMessage: 'Use Manual for bring-up and dry runs, then migrate to Victron, Bridge, Tesla, Enphase, SolarEdge, or EcoFlow when telemetry is ready.',
    },
    victron: {
      heading: 'GX MQTT bring-up',
      steps: [
        'Confirm the GX device publishes retained MQTT topics on-LAN and that the dashboard can reach the broker.',
        'Verify production, consumption, grid, and battery SoC all update during a real power swing.',
        'Leave the page open long enough to build a clean verification strip before enabling enforcement.',
      ],
      pivotMessage: 'Victron is the strongest on-site choice when you need fast local battery-aware control.',
    },
    bridge: {
      heading: 'Bridge validation',
      steps: [
        'Check that the bridge endpoint stays on-LAN and returns normalized JSON with fresh timestamps or sample age.',
        'Verify sign conventions for grid import/export against the site meter before trusting solar-only behavior.',
        'If battery SoC is synthetic or delayed, keep the battery floor conservative until history looks stable.',
      ],
      pivotMessage: 'Bridge is the best migration path when the upstream source is custom or not yet supported directly.',
    },
    ecoflow: {
      heading: 'EcoFlow contract check',
      steps: [
        'Use EcoFlow only when your endpoint implements the explicit EcoFlow HTTP bridge contract and returns normalized JSON.',
        'Confirm the payload includes fresh production and either consumption or net grid, plus battery SoC when available.',
        'If your EcoFlow source cannot satisfy that contract, switch to Bridge or Manual instead of forcing this provider.',
      ],
      pivotMessage: 'EcoFlow is intentionally narrow today: it is a limited live bridge contract, not broad direct EcoFlow auth/protocol support.',
    },
    tesla: {
      heading: 'Powerwall local checks',
      steps: [
        'Validate local gateway auth first; a successful save does not guarantee stable local access.',
        'Confirm battery SoC and aggregate load stay fresh during import/export changes.',
        'Keep off-grid protection independent until the verification history proves stable local telemetry.',
      ],
      pivotMessage: 'Tesla local is usable today, but battery-floor trust should come from repeated successful checks.',
    },
    enphase: {
      heading: 'Envoy on-site checks',
      steps: [
        'Make sure the Envoy has consumption metering if you expect load and net-grid aware policy.',
        'Compare PV, load, and grid numbers to the installer portal or local site meter during a live transition.',
        'Treat battery SoC as best-effort until the verification history shows consistent coverage.',
      ],
      pivotMessage: 'Enphase is strongest when local consumption meters are installed and visible to the gateway.',
    },
    solaredge: {
      heading: 'Cloud polling checks',
      steps: [
        'Verify the currentPowerFlow URL and API key on-site before saving enforcement settings.',
        'Expect slower updates than LAN providers and confirm import/export direction carefully.',
        'Use a conservative battery threshold because cloud freshness is not ideal for fast battery protection loops.',
      ],
      pivotMessage: 'SolarEdge is better for coarse policy and commissioning visibility than for fast closed-loop off-grid control.',
    },
  };
  const checklist = providerChecklist[providerId];

  return (
    <div style={{
      marginTop: 16,
      padding: 14,
      borderRadius: 'var(--radius)',
      background: 'rgba(255,255,255,0.03)',
      border: '1px solid var(--border)',
    }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, flexWrap: 'wrap', marginBottom: 12 }}>
        <div>
          <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)', fontWeight: 700, letterSpacing: '0.04em', textTransform: 'uppercase' }}>
            Provider Setup
          </div>
          <div style={{ fontSize: '0.95rem', color: 'var(--text)', fontWeight: 700, marginTop: 4 }}>{providerLabel}</div>
        </div>
        <div style={{
          padding: '8px 10px',
          borderRadius: 999,
          background: 'rgba(255,255,255,0.04)',
          border: '1px solid var(--border)',
          color: stageColor,
          fontSize: '0.72rem',
          fontWeight: 700,
          whiteSpace: 'nowrap',
        }}>
          {stageLabel(providerMeta.stage)}
        </div>
      </div>

      <div style={{ display: 'grid', gap: 10, gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))' }}>
        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Recommended use</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text)', lineHeight: 1.55 }}>{providerMeta.recommendedUse}</div>
        </div>

        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Trust boundary</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            <div style={{ color: toneColor(providerMeta.trustTone), fontWeight: 600, marginBottom: 6 }}>{providerMeta.trustBoundaryLabel}</div>
            <div>{providerMeta.trustBoundaryDetail}</div>
          </div>
        </div>

        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Expected data</div>
          <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
            {providerMeta.expectedFields.map(field => (
              <span key={field} style={{
                padding: '4px 8px',
                borderRadius: 999,
                background: 'rgba(59,130,246,0.12)',
                border: '1px solid rgba(59,130,246,0.18)',
                color: 'var(--text)',
                fontSize: '0.72rem',
                fontWeight: 600,
              }}>
                {field}
              </span>
            ))}
          </div>
        </div>

        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>{checklist.heading}</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            {checklist.steps.map(step => (
              <div key={step} style={{ marginTop: 4 }}>{step}</div>
            ))}
          </div>
        </div>

        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Off-grid note</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>{providerMeta.offGridCue}</div>
        </div>

        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Fail-safe expectation</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>{providerMeta.failSafeExpectation}</div>
        </div>

        <div style={{ background: 'rgba(255,255,255,0.02)', border: '1px solid var(--border)', borderRadius: 10, padding: '10px 12px' }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Backend scope</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', lineHeight: 1.55 }}>
            {providerMeta.backendScope || (providerMeta.providerLiveBackend ? 'The API did not publish a backend scope note for this provider.' : 'This provider does not currently advertise a live backend scope.')}
            {providerMeta.recommendedProvider && (
              <div style={{ marginTop: 6 }}>Fallback: {providerMeta.recommendedProvider}</div>
            )}
          </div>
        </div>
      </div>

      {providerMeta.acceptedPayloadShapes.length > 0 && (
        <div style={{
          marginTop: 12,
          padding: '10px 12px',
          borderRadius: 10,
          background: 'rgba(59,130,246,0.08)',
          border: '1px solid rgba(59,130,246,0.18)',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>Backend accepts</div>
          <div style={{ display: 'grid', gap: 6 }}>
            {providerMeta.acceptedPayloadShapes.map(shape => (
              <div key={shape} style={{ fontSize: '0.74rem', color: 'var(--text)', lineHeight: 1.5 }}>{shape}</div>
            ))}
          </div>
        </div>
      )}

      {providerId === 'ecoflow' && (
        <div style={{
          marginTop: 12,
          padding: '10px 12px',
          borderRadius: 10,
          background: 'rgba(59,130,246,0.08)',
          border: '1px solid rgba(59,130,246,0.18)',
        }}>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginBottom: 6 }}>EcoFlow bridge contract</div>
          <pre style={{
            margin: 0,
            whiteSpace: 'pre-wrap',
            wordBreak: 'break-word',
            fontSize: '0.72rem',
            lineHeight: 1.5,
            color: 'var(--text)',
            fontFamily: "'JetBrains Mono', monospace",
          }}>{`{
  "productionWatts": 1200,
  "consumptionWatts": 900,
  "netGridWatts": -300,
  "batterySocPct": 62,
  "sampleAgeMs": 850
}`}</pre>
        </div>
      )}

      <div style={{
        marginTop: 12,
        padding: '10px 12px',
        borderRadius: 10,
        background: providerMeta.stage === 'live' ? 'rgba(34,197,94,0.06)' : 'rgba(234,179,8,0.08)',
        border: `1px solid ${providerMeta.stage === 'live' ? 'rgba(34,197,94,0.16)' : 'rgba(234,179,8,0.24)'}`,
        fontSize: '0.76rem',
        color: 'var(--text-dim)',
        lineHeight: 1.55,
      }}>
        <div style={{ color: toneColor(providerMeta.trustTone), fontWeight: 600, marginBottom: validationMessages.length > 0 ? 8 : 0 }}>
          {providerMeta.trustLabel}
        </div>
        <div style={{ marginBottom: 6 }}>{checklist.pivotMessage}</div>
        {validationMessages.length > 0 ? validationMessages.map(message => (
          <div key={message} style={{ marginTop: 4 }}>{message}</div>
        )) : providerMeta.trustCues.map(cue => (
          <div key={cue} style={{ marginTop: 4 }}>{cue}</div>
        ))}
      </div>
    </div>
  );
}
