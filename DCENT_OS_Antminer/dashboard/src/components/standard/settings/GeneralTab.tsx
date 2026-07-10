import React from 'react';
import { useMinerStore } from '../../../store/miner';
import type { OperatingMode } from '../../../api/types';
import { ModeSwitch } from '../../common/ModeSwitch';
import { LanguageSelector } from '../../../i18n/i18n';
import { PowerCalibrationCard } from '../../common/PowerCalibrationCard';
import { DonationFeeCard } from '../../common/DonationFeeCard';
import { getLiveWallWatts } from '../../../utils/power';
import { estimateDailyProfit } from '../../../utils/thermal';
import { formatSats } from '../../../utils/format';

export function GeneralTab({ onModeSelect }: { onModeSelect: (mode: OperatingMode) => void }) {
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const settings = useMinerStore(s => s.settings);
  const updateSettings = useMinerStore(s => s.updateSettings);
  const networkDifficulty = useMinerStore(s => s.heaterStatus?.network_difficulty ?? null);
  const hashrate = status?.hashrate_ghs ?? 0;
  const liveWallWatts = getLiveWallWatts(stats?.power);
  const wattsFromLivePower = liveWallWatts > 0;
  const watts = wattsFromLivePower ? liveWallWatts : (status ? 25 : 0);
  const profitabilityPowerNote = wattsFromLivePower
    ? `Based on live wall power: ${watts}W`
    : status
      ? `Power unavailable; cost uses ${watts}W standby assumption.`
      : null;
  const profit = estimateDailyProfit(hashrate, watts, settings.btcPrice, settings.electricityRate, networkDifficulty);

  return (
    <>
      <div className="section">
        <div className="section-title">General</div>
        <div style={{
          background: 'var(--card-bg)', borderRadius: 'var(--radius)',
          padding: 16, border: '1px solid var(--border)',
        }}>
          <div style={{ marginBottom: 16 }}>
            <label style={{ fontSize: '0.75rem', color: 'var(--text-dim)', display: 'block', marginBottom: 4 }}>
              Miner Name
            </label>
            <div style={{ maxWidth: 400 }}>
              <input
                type="text"
                value={settings.minerName}
                onChange={e => updateSettings({ minerName: e.target.value })}
                placeholder="My Miner"
              />
            </div>
          </div>

          <div>
            <label style={{ fontSize: '0.75rem', color: 'var(--text-dim)', display: 'block', marginBottom: 8 }}>
              Operating Mode
            </label>
            <ModeSwitch
              currentMode={status?.mode ?? 'standard'}
              onSelect={onModeSelect}
              compact
            />
          </div>

          <div style={{ marginTop: 16 }}>
            <label style={{ fontSize: '0.75rem', color: 'var(--text-dim)', display: 'block', marginBottom: 8 }}>
              Language / Langue
            </label>
            <LanguageSelector />
          </div>

          <div style={{ marginTop: 16 }}>
            <div
              style={{
                display: 'flex',
                alignItems: 'flex-start',
                gap: 12,
                justifyContent: 'space-between',
              }}
            >
              <div style={{ flex: '1 1 auto', minWidth: 0 }}>
                <div style={{ fontSize: '0.85rem', fontWeight: 700, color: 'var(--text)' }}>
                  Beta view
                </div>
                <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginTop: 4, lineHeight: 1.5 }}>
                  Hide internal contract gates and dev telemetry from the
                  Standard dashboard. Recommended for new users. Turn off to
                  expose proxy/native honesty cards, competitive readiness,
                  and release notes.
                </div>
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={settings.betaView !== false}
                aria-label={settings.betaView !== false ? 'Disable beta view' : 'Enable beta view'}
                onClick={() => updateSettings({ betaView: !(settings.betaView !== false) })}
                className={`ds-toggle${settings.betaView !== false ? ' on' : ''}`}
              >
                <span className="ds-toggle-knob" />
              </button>
            </div>
          </div>
        </div>
      </div>

      <div className="section">
        <div className="section-title">Power Calibration</div>
        <PowerCalibrationCard />
      </div>

      <div className="section">
        <div className="section-title">Donation</div>
        <DonationFeeCard variant="full" />
      </div>

      <div className="section">
        <div className="section-title">Profitability</div>
        <div style={{
          background: 'var(--card-bg)', borderRadius: 'var(--radius)',
          padding: 16, border: '1px solid var(--border)',
        }}>
          <div className="standard-grid-2" style={{ gap: 16, marginBottom: 16 }}>
            <div>
              <label style={{ fontSize: '0.75rem', color: 'var(--text-dim)', display: 'block', marginBottom: 4 }}>
                Electricity Rate ($/kWh)
              </label>
              <input
                type="number"
                step="0.01"
                min="0"
                value={settings.electricityRate}
                onChange={e => updateSettings({ electricityRate: Number(e.target.value) })}
                aria-label="Electricity rate in dollars per kilowatt-hour"
              />
            </div>
            <div>
              <label style={{ fontSize: '0.75rem', color: 'var(--text-dim)', display: 'block', marginBottom: 4 }}>
                BTC Price (USD)
              </label>
              <input
                type="number"
                step="100"
                min="0"
                value={settings.btcPrice}
                onChange={e => updateSettings({ btcPrice: Number(e.target.value) })}
                aria-label="BTC price in USD"
              />
            </div>
          </div>

          <div style={{
            borderTop: '1px solid var(--border)', paddingTop: 12,
            textAlign: 'center',
          }} className="standard-grid-3">
            <div>
              <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginBottom: 4 }}>
                Daily Sats
              </div>
              <div style={{
                fontFamily: 'var(--font-heading)',
                fontWeight: 700, fontSize: '1.2rem', color: 'var(--accent)',
              }}>
                {formatSats(profit.sats)}
              </div>
            </div>
            <div>
              <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginBottom: 4 }}>
                Electricity Cost
              </div>
              <div style={{
                fontFamily: 'var(--font-heading)',
                fontWeight: 700, fontSize: '1.2rem', color: 'var(--red)',
              }}>
                ${profit.cost.toFixed(2)}
              </div>
            </div>
            <div>
              <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginBottom: 4 }}>
                Net Profit
              </div>
              <div style={{
                fontFamily: 'var(--font-heading)',
                fontWeight: 700, fontSize: '1.2rem',
                color: profit.profit >= 0 ? 'var(--green)' : 'var(--red)',
              }}>
                ${profit.profit >= 0 ? '+' : ''}{profit.profit.toFixed(2)}
              </div>
            </div>
          </div>
          {profitabilityPowerNote && (
            <div style={{
              marginTop: 8, textAlign: 'center',
              fontSize: '0.7rem', color: 'var(--text-dim)',
            }}>
              {profitabilityPowerNote}
            </div>
          )}
        </div>
      </div>
    </>
  );
}
