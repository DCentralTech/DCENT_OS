import { useEffect, useMemo, useRef, useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { wsManager } from '../../api/websocket';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';
import { getLiveWallWatts } from '../../utils/power';
import type {
  ChainState,
  FanState,
  StatsResponse,
  StatusResponse,
  WsMessage,
  WsMiningSyncMessage,
} from '../../api/types';

type BeatAudioState = 'idle' | 'running' | 'paused' | 'unsupported';
type BeatSyncMode = 'telemetry' | 'mining-sync';
type BeatProfileId = 'keygen' | 'dungeon' | 'tracker';

interface BeatMetrics {
  connected: boolean;
  hashrateGhs: number;
  wallWatts: number | null;
  avgTempC: number;
  fanRpm: number;
  activeChains: number;
  tempoBpm: number;
  energy: number;
  heat: number;
  brightness: number;
  poolStatus: string;
}

interface BeatRuntime {
  context: AudioContext;
  masterGain: GainNode;
  toneFilter: BiquadFilterNode;
  schedulerId: number | null;
  nextStepTime: number;
  step: number;
}

interface BeatBursts {
  accepted: number;
  rejected: number;
  warning: number;
  error: number;
  dispatch: number;
  nonce: number;
  job: number;
  cleanJob: number;
  lucky: number;
}

interface VoiceOptions {
  time: number;
  frequency: number;
  duration: number;
  volume: number;
  type: OscillatorType;
  endFrequency?: number;
  detune?: number;
  lowpassHz?: number;
}

interface BeatProfile {
  id: BeatProfileId;
  label: string;
  description: string;
  bassType: OscillatorType;
  leadType: OscillatorType;
  glitchType: OscillatorType;
  hatType: OscillatorType;
  kickBaseHz: number;
  filterMinHz: number;
  filterMaxHz: number;
  filterBias: number;
  masterGainScale: number;
  tempoBias: number;
  rootMidi: number;
  bassScale: number[];
  leadScale: number[];
  jobMotif: number[];
  cleanJobMotif: number[];
  acceptedMotif: number[];
  luckyMotif: number[];
}

const SEQUENCE_STEPS = 8;
const LOOKAHEAD_S = 0.12;
const SCHEDULER_INTERVAL_MS = 40;
const VOLUME_STORAGE_KEY = 'dcentos-beat-lab-volume';
const PROFILE_STORAGE_KEY = 'dcentos-beat-lab-profile';

const BEAT_PROFILES: Record<BeatProfileId, BeatProfile> = {
  keygen: {
    id: 'keygen',
    label: 'Keygen',
    description: 'Bright cracktro arps with a glassy square-wave bassline.',
    bassType: 'square',
    leadType: 'triangle',
    glitchType: 'sawtooth',
    hatType: 'square',
    kickBaseHz: 120,
    filterMinHz: 1200,
    filterMaxHz: 4600,
    filterBias: 0.2,
    masterGainScale: 1,
    tempoBias: 10,
    rootMidi: 44,
    bassScale: [0, 7, 10, 12, 14, 17],
    leadScale: [12, 16, 19, 24],
    jobMotif: [0, 7, 12],
    cleanJobMotif: [12, 19, 24, 31],
    acceptedMotif: [12, 19, 24],
    luckyMotif: [12, 16, 19, 24, 31],
  },
  dungeon: {
    id: 'dungeon',
    label: 'Dungeon',
    description: 'Dark lower-register pulses with ritual-style block resets.',
    bassType: 'sawtooth',
    leadType: 'sine',
    glitchType: 'square',
    hatType: 'triangle',
    kickBaseHz: 96,
    filterMinHz: 650,
    filterMaxHz: 2200,
    filterBias: -0.12,
    masterGainScale: 0.92,
    tempoBias: -8,
    rootMidi: 33,
    bassScale: [0, 3, 5, 7, 10, 12],
    leadScale: [12, 15, 17, 19],
    jobMotif: [0, 3, 7],
    cleanJobMotif: [0, 7, 12, 19],
    acceptedMotif: [7, 10, 12],
    luckyMotif: [7, 10, 14, 19, 24],
  },
  tracker: {
    id: 'tracker',
    label: 'Tracker',
    description: 'Rigid sample-tracker groove with fast step accents and crisp sync.',
    bassType: 'square',
    leadType: 'square',
    glitchType: 'sawtooth',
    hatType: 'square',
    kickBaseHz: 110,
    filterMinHz: 900,
    filterMaxHz: 3400,
    filterBias: 0.05,
    masterGainScale: 0.96,
    tempoBias: 4,
    rootMidi: 40,
    bassScale: [0, 2, 5, 7, 9, 12],
    leadScale: [12, 14, 17, 21],
    jobMotif: [0, 5, 7],
    cleanJobMotif: [12, 17, 24],
    acceptedMotif: [12, 17, 21],
    luckyMotif: [12, 14, 17, 21, 24],
  },
};

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

function lerp(min: number, max: number, t: number) {
  return min + (max - min) * t;
}

function midiToFrequency(midi: number) {
  return 440 * Math.pow(2, (midi - 69) / 12);
}

function average(values: number[]) {
  if (values.length === 0) {
    return 0;
  }

  return values.reduce((sum, value) => sum + value, 0) / values.length;
}

function summarizeEvent(message: string) {
  return message.length > 58 ? `${message.slice(0, 55)}...` : message;
}

function isBeatProfileId(value: string | null): value is BeatProfileId {
  return value === 'keygen' || value === 'dungeon' || value === 'tracker';
}

function getAverageTemp(chains: ChainState[]) {
  return average(chains.filter(chain => chain.chips > 0).map(chain => chain.temp_c));
}

function getAverageFanRpm(fans?: FanState | null) {
  if (!fans) {
    return 0;
  }

  if (fans.per_fan && fans.per_fan.length > 0) {
    return average(fans.per_fan.map(fan => fan.rpm));
  }

  return fans.rpm;
}

function buildBeatMetrics(
  status: StatusResponse | null,
  stats: StatsResponse | null,
  wsConnected: boolean,
  profile: BeatProfile,
): BeatMetrics {
  const chains = status?.chains ?? [];
  const hashrateGhs = status?.hashrate_5s_ghs ?? status?.hashrate_ghs ?? stats?.hashrate_ghs ?? 0;
  const liveWallWatts = getLiveWallWatts(stats?.power ?? status?.power);
  const wallWatts = liveWallWatts > 0 ? liveWallWatts : null;
  const avgTempC = getAverageTemp(chains);
  const fanRpm = getAverageFanRpm(status?.fans ?? stats?.fans ?? null);
  const activeChains = chains.filter(chain => chain.chips > 0).length;
  const energy = clamp((wallWatts ?? 0) / 3400, 0, 1);
  const heat = clamp((avgTempC - 38) / 34, 0, 1);
  const brightness = clamp((fanRpm - 900) / 4200, 0, 1);
  const tempoBpm = clamp(
    Math.round(76 + (hashrateGhs / 120000) * 72 + activeChains * 8 + profile.tempoBias),
    68,
    176,
  );

  return {
    connected: wsConnected || Boolean(status),
    hashrateGhs,
    wallWatts,
    avgTempC,
    fanRpm,
    activeChains,
    tempoBpm,
    energy,
    heat,
    brightness,
    poolStatus: status?.pool?.status ?? 'idle',
  };
}

function buildBeatPattern(metrics: BeatMetrics, profileId: BeatProfileId) {
  if (!metrics.connected) {
    return [1, 0.15, 0.3, 0.1, 0.9, 0.15, 0.25, 0.1];
  }

  const chainDensity = clamp(metrics.activeChains / 3, 0.25, 1);
  const phase = Math.floor(metrics.hashrateGhs / 4000) % SEQUENCE_STEPS;

  switch (profileId) {
    case 'dungeon':
      return Array.from({ length: SEQUENCE_STEPS }, (_, step) => {
        let value = step === 0 || step === 4 ? 1 : 0.14 + metrics.heat * 0.16;
        if (step % 2 === 0) {
          value = Math.max(value, 0.28 + metrics.energy * 0.18);
        }
        if ((step + phase) % 4 === 0) {
          value = Math.max(value, 0.45 + chainDensity * 0.26);
        }
        if (metrics.heat > 0.58 && step === 6) {
          value = Math.max(value, 0.78);
        }
        return clamp(value, 0.08, 1);
      });
    case 'tracker':
      return Array.from({ length: SEQUENCE_STEPS }, (_, step) => {
        let value = step === 0 || step === 4 ? 1 : 0.18 + metrics.brightness * 0.12;
        if (step % 2 === 0) {
          value = Math.max(value, 0.48 + metrics.energy * 0.24);
        }
        if ((step + phase) % 2 === 0) {
          value = Math.max(value, 0.42 + chainDensity * 0.24);
        }
        return clamp(value, 0.08, 1);
      });
    default:
      return Array.from({ length: SEQUENCE_STEPS }, (_, step) => {
        let value = step === 0 || step === 4 ? 1 : 0.18 + metrics.energy * 0.18;

        if ((step + phase) % Math.max(1, 4 - metrics.activeChains) === 0) {
          value = Math.max(value, 0.45 + chainDensity * 0.35);
        }

        if (metrics.brightness > 0.45 && step % 2 === 1) {
          value = Math.max(value, 0.4 + metrics.brightness * 0.24);
        }

        return clamp(value, 0.08, 1);
      });
  }
}

function formatLiveWallPower(watts: number | null) {
  return watts != null ? `${watts.toFixed(0)} W` : 'Unavailable';
}

function getAudioContextCtor(): (new () => AudioContext) | null {
  const maybeWindow = window as Window & typeof globalThis & { webkitAudioContext?: new () => AudioContext };
  return maybeWindow.AudioContext ?? maybeWindow.webkitAudioContext ?? null;
}

function disconnectNodes(nodes: AudioNode[]) {
  for (const node of nodes) {
    try {
      node.disconnect();
    } catch {
      // Node may already be disconnected.
    }
  }
}

function playVoice(context: AudioContext, destination: AudioNode, options: VoiceOptions) {
  const oscillator = context.createOscillator();
  const gain = context.createGain();
  const filter = context.createBiquadFilter();
  const attack = Math.min(0.02, options.duration * 0.25);
  const peak = Math.max(0.00015, options.volume);

  oscillator.type = options.type;
  oscillator.frequency.setValueAtTime(options.frequency, options.time);
  if (options.endFrequency) {
    oscillator.frequency.exponentialRampToValueAtTime(Math.max(24, options.endFrequency), options.time + options.duration);
  }
  if (options.detune) {
    oscillator.detune.setValueAtTime(options.detune, options.time);
  }

  filter.type = 'lowpass';
  filter.frequency.setValueAtTime(options.lowpassHz ?? Math.max(900, options.frequency * 6), options.time);

  gain.gain.setValueAtTime(0.0001, options.time);
  gain.gain.exponentialRampToValueAtTime(peak, options.time + attack);
  gain.gain.exponentialRampToValueAtTime(0.0001, options.time + options.duration);

  oscillator.connect(filter);
  filter.connect(gain);
  gain.connect(destination);

  oscillator.onended = () => {
    disconnectNodes([oscillator, filter, gain]);
  };

  oscillator.start(options.time);
  oscillator.stop(options.time + options.duration + 0.03);
}

function playKick(
  context: AudioContext,
  destination: AudioNode,
  time: number,
  intensity: number,
  profile: BeatProfile,
) {
  playVoice(context, destination, {
    time,
    frequency: profile.kickBaseHz + intensity * 26,
    endFrequency: 42,
    duration: 0.18,
    volume: (0.08 + intensity * 0.08) * profile.masterGainScale,
    type: profile.id === 'dungeon' ? 'triangle' : 'sine',
    lowpassHz: profile.id === 'dungeon' ? 240 : 320,
  });
}

function playBass(
  context: AudioContext,
  destination: AudioNode,
  time: number,
  midi: number,
  intensity: number,
  heat: number,
  profile: BeatProfile,
) {
  const frequency = midiToFrequency(midi);

  playVoice(context, destination, {
    time,
    frequency,
    endFrequency: frequency * (0.96 - heat * 0.03),
    duration: 0.22 + intensity * 0.12,
    volume: (0.028 + intensity * 0.028) * profile.masterGainScale,
    type: profile.bassType,
    lowpassHz: profile.filterMinHz + heat * 900,
  });
}

function playHat(
  context: AudioContext,
  destination: AudioNode,
  time: number,
  brightness: number,
  intensity: number,
  profile: BeatProfile,
) {
  playVoice(context, destination, {
    time,
    frequency: 1400 + brightness * 3000,
    endFrequency: 900 + brightness * 800,
    duration: profile.id === 'tracker' ? 0.032 : 0.045,
    volume: (0.008 + intensity * 0.012) * profile.masterGainScale,
    type: profile.hatType,
    lowpassHz: profile.filterMaxHz + 2200,
  });
}

function playLead(
  context: AudioContext,
  destination: AudioNode,
  time: number,
  midi: number,
  brightness: number,
  profile: BeatProfile,
  durationScale = 1,
  volumeBoost = 1,
) {
  const frequency = midiToFrequency(midi);

  playVoice(context, destination, {
    time,
    frequency,
    endFrequency: frequency * 1.02,
    duration: (0.11 + brightness * 0.02) * durationScale,
    volume: (0.018 + brightness * 0.022) * profile.masterGainScale * volumeBoost,
    type: profile.leadType,
    lowpassHz: profile.filterMaxHz,
  });
}

function playGlitch(
  context: AudioContext,
  destination: AudioNode,
  time: number,
  midi: number,
  severity: number,
  profile: BeatProfile,
) {
  const frequency = midiToFrequency(midi);

  playVoice(context, destination, {
    time,
    frequency,
    endFrequency: frequency * 0.42,
    duration: 0.09 + severity * 0.05,
    volume: (0.016 + severity * 0.02) * profile.masterGainScale,
    type: profile.glitchType,
    detune: severity * 24,
    lowpassHz: profile.filterMinHz + 700,
  });
}

function playPulse(
  context: AudioContext,
  destination: AudioNode,
  time: number,
  frequency: number,
  intensity: number,
  type: OscillatorType,
  lowpassHz: number,
) {
  playVoice(context, destination, {
    time,
    frequency,
    endFrequency: frequency * 0.82,
    duration: 0.05,
    volume: 0.005 + intensity * 0.012,
    type,
    lowpassHz,
  });
}

function playMotif(
  context: AudioContext,
  destination: AudioNode,
  time: number,
  rootMidi: number,
  intervals: number[],
  profile: BeatProfile,
  brightness: number,
  intensity: number,
  spacingS = 0.055,
) {
  intervals.forEach((interval, index) => {
    playLead(
      context,
      destination,
      time + index * spacingS,
      rootMidi + interval,
      brightness,
      profile,
      0.8,
      intensity * Math.max(0.45, 1 - index * 0.08),
    );
  });
}

function updateRuntimeMix(runtime: BeatRuntime, metrics: BeatMetrics, volume: number, profile: BeatProfile) {
  const now = runtime.context.currentTime;
  const masterLevel = (volume / 100) * 0.22 * profile.masterGainScale;
  const filterAmount = clamp((metrics.brightness * 0.55) + ((1 - metrics.heat) * 0.35) + profile.filterBias, 0, 1);
  const filterHz = lerp(profile.filterMinHz, profile.filterMaxHz, filterAmount);
  const q = lerp(0.8, 8.5, metrics.heat);

  runtime.masterGain.gain.cancelScheduledValues(now);
  runtime.masterGain.gain.setTargetAtTime(masterLevel, now, 0.02);
  runtime.toneFilter.frequency.cancelScheduledValues(now);
  runtime.toneFilter.frequency.setTargetAtTime(filterHz, now, 0.04);
  runtime.toneFilter.Q.cancelScheduledValues(now);
  runtime.toneFilter.Q.setTargetAtTime(q, now, 0.04);
}

function loadStoredVolume() {
  const raw = localStorage.getItem(VOLUME_STORAGE_KEY);
  const parsed = raw ? Number(raw) : NaN;

  return Number.isFinite(parsed) ? clamp(parsed, 0, 100) : 42;
}

function loadStoredProfile(): BeatProfileId {
  const stored = localStorage.getItem(PROFILE_STORAGE_KEY);
  return isBeatProfileId(stored) ? stored : 'keygen';
}

function describeMiningSyncEvent(message: WsMiningSyncMessage) {
  switch (message.event) {
    case 'clean_job':
      return `New block reset for job ${message.job_id ?? 'unknown'}`;
    case 'job_received':
      return `Fresh pool job ${message.job_id ?? 'unknown'} arrived`;
    case 'share_accepted':
      return `Share accepted at diff ${message.difficulty?.toFixed(0) ?? '?'}`;
    case 'lucky_share':
      return `Lucky share! ${message.difficulty?.toFixed(0) ?? '?'} diff spike`;
    case 'share_rejected':
      return `Rejected share: ${message.error_msg ?? 'pool rejected work'}`;
    default:
      return 'Live mining sync event';
  }
}

export function BeatLab() {
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const wsConnected = useMinerStore(s => s.wsConnected);
  const latestLog = useMinerStore(s => s.logEntries[s.logEntries.length - 1] ?? null);

  const [profileId, setProfileId] = useState<BeatProfileId>(() => loadStoredProfile());
  const profile = useMemo(() => BEAT_PROFILES[profileId], [profileId]);
  const metrics = useMemo(() => buildBeatMetrics(status, stats, wsConnected, profile), [status, stats, wsConnected, profile]);
  const pattern = useMemo(() => buildBeatPattern(metrics, profile.id), [metrics, profile.id]);
  const [audioState, setAudioState] = useState<BeatAudioState>(() => getAudioContextCtor() ? 'idle' : 'unsupported');
  const [lastMiningSyncAt, setLastMiningSyncAt] = useState(0);
  const [syncClock, setSyncClock] = useState(() => Date.now());
  const [volume, setVolume] = useState(() => loadStoredVolume());
  const [transportStep, setTransportStep] = useState(0);
  const [lastEvent, setLastEvent] = useState('Waiting for telemetry pulses');
  const syncMode: BeatSyncMode = wsConnected && (syncClock - lastMiningSyncAt) < 5000 ? 'mining-sync' : 'telemetry';

  const runtimeRef = useRef<BeatRuntime | null>(null);
  const metricsRef = useRef(metrics);
  const volumeRef = useRef(volume);
  const profileRef = useRef(profile);
  const burstsRef = useRef<BeatBursts>({
    accepted: 0,
    rejected: 0,
    warning: 0,
    error: 0,
    dispatch: 0,
    nonce: 0,
    job: 0,
    cleanJob: 0,
    lucky: 0,
  });
  const previousAcceptedRef = useRef<number | null>(null);
  const previousRejectedRef = useRef<number | null>(null);
  const latestLogIdRef = useRef<number | null>(null);

  useEffect(() => {
    metricsRef.current = metrics;
    if (runtimeRef.current) {
      updateRuntimeMix(runtimeRef.current, metrics, volumeRef.current, profileRef.current);
    }
  }, [metrics]);

  useEffect(() => {
    volumeRef.current = volume;
    localStorage.setItem(VOLUME_STORAGE_KEY, String(volume));
    if (runtimeRef.current) {
      updateRuntimeMix(runtimeRef.current, metricsRef.current, volume, profileRef.current);
    }
  }, [volume]);

  useEffect(() => {
    profileRef.current = profile;
    localStorage.setItem(PROFILE_STORAGE_KEY, profile.id);
    if (runtimeRef.current) {
      updateRuntimeMix(runtimeRef.current, metricsRef.current, volumeRef.current, profile);
    }
  }, [profile]);

  useEffect(() => {
    const intervalId = window.setInterval(() => setSyncClock(Date.now()), 1000);
    return () => window.clearInterval(intervalId);
  }, []);

  useEffect(() => {
    if (syncMode === 'mining-sync') {
      return;
    }

    const accepted = status?.accepted;
    if (accepted == null) {
      previousAcceptedRef.current = null;
      return;
    }

    if (previousAcceptedRef.current != null && accepted > previousAcceptedRef.current) {
      const delta = accepted - previousAcceptedRef.current;
      burstsRef.current.accepted = Math.min(burstsRef.current.accepted + delta, 6);
      setLastEvent(`${delta} accepted share${delta > 1 ? 's' : ''} sparked the lead line`);
    }

    previousAcceptedRef.current = accepted;
  }, [status?.accepted, syncMode]);

  useEffect(() => {
    if (syncMode === 'mining-sync') {
      return;
    }

    const rejected = status?.rejected;
    if (rejected == null) {
      previousRejectedRef.current = null;
      return;
    }

    if (previousRejectedRef.current != null && rejected > previousRejectedRef.current) {
      const delta = rejected - previousRejectedRef.current;
      burstsRef.current.rejected = Math.min(burstsRef.current.rejected + delta, 4);
      setLastEvent(`${delta} rejected share${delta > 1 ? 's' : ''} glitched the pattern`);
    }

    previousRejectedRef.current = rejected;
  }, [status?.rejected, syncMode]);

  useEffect(() => {
    if (!latestLog || latestLogIdRef.current === latestLog.id) {
      return;
    }

    latestLogIdRef.current = latestLog.id;

    if (latestLog.level === 'warn') {
      burstsRef.current.warning = Math.min(burstsRef.current.warning + 1, 4);
      setLastEvent(`Warning pulse: ${summarizeEvent(latestLog.message)}`);
    }

    if (latestLog.level === 'error') {
      burstsRef.current.error = Math.min(burstsRef.current.error + 1, 4);
      setLastEvent(`Error glitch: ${summarizeEvent(latestLog.message)}`);
    }
  }, [latestLog]);

  useEffect(() => {
    return wsManager.subscribe((message: WsMessage) => {
      if (message.type !== 'mining_sync') {
        return;
      }

      setLastMiningSyncAt(Date.now());

      switch (message.event) {
        case 'job_received':
          burstsRef.current.job = Math.min(burstsRef.current.job + 1, 4);
          setLastEvent(describeMiningSyncEvent(message));
          break;
        case 'clean_job':
          burstsRef.current.cleanJob = Math.min(burstsRef.current.cleanJob + 1, 4);
          setLastEvent(describeMiningSyncEvent(message));
          break;
        case 'dispatch_burst':
          burstsRef.current.dispatch = Math.min(burstsRef.current.dispatch + (message.count ?? 1), 128);
          break;
        case 'nonce_burst':
          burstsRef.current.nonce = Math.min(burstsRef.current.nonce + (message.count ?? 1), 196);
          break;
        case 'share_accepted':
          burstsRef.current.accepted = Math.min(burstsRef.current.accepted + 1, 8);
          setLastEvent(describeMiningSyncEvent(message));
          break;
        case 'lucky_share':
          burstsRef.current.lucky = Math.min(burstsRef.current.lucky + 1, 4);
          setLastEvent(describeMiningSyncEvent(message));
          break;
        case 'share_rejected':
          burstsRef.current.rejected = Math.min(burstsRef.current.rejected + 1, 6);
          setLastEvent(describeMiningSyncEvent(message));
          break;
      }
    });
  }, []);

  function stopScheduler(runtime: BeatRuntime) {
    if (runtime.schedulerId !== null) {
      window.clearInterval(runtime.schedulerId);
      runtime.schedulerId = null;
    }
  }

  function disposeRuntime() {
    const runtime = runtimeRef.current;
    if (!runtime) {
      return;
    }

    stopScheduler(runtime);
    disconnectNodes([runtime.toneFilter, runtime.masterGain]);
    runtimeRef.current = null;
    void runtime.context.close().catch(() => {});
  }

  function scheduleStep(runtime: BeatRuntime, step: number, time: number) {
    const liveMetrics = metricsRef.current;
    const liveProfile = profileRef.current;
    const livePattern = buildBeatPattern(liveMetrics, liveProfile.id);
    const stepIntensity = livePattern[step];
    const bursts = burstsRef.current;
    const root = liveProfile.rootMidi + (liveMetrics.activeChains * 2) + Math.round(liveMetrics.heat * 3);
    const bassIndex = (step + liveMetrics.activeChains + Math.round(liveMetrics.brightness * 3)) % liveProfile.bassScale.length;
    const leadIndex = (step + Math.round(liveMetrics.energy * 4)) % liveProfile.leadScale.length;

    updateRuntimeMix(runtime, liveMetrics, volumeRef.current, liveProfile);

    if (!liveMetrics.connected) {
      if (step === 0 || step === 4) {
        playKick(runtime.context, runtime.masterGain, time, 0.25, liveProfile);
      }
      if (step === 2 || step === 6) {
        playHat(runtime.context, runtime.masterGain, time, 0.15, 0.2, liveProfile);
      }
      return;
    }

    if (step === 0 || step === 4 || (liveProfile.id === 'tracker' && liveMetrics.energy > 0.58 && step % 2 === 0)) {
      playKick(runtime.context, runtime.masterGain, time, stepIntensity, liveProfile);
    }

    if (stepIntensity > (liveProfile.id === 'dungeon' ? 0.28 : 0.34)) {
      playBass(
        runtime.context,
        runtime.masterGain,
        time,
        root + liveProfile.bassScale[bassIndex],
        stepIntensity,
        liveMetrics.heat,
        liveProfile,
      );
    }

    if (liveMetrics.brightness > 0.18 && (step % 2 === 1 || liveMetrics.activeChains > 2 || liveProfile.id === 'tracker')) {
      playHat(runtime.context, runtime.masterGain, time + 0.01, liveMetrics.brightness, stepIntensity, liveProfile);
    }

    if (liveMetrics.energy > 0.55 && (step === 3 || (liveProfile.id === 'keygen' && step === 7))) {
      playLead(
        runtime.context,
        runtime.masterGain,
        time + 0.02,
        root + liveProfile.leadScale[leadIndex],
        liveMetrics.brightness,
        liveProfile,
      );
    }

    if (bursts.cleanJob > 0) {
      playKick(runtime.context, runtime.masterGain, time, 1, liveProfile);
      playMotif(
        runtime.context,
        runtime.masterGain,
        time + 0.01,
        liveProfile.rootMidi,
        liveProfile.cleanJobMotif,
        liveProfile,
        liveMetrics.brightness,
        1,
        0.05,
      );
      bursts.cleanJob -= 1;
    } else if (bursts.job > 0) {
      playMotif(
        runtime.context,
        runtime.masterGain,
        time + 0.015,
        liveProfile.rootMidi,
        liveProfile.jobMotif,
        liveProfile,
        liveMetrics.brightness,
        0.75,
        0.055,
      );
      bursts.job -= 1;
    }

    if (bursts.lucky > 0) {
      playKick(runtime.context, runtime.masterGain, time + 0.02, 1, liveProfile);
      playMotif(
        runtime.context,
        runtime.masterGain,
        time + 0.015,
        liveProfile.rootMidi,
        liveProfile.luckyMotif,
        liveProfile,
        liveMetrics.brightness,
        1,
        0.045,
      );
      bursts.lucky -= 1;
    } else if (bursts.accepted > 0) {
      playMotif(
        runtime.context,
        runtime.masterGain,
        time + 0.015,
        liveProfile.rootMidi,
        liveProfile.acceptedMotif,
        liveProfile,
        liveMetrics.brightness,
        0.82,
        0.055,
      );
      bursts.accepted -= 1;
    }

    if (bursts.error > 0) {
      playGlitch(runtime.context, runtime.masterGain, time + 0.02, root + 28, 1.2, liveProfile);
      playGlitch(runtime.context, runtime.masterGain, time + 0.09, root + 19, 0.95, liveProfile);
      bursts.error -= 1;
    } else if (bursts.rejected > 0) {
      playGlitch(runtime.context, runtime.masterGain, time + 0.02, root + 24, 0.9, liveProfile);
      bursts.rejected -= 1;
    } else if (bursts.warning > 0 && step % 4 === 2) {
      playGlitch(runtime.context, runtime.masterGain, time + 0.03, root + 17, 0.55, liveProfile);
      bursts.warning -= 1;
    }

    if (bursts.dispatch > 0) {
      const dispatchAccent = clamp(bursts.dispatch / 18, 0.18, 1);
      playPulse(
        runtime.context,
        runtime.masterGain,
        time + 0.008,
        240 + dispatchAccent * 220,
        dispatchAccent,
        liveProfile.id === 'tracker' ? 'square' : 'triangle',
        liveProfile.filterMinHz + 500,
      );
      bursts.dispatch = Math.max(0, bursts.dispatch - 8);
    }

    if (bursts.nonce > 0) {
      const nonceAccent = clamp(bursts.nonce / 40, 0.2, 1);
      playHat(runtime.context, runtime.masterGain, time + 0.005, liveMetrics.brightness + nonceAccent * 0.2, stepIntensity + nonceAccent * 0.2, liveProfile);
      if (nonceAccent > 0.6 && step % 2 === 0) {
        playLead(
          runtime.context,
          runtime.masterGain,
          time + 0.02,
          root + liveProfile.leadScale[(leadIndex + 1) % liveProfile.leadScale.length],
          liveMetrics.brightness,
          liveProfile,
          0.55,
          0.7,
        );
      }
      bursts.nonce = Math.max(0, bursts.nonce - 14);
    }
  }

  function startScheduler(runtime: BeatRuntime) {
    stopScheduler(runtime);
    runtime.step = 0;
    runtime.nextStepTime = runtime.context.currentTime + 0.05;

    runtime.schedulerId = window.setInterval(() => {
      while (runtime.nextStepTime < runtime.context.currentTime + LOOKAHEAD_S) {
        scheduleStep(runtime, runtime.step, runtime.nextStepTime);
        setTransportStep(runtime.step);
        runtime.step = (runtime.step + 1) % SEQUENCE_STEPS;
        runtime.nextStepTime += (60 / metricsRef.current.tempoBpm) / 2;
      }
    }, SCHEDULER_INTERVAL_MS);
  }

  async function ensureRuntime() {
    const AudioContextCtor = getAudioContextCtor();
    if (!AudioContextCtor) {
      setAudioState('unsupported');
      return null;
    }

    if (runtimeRef.current && runtimeRef.current.context.state !== 'closed') {
      return runtimeRef.current;
    }

    const context = new AudioContextCtor();
    const toneFilter = context.createBiquadFilter();
    const masterGain = context.createGain();
    const runtime: BeatRuntime = {
      context,
      masterGain,
      toneFilter,
      schedulerId: null,
      nextStepTime: 0,
      step: 0,
    };

    toneFilter.type = 'lowpass';
    toneFilter.frequency.value = 2400;
    toneFilter.Q.value = 1.2;
    toneFilter.connect(masterGain);
    masterGain.connect(context.destination);
    masterGain.gain.value = 0.0001;

    runtimeRef.current = runtime;
    updateRuntimeMix(runtime, metricsRef.current, volumeRef.current, profileRef.current);
    return runtime;
  }

  async function handleArmAudio() {
    const runtime = await ensureRuntime();
    if (!runtime) {
      return;
    }

    await runtime.context.resume();
    echoCli(`beat arm --profile ${profile.id}`);
    startScheduler(runtime);
    setAudioState('running');
    setLastEvent(syncMode === 'mining-sync'
      ? `Beat lab armed with ${profile.label} profile and live job/nonce sync`
      : `Beat lab armed with ${profile.label} profile from live telemetry`);
  }

  async function handlePauseAudio() {
    const runtime = runtimeRef.current;
    if (!runtime) {
      return;
    }

    stopScheduler(runtime);
    await runtime.context.suspend();
    setAudioState('paused');
    setTransportStep(0);
  }

  function handleProfileSelect(nextProfileId: BeatProfileId) {
    if (nextProfileId === profileId) {
      return;
    }

    setProfileId(nextProfileId);
    setLastEvent(`Profile changed to ${BEAT_PROFILES[nextProfileId].label}`);
  }

  useEffect(() => () => {
    disposeRuntime();
  }, []);

  const chainRows = status?.chains ?? [];
  const audioChipTone = audioState === 'running'
    ? 'success'
    : audioState === 'paused'
      ? 'warning'
      : audioState === 'unsupported'
        ? 'danger'
        : 'neutral';
  const transportChipTone = metrics.connected ? 'success' : 'warning';
  const syncChipTone = syncMode === 'mining-sync' ? 'success' : 'info';
  const poolChipTone = /alive|connected|running|active|mining/i.test(metrics.poolStatus) ? 'success' : 'neutral';

  const beatStatusTone = audioState === 'running' ? '' : audioState === 'unsupported' ? 'danger' : 'neutral';

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// beat lab</div>
          <h2 className="hacker-inspector-title">8-Bit Miner Sonification</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${beatStatusTone}`}>AUDIO {audioState.toUpperCase()}</span>
          {audioState !== 'running' ? (
            <button className="hacker-inspector-refresh" onClick={() => { void handleArmAudio(); }} disabled={audioState === 'unsupported'}>
              {audioState === 'paused' ? '▶ RESUME' : '▶ ARM'}
            </button>
          ) : (
            <button className="hacker-inspector-help" onClick={() => { void handlePauseAudio(); }}>
              ⏸ PAUSE
            </button>
          )}
          <CliHint cmd={`beat arm --profile ${profile.id}`} />
        </div>
      </header>

      <div className="hacker-inspector-body">
      <div className="bl-section-grid">
        <section className="register-inspector ds-card-hover">
          <div className="adv-section-eyebrow">
            Transport
          </div>
          <div className="adv-kv-stack is-gap-10">
            <div className="adv-kv-row is-78">
              <span className="adv-kv-k">Master volume</span>
              <span className="adv-kv-v is-orange">{volume}%</span>
            </div>
            <input
              className="voltage-slider"
              type="range"
              min={0}
              max={100}
              value={volume}
              onChange={event => setVolume(Number(event.target.value))}
              aria-label="Beat lab volume"
            />
            <div className="bl-kv-stack-76">
              <div className="adv-kv-row">
                <span className="adv-kv-k">Tempo</span>
                <span className="adv-kv-v">{metrics.tempoBpm} BPM</span>
              </div>
              <div className="adv-kv-row">
                <span className="adv-kv-k">Sync source</span>
                <span className="adv-kv-v">{syncMode === 'mining-sync' ? 'job / nonce events' : 'telemetry only'}</span>
              </div>
              <div className="adv-kv-row">
                <span className="adv-kv-k">Last event</span>
                <span className="adv-kv-v is-orange is-right">{lastEvent}</span>
              </div>
            </div>
          </div>
        </section>

        <section className="register-inspector ds-card-hover">
          <div className="adv-section-eyebrow">
            Sound Profiles
          </div>
          <div className="adv-kv-stack">
            {Object.values(BEAT_PROFILES).map(candidate => {
              const active = candidate.id === profileId;
              return (
                <button
                  key={candidate.id}
                  type="button"
                  className={`btn ${active ? 'btn-primary' : 'btn-secondary'} bl-profile-btn`}
                  onClick={() => handleProfileSelect(candidate.id)}
                >
                  <span>{candidate.label}</span>
                  <span className="bl-profile-desc">{candidate.description}</span>
                </button>
              );
            })}
          </div>
        </section>

        <section className="register-inspector ds-card-hover">
          <div className="adv-section-eyebrow">
            Signal Mapping
          </div>
          <div className="adv-kv-stack bl-map-stack">
            <div className="adv-kv-row">
              <span className="adv-kv-k">Hashrate</span>
              <span className="adv-kv-v">Tempo + bass stride</span>
            </div>
            <div className="adv-kv-row">
              <span className="adv-kv-k">Wall watts</span>
              <span className="adv-kv-v">Kick density</span>
            </div>
            <div className="adv-kv-row">
              <span className="adv-kv-k">Average chip temp</span>
              <span className="adv-kv-v">Filter bite + grit</span>
            </div>
            <div className="adv-kv-row">
              <span className="adv-kv-k">Jobs / clean jobs</span>
              <span className="adv-kv-v is-orange">Reset motifs</span>
            </div>
            <div className="adv-kv-row">
              <span className="adv-kv-k">Dispatch / nonce bursts</span>
              <span className="adv-kv-v">Percussive sync clicks</span>
            </div>
            <div className="adv-kv-row">
              <span className="adv-kv-k">Accepted / lucky shares</span>
              <span className="adv-kv-v is-orange">Arps + fanfare motifs</span>
            </div>
            <div className="adv-kv-row">
              <span className="adv-kv-k">Rejected shares / logs</span>
              <span className="adv-kv-v is-red">Glitch stabs</span>
            </div>
          </div>
        </section>

        <section className="register-inspector ds-card-hover">
          <div className="adv-section-eyebrow">
            Live Mix
          </div>
          <div className="bl-mix-grid">
            {[
              { label: 'Profile', value: profile.label, accent: 'var(--accent-orange)' },
              { label: 'Hashrate', value: `${(metrics.hashrateGhs / 1000).toFixed(1)} TH/s`, accent: 'var(--accent)' },
              { label: 'Wall draw', value: formatLiveWallPower(metrics.wallWatts), accent: 'var(--accent)' },
              { label: 'Heat', value: `${metrics.avgTempC.toFixed(1)} C`, accent: 'var(--yellow)' },
              { label: 'Fans', value: `${metrics.fanRpm.toFixed(0)} RPM`, accent: 'var(--accent)' },
              { label: 'Sync lane', value: syncMode === 'mining-sync' ? 'jobs + nonces' : 'status only', accent: 'var(--accent-orange)' },
            ].map(card => (
              <div key={card.label} className="glass-card adv-stat-card bl-mix-card">
                <span className="adv-stat-label">{card.label}</span>
                <span className="adv-stat-value" style={{ color: card.accent }}>{card.value}</span>
              </div>
            ))}
          </div>
        </section>
      </div>

      <section className="register-inspector ds-card-hover adv-mb-16">
        <div className="adv-section-eyebrow">
          Telemetry Sequence
        </div>
        <div className="bl-seq-grid">
          {pattern.map((value, index) => {
            const isActive = audioState === 'running' && index === transportStep;
            return (
              <div
                key={index}
                className={`glass-card bl-seq-cell${isActive ? ' is-active' : ''}`}
              >
                <span className={`bl-seq-num${isActive ? ' is-active' : ''}`}>
                  {index + 1}
                </span>
                <div className="bl-seq-track">
                  <div
                    className={`bl-seq-bar${isActive ? ' is-active' : ''}`}
                    style={{ height: `${Math.round(14 + (value * 42))}px` }}
                  />
                </div>
              </div>
            );
          })}
        </div>
      </section>

      <section className="register-inspector ds-card-hover">
        <div className="adv-section-eyebrow">
          Chain Voices
        </div>
        {chainRows.length > 0 ? (
          <div className="adv-kv-stack is-gap-10">
            {chainRows.map((chain, index) => {
              const chainLevel = clamp(chain.hashrate_ghs / Math.max(metrics.hashrateGhs / Math.max(metrics.activeChains, 1), 1), 0.08, 1);
              const tempTint = clamp((chain.temp_c - 40) / 30, 0, 1);
              return (
                <div key={chain.id} className="glass-card bl-voice-card">
                  <div className="bl-voice-head">
                    <div className="bl-voice-name">
                      Chain {chain.id}
                    </div>
                    <div className="bl-voice-meta">
                      Voice {index + 1} · {chain.frequency_mhz} MHz · {chain.status}
                    </div>
                  </div>
                  <div className="bl-voice-track">
                    <div
                      className="bl-voice-fill"
                      style={{
                        width: `${Math.round(chainLevel * 100)}%`,
                        background: `linear-gradient(90deg, rgba(0,255,65,0.35) 0%, rgba(250,165,0,${0.4 + tempTint * 0.45}) 100%)`,
                      }}
                    />
                  </div>
                  <div className="bl-voice-foot">
                    <span className="adv-kv-k">{chain.chips} chips · {chain.hashrate_ghs.toFixed(0)} GH/s</span>
                    <span style={{ color: tempTint > 0.65 ? 'var(--yellow)' : 'var(--accent)' }}>{chain.temp_c.toFixed(1)} C</span>
                  </div>
                </div>
              );
            })}
          </div>
        ) : (
          <div className="adv-state is-inline bl-empty">
            Waiting for chain telemetry. Once the miner reports active boards, each chain will push the sequencer harder and carve its own voice lane.
          </div>
        )}
      </section>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{syncMode === 'mining-sync' ? 'job/nonce sync' : 'telemetry sync'}</span>
          <span>{metrics.connected ? 'telemetry live' : 'standby'}</span>
          <span>pool: {metrics.poolStatus || 'idle'}</span>
        </div>
      </footer>
    </div>
  );
}
