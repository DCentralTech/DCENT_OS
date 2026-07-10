import React, { useState, useEffect } from 'react';
import { useMinerStore } from '../../store/miner';
import { api } from '../../api/client';
import { NightMode } from './NightMode';
import { AccentColorPicker } from '../common/AccentColorPicker';
import { LedSettings } from '../common/LedSettings';
import { ModePillSwitch } from '../common/ModePillSwitch';
import { NextStepsPanel } from '../common/NextStepsPanel';
import { PowerCalibrationCard } from '../common/PowerCalibrationCard';
import { useModeNavigation } from '../../hooks/useModeNavigation';
import { InfoBanner } from '../common/InfoBanner';
import { glossaryText } from '../../utils/glossary';
import type { PoolsResponse } from '../../api/types';

export function SettingsView() {
  const settings = useMinerStore(s => s.settings);
  const updateSettings = useMinerStore(s => s.updateSettings);
  const systemInfo = useMinerStore(s => s.systemInfo);
  const status = useMinerStore(s => s.status);

  const [minerName, setMinerName] = useState(settings.minerName);
  const [electricityRate, setElectricityRate] = useState(String(settings.electricityRate));
  const [btcPrice, setBtcPrice] = useState(String(settings.btcPrice));
  const [powerBudget, setPowerBudget] = useState(
    settings.powerBudgetWatts != null ? String(settings.powerBudgetWatts) : ''
  );
  const [saved, setSaved] = useState(false);

  // Earnings setup state
  const [poolInfo, setPoolInfo] = useState<PoolsResponse | null>(null);
  const [btcAddress, setBtcAddress] = useState('');
  const [savingPool, setSavingPool] = useState(false);
  const [poolSaved, setPoolSaved] = useState(false);
  const { switchMode, startTaskHandoff } = useModeNavigation();

  // Fetch pool info on mount
  useEffect(() => {
    api.getPools()
      .then(pools => {
        setPoolInfo(pools);
        if (pools.pools && pools.pools.length > 0) {
          const worker = pools.pools[0].worker || '';
          const wallet = worker.split('.')[0] || '';
          setBtcAddress(wallet);
        }
      })
      .catch(() => {
        // Miner might not be running — show subtle warning
        useMinerStore.getState().addToast('Could not load pool info', 'warning');
      });
  }, []);

  const handleSavePool = async () => {
    if (!poolInfo?.pools?.length) return;
    setSavingPool(true);
    try {
      const currentPool = poolInfo.pools[0];
      const workerSuffix = currentPool.worker?.includes('.')
        ? currentPool.worker.split('.').slice(1).join('.')
        : minerName.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'heater';
      const worker = btcAddress ? btcAddress + '.' + workerSuffix : workerSuffix;
      await api.configurePools({
        url: currentPool.url,
        worker,
        password: currentPool.password || 'x',
      });
      setPoolSaved(true);
      setTimeout(() => setPoolSaved(false), 2000);
    } catch {
      useMinerStore.getState().addToast('Failed to save pool address', 'error');
    } finally {
      setSavingPool(false);
    }
  };

  // Sync from store
  useEffect(() => {
    setMinerName(settings.minerName);
    setElectricityRate(String(settings.electricityRate));
    setBtcPrice(String(settings.btcPrice));
    setPowerBudget(settings.powerBudgetWatts != null ? String(settings.powerBudgetWatts) : '');
  }, [settings]);

  const handleSave = () => {
    const newBtcPrice = parseFloat(btcPrice) || 100000;
    const priceChanged = newBtcPrice !== settings.btcPrice;
    updateSettings({
      minerName,
      electricityRate: (() => { const p = parseFloat(electricityRate); return !isNaN(p) && p >= 0 ? p : 0.10; })(),
      btcPrice: newBtcPrice,
      powerBudgetWatts: powerBudget ? parseInt(powerBudget, 10) : null,
      ...(priceChanged ? { btcPriceLastUpdated: Date.now() } : {}),
    });
    setSaved(true);
    setTimeout(() => setSaved(false), 2000);
  };

  // BTC price staleness check
  const btcPriceAge = settings.btcPriceLastUpdated
    ? Math.floor((Date.now() - settings.btcPriceLastUpdated) / (1000 * 60 * 60 * 24))
    : null;
  const btcPriceStale = btcPriceAge !== null && btcPriceAge > 7;

  const handleTempUnitToggle = () => {
    updateSettings({
      temperatureUnit: settings.temperatureUnit === 'C' ? 'F' : 'C',
    });
  };

  const version = systemInfo?.version ?? status?.firmware_version ?? '--';
  const model = systemInfo?.model ?? '--';

  return (
    <div className="settings-view nest-settings">
      <h2 className="settings-title">Settings</h2>

      <NextStepsPanel mode="heater" />

      {/* ── Kit card 1: Temperature & rate ──────────────────────────────
          Recomposed into the kit `HeaterSettings` "Temperature & rate"
          card grammar. Production is richer than the 3-row kit demo, so
          every real setting + its api/store wiring is kept verbatim;
          only the visual container is the kit card. */}
      <div className="nest-card heater-settings-card">
        <div className="nest-card-eyebrow heater-settings-eyebrow">
          <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M14 14.76V3.5a2.5 2.5 0 0 0-5 0v11.26a4.5 4.5 0 1 0 5 0z" />
          </svg>
          Temperature &amp; rate
        </div>

      {/* Miner Name */}
      <div className="settings-section">
        <label className="settings-label" htmlFor="settings-miner-name">Device Name</label>
        <input
          id="settings-miner-name"
          className="settings-input"
          type="text"
          value={minerName}
          onChange={e => setMinerName(e.target.value)}
          placeholder="My Heater"
        />
      </div>

      {/* Temperature Unit */}
      <div className="settings-section">
        <div className="settings-row">
          <div>
            <label className="settings-label">Temperature Unit</label>
            <div className="settings-hint">Display temperatures in Celsius or Fahrenheit</div>
          </div>
          <button
            className="temp-unit-toggle"
            role="switch"
            aria-checked={settings.temperatureUnit === 'F'}
            aria-label="Temperature unit"
            onClick={handleTempUnitToggle}
            onKeyDown={(e) => {
              if (e.key === ' ' || e.key === 'Enter') {
                e.preventDefault();
                handleTempUnitToggle();
              }
            }}
          >
            <span className={settings.temperatureUnit === 'C' ? 'active' : ''}>C</span>
            <span className={settings.temperatureUnit === 'F' ? 'active' : ''}>F</span>
          </button>
        </div>
      </div>

      {/* Electricity Rate */}
      <div className="settings-section">
        <label
          className="settings-label"
          htmlFor="settings-electricity-rate"
          data-tooltip="Your power price per kilowatt-hour — check a recent utility bill. Used to estimate daily running cost and net value."
        >
          Electricity Rate
        </label>
        <div className="settings-input-group">
          <span className="settings-input-prefix" aria-hidden="true">$</span>
          <input
            id="settings-electricity-rate"
            className="settings-input with-prefix"
            type="number"
            step="0.01"
            min="0"
            value={electricityRate}
            onChange={e => setElectricityRate(e.target.value)}
            aria-label="Electricity rate in dollars per kilowatt-hour"
          />
          <span className="settings-input-suffix" aria-hidden="true">/kWh</span>
        </div>
      </div>

      {/* Power Budget */}
      <div className="settings-section">
        <label
          className="settings-label"
          htmlFor="settings-power-budget"
          data-tooltip={glossaryText('power_budget')}
        >
          Power Budget
        </label>
        <div id="settings-power-budget-hint" className="settings-hint">Dashboard reference only — recorded for circuit/cost planning, not enforced on the miner. Leave empty if you don't track one. To actually limit draw, set a power/efficiency target in tuning.</div>
        <div className="settings-input-group">
          <input
            id="settings-power-budget"
            className="settings-input with-suffix"
            type="number"
            step="50"
            min="0"
            value={powerBudget}
            onChange={e => setPowerBudget(e.target.value)}
            placeholder="No limit"
            aria-describedby="settings-power-budget-hint"
          />
          <span className="settings-input-suffix" aria-hidden="true">W</span>
        </div>
      </div>

      <div className="settings-section">
        <PowerCalibrationCard />
      </div>

      {/* BTC Price */}
      <div className="settings-section">
        <label
          className="settings-label"
          htmlFor="settings-btc-price"
          data-tooltip={glossaryText('btc_price_display')}
        >
          Bitcoin Price
        </label>
        <div className="settings-hint">Used to estimate daily earnings in dollars</div>
        <div className="settings-input-group">
          <span className="settings-input-prefix" aria-hidden="true">$</span>
          <input
            id="settings-btc-price"
            className="settings-input with-prefix"
            type="number"
            step="100"
            min="0"
            value={btcPrice}
            onChange={e => setBtcPrice(e.target.value)}
            aria-label="Bitcoin price in US dollars"
          />
          <span className="settings-input-suffix" aria-hidden="true">USD</span>
        </div>
        <div className="settings-microcopy">
          Price is manually set. Update it periodically for accurate earnings estimates.
          {btcPriceAge !== null && (
            <span> Last updated {btcPriceAge === 0 ? 'today' : `${btcPriceAge} day${btcPriceAge === 1 ? '' : 's'} ago`}.</span>
          )}
        </div>
        {btcPriceStale && (
          <InfoBanner tone="warn" className="settings-inline-banner" dense>
            Bitcoin price hasn't been updated in {btcPriceAge} days. Earnings estimates may be inaccurate.
          </InfoBanner>
        )}
      </div>

      {/* Save button */}
      <button className="settings-save-btn" onClick={handleSave} aria-live="polite">
        {saved ? 'Saved!' : 'Save Settings'}
      </button>
      </div>{/* ── end kit card 1: Temperature & rate ── */}

      {/* ── Kit card 2: Quiet hours ─────────────────────────────────────
          The kit "Quiet hours" card. Production's real Quiet Hours surface
          is the <NightMode/> component (its own api wiring). Wrapped in the
          kit card chrome; component + wiring untouched. */}
      <div className="nest-card heater-settings-card heater-settings-quiet-card">
        <div className="nest-card-eyebrow heater-settings-eyebrow">
          <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
          </svg>
          Quiet hours
        </div>
        <NightMode />
      </div>

      {/* ── Kit card 3: Earnings setup ──────────────────────────────────
          The kit "Earnings setup" card. Every real earnings setting +
          api/store wiring (pool fetch, address save, handoffs) preserved. */}
      <div className="nest-card heater-settings-card heater-settings-earnings-card">
        <div className="nest-card-eyebrow heater-settings-eyebrow">
          <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M12 1v22M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6" />
          </svg>
          Earnings setup
        </div>
      <div className="settings-section">
        {/* Section heading now provided by the kit card eyebrow above
            (recomposed to the kit "Earnings setup" card grammar). */}
        {!btcAddress && (
          <InfoBanner tone="warn" className="settings-inline-banner">
            Add your Bitcoin address to start earning. Without it, your heater
            runs but earns nothing.
          </InfoBanner>
        )}

        {/* Current Pool */}
        {poolInfo?.pools && poolInfo.pools.length > 0 && (
          <div className="settings-pool-block">
            <div
              className="settings-hint settings-hint--tight"
              data-tooltip={glossaryText('pool_state')}
            >
              Current Pool
            </div>
            <div className="settings-pool-url">
              {poolInfo.pools[0].url || 'Not configured'}
            </div>
          </div>
        )}

        {/* Bitcoin Address */}
        <div className="settings-pool-block">
          <label htmlFor="settings-btc-address" className="settings-hint settings-hint--block">Bitcoin Address</label>
          <input
            id="settings-btc-address"
            className="settings-input"
            type="text"
            value={btcAddress}
            onChange={e => setBtcAddress(e.target.value)}
            placeholder="Paste your Bitcoin address (bc1... or 1...)"
            aria-describedby="settings-btc-address-hint"
          />
          <div id="settings-btc-address-hint" className="settings-hint" style={{ marginTop: 4 }}>
            Where your mining rewards are sent
          </div>
        </div>

        {/* Save Pool Button */}
        <button
          className={`settings-save-btn${(savingPool || !poolInfo?.pools?.length) ? ' is-dim' : ''}`}
          onClick={handleSavePool}
          disabled={savingPool || !poolInfo?.pools?.length}
          aria-live="polite"
        >
          {poolSaved ? 'Saved!' : savingPool ? 'Saving...' : 'Save Address'}
        </button>

        {/* Change Pool link */}
        <div className="settings-link-row">
          <button
            type="button"
            className="settings-text-link"
            onClick={() => { void startTaskHandoff('standard', 'pools', { returnLabel: 'Back to Heater Settings' }); }}
          >
            Open Pool Setup
          </button>
          <div className="settings-hint settings-hint--tight">
            Jump straight into Mining mode pool controls to change pool endpoints or SV2 settings
          </div>
        </div>
      </div>
      </div>{/* ── end kit card 3: Earnings setup ── */}

      {/* ── Kit card 4: Accent color ────────────────────────────────────
          The kit "Accent color" card. AccentColorPicker kept as-is. */}
      <div className="nest-card heater-settings-card heater-settings-accent-card">
        <div className="nest-card-eyebrow heater-settings-eyebrow">
          <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.38-.5-2-1-3-1.072-2.143-.224-4.054 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.153.433-2.294 1-3a2.5 2.5 0 0 0 2.5 2.5z" />
          </svg>
          Accent color
        </div>
        <div className="settings-section">
          <div className="settings-hint settings-hint--spaced">
            Personalize the dashboard accent. Affects every mode.
          </div>
          <AccentColorPicker />
        </div>
      </div>

      {/* ── Extra kit cards (production is richer than the kit demo —
          these real settings are NOT dropped, just moved into the kit
          card grammar). Device info / dashboard mode / LED settings. ──── */}
      <div className="nest-card heater-settings-card heater-settings-device-card">
        <div className="nest-card-eyebrow heater-settings-eyebrow">
          <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <circle cx="12" cy="12" r="10" />
            <line x1="12" y1="16" x2="12" y2="12" />
            <line x1="12" y1="8" x2="12.01" y2="8" />
          </svg>
          Device info
        </div>
      <div className="settings-section">
        <div className="settings-info-grid">
          <div className="settings-info-row">
            <span>Firmware</span>
            <span>DCENTos v{version}</span>
          </div>
          <div className="settings-info-row">
            <span>Model</span>
            <span>{model}</span>
          </div>
          {systemInfo?.hostname && (
            <div className="settings-info-row">
              <span>Hostname</span>
              <span>{systemInfo.hostname}</span>
            </div>
          )}
          {systemInfo?.mac && (
            <div className="settings-info-row">
              <span>MAC Address</span>
              <span className="settings-mono">{systemInfo.mac}</span>
            </div>
          )}
        </div>
      </div>
      </div>{/* ── end kit card: Device info ── */}

      {/* Dashboard Modes — kit card grammar */}
      <div className="nest-card heater-settings-card heater-settings-mode-card">
        <div className="nest-card-eyebrow heater-settings-eyebrow">
          <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M3 9.5L12 3l9 6.5V21a1 1 0 0 1-1 1h-5v-6h-6v6H4a1 1 0 0 1-1-1V9.5z" />
          </svg>
          Dashboard mode
        </div>
      <div className="settings-section">
        <div className="settings-hint settings-hint--spaced">
          Switch between heater, mining, and hacker views without hunting through hidden menus.
        </div>
        <ModePillSwitch />
        <div className="settings-mode-actions">
          <button className="settings-save-btn" onClick={() => { void startTaskHandoff('standard', 'pools', { returnLabel: 'Back to Heater Settings' }); }}>
            Open Pool Setup In Mining Mode
          </button>
          <button className="settings-save-btn" onClick={() => { void startTaskHandoff('standard', 'temperature', { returnLabel: 'Back to Heater Settings' }); }}>
            Open Cooling Details In Mining Mode
          </button>
          <button className="settings-save-btn settings-save-btn--ghost" onClick={() => { void switchMode('hacker', 'dashboard'); }}>
            Open Advanced Tools
          </button>
        </div>
      </div>
      </div>{/* ── end kit card: Dashboard mode ── */}

      {/* LED Settings */}
      <div className="settings-led-card nest-card heater-settings-card heater-settings-led-card">
        <LedSettings />
      </div>

      {/* Bottom padding for nav bar */}
      <div className="settings-bottom-spacer" aria-hidden="true" />
    </div>
  );
}
