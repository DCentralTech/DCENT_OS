// MinerTypeCard — surfaces `/api/miner/type` (W11.12) plus the W13.D1
// PVT envelope row. Read-only card; safe everywhere (About, Settings,
// Tuning), degrades gracefully when the daemon is down.
//
// PVT row shows:
//   • SKU + grade chip (color-coded by tier)
//   • Voltage range "1320–1380 mV"
//   • Frequency range "465–545 MHz"
//   • Chip count "126 × 4 chains"
//   • voltage_fixed   → "Voltage: fixed 1530 mV (DVS unavailable)"
//   • inverted_curve  → anomaly tooltip (marginal silicon)
//   • requires_apw12_plus → "APW12+ required" badge
//
// Cross-references:
//   •
//   •  (memory rule)
//   •  (memory rule)

import React, { useEffect, useState } from 'react';
import { api } from '../../api/client';
import type { MinerTypeResponse } from '../../api/types';

interface GradeStyle {
  label: string;
  color: string;
  bg: string;
  border: string;
}

function gradeStyle(grade?: string): GradeStyle {
  switch (grade) {
    case 'efficiency':
      return { label: 'EFFICIENCY', color: 'var(--green, #10B981)', bg: 'rgba(16,185,129,0.12)', border: 'rgba(16,185,129,0.35)' };
    case 'high-bin':
    case 'high-bin-extended':
      return { label: 'HIGH-BIN', color: 'var(--accent, #FAA500)', bg: 'rgba(250,165,0,0.15)', border: 'rgba(250,165,0,0.40)' };
    case 'low-power-salvage':
      return { label: 'SALVAGE', color: '#9CA3AF', bg: 'rgba(156,163,175,0.10)', border: 'rgba(156,163,175,0.30)' };
    case 'single-voltage':
      return { label: 'SINGLE-V', color: '#A855F7', bg: 'rgba(168,85,247,0.12)', border: 'rgba(168,85,247,0.35)' };
    case 'mixable':
      return { label: 'MIXABLE',  color: '#3B82F6', bg: 'rgba(59,130,246,0.12)', border: 'rgba(59,130,246,0.35)' };
    case 'low-freq-extended':
      return { label: 'LOW-EXT',  color: '#06B6D4', bg: 'rgba(6,182,212,0.12)', border: 'rgba(6,182,212,0.35)' };
    case 'standard':
    default:
      return { label: 'STANDARD', color: 'var(--text-dim, #B5B5BD)', bg: 'rgba(255,255,255,0.04)', border: 'rgba(255,255,255,0.10)' };
  }
}

export function MinerTypeCard() {
  const [miner, setMiner] = useState<MinerTypeResponse | null | undefined>(undefined);

  useEffect(() => {
    let cancelled = false;
    api.getMinerType()
      .then(r => { if (!cancelled) setMiner(r); })
      .catch(() => { if (!cancelled) setMiner(null); });
    return () => { cancelled = true; };
  }, []);

  return (
    <div
      className="ds-card"
      data-testid="miner-type-card"
      style={{
        background: 'var(--card-bg)',
        border: '1px solid var(--border)',
        borderRadius: 12,
        padding: 20,
        marginTop: 16,
      }}
    >
      <div style={{
        fontSize: '0.7rem',
        fontWeight: 700,
        letterSpacing: '0.08em',
        textTransform: 'uppercase',
        color: 'var(--text-dim)',
        marginBottom: 10,
      }}>
        Hardware Identity
      </div>

      <KvSection title="Identity">
        <Kv label="Model"          value={miner?.model} />
        <Kv label="ASIC"           value={miner?.asic} mono />
        <Kv label="Hashboard"      value={miner?.hashboard} mono />
        <Kv label="Control board"  value={miner?.control_board} mono />
        <Kv label="SoC"            value={miner?.soc} />
        <Kv label="Firmware"       value={miner ? `${miner.firmware} ${miner.firmware_version}` : undefined} />
      </KvSection>

      <div style={{ height: 1, background: 'var(--border)', margin: '14px 0' }} />

      <PvtRow miner={miner} />
    </div>
  );
}

function PvtRow({ miner }: { miner: MinerTypeResponse | null | undefined }) {
  const grade = miner?.pvt_grade;
  const style = gradeStyle(grade);
  const hasPvt = !!miner && (miner.sku_chain_count ?? 0) > 0;

  return (
    <div data-testid="miner-pvt-row">
      <div style={{
        fontSize: '0.65rem',
        fontWeight: 700,
        letterSpacing: '0.06em',
        textTransform: 'uppercase',
        color: 'var(--text-dim)',
        marginBottom: 8,
        display: 'flex',
        alignItems: 'center',
        gap: 8,
      }}>
        <span>PVT</span>
        {hasPvt && (
          <span
            className="ds-chip"
            data-testid="pvt-grade-chip"
            data-grade={grade}
            style={{
              fontSize: '0.65rem',
              padding: '2px 8px',
              borderRadius: 6,
              background: style.bg,
              color: style.color,
              border: `1px solid ${style.border}`,
              letterSpacing: '0.06em',
              fontWeight: 700,
            }}
          >
            {style.label}
          </span>
        )}
        {hasPvt && miner!.requires_apw12_plus && (
          <span
            className="ds-chip"
            data-testid="pvt-apw12plus-badge"
            title="This SKU requires the APW12+ register-based PSU protocol. APW12 SMBus is not supported."
            style={{
              fontSize: '0.62rem',
              padding: '2px 8px',
              borderRadius: 6,
              background: 'rgba(245,158,11,0.12)',
              color: 'var(--amber, #F59E0B)',
              border: '1px solid rgba(245,158,11,0.35)',
              fontWeight: 700,
            }}
          >
            APW12+ REQUIRED
          </span>
        )}
        {hasPvt && miner!.inverted_curve && (
          <span
            className="ds-chip"
            data-testid="pvt-inverted-curve-badge"
            title="Marginal silicon — voltage may not scale linearly with frequency. Autotuner heuristics use the inverted table."
            style={{
              fontSize: '0.62rem',
              padding: '2px 8px',
              borderRadius: 6,
              background: 'rgba(168,85,247,0.10)',
              color: '#A855F7',
              border: '1px solid rgba(168,85,247,0.30)',
              fontWeight: 700,
            }}
          >
            INVERTED CURVE
          </span>
        )}
      </div>

      {!hasPvt && (
        <div className="cp-empty-note">
          No PVT envelope available for this hardware.
        </div>
      )}

      {hasPvt && (
        <>
          <Kv
            label="Voltage range"
            value={
              miner!.voltage_fixed && miner!.pvt_voltage_min_mv === miner!.pvt_voltage_max_mv
                ? `fixed ${miner!.pvt_voltage_min_mv} mV (DVS unavailable)`
                : `${miner!.pvt_voltage_min_mv}–${miner!.pvt_voltage_max_mv} mV`
            }
            tooltip={
              miner!.voltage_fixed
                ? 'This SKU has a single-voltage VRM (BHB42803). Voltage cannot be adjusted.'
                : undefined
            }
          />
          <Kv
            label="Frequency range"
            value={`${miner!.pvt_freq_min_mhz}–${miner!.pvt_freq_max_mhz} MHz`}
          />
          <Kv
            label="Chips"
            value={`${miner!.sku_asics_per_chain} × ${miner!.sku_chain_count} chains`}
          />
          {miner!.mix_levels_supported && (
            <Kv
              label="Mix levels"
              value="supported (per-chain dispatch)"
              tooltip="This SKU supports per-chain frequency dispatch. Per-chain control ships in W14+."
            />
          )}
        </>
      )}
    </div>
  );
}

interface KvProps {
  label: string;
  value: string | undefined | null;
  mono?: boolean;
  tooltip?: string;
}

function Kv({ label, value, mono, tooltip }: KvProps) {
  const display = value && value.length > 0 ? value : '—';
  return (
    <div
      style={{
        display: 'flex',
        justifyContent: 'space-between',
        gap: 16,
        padding: '4px 0',
        fontSize: '0.85rem',
      }}
      title={tooltip}
    >
      <span style={{ color: 'var(--text-dim)' }}>{label}</span>
      <span
        style={{
          color: 'var(--text)',
          textAlign: 'right',
          fontFamily: mono ? 'var(--font-mono, monospace)' : undefined,
          wordBreak: 'break-all',
        }}
      >
        {display}
      </span>
    </div>
  );
}

function KvSection(props: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <div style={{
        fontSize: '0.65rem',
        fontWeight: 700,
        letterSpacing: '0.06em',
        textTransform: 'uppercase',
        color: 'var(--text-dim)',
        marginBottom: 6,
      }}>
        {props.title}
      </div>
      <div>{props.children}</div>
    </div>
  );
}

export default MinerTypeCard;
