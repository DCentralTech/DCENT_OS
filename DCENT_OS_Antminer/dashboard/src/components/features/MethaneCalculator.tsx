// Methane Mitigation Calculator — Environmental impact from methane capture mining
// Calculates CO2-eq offset, carbon credits, and trees equivalent

import React, { useState, useMemo } from 'react';
import type { GasType, MethaneInputs, MethaneResults } from '../../api/feature-types';
import { useTranslation } from '../../i18n/i18n';
import { InfoDot } from '../common/Tooltip';

const GAS_ENERGY_CONTENT: Record<GasType, number> = {
  flared: 1020,     // BTU/ft3 for natural gas
  vented: 1020,     // BTU/ft3
  landfill: 500,    // BTU/ft3 (lower quality)
  biogas: 600,      // BTU/ft3 (methane + CO2 mix)
};

// GWP (Global Warming Potential): 1 ton CH4 = 28 tons CO2-eq (IPCC AR5, 100-yr)
const METHANE_GWP = 28;
// CO2 per MMBTU of natural gas combustion
const CO2_PER_MMBTU = 0.053; // tons CO2 per MMBTU

function calculateMethane(inputs: MethaneInputs): MethaneResults {
  const { gasFlowRateMcfh, gasType, generatorEfficiencyPct } = inputs;

  // Gas energy (MMBTU/hr) = MCF/h * BTU/ft3 / 1,000,000 * 1000
  const energyMmBtuH = (gasFlowRateMcfh * GAS_ENERGY_CONTENT[gasType]) / 1000;

  // Power available (kW) = energy * efficiency * conversion
  // 1 MMBTU/hr = 293.07 kW
  const powerAvailableKw = energyMmBtuH * 293.07 * (generatorEfficiencyPct / 100);

  // CO2 offset calculation
  const hoursPerYear = 8760;
  const mmBtuPerYear = energyMmBtuH * hoursPerYear;

  // Direct CO2 from combustion (tons/yr)
  const directCo2 = mmBtuPerYear * CO2_PER_MMBTU;

  // For vented gas: methane that would escape = much worse than CO2
  // Combustion converts CH4 to CO2, so offset = (methane GWP - CO2 from combustion)
  let co2Offset: number;
  if (gasType === 'vented') {
    // Vented methane is the worst case - full GWP applies
    const methaneVolumeFt3Yr = gasFlowRateMcfh * 1000 * hoursPerYear;
    const methaneDensityLbFt3 = 0.0423;
    const methaneTonsYr = (methaneVolumeFt3Yr * methaneDensityLbFt3) / 2000;
    co2Offset = methaneTonsYr * METHANE_GWP;
  } else if (gasType === 'flared') {
    // Flared gas is already being combusted, but inefficiently (80-98%)
    // Mining captures the energy, offset = avoided inefficiency + energy value
    co2Offset = directCo2 * 0.15; // ~15% improvement over flaring
  } else {
    // Landfill/biogas: capturing methane that would otherwise escape
    const methaneVolumeFt3Yr = gasFlowRateMcfh * 1000 * hoursPerYear * 0.55; // ~55% methane content
    const methaneDensityLbFt3 = 0.0423;
    const methaneTonsYr = (methaneVolumeFt3Yr * methaneDensityLbFt3) / 2000;
    co2Offset = methaneTonsYr * METHANE_GWP;
  }

  // Carbon credit estimate ($20-50/ton CO2e, use $30 average)
  const carbonCreditEstimateUsd = co2Offset * 30;

  // Trees equivalent: 1 tree absorbs ~48 lbs (0.024 tons) CO2/yr
  const treesEquivalent = co2Offset / 0.024;

  // Methane destroyed
  const methaneVolFt3Yr = gasFlowRateMcfh * 1000 * hoursPerYear;
  const methaneDestroyedTonsYr = (methaneVolFt3Yr * 0.0423) / 2000;

  return {
    powerAvailableKw,
    co2OffsetTonsYr: co2Offset,
    carbonCreditEstimateUsd,
    treesEquivalent: Math.round(treesEquivalent),
    methaneDestroyedTonsYr,
  };
}

export function MethaneCalculator() {
  const { t } = useTranslation();

  const [inputs, setInputs] = useState<MethaneInputs>({
    gasFlowRateMcfh: 10,
    gasType: 'vented',
    generatorEfficiencyPct: 35,
  });

  const results = useMemo(() => calculateMethane(inputs), [inputs]);

  const update = (partial: Partial<MethaneInputs>) => {
    setInputs(prev => ({ ...prev, ...partial }));
  };

  return (
    <div className="feat-card">
      <h3 className="feat-card-title feat-title-green">
        {t('methane.title')}
        <InfoDot
          placement="bottom"
          label="What the methane calculator estimates"
          content={
            <>
              Flared or vented methane is a potent greenhouse gas. Burning it in
              a generator to mine instead converts it to far-weaker CO₂ while
              earning Bitcoin. This estimates the CO₂-equivalent you'd offset,
              plus carbon-credit and tree-equivalent framing. Estimates from your
              gas inputs — not metered.
            </>
          }
        />
      </h3>
      <p className="feat-subtitle">{t('methane.subtitle')}</p>

      <div className="feat-form-grid" style={{ marginTop: 16 }}>
        <div className="feat-input-group">
          <label className="feat-label">{t('methane.gasFlowRate')}</label>
          <input
            type="number"
            min="0"
            step="1"
            value={inputs.gasFlowRateMcfh}
            onChange={e => update({ gasFlowRateMcfh: Number(e.target.value) })}
            className="feat-input"
          />
        </div>

        <div className="feat-input-group">
          <label className="feat-label">{t('methane.gasType')}</label>
          <select
            value={inputs.gasType}
            onChange={e => update({ gasType: e.target.value as GasType })}
            className="feat-input"
          >
            <option value="flared">{t('methane.flared')}</option>
            <option value="vented">{t('methane.vented')}</option>
            <option value="landfill">{t('methane.landfill')}</option>
            <option value="biogas">{t('methane.biogas')}</option>
          </select>
        </div>

        <div className="feat-input-group">
          <label className="feat-label">{t('methane.generatorEfficiency')}</label>
          <input
            type="number"
            min="10"
            max="60"
            value={inputs.generatorEfficiencyPct}
            onChange={e => update({ generatorEfficiencyPct: Number(e.target.value) })}
            className="feat-input"
          />
        </div>
      </div>

      {/* Results */}
      <div className="feat-methane-results">
        <div className="feat-result-card feat-result-blue">
          <div className="feat-result-label">{t('methane.powerAvailable')}</div>
          <div className="feat-result-value">{results.powerAvailableKw.toFixed(1)} kW</div>
        </div>
        <div className="feat-result-card feat-result-green">
          <div className="feat-result-label">{t('methane.co2Offset')}</div>
          <div className="feat-result-value">
            {results.co2OffsetTonsYr.toFixed(0)} {t('methane.tonsPerYear')}
          </div>
        </div>
        <div className="feat-result-card feat-result-green">
          <div className="feat-result-label">{t('methane.carbonCredit')}</div>
          <div className="feat-result-value">${results.carbonCreditEstimateUsd.toLocaleString()}/yr</div>
        </div>
        <div className="feat-result-card feat-result-green">
          <div className="feat-result-label">{t('methane.treesEquivalent')}</div>
          <div className="feat-result-value">{results.treesEquivalent.toLocaleString()} trees</div>
        </div>
      </div>

      {/* Environmental impact running total */}
      <div className="feat-impact-banner">
        <div className="feat-impact-text">
          Mining with this gas source is estimated to prevent{' '}
          <strong>{results.co2OffsetTonsYr.toFixed(0)} tons</strong> of CO2-equivalent
          emissions per year — roughly the same as planting{' '}
          <strong>{results.treesEquivalent.toLocaleString()} trees</strong> (estimate).
        </div>
      </div>
    </div>
  );
}
