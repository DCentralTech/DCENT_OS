import React from 'react';
import type { ChainState } from '../../api/types';
import { formatHashrateShort, formatVoltage, formatFrequency, chainLabel } from '../../utils/format';
import { useTemp } from '../../hooks/useTemp';
import { glossary } from '../../utils/glossary';

/**
 * COMP-CHIPSTRIP Level A — `ChainStrip` cell state (DCENT Design Language —
 * component-contract.md §4). `ChainCard` is the OS `[OS-only]` per-chain cell
 * (axe is single-chain and SKIPS Level A entirely — NEVER push ChainStrip onto
 * axe). This `ChainCellState` type ADVERTISES the §4 closed `ChainState` enum so
 * the existing honest power-presence logic is contract-legible; it is a
 * type-level advertisement — no behavior change.
 *
 * §4 closed enum: `active | standby | unpowered | error`. The `unpowered` rung
 * is proven by `hashrate==0 && freq==0 && voltage==0` (the `isUnpowered` test
 * below), **NEVER** by temperature (BUG-11) — a healthy mining board can report
 * the XADC die-temp fallback, and an idle board can report temp 0 with the rail
 * still up, so power presence is read from hashrate/freq/voltage only.
 */
export type ChainCellState = 'active' | 'standby' | 'unpowered' | 'error';

interface ChainCardProps {
  chain: ChainState;
  compact?: boolean;
  /**
   * P3-35(c): 0-based array position, used to render a consistent 1-based
   * "Chain N" ordinal (matching the chain-presence panel) instead of the raw
   * hardware/FPGA id. Omitted callers fall back to the hardware id.
   */
  position?: number;
}

export function ChainCard({ chain, compact, position }: ChainCardProps) {
  const temp = useTemp();
  const hr = formatHashrateShort(chain.hashrate_ghs);
  const isActive = chain.hashrate_ghs > 0 || chain.frequency_mhz > 0;
  const statusColor = chain.status === 'active' ? 'var(--green)'
    : chain.status === 'error' ? 'var(--red)'
    : chain.status === 'disabled' || chain.status === 'offline' ? 'var(--text-dim)'
    : 'var(--yellow)';

  // Detect unpowered boards: 0 hashrate, 0 freq, 0 voltage.
  // BUG-11: do NOT include `temp_c === 0` in this test. On S9 the on-board
  // sensors are silent (need 12V hashboard power) so a healthy, mining board
  // reports the XADC die-temp FALLBACK, not a board sensor — and at idle it
  // can report 0 with the rail still up. Power presence is proven by
  // hashrate/freq/voltage, never by temperature.
  const isUnpowered = chain.hashrate_ghs === 0 && chain.frequency_mhz === 0
    && chain.voltage_mv === 0;
  const isDieFallback = chain.temp_source === 'soc_die_fallback';

  return (
    <div className="chain-card cp-chain-card">
      <div className="chain-id" title={`Hardware chain id ${chain.id}`}>
        <span style={{ color: statusColor, marginRight: 6 }}>{'\u25CF'}</span>
        {position != null ? chainLabel(position) : `Chain ${chain.id}`}
        <span style={{ float: 'right', fontSize: '0.75rem', color: 'var(--text-dim)' }}>
          {chain.chips > 0 ? `${chain.chips} chips` : 'Detected'}
        </span>
      </div>
      {isUnpowered ? (
        <div className="cp-chain-standby">
          {glossary('telemetry_unpowered').term}
        </div>
      ) : (
        <>
          <div className="chain-row">
            <span className="label">Hashrate</span>
            <span className="value">
              {chain.hashrate_ghs > 0 ? `${hr.value} ${hr.unit}` : 'Standby'}
            </span>
          </div>
          <div className="chain-row">
            <span className="label">Frequency</span>
            <span className="value">
              {chain.frequency_mhz > 0 ? formatFrequency(chain.frequency_mhz) : 'Standby'}
            </span>
          </div>
          <div className="chain-row">
            <span className="label">Voltage</span>
            <span className="value">
              {chain.voltage_mv > 0 ? formatVoltage(chain.voltage_mv) : 'Standby'}
            </span>
          </div>
          <div className="chain-row">
            <span className="label">
              {isDieFallback ? 'Temp (SoC die)' : 'Temperature'}
            </span>
            <span
              className="value"
              style={chain.temp_c > 65 ? { color: 'var(--red)' } : undefined}
              title={isDieFallback
                ? 'Hash board temp sensors returned no data (normal on S9 — they need 12V board power). Showing the Zynq SoC die-temperature fallback, which runs cooler than a true board sensor.'
                : undefined}
            >
              {chain.temp_c > 0
                ? `${temp.format(chain.temp_c)}${isDieFallback ? ' (die)' : ''}`
                : 'N/A'}
            </span>
          </div>
          {!compact && (
            <div className="chain-row">
              <span className="label">Errors</span>
              <span className="value" style={chain.errors > 100 ? { color: 'var(--yellow)' } : undefined}>
                {chain.errors.toLocaleString()}
              </span>
            </div>
          )}
        </>
      )}
    </div>
  );
}
