// DCENT_OS Setup Wizard — Name step.
//
// Structural recreation of the kit `NameStep` (ui_kits/wizard): the
// hostname field with the `http://<name>.local` hint, a friendly
// room/zone field, suggestion chips, a live preview, and the folded-in
// network info card.
//
// Real wiring preserved: the ONLY persisted value is the miner name
// (production maps it to the hostname in SetupWizard → api.setupMode).
// The kit's "Room / zone" field is purely cosmetic ("Doesn't affect
// mining" — the kit's own words); production has no room endpoint, so it
// is local-state-only and NOT sent through a fabricated call.

import React, { useState, useEffect, useRef } from 'react';
import type { OperatingMode } from '../../api/types';
import { NetworkInfoPreview } from './NetworkStep';

interface NameStepProps {
  value: string;
  mode: OperatingMode | null;
  onChange: (name: string) => void;
}

const SUGGESTIONS: Record<string, string[]> = {
  heater: ['Living Room Heater', 'Bedroom Warmer', 'Garage Heater', 'Basement Furnace'],
  standard: ['Garage Miner', 'Mining Rig 1', 'Home Miner', 'Stack Sats Machine'],
  hacker: ['Dev Rig', 'Test Bench', 'Repair Station', 'Lab Unit'],
};
const DEFAULT_SUGGESTIONS = ['My Miner', 'Home Miner', 'Stack Sats', 'The Heater'];

function toHostname(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'dcentos';
}

export function NameStep({ value, mode, onChange }: NameStepProps) {
  const [room, setRoom] = useState('Living room');
  const [blurred, setBlurred] = useState(false);
  const defaultAppliedRef = useRef(false);
  const isEmpty = blurred && value.trim().length === 0;
  const suggestions = mode ? SUGGESTIONS[mode] : DEFAULT_SUGGESTIONS;

  useEffect(() => {
    if (defaultAppliedRef.current) return;
    defaultAppliedRef.current = true;
    if (!value) onChange('My Miner');
  }, [onChange, value]);

  const hostname = toHostname(value);

  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">Name this miner</h2>
      <p className="wiz-lede">
        Use whatever helps you identify it. The hostname is what mDNS advertises on
        your LAN.
      </p>

      <div className="wiz-fld">
        <label htmlFor="wiz-name">Hostname</label>
        <input
          id="wiz-name"
          className={`wiz-input${isEmpty ? ' err' : ''}`}
          type="text"
          value={value}
          maxLength={32}
          onChange={e => onChange(e.target.value)}
          onBlur={() => setBlurred(true)}
          placeholder="heater01"
          aria-invalid={isEmpty}
          aria-describedby={isEmpty ? 'wiz-name-error' : 'wiz-name-hint'}
        />
        {isEmpty ? (
          <span id="wiz-name-error" className="wiz-err">
            Give your miner a name so it&apos;s easy to find on your network.
          </span>
        ) : (
          <small id="wiz-name-hint" className="wiz-fld-hint">
            Available at <code>http://{hostname}.local</code> on your LAN. Lowercase
            letters, numbers, and hyphens. {value.length}/32
          </small>
        )}
      </div>

      <div className="wiz-fld">
        <label>Suggestions{mode ? ` for ${mode === 'heater' ? 'Heating' : mode === 'standard' ? 'Mining' : 'Advanced'} mode` : ''}</label>
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
          {suggestions.map(s => (
            <button
              key={s}
              type="button"
              className={`wiz-donation-preset${value === s ? ' active' : ''}`}
              onClick={() => onChange(s)}
            >
              {s}
            </button>
          ))}
        </div>
      </div>

      <div className="wiz-fld">
        <label htmlFor="wiz-room">Room / zone</label>
        <input
          id="wiz-room"
          className="wiz-input"
          type="text"
          value={room}
          onChange={e => setRoom(e.target.value)}
          placeholder="Living room, basement, garage…"
        />
        <small className="wiz-fld-hint">
          Shown on the dashboard and in Heater mode. Doesn&apos;t affect mining and
          isn&apos;t sent anywhere.
        </small>
      </div>

      <NetworkInfoPreview hostname={hostname} />
    </div>
  );
}
