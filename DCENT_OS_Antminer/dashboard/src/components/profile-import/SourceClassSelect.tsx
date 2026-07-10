// Step 4 — operator picks source_class.
//
// Constraints (per W7-D registry validation + plan):
//   - Operator can downgrade VendorExtracted -> OperatorConfirmed
//     (live-tested in their environment)
//   - Operator CANNOT upgrade to LiveConfirmed (D-Central tracked-only)
//   - Operator CANNOT downgrade existing LiveConfirmed (registry rejects)

import React from 'react';
import type { SiliconProfileBundle, SiliconSourceClass } from '../../api/profiles-silicon';
import { InfoDot } from '../common/Tooltip';

interface Props {
  bundle: SiliconProfileBundle;
  onPatch: (patch: Partial<SiliconProfileBundle>) => void;
}

interface Option {
  value: SiliconSourceClass;
  label: string;
  hint: string;
  selectable: (current: SiliconSourceClass) => boolean;
}

const OPTIONS: Option[] = [
  {
    value: 'live_confirmed',
    label: 'Live confirmed',
    hint: 'D-Central-tracked only. The wizard refuses to mark a bundle live-confirmed.',
    selectable: () => false,
  },
  {
    value: 'operator_confirmed',
    label: 'Operator confirmed',
    hint: 'You\'ve live-tested this in your own environment. Safe to downgrade VendorExtracted to here.',
    selectable: (current) => current !== 'live_confirmed',
  },
  {
    value: 'vendor_extracted',
    label: 'Vendor extracted',
    hint: 'Pulled from a stock/3rd-party firmware. Preview locally; review before promotion.',
    selectable: (current) => current !== 'live_confirmed',
  },
  {
    value: 'baked',
    label: 'Baked',
    hint: 'Compiled into the dcentrald binary at build time. Importable as a baseline only.',
    selectable: () => false,
  },
];

export function SourceClassSelect({ bundle, onPatch }: Props) {
  const current = bundle.source_class;
  const isLockedLive = current === 'live_confirmed';

  return (
    <div className="section">
      <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary, #8b8b9e)', marginBottom: 12 }}>
        Pick the trust level for this bundle.{' '}
        <InfoDot
          placement="bottom"
          label="What the trust level controls"
          content={
            <>
              The trust level is honest provenance, not a quality grade. The runtime
              registry uses it as a precedence ladder so a more-trusted profile is
              never silently overridden by a less-trusted one:{' '}
              <strong>Live confirmed</strong> (hard evidence on real hardware) beats{' '}
              <strong>Operator confirmed</strong> (you live-tested it locally) beats{' '}
              <strong>Vendor extracted</strong> (pulled from a 3rd-party firmware
              for operator review). It is never upgraded for you.
            </>
          }
        />{' '}
        The runtime registry uses this to gate which
        bundles can take precedence — LiveConfirmed beats OperatorConfirmed beats VendorExtracted.
      </div>

      {isLockedLive && (
        <div style={{
          padding: '10px 12px',
          borderRadius: 8,
          marginBottom: 12,
          background: 'rgba(247,147,26,0.08)',
          border: '1px solid rgba(247,147,26,0.22)',
          color: 'var(--text)',
          fontSize: '0.78rem',
        }}>
          This bundle is already <strong>LiveConfirmed</strong>. The registry rejects downgrades —
          you can re-import it as-is, but the source_class can't be lowered from this UI.
        </div>
      )}

      <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
        {OPTIONS.map((opt) => {
          const selectable = opt.selectable(current);
          const checked = bundle.source_class === opt.value;
          return (
            <label
              key={opt.value}
              className={`p4-trust-card${checked ? ' is-checked' : ''}${!selectable ? ' is-locked' : ''}`}
            >
              <input
                type="radio"
                name="source-class"
                value={opt.value}
                checked={checked}
                disabled={!selectable}
                onChange={() => { if (selectable) onPatch({ source_class: opt.value }); }}
                style={{ marginTop: 2 }}
              />
              <div style={{ flex: 1 }}>
                <div style={{ fontWeight: 700, fontSize: '0.85rem', color: 'var(--text)' }}>
                  {opt.label}
                  {!selectable && (
                    <span style={{ marginLeft: 8, fontSize: '0.65rem', color: 'var(--text-dim, #6E6E80)', textTransform: 'uppercase', letterSpacing: '0.05em', fontWeight: 700 }}>
                      not selectable
                    </span>
                  )}
                </div>
                <div style={{ fontSize: '0.75rem', color: 'var(--text-secondary, #8b8b9e)', marginTop: 2 }}>
                  {opt.hint}
                </div>
              </div>
            </label>
          );
        })}
      </div>
    </div>
  );
}
