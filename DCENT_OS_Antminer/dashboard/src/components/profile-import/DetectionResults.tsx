// Step 2 — render detected (model, hashboard, chip, # presets,
// detected source_class). Operator can override chip/hashboard if
// detection failed.
//
// W8-A flagged BM1360 / BM1491 as named-only placeholders — we
// render a "no validated profile" badge when those chips are
// detected (cores_per_chip=0 sentinel + W8-A `[GAP]` discipline).

import React from 'react';
import type { SiliconChip, SiliconProfileBundle } from '../../api/profiles-silicon';

const CHIP_OPTIONS: SiliconChip[] = [
  'bm1387', 'bm1397', 'bm1398', 'bm1362', 'bm1366', 'bm1368',
  'bm1370', 'bm1360', 'bm1491', 'bm1485',
];

const PLACEHOLDER_CHIPS: SiliconChip[] = ['bm1360', 'bm1491'];

interface Props {
  bundle: SiliconProfileBundle;
  onPatch: (patch: Partial<SiliconProfileBundle>) => void;
}

export function DetectionResults({ bundle, onPatch }: Props) {
  const isPlaceholderChip = PLACEHOLDER_CHIPS.includes(bundle.chip);

  return (
    <div className="section">
      <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 12 }}>
        Review what was detected. You can override the chip and hashboard if the auto-detection
        looks wrong — these directly determine which preset table loads at runtime.
      </div>

      <div className="table-wrap">
        <table
          aria-label="Detected profile fields"
          style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.85rem' }}
        >
        <tbody>
          <Row label="Miner model">
            <input
              id="det-miner-model"
              type="text"
              aria-label="Miner model"
              value={bundle.miner_model}
              onChange={(e) => onPatch({ miner_model: e.target.value })}
              style={inputStyle}
            />
          </Row>
          <Row label="Hashboard">
            <input
              id="det-hashboard"
              type="text"
              aria-label="Hashboard identifier"
              value={bundle.hashboard}
              onChange={(e) => onPatch({ hashboard: e.target.value })}
              placeholder="BHB42601 / bhb-s9-generic / ..."
              style={inputStyle}
            />
          </Row>
          <Row label="Chip">
            <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
              <select
                id="det-chip"
                aria-label="ASIC chip type"
                value={bundle.chip}
                onChange={(e) => onPatch({ chip: e.target.value as SiliconChip })}
                style={{ ...inputStyle, maxWidth: 200 }}
              >
                {CHIP_OPTIONS.map(c => (
                  <option key={c} value={c}>{c}</option>
                ))}
                {!CHIP_OPTIONS.includes(bundle.chip) && (
                  <option value={bundle.chip}>{bundle.chip}</option>
                )}
              </select>
              {isPlaceholderChip && (
                <span style={{
                  display: 'inline-block',
                  padding: '3px 8px',
                  borderRadius: 999,
                  background: 'rgba(240, 180, 41, 0.16)',
                  color: 'var(--yellow, #F0B429)',
                  fontSize: '0.7rem',
                  fontWeight: 700,
                  letterSpacing: '0.04em',
                  textTransform: 'uppercase',
                }}>
                  No validated profile [GAP]
                </span>
              )}
            </div>
          </Row>
          <Row label="Detected source class">
            <span style={{ fontSize: '0.85rem', color: 'var(--text)', fontWeight: 600 }}>
              {bundle.source_class}
            </span>
            <span style={{ fontSize: '0.7rem', color: 'var(--text-dim, #6E6E80)', marginLeft: 8 }}>
              (you can change this on the next step)
            </span>
          </Row>
          <Row label="Preset rows">
            <span style={{ fontSize: '0.85rem', color: 'var(--text)', fontWeight: 600 }}>
              {bundle.presets.length}
            </span>
          </Row>
          <Row label="Schema version">
            <code style={{ fontSize: '0.78rem' }}>{bundle.schema_version}</code>
          </Row>
        </tbody>
        </table>
      </div>

      {isPlaceholderChip && (
        <div style={{
          marginTop: 12,
          padding: '10px 12px',
          borderRadius: 8,
          background: 'rgba(240, 180, 41, 0.08)',
          border: '1px solid rgba(240, 180, 41, 0.22)',
          color: 'var(--text)',
          fontSize: '0.78rem',
        }}>
          <strong>Heads up:</strong> {bundle.chip} is a named-only placeholder in the chip family
          enum. <code>cores_per_chip=0</code> means no live mining work will dispatch against
          this chip until a live-verified profile is published. You can still import the bundle
          for catalog tracking, but expect the autotuner to refuse to engage.
        </div>
      )}
    </div>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <tr>
      <th scope="row" style={{ padding: '6px 8px 6px 0', color: 'var(--text-secondary, #8b8b9e)', width: 180, verticalAlign: 'top', whiteSpace: 'nowrap', fontWeight: 'normal', textAlign: 'left' }}>
        {label}
      </th>
      <td style={{ padding: '6px 0' }}>{children}</td>
    </tr>
  );
}

const inputStyle: React.CSSProperties = {
  width: '100%',
  maxWidth: 360,
  padding: '6px 10px',
  background: 'rgba(10,10,15,0.5)',
  border: '1px solid var(--border, rgba(255,255,255,0.08))',
  borderRadius: 6,
  color: 'var(--text)',
  fontFamily: 'JetBrains Mono, monospace',
  fontSize: '0.78rem',
};
