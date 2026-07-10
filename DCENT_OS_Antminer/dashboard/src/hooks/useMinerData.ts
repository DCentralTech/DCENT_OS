// WebSocket + polling data hook — connects WS, falls back to REST polling

import { useEffect, useRef } from 'react';
import { wsManager } from '../api/websocket';
import { api } from '../api/client';
import { useMinerStore, useMinerActions } from '../store/miner';
import { wsBridge } from '../store/wsBridge';
import type { StatusResponse } from '../api/types';
import { getLiveWallWatts } from '../utils/power';

const POLL_INTERVAL_MS = 5000;
const STATS_POLL_INTERVAL_MS = 10000; // Stats less frequently than status

export function useMinerData() {
  // P3-6: subscribe only to the (stable) action functions, not the whole store.
  // A bare `useMinerStore()` re-rendered App on every telemetry tick; the hook
  // uses these actions only inside effects. Reactive state is read imperatively
  // via `useMinerStore.getState()` (below) or with a focused selector (`mode`).
  const store = useMinerActions();
  const mode = useMinerStore(s => s.mode);
  const pollTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const statsPollTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const lastHistoryPush = useRef(0);
  const statusPollErrorShown = useRef(false);
  const statsPollErrorShown = useRef(false);
  const autotunerPollErrorShown = useRef(false);
  const nightModeErrorShown = useRef(false);

  // Process incoming WebSocket messages
  useEffect(() => {
    const unsub = wsBridge.subscribe((batch) => {
      store.markWsFrame(batch.at);

      const statsMessage = batch.latest.stats;
      if (statsMessage) {
        const msg = statsMessage;
        statusPollErrorShown.current = false;
        // Merge WS partial update into existing status — preserve fields
        // not present in the WS message (uptime_s, firmware_version, mode).
        const prev = useMinerStore.getState().status;
        const liveChains = Array.isArray(msg.chains) ? msg.chains : (prev?.chains ?? []);
        const fans = msg.fans.per_fan
          ? msg.fans
          : { ...msg.fans, per_fan: prev?.fans.per_fan };
        const baseStatus: StatusResponse = prev ?? {
          hashrate_ghs: msg.hashrate_ghs,
          hashrate_5s_ghs: msg.hashrate_5s_ghs,
          accepted: msg.accepted,
          rejected: msg.rejected,
          uptime_s: 0,
          firmware_version: '---',
          mode: useMinerStore.getState().mode,
          chains: liveChains,
          fans,
          pool: msg.pool,
        };
        store.setStatus({
          ...baseStatus,
          hashrate_ghs: msg.hashrate_ghs,
          hashrate_5s_ghs: msg.hashrate_5s_ghs,
          accepted: msg.accepted,
          rejected: msg.rejected,
          chains: liveChains,
          fans,
          pool: msg.pool,
        });

        const prevStats = useMinerStore.getState().stats;
        if (prevStats?.power) {
          const prevPower = prevStats.power;
          const prevStatsChains = Array.isArray(prevStats.chains) ? prevStats.chains : [];
          const prevWallWatts = 'wall_watts' in prevPower ? prevPower.wall_watts : undefined;
          const prevBtuH = 'btu_h' in prevPower ? prevPower.btu_h : undefined;
          const prevWattCap = 'watt_cap' in prevPower ? prevPower.watt_cap : undefined;
          const prevPowerSource = 'source' in prevPower ? prevPower.source : undefined;
          const prevPowerSourceDetail = 'source_detail' in prevPower ? prevPower.source_detail : undefined;
          const prevLivePowerAvailable = 'live_power_available' in prevPower ? prevPower.live_power_available : undefined;
          const prevPowerModeled = 'modeled' in prevPower ? prevPower.modeled : undefined;
          const prevPowerNote = 'note' in prevPower ? prevPower.note : undefined;
          const prevPowerCalibrated = 'calibrated' in prevPower ? prevPower.calibrated : undefined;
          const prevPowerCalibrationMultiplier =
            'calibration_multiplier' in prevPower ? prevPower.calibration_multiplier : undefined;
          store.setStats({
            ...prevStats,
            hashrate_ghs: msg.hashrate_ghs,
            hashrate_ths: msg.hashrate_ghs / 1000,
            chains: liveChains.map((liveChain) => {
              const chain = prevStatsChains.find((candidate) => candidate.id === liveChain.id);
              if (!chain) {
                return {
                  ...liveChain,
                  voltage_v: liveChain.voltage_mv / 1000,
                  hashrate_ths: liveChain.hashrate_ghs / 1000,
                  accepted: 0,
                  rejected: 0,
                  hw_errors: 0,
                };
              }
              return {
                ...chain,
                chips: liveChain.chips,
                frequency_mhz: liveChain.frequency_mhz,
                voltage_mv: liveChain.voltage_mv,
                voltage_v: liveChain.voltage_mv / 1000,
                temp_c: liveChain.temp_c,
                hashrate_ghs: liveChain.hashrate_ghs,
                hashrate_ths: liveChain.hashrate_ghs / 1000,
                errors: liveChain.errors,
                status: liveChain.status,
              };
            }),
            fans,
            power: {
              ...prevPower,
              watts: msg.power_watts ?? prevPower.watts,
              wall_watts: msg.wall_watts ?? prevWallWatts,
              efficiency_jth: msg.efficiency_jth ?? prevPower.efficiency_jth,
              btu_h: msg.btu_h ?? prevBtuH,
              source: msg.power_source ?? prevPowerSource,
              source_detail: msg.power_source_detail ?? prevPowerSourceDetail,
              live_power_available: msg.live_power_available ?? prevLivePowerAvailable,
              modeled: msg.power_modeled ?? prevPowerModeled,
              note: msg.power_note ?? prevPowerNote,
              calibrated: msg.power_calibrated ?? prevPowerCalibrated,
              calibration_multiplier: msg.power_calibration_multiplier ?? prevPowerCalibrationMultiplier,
              watt_cap: msg.watt_cap ?? prevWattCap,
            },
          });
        }

        // Push to history ring buffer every 10s (first push is immediate)
        // This fills the chart quickly on page load instead of waiting 60s.
        const now = Date.now();
        const isFirstPush = lastHistoryPush.current === 0;
        if (isFirstPush || now - lastHistoryPush.current > 10000) {
          lastHistoryPush.current = now;
          const avgTemp = liveChains.length > 0
            ? liveChains.reduce((s, c) => s + c.temp_c, 0) / liveChains.length
            : 0;
          const statsState = useMinerStore.getState().stats;
          const wsPower = getLiveWallWatts({
            watts: msg.power_watts,
            wall_watts: msg.wall_watts,
            source: msg.power_source,
            source_detail: msg.power_source_detail,
            live_power_available: msg.live_power_available,
            modeled: msg.power_modeled,
            note: msg.power_note,
            calibrated: msg.power_calibrated,
            calibration_multiplier: msg.power_calibration_multiplier,
          });
          const power = wsPower > 0 ? wsPower : getLiveWallWatts(statsState?.power);
          store.pushHistory(msg.hashrate_ghs, avgTemp, power);
        }
      }

      const heaterMessage = batch.latest.heaterStatus;
      if (heaterMessage) {
        const msg = heaterMessage;
        const previousHeaterStatus = useMinerStore.getState().heaterStatus;
        const noiseBackedByRpm =
          msg.noise_source === 'tach_estimate' ||
          (!!msg.fans?.rpm_ && (msg.fans.rpm ?? 0) > 0);
        const heaterWsHasPowerProvenance =
          msg.power_source !== undefined ||
          msg.power_source_detail !== undefined ||
          msg.live_power_available !== undefined ||
          msg.power_modeled !== undefined ||
          msg.power_note !== undefined ||
          msg.power_calibrated !== undefined ||
          msg.power_calibration_multiplier !== undefined;
        // Legacy-daemon fallback: old heater_status frames lacked REST power provenance.
        store.setHeaterStatus({
          ...previousHeaterStatus,
          power_watts: msg.power_watts,
          wall_watts: msg.wall_watts ?? previousHeaterStatus?.wall_watts,
          btu_h: msg.btu_h,
          source: heaterWsHasPowerProvenance ? msg.power_source : 'static_model_fallback',
          power_source_detail: heaterWsHasPowerProvenance
            ? msg.power_source_detail
            : 'static_power_fallback_from_miner_state',
          live_power_available: heaterWsHasPowerProvenance
            ? (msg.live_power_available ?? false)
            : false,
          power_modeled: heaterWsHasPowerProvenance ? (msg.power_modeled ?? true) : true,
          power_note: heaterWsHasPowerProvenance
            ? msg.power_note
            : 'Heater WebSocket power lacks live provenance; REST /api/home/status provides live wall-power labels.',
          calibrated: heaterWsHasPowerProvenance ? msg.power_calibrated : false,
          calibration_multiplier: heaterWsHasPowerProvenance
            ? (msg.power_calibration_multiplier ?? null)
            : null,
          noise_db: noiseBackedByRpm ? msg.noise_db : null,
          noise_source: msg.noise_source ?? 'unavailable_no_rpm_feedback',
          noise_note: msg.noise_note ?? 'Noise unavailable until fan RPM is reported',
          fans: msg.fans ?? previousHeaterStatus?.fans,
          airflow_cfm: msg.airflow_cfm,
          preset: msg.preset,
          room_temp_c: msg.room_temp_c,
          cost_today_usd: msg.cost_today_usd,
          sats_today: msg.sats_today,
          night_mode_active: msg.night_mode_active,
          night_mode_starts_in_s: msg.night_mode_starts_in_s,
          hashrate_ghs: 0,
        });
      }

      const autotunerStatusMessage = batch.latest.autotunerStatus;
      if (autotunerStatusMessage) {
        const msg = autotunerStatusMessage;
        store.setAutotunerStatus(msg.payload);
      }

      // Handle log messages from dcentrald
      for (const msg of batch.logs) {
        store.pushLog(msg.level, msg.source, msg.message);
      }
    });

    const unsubConnection = wsManager.onConnectionChange((state) => {
      if (!state.connected) {
        store.setWsConnected(false);
        store.refreshTransportState(Date.now());
      }
    });
    wsManager.connect();
    return () => {
      unsub();
      unsubConnection();
    };
  }, []);

  useEffect(() => {
    const id = setInterval(() => {
      useMinerStore.getState().refreshTransportState(Date.now());
    }, 1000);
    return () => clearInterval(id);
  }, []);

  // REST polling fallback for status
  useEffect(() => {
    const poll = async () => {
      if (useMinerStore.getState().transport === 'ws-live') return;
      store.setWsConnected(false);
      try {
        const status = await api.getStatus();
        const chains = Array.isArray(status.chains) ? status.chains : [];
        statusPollErrorShown.current = false;
        store.setStatus({ ...status, chains }); // setStatus no longer overwrites mode (fixed in store)
        store.markRestPoll(Date.now());
        api.getAutotunerStatus().then(autotunerStatus => {
          autotunerPollErrorShown.current = false;
          store.setAutotunerStatus(autotunerStatus);
        }).catch(() => {
          if (!autotunerPollErrorShown.current) {
            autotunerPollErrorShown.current = true;
            store.addToast('Autotuner status unavailable; retrying in background', 'warning');
          }
        });

        // Push to history ring buffer from REST poll too (every 10s)
        const now = Date.now();
        const isFirstPush = lastHistoryPush.current === 0;
        if (isFirstPush || now - lastHistoryPush.current > 10000) {
          lastHistoryPush.current = now;
          const avgTemp = chains.length > 0
            ? chains.reduce((s, c) => s + c.temp_c, 0) / chains.length
            : 0;
          const statsState = useMinerStore.getState().stats;
          const power = getLiveWallWatts(statsState?.power);
          store.pushHistory(status.hashrate_ghs, avgTemp, power);
        }
      } catch {
        store.setWsConnected(false);
        store.refreshTransportState(Date.now());
        if (!statusPollErrorShown.current) {
          statusPollErrorShown.current = true;
          store.addToast('Live telemetry unavailable; retrying status poll', 'warning');
        }
      }
    };

    pollTimer.current = setInterval(poll, POLL_INTERVAL_MS);
    poll(); // immediate first fetch

    return () => { if (pollTimer.current) clearInterval(pollTimer.current); };
  }, []);

  // Stats polling (power, efficiency, per-chain details)
  useEffect(() => {
    const pollStats = async () => {
      try {
        const stats = await api.getStats();
        statsPollErrorShown.current = false;
        store.setStats(stats);
      } catch {
        // Heater mode can run on firmware where the detailed stats endpoint is absent.
        if (useMinerStore.getState().mode !== 'heater' && !statsPollErrorShown.current) {
          statsPollErrorShown.current = true;
          store.addToast('Detailed stats unavailable; showing latest status telemetry', 'warning');
        }
      }
    };

    statsPollTimer.current = setInterval(pollStats, STATS_POLL_INTERVAL_MS);
    pollStats(); // immediate first fetch

    return () => { if (statsPollTimer.current) clearInterval(statsPollTimer.current); };
  }, []);

  // Load initial data — toast on failure (once, not every poll)
  useEffect(() => {
    api.getSystemInfo().then(store.setSystemInfo).catch(() => {
      store.addToast('Could not load system info', 'warning');
    });
  }, []);

  useEffect(() => {
    if (mode !== 'heater') {
      return;
    }

    let cancelled = false;

    // Heater-data load failures are NOT user-actionable errors — they happen
    // routinely on firmware/preview environments that don't expose the heater
    // endpoints. Toasts here are pure noise for a beta operator. Instead, leave
    // the store values at their defaults (heaterPresets falls back to the
    // built-in Quiet/Balanced/Comfort set via useHeaterPresets; heaterStatus /
    // nightMode stay null) and let the Basic-mode components render their
    // graceful inline "waiting for telemetry" fallback shells. Toasts stay
    // reserved for genuine user-actionable errors.
    api.getHeaterPresets().then(r => {
      if (!cancelled) {
        if (Array.isArray(r.presets)) {
          useMinerStore.getState().setHeaterPresets(r.presets);
        }
        useMinerStore.getState().setHeaterPresetScope(r.scope ?? null);
      }
    }).catch(() => {
      /* silent — PowerPresets falls back to default presets */
    });

    api.getNightMode().then(nightMode => {
      if (!cancelled) {
        nightModeErrorShown.current = false;
        useMinerStore.getState().setNightMode(nightMode);
      }
    }).catch(() => {
      /* silent — night mode is informational; NightModePill simply hides */
    });

    api.getHeaterStatus().then(status => {
      if (!cancelled) {
        useMinerStore.getState().setHeaterStatus(status);
      }
    }).catch(() => {
      /* silent — Thermostat / HeaterStatus render their waiting-state shells */
    });

    return () => {
      cancelled = true;
    };
  }, [mode]);
}
