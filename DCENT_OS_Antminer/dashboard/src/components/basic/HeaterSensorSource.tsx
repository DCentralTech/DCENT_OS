import React, { useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { api } from '../../api/client';

/**
 * Input-temperature module + source selector, shown directly under the
 * thermostat dial.
 *
 * Operator ask: "We see the huge target temperature, but we should see an
 * input temperature, a way to set it (chips, board, DCENT XPack, MQTT,
 * HomeAssistant, etc)."
 *
 * Truth-contract: the prototype fabricated a plausible room reading for every
 * source. Production does NOT. The only temperature this firmware build
 * actually ingests is the on-device ASIC junction (averaged across chains),
 * so **Chip** is real and selectable; **Board / XPack / MQTT / Home
 * Assistant** are real roadmap integrations with pairing in development for
 * this build. They render like the design's "not paired" Home-Assistant tab
 * (disabled, honest sub-label) rather than showing an invented number.
 * If the backend ever reports a room sensor (`heaterStatus.room_temp_c`),
 * it is surfaced honestly under an "External" reading.
 */
const STORAGE_KEY = 'dcent_heater_temp_source_v1';

type SourceId = 'chip' | 'board' | 'xpack' | 'mqtt' | 'ha';

interface SourceDef {
  id: SourceId;
  label: string;
  sub: string;
  detail: string;
}

const SOURCES: SourceDef[] = [
  { id: 'chip', label: 'Chip', sub: 'ASIC junction', detail: 'Averaged ASIC junction temperature across all chains. The real on-device sensor this build reads.' },
  { id: 'board', label: 'Board', sub: 'hashboard PCB', detail: 'Hashboard PCB sensor pairing is in development for this firmware build.' },
  { id: 'xpack', label: 'XPack', sub: 'external probe', detail: 'DCENT Expansion Pack thermistor port. Connect an XPack and a room probe to drive heat off room temperature.' },
  { id: 'mqtt', label: 'MQTT', sub: 'topic / sensor', detail: 'Read room temperature from an MQTT topic published by a smart thermostat or DIY sensor. Configuration is in development.' },
  { id: 'ha', label: 'Home Assistant', sub: 'climate entity', detail: 'Pair with a Home Assistant climate entity. Configuration is in development.' },
];

function readStored(): SourceId {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v && SOURCES.some(s => s.id === v)) return v as SourceId;
  } catch {
    /* localStorage unavailable */
  }
  return 'chip';
}

export function HeaterSensorSource() {
  const status = useMinerStore(s => s.status);
  const heater = useMinerStore(s => s.heaterStatus);
  const settings = useMinerStore(s => s.settings);
  const addToast = useMinerStore(s => s.addToast);
  const [source, setSource] = useState<SourceId>(readStored);
  const [feed, setFeed] = useState('');
  const [feeding, setFeeding] = useState(false);

  const unit = settings.temperatureUnit === 'F' ? 'F' : 'C';
  const toUnit = (c: number) => (unit === 'F' ? (c * 9) / 5 + 32 : c);
  const toCelsius = (v: number) => (unit === 'F' ? ((v - 32) * 5) / 9 : v);

  // Real on-device chip reading: hottest chain junction (honest — the worst
  // chain is what the thermal supervisor and the operator actually care about).
  const chainTemps = (status?.chains ?? [])
    .map(c => (typeof c?.temp_c === 'number' ? c.temp_c : null))
    .filter((v): v is number => v != null && Number.isFinite(v));
  const chipC = chainTemps.length ? Math.max(...chainTemps) : null;
  const roomC = typeof heater?.room_temp_c === 'number' ? heater.room_temp_c : null;

  // Which sources have real data in THIS build.
  const available: Record<SourceId, number | null> = {
    chip: chipC,
    board: null,
    xpack: roomC, // an XPack room probe surfaces as room_temp_c when present
    mqtt: roomC,
    ha: roomC,
  };

  const active = SOURCES.find(s => s.id === source) ?? SOURCES[0];
  const activeReading = available[active.id];
  const isPaired = activeReading != null;

  const select = (id: SourceId) => {
    setSource(id);
    try {
      localStorage.setItem(STORAGE_KEY, id);
    } catch {
      /* non-fatal */
    }
  };

  // Real backend wire: this build does not auto-poll XPack/MQTT/HA, but the
  // daemon DOES accept an externally-supplied room temperature via the real
  // POST /api/home/room-temp. Feeding it here makes the external sources
  // genuinely functional (the same endpoint an XPack/MQTT/HA bridge would
  // call) — the heater then holds to it and heater.room_temp_c reflects it.
  const submitRoomTemp = async () => {
    const v = parseFloat(feed);
    if (!Number.isFinite(v)) {
      addToast('Enter a valid room temperature', 'warning');
      return;
    }
    const tempC = toCelsius(v);
    if (tempC < -10 || tempC > 60) {
      addToast('Room temperature out of plausible range', 'warning');
      return;
    }
    setFeeding(true);
    try {
      await api.setRoomTemp({ temp_c: Math.round(tempC * 10) / 10 });
      addToast(`Room temperature set to ${v.toFixed(1)}°${unit}`, 'success');
      setFeed('');
    } catch {
      addToast('Failed to send room temperature to the daemon', 'error');
    } finally {
      setFeeding(false);
    }
  };

  return (
    // Kit `nest-sensor` (HeaterMode.jsx:21-51 / styles.css:2732). Dual-classed
    // with the production `heater-sensor*` hooks the loaded skin already pins,
    // so the coordinator skin can also address the canonical kit classes.
    <div className="heater-sensor nest-sensor" role="group" aria-label="Input temperature source">
      <div className="heater-sensor-head nest-sensor-head">
        <span className="heater-sensor-eyebrow nest-sensor-eyebrow">Input from</span>
        {isPaired ? (
          <strong className="heater-sensor-value nest-sensor-value" aria-live="polite">
            {toUnit(activeReading as number).toFixed(1)}
            <small>°{unit}</small>
          </strong>
        ) : (
          <strong className="heater-sensor-value heater-sensor-value--na nest-sensor-value" aria-live="polite">
            —<small>°{unit}</small>
          </strong>
        )}
        <span className="heater-sensor-active nest-sensor-active">
          {active.label} · {isPaired ? active.sub : 'not paired'}
        </span>
      </div>
      <div className="heater-sensor-tabs nest-sensor-tabs">
        {SOURCES.map(s => {
          const paired = available[s.id] != null;
          const isActive = source === s.id;
          return (
            <button
              key={s.id}
              type="button"
              className={`heater-sensor-tab nest-sensor-tab${isActive ? ' is-active active' : ''}${paired ? '' : ' is-unpaired unavail'}`}
              onClick={() => select(s.id)}
              aria-pressed={isActive}
              data-tooltip={s.detail}
            >
              <span>{s.label}</span>
              {!paired && <small>not paired</small>}
            </button>
          );
        })}
      </div>

      {source !== 'chip' && (
        <div className="heater-sensor-feed">
          <p className="heater-sensor-feed-note">
            This build doesn&apos;t auto-poll {active.label}. Feed the room
            temperature here (or POST <code>/api/home/room-temp</code> from your
            {' '}{active.label} bridge) and the heater holds to it.
          </p>
          <div className="heater-sensor-feed-row">
            <input
              className="heater-sensor-feed-input"
              type="number"
              step="0.1"
              inputMode="decimal"
              placeholder={`Room °${unit}`}
              value={feed}
              aria-label={`Room temperature in °${unit}`}
              onChange={e => setFeed(e.target.value)}
              onKeyDown={e => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  void submitRoomTemp();
                }
              }}
            />
            <button
              type="button"
              className="heater-sensor-feed-btn"
              onClick={() => void submitRoomTemp()}
              disabled={feeding || feed.trim() === ''}
            >
              {feeding ? 'Sending…' : 'Set'}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
