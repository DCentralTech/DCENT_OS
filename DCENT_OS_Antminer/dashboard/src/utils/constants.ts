// API endpoints, refresh intervals, and app constants
// "It's pretty DCENT." — D-Central Technologies

export const API_BASE = '';

// Easter egg quotes — shown randomly in the loading screen and console
export const QUOTES = [
  // Bitcoin
  "\"Chancellor on brink of second bailout for banks.\" — Satoshi Nakamoto",
  "\"Running bitcoin.\" — Hal Finney, Jan 11, 2009",
  "\"I am hodling.\" — GameKyuubi, 2013",
  "\"Not your keys, not your coins.\" — Andreas Antonopoulos",
  "\"Stay humble, stack sats.\" — Matt Odell",
  // Hacking
  "\"Hack the planet!\" — Dade Murphy",
  // Star Wars / Pop culture
  "\"May the hash force be with you.\"",
  "\"That's no moon... that's a space heater.\"",
  "\"This is the way.\" — Din Djarin",
  // D-Central
  "\"It's pretty DCENT.\"",
  "\"Mining Hackers since 2016.\" — D-Central Technologies",
  "\"Every watt earns sats.\"",
  "\"Your miner, your rules.\"",
  "\"Heat your home. Stack your sats.\"",
  // Mining-flavored status lines (domain depth)
  "Probing hashboard 6…",
  "Listening for nonces on chain 7…",
  "Synchronizing chip frequencies…",
  "Resolving Stratum endpoint…",
  "Reading PIC firmware revision…",
  "Walking the FPGA register map…",
  "Asserting PWR_CONTROL…",
  "Subscribing to mining.notify…",
  "Negotiating ASICBoost version-rolling…",
  "Computing midstate over the block header…",
  "Polling LM75 sensors…",
  "Waiting on first accepted share…",
  "Holding chains warm on disconnect…",
  "Disabling hash before raising fans…",
] as const;

export function randomQuote(): string {
  return QUOTES[Math.floor(Math.random() * QUOTES.length)];
}

// Refresh intervals
export const WS_RECONNECT_MS = 1000;
export const POLL_INTERVAL_MS = 5000;
export const HISTORY_PUSH_INTERVAL_MS = 60000;
export const BTC_PRICE_REFRESH_MS = 300000; // 5 minutes

// Chart time ranges
export const TIME_RANGES = [
  { label: '15m', seconds: 900 },
  { label: '1h', seconds: 3600 },
  { label: '6h', seconds: 21600 },
  { label: '24h', seconds: 86400 },
  { label: '7d', seconds: 604800 },
  { label: '30d', seconds: 2592000 },
] as const;

// Home-mode fan curve presets: array of [temp_c, pwm_pct] control points.
// Loud emergency airflow is owned by the daemon's thermal supervisor, not the
// dashboard curve editor.
export interface FanCurvePoint {
  temp: number;
  pwm: number;
}

export const FAN_CURVE_PRESETS: Record<string, FanCurvePoint[]> = {
  quiet: [
    { temp: 30, pwm: 8 },
    { temp: 45, pwm: 10 },
    { temp: 55, pwm: 20 },
    { temp: 65, pwm: 30 },
    { temp: 75, pwm: 30 },
  ],
  balanced: [
    { temp: 30, pwm: 10 },
    { temp: 40, pwm: 20 },
    { temp: 50, pwm: 25 },
    { temp: 60, pwm: 30 },
    { temp: 70, pwm: 30 },
  ],
  performance: [
    { temp: 30, pwm: 25 },
    { temp: 40, pwm: 30 },
    { temp: 50, pwm: 30 },
    { temp: 55, pwm: 30 },
    { temp: 65, pwm: 30 },
  ],
};

// Pool templates for setup wizard
export interface PoolTemplate {
  name: string;
  url: string;
  category: 'pooled' | 'solo';
  highlighted?: boolean;
  description: string;
  sv2_url?: string;
  sv2_supported?: boolean;
}

export const POOL_TEMPLATES: PoolTemplate[] = [
  // ─── Recommended for home miners ───
  { name: 'DCENT_Pool', url: 'stratum+tcp://pool.d-central.tech:3333', category: 'pooled', description: 'D-Central\'s Solo/Guild pool — a trustless, MMORPG-style take on solo mining: hunt blocks solo, or join a guild to share the block reward with fellow miners, fully non-custodial. First-class DCENT_OS support.' },
  { name: 'Ocean', url: 'stratum+tcp://mine.ocean.xyz:3334', category: 'pooled', highlighted: true, description: 'Non-custodial payouts direct to your wallet — no account needed, transparent block templates' },
  { name: 'Braiins Pool', url: 'stratum+tcp://stratum.braiins.com:3333', category: 'pooled', highlighted: true, description: 'The original Bitcoin mining pool (est. 2010, formerly Slush Pool) — Stratum V2, 0% fee on shared rewards', sv2_url: 'stratum2+tcp://v2.stratum.braiins.com:3336', sv2_supported: true },
  { name: 'DEMAND', url: 'stratum+tcp://mining.dmnd.work:1000', category: 'pooled', description: 'Stratum V2 pool — encrypted connections, miner-built block templates', sv2_url: 'stratum2+tcp://mining.dmnd.work:2000', sv2_supported: true },
  { name: 'Kryptex', url: 'stratum+tcp://btc.kryptex.network:7014', category: 'pooled', description: 'No registration required — wallet-based mining, EU/US/Asia servers' },
  { name: 'Hiveon', url: 'stratum+tcp://btc.hiveon.com:3333', category: 'pooled', description: 'Zero-fee pool — no KYC, auto-selects nearest server' },

  // ─── Major pools ───
  { name: 'Foundry USA', url: 'stratum+tcp://btc.foundryusapool.com:3333', category: 'pooled', description: 'Largest pool (~33% hashrate) — US-based, stable payouts, account required' },
  { name: 'AntPool', url: 'stratum+tcp://stratum.antpool.com:3333', category: 'pooled', description: 'Bitmain-operated pool (~15% hashrate) — global servers, account required' },
  { name: 'F2Pool', url: 'stratum+tcp://btc.f2pool.com:3333', category: 'pooled', description: 'Major global pool with regional servers (NA, EU, Asia, Africa, Latin America)' },
  { name: 'ViaBTC', url: 'stratum+tcp://btc.viabtc.com:3333', category: 'pooled', description: 'Large pool (~9% hashrate) — flexible payout options, global servers' },
  { name: 'Binance Pool', url: 'stratum+tcp://sha256.poolbinance.com:8888', category: 'pooled', description: 'Binance exchange pool (~7% hashrate) — Binance account required' },
  { name: 'SpiderPool', url: 'stratum+tcp://btc-us.spiderpool.com:2309', category: 'pooled', description: 'Fast-growing pool (~7% hashrate) — regional servers worldwide' },
  { name: 'Luxor', url: 'stratum+tcp://btc.global.luxor.tech:700', category: 'pooled', description: 'North American pool — auto-routing to nearest server, firmware integration' },
  { name: 'NiceHash', url: 'stratum+tcp://sha256.usa.nicehash.com:3334', category: 'pooled', description: 'Hashrate marketplace — sell your hashrate to buyers, paid in BTC' },

  // ─── Other pools ───
  { name: 'EMCD', url: 'stratum+tcp://gate.emcd.io:7878', category: 'pooled', description: 'Growing pool (~3.5% hashrate) — regional endpoints (EU, US, Asia)' },
  { name: 'CloverPool', url: 'stratum+tcp://stratum.cloverpool.com:3333', category: 'pooled', description: 'Formerly BTC.com — long-running pool, rebranded 2024' },
  { name: 'Poolin', url: 'stratum+tcp://btc.ss.poolin.com:443', category: 'pooled', description: 'Established pool — multiple port options (443, 1883, 700)' },

  // ─── Solo mining services ───
  { name: 'SoloCK', url: 'stratum+tcp://solo.ckpool.org:3333', category: 'solo', highlighted: true, description: 'The original solo mining proxy (est. 2014) — no account, 2% fee only if you find a block' },
  { name: 'Public Pool', url: 'stratum+tcp://public-pool.io:21496', category: 'solo', highlighted: true, description: 'Zero-fee open-source solo pool — self-hostable, popular with BitAxe miners' },
  { name: 'Braiins Solo', url: 'stratum+tcp://solo.stratum.braiins.com:3333', category: 'solo', description: 'Solo mining from the Braiins team — 0.5% fee, no registration, wallet as username' },
  { name: 'AtlasPool', url: 'stratum+tcp://solo.atlaspool.io:3333', category: 'solo', description: 'Global solo pool with 100+ points of presence — 1.5% fee, anycast routing' },
  { name: 'SoloMining.xyz', url: 'stratum+tcp://btc.solomining.xyz:1313', category: 'solo', description: 'Solo pool with real-time worker dashboard — 1% fee, direct-to-wallet payouts' },
  { name: 'Kano', url: 'stratum+tcp://stratum.kano.is:3333', category: 'solo', description: 'Long-running small pool (est. 2014) — personal payout, low minimum threshold' },
];

// Heater noise icons
export const NOISE_ICONS = ['🤫', '🔈', '🔉', '🔊', '📢'] as const;

// FPGA register shortcuts (S9)
export const FPGA_REGISTERS = [
  { name: 'Version', chain: 6, offset: '0x0000', desc: 'FPGA version register' },
  { name: 'Build ID', chain: 6, offset: '0x0004', desc: 'FPGA build timestamp' },
  { name: 'Work TX', chain: 6, offset: '0x3000', desc: 'Work transmit FIFO' },
  { name: 'Cmd RX', chain: 6, offset: '0x1000', desc: 'Command receive FIFO' },
  { name: 'Work RX', chain: 6, offset: '0x2000', desc: 'Work receive FIFO (nonces)' },
  { name: 'Fan PWM', chain: 0, offset: '0x0000', desc: 'Fan controller PWM register' },
] as const;

// ASIC commands
export const ASIC_COMMANDS = [
  'ReadReg', 'WriteReg', 'SetAddress', 'ChipID',
  'SetFrequency', 'ChainInactive', 'SetBaudRate',
] as const;

// Mode descriptions for wizard
export const MODE_DESCRIPTIONS = {
  heater: {
    title: 'Space Heater',
    subtitle: 'I want to heat my home and earn Bitcoin',
    description: 'Thermostat-first interface. Control your miner like a space heater — set power level, see BTU output, track sats earned. Perfect for home use.',
    icon: '🔥',
  },
  standard: {
    title: 'Mining',
    subtitle: 'I want to maximize hashrate and efficiency',
    description: 'Full mining dashboard with hashrate charts, pool management, fan control, tuning profiles, and profitability tracking.',
    icon: '⛏️',
  },
  hacker: {
    title: 'Hacker',
    subtitle: "I build, break, and fix miners",
    description: 'Everything from Mining mode plus raw FPGA access, ASIC commands, PID tuning, chip maps, voltage control, and diagnostics. Handle with care.',
    icon: '🔧',
  },
} as const;
