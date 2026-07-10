import React, { useState, useEffect } from 'react';
import { api } from '../../api/client';
import { useMinerStore } from '../../store/miner';
import { ActionButton } from '../common/ActionButton';
import { formatFrequency, formatVoltage, voltageToPic } from '../../utils/format';
import type { MinerTypeResponse } from '../../api/types';

interface ProfileDef {
  id: string;
  name: string;
  description: string;
  freq: number;
  voltage: number; // Board voltage in mV (e.g., 8000 = 8.0V)
  powerEst: string;
  efficiencyEst: string;
  hashrateEst: string;
  icon: string;
}

// S9 BM1387 preset tuning profiles. Estimates below are preset expectations, not live telemetry.
// PIC voltage formula: voltage_V = (1608.420446 - pic_val) / 170.423497
const PRESET_PROFILES: ProfileDef[] = [
  {
    id: 'whisper',
    name: 'Low Power',
    description: 'Space heater mode. Minimal power draw with a home fan-cap request; verify noise from RPM.',
    freq: 400, voltage: 7500,
    powerEst: '~200W', efficiencyEst: '~105 J/TH', hashrateEst: '~6 TH/s',
    icon: '\u{1F56A}', // candle
  },
  {
    id: 'eco',
    name: 'Eco',
    description: 'Best efficiency. 475 MHz @ 7.8V — community sweet spot for W/TH.',
    freq: 475, voltage: 7800,
    powerEst: '~300W', efficiencyEst: '~80 J/TH', hashrateEst: '~9 TH/s',
    icon: '\u{1F343}', // leaf
  },
  {
    id: 'balanced',
    name: 'Balanced',
    description: 'Good hashrate with reasonable power. 550 MHz @ 8.5V.',
    freq: 550, voltage: 8500,
    powerEst: '~800W', efficiencyEst: '~70 J/TH', hashrateEst: '~11.5 TH/s',
    icon: '\u2696', // balance scale
  },
  {
    id: 'performance',
    name: 'Performance',
    description: 'Stock-like hashrate. 650 MHz @ 9.0V. Needs good cooling and 240V PSU.',
    freq: 650, voltage: 9000,
    powerEst: '~1,350W', efficiencyEst: '~100 J/TH', hashrateEst: '~13.5 TH/s',
    icon: '\u26A1', // lightning
  },
  {
    id: 'custom',
    name: 'Custom',
    description: 'Per-chain frequency and voltage control. For advanced users.',
    freq: 500, voltage: 8000,
    powerEst: 'Varies', efficiencyEst: 'Varies', hashrateEst: 'Varies',
    icon: '\u2699', // gear
  },
];

export function TuningProfiles() {
  const status = useMinerStore(s => s.status);
  const addAlert = useMinerStore(s => s.addAlert);
  const chains = status?.chains ?? [];

  const [activeProfile, setActiveProfile] = useState('balanced');
  const [pendingProfile, setPendingProfile] = useState<string | null>(null);
  // Capture the REAL chain id alongside the slider values so applyCustom can
  // address the correct chain (STD-A-04) instead of the S9-only literal i+6.
  const [customChains, setCustomChains] = useState<{ id: number; freq: number; voltage: number }[]>([]);

  // W13.D2 — fetch the SKU PVT envelope so we can gate the voltage slider
  // (single-voltage VRM SKUs) and the per-chain inputs (mix_levels SKUs).
  const [miner, setMiner] = useState<MinerTypeResponse | null>(null);
  useEffect(() => {
    let cancelled = false;
    api.getMinerType()
      .then(r => { if (!cancelled) setMiner(r); })
      .catch(() => { if (!cancelled) setMiner(null); });
    return () => { cancelled = true; };
  }, []);

  // Initialize custom chain values from current status
  useEffect(() => {
    if (chains.length > 0 && customChains.length === 0) {
      setCustomChains(chains.map(c => ({
        id: c.id,
        freq: c.frequency_mhz,
        voltage: c.voltage_mv,
      })));
    }
  }, [chains]);

  // On a positively-detected non-BM1387 SKU, force the per-chain Custom view
  // (the S9 presets do not apply). Runs once the SKU is known.
  useEffect(() => {
    const asic = miner?.asic ?? null;
    if (asic != null && !/1387/.test(asic)) {
      setActiveProfile('custom');
      setPendingProfile(null);
    }
  }, [miner]);

  // The PRESET_PROFILES are S9 / BM1387-specific (200–650 MHz, 7.5–9.0V).
  // On a positively-detected non-BM1387 SKU those freq/voltage targets are
  // wrong (and potentially unsafe), so suppress the presets and steer the
  // operator to the per-chain Custom path (which respects the live PVT
  // envelope). When the SKU is unknown (null miner / older daemon) we keep the
  // historical S9 preset behaviour to avoid regressing the S9 case.
  const detectedAsic = miner?.asic ?? null;
  const isS9PresetSku = detectedAsic == null || /1387/.test(detectedAsic);

  // PVT envelope flags. Default to false / null when the SKU is unknown.
  const voltageFixed = !!miner?.voltage_fixed;
  const mixLevelsSupported = !!miner?.mix_levels_supported;
  const pvtFreqMin = miner?.pvt_freq_min_mhz ?? 0;
  const pvtFreqMax = miner?.pvt_freq_max_mhz ?? 0;
  const pvtVoltMin = miner?.pvt_voltage_min_mv ?? 0;
  const pvtVoltMax = miner?.pvt_voltage_max_mv ?? 0;
  const haveEnvelope = pvtFreqMin > 0 && pvtFreqMax > 0;
  const [envelopeError, setEnvelopeError] = useState<string | null>(null);

  const requestProfile = (profileId: string) => {
    if (profileId === 'custom') {
      setActiveProfile('custom');
      setPendingProfile(null);
      return;
    }
    setPendingProfile(profileId);
  };

  const confirmProfile = async () => {
    if (!pendingProfile) return;
    const profileId = pendingProfile;
    setPendingProfile(null);
    setActiveProfile(profileId);

    const profile = PRESET_PROFILES.find(p => p.id === profileId)!;
    const fanMode = profileId === 'whisper' || profileId === 'eco' ? 'quiet'
      : profileId === 'performance' ? 'performance' : 'balanced';

    try {
      await api.saveProfile({
        name: profileId,
        frequency_mhz: profile.freq,
        voltage_mv: profile.voltage,
        fan_mode: fanMode,
      });
      // Honest copy: POST /api/profiles saves to the tuning profile library; it
      // does not itself push freq/voltage to the chips (the daemon boots from
      // dcentrald.toml, not the library). Custom apply uses /api/debug/chip/*.
      addAlert('info', `Saved ${profile.name} profile (${profile.freq} MHz @ ${(profile.voltage / 1000).toFixed(1)}V) to the tuning library.`);
    } catch {
      addAlert('warning', 'Failed to apply tuning profile');
    }
  };

  const cancelProfile = () => {
    setPendingProfile(null);
  };

  const updateCustomChain = (index: number, field: 'freq' | 'voltage', value: number) => {
    // W13.D2 — refuse out-of-envelope inputs (don't silently clamp).
    if (field === 'freq' && haveEnvelope) {
      if (value < pvtFreqMin || value > pvtFreqMax) {
        setEnvelopeError(`Your SKU only supports ${pvtFreqMin}–${pvtFreqMax} MHz`);
        return;
      }
    }
    if (field === 'voltage' && voltageFixed) {
      setEnvelopeError('This SKU has a fixed-voltage VRM (BHB42803). Voltage cannot be adjusted.');
      return;
    }
    if (field === 'voltage' && haveEnvelope && pvtVoltMin > 0 && pvtVoltMax > 0) {
      if (value < pvtVoltMin || value > pvtVoltMax) {
        setEnvelopeError(`Your SKU only supports ${pvtVoltMin}–${pvtVoltMax} mV`);
        return;
      }
    }
    setEnvelopeError(null);
    setCustomChains(prev => prev.map((c, i) => i === index ? { ...c, [field]: value } : c));
  };

  const applyCustom = async () => {
    const failures: string[] = [];
    let freqWrites = 0;
    let voltWrites = 0;
    let voltageUnsupportedSku = false;

    for (let i = 0; i < customChains.length; i++) {
      const entry = customChains[i];
      const live = chains[i];

      // STD-A-04 — address the REAL chain id (entry.id), not the S9 FPGA-numbering
      // literal i+6 (which targets the wrong chain on Amlogic/BB chains 0/1/2).
      try {
        await api.setChipFrequency({ chain: entry.id, chip: -1, freq_mhz: entry.freq, confirm: true });
        freqWrites++;
      } catch {
        failures.push(`chain ${entry.id} frequency`);
      }

      // STD-A-02 — also push the per-chain VOLTAGE the slider captured, but only
      // when it actually changed, the SKU is not fixed-voltage, and the target can
      // be expressed as a real PIC16F1704 DAC pic_value. /api/debug/chip/voltage
      // speaks pic_value, which is an S9/BM1387-only transfer function — never
      // fabricate a pic_value for a board the formula does not apply to.
      const voltageChanged = live ? entry.voltage !== live.voltage_mv : false;
      if (voltageChanged && !voltageFixed) {
        const picValue = isS9PresetSku ? voltageToPic(entry.voltage / 1000) : null;
        if (picValue == null) {
          voltageUnsupportedSku = true;
        } else {
          try {
            await api.setChipVoltage({ chain: entry.id, pic_value: picValue, confirm: true });
            voltWrites++;
          } catch {
            failures.push(`chain ${entry.id} voltage`);
          }
        }
      }
    }

    // STD-A-02 — only claim success after the writes resolve, and surface per-chain
    // failures honestly instead of always toasting "Custom tuning applied".
    if (failures.length > 0) {
      addAlert('warning', `Custom tuning partially failed — not applied: ${failures.join(', ')}.`);
    } else if (freqWrites + voltWrites === 0) {
      addAlert('info', 'No pending changes to apply.');
    } else {
      const parts = [`${freqWrites} frequency`];
      if (voltWrites > 0) parts.push(`${voltWrites} voltage`);
      addAlert('info', `Custom tuning applied (${parts.join(' + ')} write${freqWrites + voltWrites === 1 ? '' : 's'}).`);
    }
    if (voltageUnsupportedSku) {
      addAlert('warning', 'Per-chain voltage cannot be set from here on this SKU (no PIC DAC voltage path); frequency changes were still applied.');
    }
  };

  //  Agent W3 — pending-vs-current diff for the per-chain sliders.
  // "Pending" means the operator dragged a slider but hasn't clicked Apply.
  // We compare against live chain telemetry (chains[i]) at slider time.
  const hasPendingCustomChanges = customChains.some((c, i) => {
    const live = chains[i];
    if (!live) return false;
    return c.freq !== live.frequency_mhz || c.voltage !== live.voltage_mv;
  });

  // Revert restores slider state to live current values; safe because
  // it does not call the backend.
  const revertCustom = () => {
    setCustomChains(chains.map(c => ({
      id: c.id,
      freq: c.frequency_mhz,
      voltage: c.voltage_mv,
    })));
    setEnvelopeError(null);
  };

  // Soft warning threshold for voltage: 80% of the way to the PVT cap.
  // The HAL hard-cap stays the slider max; this is advisory only.
  const voltSoftWarn = (haveEnvelope && pvtVoltMin > 0 && pvtVoltMax > 0)
    ? pvtVoltMin + Math.round((pvtVoltMax - pvtVoltMin) * 0.8)
    : null;

  const activePreset = PRESET_PROFILES.find(p => p.id === activeProfile);
  const heroPresetName = activePreset?.name ?? 'Custom';
  const heroBadgeTone: 'good' | 'warn' = activeProfile === 'performance' ? 'warn' : 'good';
  const heroBadgeLabel = activeProfile === 'performance'
    ? 'stock-class draw'
    : activeProfile === 'whisper' ? 'space heater'
    : activeProfile === 'eco' ? 'efficient'
    : activeProfile === 'balanced' ? 'balanced'
    : 'per-chain';
  const skuLabel = miner?.model ?? miner?.hashboard ?? null;
  const skuEnvelopeText = haveEnvelope
    ? `${pvtFreqMin}–${pvtFreqMax} MHz${pvtVoltMin > 0 && pvtVoltMax > 0 ? ` · ${pvtVoltMin}–${pvtVoltMax} mV` : ''}`
    : 'envelope unknown';

  return (
    <div className="page-content">
      <div className="page-hero-strip">
        <div className="page-hero-summary">
          <div className="page-hero-eyebrow">TUNING</div>
          <div className="page-hero-title">Profile Targets</div>
          <div className="page-hero-stat">{heroPresetName}</div>
          <div className="page-hero-substat">
            {activePreset?.description ?? 'Per-chain frequency and voltage control.'}
          </div>
        </div>
        <div className="hero-kpi-strip">
          <div className="hero-kpi">
            <div className="kpi-label">Target Hashrate</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{activePreset?.hashrateEst ?? '—'}</span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Target Watts</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{activePreset?.powerEst ?? '—'}</span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">J/TH</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{activePreset?.efficiencyEst ?? '—'}</span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">SKU Envelope</div>
            <div className="kpi-value">
              <span className="kpi-num-anim" style={{ fontSize: '0.95rem' }}>
                {skuLabel ?? 'unknown'}
              </span>
            </div>
            <div className="kpi-sub">{skuEnvelopeText}</div>
          </div>
        </div>
      </div>

      <section className="section">
      <div className="section-title">
        Tuning Profiles
        <span className={`small-tag ${heroBadgeTone}`}>{heroBadgeLabel}</span>
      </div>

      {/* P2 (Wave-9): the PRESET_PROFILES are S9 / BM1387-specific. On a
          positively-detected non-BM1387 SKU we suppress them and steer the
          operator to the envelope-aware per-chain Custom controls below. */}
      {!isS9PresetSku && (
        <div
          data-testid="tuning-non-s9-banner"
          style={{
            background: 'rgba(245,158,11,0.08)',
            border: '1px solid rgba(245,158,11,0.30)',
            borderRadius: 8,
            padding: 12,
            marginBottom: 16,
            fontSize: '0.82rem',
            color: 'var(--text)',
            lineHeight: 1.5,
          }}
        >
          <strong style={{ color: '#F59E0B' }}>Presets are S9 (BM1387) only.</strong>{' '}
          This miner reports <span style={{ fontFamily: "'JetBrains Mono', monospace" }}>{detectedAsic}</span>,
          so the canned Low Power / Eco / Balanced / Performance targets do not apply.
          Use the per-chain controls below — they stay inside this SKU&apos;s
          {haveEnvelope ? ` ${skuEnvelopeText}` : ' detected'} envelope.
        </div>
      )}

      {isS9PresetSku && (
      <div style={{
        display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(260px, 1fr))', gap: 12,
        marginBottom: 24,
      }}>
        {PRESET_PROFILES.map(profile => {
          const isActive = activeProfile === profile.id;
          return (
            <button
              key={profile.id}
              type="button"
              onClick={() => requestProfile(profile.id)}
              className={`profile-card ${isActive ? 'active' : ''}`}
              aria-pressed={isActive}
            >
              <div className="profile-card-header">
                <span>{profile.icon}</span>
                {profile.name}
                {isActive && (
                  <span className="profile-card-badge">
                    ACTIVE
                  </span>
                )}
              </div>
              <div className="profile-card-copy">
                {profile.description}
              </div>
              <div className="profile-card-stats">
                <div>
                  <span className="profile-card-stat-label">Freq: </span>
                  <span className="metric-value">
                    {profile.freq} MHz
                  </span>
                </div>
                <div>
                  <span className="profile-card-stat-label">Voltage: </span>
                  <span className="metric-value">
                    {(profile.voltage / 1000).toFixed(1)}V
                  </span>
                </div>
                <div>
                  <span className="profile-card-stat-label">Hashrate est.: </span>
                  <span style={{ color: 'var(--text)' }}>{profile.hashrateEst}</span>
                </div>
                <div>
                  <span className="profile-card-stat-label">Power est.: </span>
                  <span style={{ color: 'var(--text)' }}>{profile.powerEst}</span>
                </div>
                <div className="profile-card-stat full">
                  <span className="profile-card-stat-label">Efficiency est.: </span>
                  <span style={{ color: 'var(--text)' }}>{profile.efficiencyEst}</span>
                </div>
              </div>
            </button>
          );
        })}
      </div>
      )}

      {/* Profile change confirmation dialog */}
      {pendingProfile && (() => {
        const pending = PRESET_PROFILES.find(p => p.id === pendingProfile)!;
        return (
          <div style={{
            background: 'var(--card-bg)', border: '2px solid var(--yellow)',
            borderRadius: 'var(--radius)', padding: 20, marginBottom: 20,
          }}>
            <div style={{
              fontFamily: "var(--font-heading)",
              fontWeight: 700, fontSize: '1.1rem',
              color: 'var(--yellow)', marginBottom: 12,
            }}>
              Save Tuning Profile
            </div>
            <div style={{ fontSize: '0.85rem', color: 'var(--text)', marginBottom: 16, lineHeight: 1.6 }}>
              Save the <strong>{pending.name}</strong> profile&nbsp;
              ({pending.freq} MHz @ {(pending.voltage / 1000).toFixed(1)}V) to your tuning library?
              <br />
              This stores the target in the library for later use — it does <strong>not</strong> change
              the frequency or voltage on the running miner. To change the live miner, use the per-chain
              Custom controls below.
            </div>
            <div className="standard-inline-actions">
              <button className="btn btn-primary" onClick={confirmProfile} style={{ padding: '8px 20px' }}>
                Save to Library
              </button>
              <button className="btn btn-secondary" onClick={cancelProfile} style={{ padding: '8px 20px' }}>
                Cancel
              </button>
            </div>
          </div>
        );
      })()}

      {/* Custom per-chain tuning */}
      {activeProfile === 'custom' && customChains.length > 0 && (
        <div>
          <div className="section-title">Per-Chain Tuning</div>
          {/* Wave 4 — pending-changes bar. Visible only when the operator
              has dragged a slider but hasn't applied. Provides clear
              Apply / Revert affordances. */}
          {hasPendingCustomChanges && (
            <div className="tuning-pending-bar" role="status" aria-live="polite" data-testid="tuning-pending-bar">
              <div className="tpb-msg">
                <strong>Pending changes.</strong> Slider values differ from
                what's currently running on the hash boards.
              </div>
              <div className="tpb-actions">
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={revertCustom}
                  data-testid="tuning-revert"
                  style={{ padding: '6px 14px' }}
                >
                  Revert to current
                </button>
              </div>
            </div>
          )}
          {/* W13.D2 — PVT envelope advisory + gate */}
          {(voltageFixed || mixLevelsSupported || envelopeError) && (
            <div
              data-testid="tuning-envelope-banner"
              style={{
                background: 'rgba(245,158,11,0.08)',
                border: '1px solid rgba(245,158,11,0.30)',
                borderRadius: 8,
                padding: 10,
                marginBottom: 12,
                fontSize: '0.8rem',
                color: 'var(--text)',
              }}
            >
              {voltageFixed && (
                <div>
                  <strong style={{ color: '#F59E0B' }}>Voltage locked.</strong>{' '}
                  This SKU has a fixed-voltage VRM (BHB42803). Voltage cannot be adjusted from
                  the dashboard.
                </div>
              )}
              {mixLevelsSupported && (
                <div style={{ marginTop: voltageFixed ? 6 : 0 }}>
                  <strong style={{ color: '#F59E0B' }}>Mix levels supported.</strong>{' '}
                  Per-chain frequency support coming in W14.
                </div>
              )}
              {envelopeError && (
                <div style={{ marginTop: 6, color: '#EF4444' }} data-testid="tuning-envelope-error">
                  {envelopeError}
                </div>
              )}
            </div>
          )}
          <div style={{ display: 'grid', gap: 12 }}>
            {customChains.map((chain, i) => {
              const freqMin = haveEnvelope ? pvtFreqMin : 200;
              const freqMax = haveEnvelope ? pvtFreqMax : 800;
              const voltMin = haveEnvelope && pvtVoltMin > 0 ? pvtVoltMin : 7000;
              const voltMax = haveEnvelope && pvtVoltMax > 0 ? pvtVoltMax : 9500;
              const freqStep = haveEnvelope ? 5 : 25;
              const voltStep = haveEnvelope ? 5 : 100;
              return (
              <div key={i} style={{
                background: 'var(--card-bg)', borderRadius: 'var(--radius)',
                padding: 16, border: '1px solid var(--border)',
              }}>
                <div style={{
                  fontFamily: "var(--font-heading)",
                  fontWeight: 700, color: 'var(--accent)', marginBottom: 12,
                }}>
                  Chain {chain.id}
                  {chains[i] && (
                    <span style={{ fontSize: '0.75rem', color: 'var(--text-dim)', marginLeft: 12 }}>
                      Current: {formatFrequency(chains[i].frequency_mhz)} / {formatVoltage(chains[i].voltage_mv)}
                    </span>
                  )}
                </div>
                <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 16 }}>
                  <div title={mixLevelsSupported ? 'Per-chain frequency support coming in W14' : undefined}>
                    <label className="field-label">
                      Frequency: {chain.freq} MHz
                    </label>
                    <input
                      data-testid={`tuning-freq-slider-${i}`}
                      type="range" min={freqMin} max={freqMax} step={freqStep}
                      value={chain.freq}
                      disabled={mixLevelsSupported}
                      onChange={e => updateCustomChain(i, 'freq', Number(e.target.value))}
                      style={{ width: '100%' }}
                    />
                    <div style={{
                      display: 'flex', justifyContent: 'space-between',
                      fontSize: '0.65rem', color: 'var(--text-dim)',
                    }}>
                      <span>{freqMin}</span>
                      <span>{Math.round((freqMin + freqMax) / 2)}</span>
                      <span>{freqMax}</span>
                    </div>
                  </div>
                  <div title={voltageFixed ? 'This SKU has a fixed-voltage VRM (BHB42803). Voltage cannot be adjusted.' : undefined}>
                    <label className="field-label">
                      Voltage: {voltageFixed
                        ? `fixed ${pvtVoltMin || chain.voltage} mV`
                        : `${(chain.voltage / 1000).toFixed(1)}V`}
                    </label>
                    <input
                      data-testid={`tuning-volt-slider-${i}`}
                      type="range" min={voltMin} max={voltMax} step={voltStep}
                      value={voltageFixed ? (pvtVoltMin || chain.voltage) : chain.voltage}
                      disabled={voltageFixed}
                      onChange={e => updateCustomChain(i, 'voltage', Number(e.target.value))}
                      style={{ width: '100%' }}
                    />
                    <div style={{
                      display: 'flex', justifyContent: 'space-between',
                      fontSize: '0.65rem', color: 'var(--text-dim)',
                    }}>
                      <span>{(voltMin / 1000).toFixed(1)}V</span>
                      <span>{((voltMin + voltMax) / 2000).toFixed(1)}V</span>
                      <span>{(voltMax / 1000).toFixed(1)}V</span>
                    </div>
                  </div>
                </div>
              </div>
              );
            })}
          </div>
          <div style={{ marginTop: 12 }}>
            <ActionButton
              label="Apply Custom Tuning"
              onClick={applyCustom}
              confirm="This will change frequency and voltage on your hash boards. Continue?"
            />
          </div>
        </div>
      )}
      </section>
    </div>
  );
}
