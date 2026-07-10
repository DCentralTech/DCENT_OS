// Data Export System — CSV/JSON export with tax reporting helper
// Feature no competitor has: built-in tax reporting from the miner itself

import React, { useState, useCallback } from 'react';
import type { ExportFormat, ExportDataType } from '../../api/feature-types';
import { useTranslation } from '../../i18n/i18n';
import { useMinerStore } from '../../store/miner';
import { InfoDot } from '../common/Tooltip';

function todayStr(): string {
  return new Date().toISOString().split('T')[0];
}

function thirtyDaysAgoStr(): string {
  const d = new Date();
  d.setDate(d.getDate() - 30);
  return d.toISOString().split('T')[0];
}

function downloadFile(content: string, filename: string, mimeType: string) {
  const blob = new Blob([content], { type: mimeType });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

export function DataExport() {
  const { t } = useTranslation();
  const addAlert = useMinerStore(s => s.addAlert);
  const hashrateHistory = useMinerStore(s => s.hashrateHistory);
  const powerHistory = useMinerStore(s => s.powerHistory);
  const tempHistory = useMinerStore(s => s.tempHistory);

  const [format, setFormat] = useState<ExportFormat>('csv');
  const [dataType, setDataType] = useState<ExportDataType>('all');
  const [startDate, setStartDate] = useState(thirtyDaysAgoStr);
  const [endDate, setEndDate] = useState(todayStr);

  const [generatingTax, setGeneratingTax] = useState(false);

  const handleExport = useCallback(() => {
    // Build data from history buffers
    const start = new Date(startDate).getTime() / 1000;
    const end = new Date(endDate).getTime() / 1000 + 86400;

    type HistoryEntry = { time: number; value: number };
    const filterByDate = (arr: HistoryEntry[]) =>
      arr.filter(p => p.time >= start && p.time <= end);

    let data: Record<string, unknown>[] = [];

    if (dataType === 'hashrate' || dataType === 'all') {
      const filtered = filterByDate(hashrateHistory);
      data = filtered.map(p => ({
        timestamp: new Date(p.time * 1000).toISOString(),
        hashrate_ghs: p.value,
      }));
    }
    if (dataType === 'temperature' || dataType === 'all') {
      const filtered = filterByDate(tempHistory);
      const tempData = filtered.map(p => ({
        timestamp: new Date(p.time * 1000).toISOString(),
        temperature_c: p.value,
      }));
      data = dataType === 'all'
        ? data.map((d, i) => ({ ...d, temperature_c: tempData[i]?.temperature_c }))
        : tempData;
    }
    if (dataType === 'power' || dataType === 'all') {
      const filtered = filterByDate(powerHistory);
      const powerData = filtered.map(p => ({
        timestamp: new Date(p.time * 1000).toISOString(),
        power_watts: p.value,
      }));
      data = dataType === 'all'
        ? data.map((d, i) => ({ ...d, power_watts: powerData[i]?.power_watts }))
        : powerData;
    }

    if (data.length === 0) {
      addAlert('warning', 'No data available for the selected date range.');
      return;
    }

    if (format === 'csv') {
      const headers = Object.keys(data[0]);
      const csv = [
        headers.join(','),
        ...data.map(row => headers.map(h => row[h] ?? '').join(',')),
      ].join('\n');
      downloadFile(csv, `dcentos-${dataType}-${startDate}-to-${endDate}.csv`, 'text/csv');
    } else {
      const json = JSON.stringify(data, null, 2);
      downloadFile(json, `dcentos-${dataType}-${startDate}-to-${endDate}.json`, 'application/json');
    }

    addAlert('info', `Exported ${data.length} records as ${format.toUpperCase()}.`);
  }, [format, dataType, startDate, endDate, hashrateHistory, tempHistory, powerHistory, addAlert]);

  const generateTaxReport = useCallback(async () => {
    setGeneratingTax(true);
    addAlert(
      'warning',
      'Tax report is unavailable until DCENT_OS exposes real share/revenue history. No estimated BTC or default power values were generated.',
    );
    setGeneratingTax(false);
  }, [addAlert]);

  return (
    <div className="feat-page">
      <div className="feat-header">
        <h2 className="feat-title">{t('export.title')}</h2>
        <p className="feat-subtitle">{t('export.subtitle')}</p>
      </div>

      {/* Export config */}
      <div className="feat-card">
        <div className="feat-form-grid">
          <div className="feat-input-group">
            <label className="feat-label">{t('export.format')}</label>
            <select
              value={format}
              onChange={e => setFormat(e.target.value as ExportFormat)}
              className="feat-input"
            >
              <option value="csv">CSV</option>
              <option value="json">JSON</option>
            </select>
          </div>

          <div className="feat-input-group">
            <label className="feat-label">{t('export.dataType')}</label>
            <select
              value={dataType}
              onChange={e => setDataType(e.target.value as ExportDataType)}
              className="feat-input"
            >
              <option value="all">All Data</option>
              <option value="hashrate">Hashrate</option>
              <option value="temperature">Temperature</option>
              <option value="power">Power</option>
              {/* Wave-13: earnings export has no data branch yet (would silently
                  yield "No data available") — disabled until share history is
                  exposed by the daemon. */}
              <option value="earnings" disabled>Earnings (coming soon)</option>
            </select>
          </div>

          <div className="feat-input-group">
            <label className="feat-label">{t('export.startDate')}</label>
            <input
              type="date"
              value={startDate}
              onChange={e => setStartDate(e.target.value)}
              className="feat-input"
            />
          </div>

          <div className="feat-input-group">
            <label className="feat-label">{t('export.endDate')}</label>
            <input
              type="date"
              value={endDate}
              onChange={e => setEndDate(e.target.value)}
              className="feat-input"
            />
          </div>
        </div>

        <div className="feat-actions" style={{ marginTop: 16 }}>
          <button className="feat-btn feat-btn-primary" onClick={handleExport}>
            {t('common.export')}
          </button>
        </div>
      </div>

      {/* Tax Reporting Helper */}
      <div className="feat-card">
        <h3 className="feat-card-title">
          {t('export.taxHelper')}
          <InfoDot
            placement="bottom"
            label="How the tax helper handles data"
            content={
              <>
                Honest by design: the tax export only uses real share and revenue
                history the miner actually recorded. It will never invent
                estimated BTC, a default wattage, or placeholder values to fill a
                report — if the data isn't there, it tells you instead of guessing.
              </>
            }
          />
        </h3>
        <p className="feat-subtitle">{t('export.taxSubtitle')}</p>

        <div className="feat-actions" style={{ marginTop: 16 }}>
          <button
            className="feat-btn feat-btn-secondary"
            onClick={generateTaxReport}
            disabled={generatingTax}
          >
            {generatingTax ? t('common.loading') : t('export.generateReport')}
          </button>
        </div>

        <div style={{ marginTop: 12, color: 'var(--text-dim)', fontSize: '0.85rem' }}>
          Tax export requires real share/revenue history. DCENT_OS will not generate estimated BTC,
          default-wattage, or placeholder tax values.
        </div>
      </div>
    </div>
  );
}
