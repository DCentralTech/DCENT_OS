import React, { useState } from 'react';
import { api } from '../../../api/client';
import { useMinerStore } from '../../../store/miner';

export function SecurityTab() {
  const settings = useMinerStore(s => s.settings);
  const updateSettings = useMinerStore(s => s.updateSettings);
  const addAlert = useMinerStore(s => s.addAlert);
  const setupStatus = useMinerStore(s => s.setupStatus);
  const [newPassword, setNewPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [savingPassword, setSavingPassword] = useState(false);
  const [savingSafety, setSavingSafety] = useState(false);
  const [circuitVoltage, setCircuitVoltage] = useState('');
  const [circuitAmperage, setCircuitAmperage] = useState('');

  const savePassword = async () => {
    if (newPassword !== confirmPassword) {
      addAlert('warning', 'Passwords do not match');
      return;
    }
    if (newPassword.length === 0) {
      updateSettings({ password: null });
      addAlert('info', 'Local dashboard password cleared from this browser');
      setNewPassword('');
      setConfirmPassword('');
      return;
    }
    if (newPassword.length < 8) {
      addAlert('warning', 'Password must be at least 8 characters');
      return;
    }
    setSavingPassword(true);
    try {
      const token = await api.configureAuthPassword(newPassword);
      updateSettings({
        password: newPassword,
        ...(token ? { apiToken: token } : {}),
      });
      try {
        const fresh = await api.getSetupStatus();
        useMinerStore.getState().setSetupStatus(fresh);
      } catch {
        /* non-fatal */
      }
      addAlert('info', 'Owner password set — write and control actions are now protected');
      setNewPassword('');
      setConfirmPassword('');
    } catch (err) {
      addAlert(
        'warning',
        err instanceof Error ? `Failed to set password: ${err.message}` : 'Failed to set password',
      );
    } finally {
      setSavingPassword(false);
    }
  };

  const completeSafetyCheck = async () => {
    setSavingSafety(true);
    try {
      await api.setupSafety();
      const v = Number(circuitVoltage);
      const a = Number(circuitAmperage);
      if (Number.isFinite(v) && v > 0 && Number.isFinite(a) && a > 0) {
        await api.setupCircuit({ voltage: v, amperage: a });
      }
      try {
        const fresh = await api.getSetupStatus();
        useMinerStore.getState().setSetupStatus(fresh);
      } catch {
        /* non-fatal */
      }
      addAlert('info', 'Circuit & safety check completed — the autotuner will now cap power to your declared circuit');
      setCircuitVoltage('');
      setCircuitAmperage('');
    } catch (err) {
      addAlert(
        'warning',
        err instanceof Error ? `Failed to complete the safety check: ${err.message}` : 'Failed to complete the safety check',
      );
    } finally {
      setSavingSafety(false);
    }
  };

  return (
    <div className="section" id="security">
      <div className="section-title">Security</div>
      <div style={{
        background: 'var(--card-bg)', borderRadius: 'var(--radius)',
        padding: 16, border: '1px solid var(--border)', maxWidth: 400,
      }}>
        <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)', marginBottom: 10 }}>
          Dashboard Password
        </div>
        {setupStatus?.password_opt_out === true && setupStatus?.auth?.password_set !== true && (
          <div style={{
            fontSize: '0.78rem', lineHeight: 1.55, marginBottom: 12,
            color: '#FBBF24',
            background: 'rgba(245,158,11,0.08)',
            border: '1px solid rgba(245,158,11,0.3)',
            borderRadius: 8, padding: '10px 12px',
          }}>
              You chose to run without an owner password. That&apos;s fine —
            the dashboard stays viewable. Setting one here protects every
            write &amp; control action (tuning, pools, restart, restore).
            Recommended, never required.
          </div>
        )}
        <div style={{ display: 'grid', gap: 10 }}>
          <input
            type="password"
            value={newPassword}
            onChange={e => setNewPassword(e.target.value)}
            placeholder="New password (blank to clear local)"
            autoComplete="new-password"
          />
          <input
            type="password"
            value={confirmPassword}
            onChange={e => setConfirmPassword(e.target.value)}
            placeholder="Confirm password"
            autoComplete="new-password"
          />
          <button
            className="btn btn-primary"
            onClick={() => { void savePassword(); }}
            disabled={savingPassword}
          >
            {savingPassword
                ? 'Saving…'
              : settings.password ? 'Change Password' : 'Set Password'}
          </button>
        </div>
        {settings.password && (
          <div style={{ fontSize: '0.75rem', color: 'var(--green)', marginTop: 8 }}>
            Password protection is enabled
          </div>
        )}
      </div>

      <div
        id="circuit-safety"
        style={{
          background: 'var(--card-bg)', borderRadius: 'var(--radius)',
          padding: 16, border: '1px solid var(--border)', maxWidth: 400,
          marginTop: 16,
        }}
      >
        <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)', marginBottom: 10 }}>
          Circuit &amp; Safety Check
        </div>
        {setupStatus?.safety_opt_out === true ? (
          <div style={{
            fontSize: '0.78rem', lineHeight: 1.55, marginBottom: 12,
            color: '#FBBF24',
            background: 'rgba(245,158,11,0.08)',
            border: '1px solid rgba(245,158,11,0.3)',
            borderRadius: 8, padding: '10px 12px',
          }}>
            You chose to run without the circuit/breaker check. That&apos;s
              fine — the dashboard and logs stay fully viewable. Completing it
            keeps the autotuner from pushing power past your breaker (the #1
            home-mining gotcha). Optionally declare your circuit below, then
            acknowledge. Recommended, never required.
          </div>
        ) : (
          <div style={{ fontSize: '0.75rem', color: 'var(--green)', marginBottom: 10 }}>
            Circuit &amp; safety check completed
          </div>
        )}
        {setupStatus?.safety_opt_out === true && (
          <div style={{ display: 'grid', gap: 10 }}>
            <input
              type="number"
              inputMode="numeric"
              min={48}
              max={480}
              value={circuitVoltage}
              onChange={e => setCircuitVoltage(e.target.value)}
              placeholder="Circuit voltage V (optional, e.g. 240)"
            />
            <input
              type="number"
              inputMode="numeric"
              min={5}
              max={100}
              value={circuitAmperage}
              onChange={e => setCircuitAmperage(e.target.value)}
              placeholder="Breaker amps A (optional, e.g. 30)"
            />
            <button
              className="btn btn-primary"
              onClick={() => { void completeSafetyCheck(); }}
              disabled={savingSafety}
            >
              {savingSafety ? 'Saving…' : 'Complete the circuit & safety check'}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
