import React, { useEffect, useState } from 'react';
import { api } from '../../api/client';
import type { PowerCalibrationResponse } from '../../api/types';
import { useMinerStore } from '../../store/miner';

function formatMaybeWatts(watts: number | null | undefined): string {
  return watts && watts > 0 ? `${Math.round(watts).toLocaleString()} W` : '--';
}

function calibrationPowerLabel(calibration: PowerCalibrationResponse | null): string {
  if (!calibration) {
    return 'Power telemetry unavailable';
  }
  if (calibration.power_source_detail === 'pmbus_measured') {
    return 'PMBus measured power';
  }
  if (calibration.power_source_detail === 'adc_measured') {
    return 'ADC measured power';
  }
  if (calibration.live_power_available === false) {
    return 'Power telemetry unavailable';
  }
  if (calibration.power_source_detail === 'wall_calibrated_estimate') {
    return 'Wall-meter calibrated estimate';
  }
  if (calibration.power_modeled) {
    return 'Modeled runtime estimate';
  }
  return calibration.power_source || 'Power telemetry';
}

export function PowerCalibrationCard() {
  const addToast = useMinerStore(s => s.addToast);
  const [calibration, setCalibration] = useState<PowerCalibrationResponse | null>(null);
  const [measuredWallWatts, setMeasuredWallWatts] = useState('');
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    api.getPowerCalibration()
      .then(setCalibration)
      .catch(() => {
        addToast('Could not load power calibration', 'warning');
      });
  }, [addToast]);

  const handleCalibrate = async () => {
    const measured = Number(measuredWallWatts);
    if (!Number.isFinite(measured) || measured <= 0) {
      addToast('Enter the wall-meter reading in watts', 'warning');
      return;
    }

    setSaving(true);
    try {
      const response = await api.updatePowerCalibration({ measured_wall_watts: measured });
      if (response.status === 'error') {
        addToast(response.message || 'Failed to save power calibration', 'error');
        return;
      }
      setCalibration(await api.getPowerCalibration());
      setMeasuredWallWatts('');
      addToast('Power calibration saved', 'success');
    } catch {
      addToast('Failed to save power calibration', 'error');
    } finally {
      setSaving(false);
    }
  };

  const handleClear = async () => {
    setSaving(true);
    try {
      const response = await api.updatePowerCalibration({ enabled: false });
      if (response.status === 'error') {
        addToast(response.message || 'Failed to clear power calibration', 'error');
        return;
      }
      setCalibration(await api.getPowerCalibration());
      addToast('Power calibration cleared', 'success');
    } catch {
      addToast('Failed to clear power calibration', 'error');
    } finally {
      setSaving(false);
    }
  };

  const updatedAt = calibration?.updated_at_ms
    ? new Date(calibration.updated_at_ms).toLocaleString()
    : null;
  const powerLabel = calibrationPowerLabel(calibration);

  return (
    <div style={{
      background: 'var(--card-bg)', borderRadius: 'var(--radius)',
      padding: 16, border: '1px solid var(--border)',
    }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 12, marginBottom: 10 }}>
        <div>
          <div style={{ fontWeight: 700, color: 'var(--text)' }}>Power Calibration</div>
          <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', marginTop: 4, lineHeight: 1.5 }}>
            Enter the reading from your wall meter, Kill-A-Watt, or smart plug while the miner is stable.
            DCENT_OS keeps showing both household wall power and miner unit power before PSU losses.
          </div>
        </div>
        {calibration?.enabled && (
          <div style={{
            whiteSpace: 'nowrap', fontSize: '0.72rem', fontWeight: 700,
            color: 'var(--accent)', background: 'rgba(247,147,26,0.12)',
            border: '1px solid rgba(247,147,26,0.24)', borderRadius: 999,
            padding: '6px 10px',
          }}>
            Active x{calibration.multiplier.toFixed(3)}
          </div>
        )}
      </div>

      <div style={{
        display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))',
        gap: 10, marginBottom: 12,
      }}>
        <div style={{ padding: 10, borderRadius: 10, background: 'var(--bg)', border: '1px solid var(--border)' }}>
          <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)', marginBottom: 4 }}>Current Wall Power</div>
          <div style={{ fontWeight: 700 }}>{formatMaybeWatts(calibration?.current_reported_wall_watts)}</div>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginTop: 4 }}>{powerLabel}</div>
        </div>
        <div style={{ padding: 10, borderRadius: 10, background: 'var(--bg)', border: '1px solid var(--border)' }}>
          <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)', marginBottom: 4 }}>Current Unit Power</div>
          <div style={{ fontWeight: 700 }}>{formatMaybeWatts(calibration?.current_reported_unit_watts)}</div>
          <div style={{ fontSize: '0.68rem', color: 'var(--text-dim)', marginTop: 4 }}>
            {calibration?.power_modeled ? 'Estimate source' : 'Reported source'}
          </div>
        </div>
        <div style={{ padding: 10, borderRadius: 10, background: 'var(--bg)', border: '1px solid var(--border)' }}>
          <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)', marginBottom: 4 }}>Reference Meter Reading</div>
          <div style={{ fontWeight: 700 }}>{formatMaybeWatts(calibration?.reference_wall_watts)}</div>
        </div>
      </div>

      {calibration?.power_note && (
        <div style={{
          marginBottom: 12, padding: 10, borderRadius: 10,
          background: calibration.live_power_available === false ? 'rgba(245,158,11,0.08)' : 'var(--bg)',
          border: '1px solid var(--border)',
          fontSize: '0.76rem', color: 'var(--text-secondary)', lineHeight: 1.5,
        }}>
          {calibration.power_note}
        </div>
      )}

      {calibration?.enabled && (
        <div style={{
          marginBottom: 12, padding: 10, borderRadius: 10,
          background: 'rgba(34,197,94,0.08)', border: '1px solid rgba(34,197,94,0.2)',
          fontSize: '0.78rem', color: 'var(--text-secondary)', lineHeight: 1.5,
        }}>
          Calibrated from {formatMaybeWatts(calibration.estimated_wall_watts)} estimated wall / {formatMaybeWatts(calibration.estimated_unit_watts)} estimated unit
          to {formatMaybeWatts(calibration.reference_wall_watts)} measured wall.
          {updatedAt && ` Saved ${updatedAt}.`}
        </div>
      )}

      <div style={{ display: 'flex', gap: 10, flexWrap: 'wrap', alignItems: 'center' }}>
        <div style={{ flex: '1 1 220px', minWidth: 180 }}>
          <label style={{ fontSize: '0.72rem', color: 'var(--text-dim)', display: 'block', marginBottom: 4 }}>
            Measured Wall Watts
          </label>
          <input
            type="number"
            min="50"
            step="1"
            value={measuredWallWatts}
            onChange={e => setMeasuredWallWatts(e.target.value)}
            placeholder="e.g. 1310"
          />
        </div>
        <button className="btn btn-primary" onClick={handleCalibrate} disabled={saving}>
          {saving ? 'Saving...' : 'Calibrate To Meter'}
        </button>
        {calibration?.enabled && (
          <button className="btn btn-secondary" onClick={handleClear} disabled={saving}>
            Clear Calibration
          </button>
        )}
      </div>

      <div style={{ fontSize: '0.72rem', color: 'var(--text-dim)', marginTop: 10, lineHeight: 1.5 }}>
        Best results: let the miner hold a stable target for about a minute before entering the reading.
        This is especially useful on estimate-only PSU setups such as APW3, APW7, APW9, APW9+, and APW12 families.
      </div>
    </div>
  );
}
