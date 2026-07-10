import React, { useEffect, useState } from 'react';
import { useMinerStore } from '../../store/miner';
import { api } from '../../api/client';
import { DcentOsLogo } from './DcentOsLogo';
import { DonationFeeCard } from './DonationFeeCard';
import { ApiCompatibilityManifestCard } from './ApiCompatibilityManifestCard';
import { NetworkInfoCard } from './NetworkInfoCard';
import { MinerTypeCard } from './MinerTypeCard';

// CC-2: neutral fallbacks. The previous S9-specific literals (Antminer S9 /
// BM1387 / Zynq-7010) would assert a specific WRONG identity on a non-S9 unit
// (e.g. an S19j Pro / S21) whenever both /api/system/info and the store are
// unavailable (the wedged-daemon case 's timeout fix targets). An
// unidentified unit must say "Unknown", never claim to be an S9.
const FALLBACK_VERSION = 'Unknown';
const FALLBACK_MODEL = 'Unknown';
const FALLBACK_CHIP = 'Unknown';
const FALLBACK_SOC = 'Unknown';

export function AboutPage() {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const status = useMinerStore(s => s.status);

  const [apiInfo, setApiInfo] = useState<Record<string, string> | null>(null);

  // Attempt to fetch live system info from API
  useEffect(() => {
    api.getSystemInfo()
      .then((info) => {
        setApiInfo({
          version: info.version,
          model: info.model,
          chip_type: info.chip_type,
          board: info.board,
          soc: info.soc,
          hostname: info.hostname,
          mac: info.mac,
        });
      })
      .catch(() => {
        // API not available — use store data or hardcoded fallbacks
      });
  }, []);

  const version = apiInfo?.version ?? systemInfo?.version ?? status?.firmware_version ?? FALLBACK_VERSION;
  const model = apiInfo?.model ?? systemInfo?.model ?? FALLBACK_MODEL;
  const chipType = apiInfo?.chip_type ?? systemInfo?.chip_type ?? FALLBACK_CHIP;
  const board = apiInfo?.board ?? systemInfo?.board ?? '---';
  const soc = apiInfo?.soc ?? systemInfo?.soc ?? FALLBACK_SOC;
  const hostname = apiInfo?.hostname ?? systemInfo?.hostname ?? '';
  const mac = apiInfo?.mac ?? systemInfo?.mac ?? '';

  return (
    <div style={{ padding: '24px', maxWidth: 600 }}>
      <div style={{ marginBottom: 8 }}>
        <DcentOsLogo width={220} />
      </div>
      <div style={{
        fontSize: '0.85rem',
        color: 'var(--text-dim)',
        marginBottom: 24,
      }}>
        Custom Mining Firmware for Antminer Hardware
      </div>

      {/* Firmware Info */}
      <AboutCard title="Firmware">
        <InfoRow label="Version" value={`dcentrald v${version}`} accent />
        <InfoRow label="Distribution" value="DCENT_OS" />
        <InfoRow label="License" value="GPL-3.0 -- 100% Open Source" />
        {hostname && <InfoRow label="Hostname" value={hostname} />}
        {mac && <InfoRow label="MAC" value={mac} mono />}
      </AboutCard>

      <ApiCompatibilityManifestCard />

      {/* Hardware Info */}
      <AboutCard title="Hardware">
        <InfoRow label="Model" value={model} />
        <InfoRow label="ASIC Chip" value={chipType} />
        <InfoRow label="SoC" value={soc} />
        <InfoRow label="Board" value={board} />
      </AboutCard>

      {/* W11.12: Stock-CGI parity panel — network identity, hardware
          identity (concise pyasic-style bundle), and a redacted support
          bundle download. Backs `/api/network/info`, `/api/miner/type`,
          and `/api/log/backup`. Read-only; degrades gracefully when the
          daemon is unavailable. */}
      <NetworkInfoCard />

      {/* W13.D2: Hardware Identity card with PVT envelope row + grade chip
          (color-coded by tier), voltage/freq range, chip count, plus the
          voltage_fixed / inverted_curve / requires_apw12_plus tooltips. */}
      <MinerTypeCard />

      {/* Developer */}
      <AboutCard title="Developer">
        <div style={{ fontSize: '0.9rem', color: 'var(--text, #E8E8E8)', lineHeight: 1.8 }}>
          <div><strong style={{ color: 'var(--accent, #FAA500)' }}>D-Central Technologies</strong></div>
          <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)' }}>
            Canada's leading Bitcoin mining technology company.
            Founded 2016, Laval QC. Self-described "Mining Hackers."
          </div>
        </div>
      </AboutCard>

      {/* Donation */}
      <AboutCard title="Donation">
        <DonationFeeCard variant="compact" />
      </AboutCard>

      {/* W9.7 (Wave-4 honest-claims revision): "Coming from VNish?" copy.
          The public CHANGELOG asks the firmware to make the competitor
          devfee contrast explicit, but it must stay factual and
          non-disparaging. VNish builds carry a binary-baked developer fee
          (~2-3% per D-Central's reverse-engineering); DCENT_OS's donation
          is 0-5% configurable, clearly labelled as a donation, and fully
          disableable. Every claim here is verifiable from public RE docs
 and the donation route contract
          (/api/donation/info). */}
      <AboutCard title="Coming from VNish?">
        <div style={{ fontSize: '0.85rem', color: 'var(--text, #E8E8E8)', lineHeight: 1.7 }}>
          VNish builds carry a{' '}
          <strong style={{ color: 'var(--accent, #FAA500)' }}>
            binary-baked developer fee (~2-3%)
          </strong>
          , per D-Central&rsquo;s reverse-engineering. DCENT_OS&rsquo;s
          donation is <strong>0-5% configurable</strong>, clearly labelled
          as a donation rather than a fee, and fully disableable.
        </div>
        <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', marginTop: 10, lineHeight: 1.65 }}>
          The donation pool URL and on-chain payout address are publicly
          disclosed at <code style={{ fontFamily: "'JetBrains Mono', monospace" }}>/api/donation/info</code>
          {' '}so you can independently verify on-chain (mempool.space, etc.)
          that the donation slice flows where the firmware claims.
        </div>
        <div style={{ fontSize: '0.78rem', color: 'var(--text-dim)', marginTop: 10, lineHeight: 1.65, fontStyle: 'italic' }}>
          Last 12 months saved D-Central donors: published in the public pool payout history
          on the block explorer linked from the donation card. We don't keep a private
          tally — the chain is the tally.
        </div>
      </AboutCard>

      {/* Links */}
      <AboutCard title="Links">
        <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
          <AboutLink
            href="https://d-central.tech/"
            label="D-Central Technologies"
            description="Official website"
          />
          <AboutLink
            href="https://github.com/d-central-tech/dcentos"
            label="GitHub Repository"
            description="Source code, issues, contributions"
          />
          <AboutLink
            href="https://d-central.tech/support/"
            label="Support"
            description="Help, documentation, contact"
          />
          <AboutLink
            href="https://d-central.tech/fund/go?source=dcent_os&placement=settings"
            label="Fund the Sovereign Stack"
            description="Keep this open firmware alive — Bitcoin or card"
          />
        </div>
      </AboutCard>

      {/* License */}
      <AboutCard title="License">
        <div style={{ fontSize: '0.85rem', color: 'var(--text, #E8E8E8)', lineHeight: 1.6 }}>
          DCENT_OS is free and open-source software licensed under the
          {' '}<strong>GNU General Public License v3.0</strong> (GPL-3.0).
        </div>
        <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)', marginTop: 8 }}>
          You are free to use, modify, and distribute this firmware.
          Source code is available on{' '}
          <a
            href="https://github.com/d-central-tech/dcentos"
            target="_blank"
            rel="noopener noreferrer"
            style={{ color: 'var(--accent, #FAA500)' }}
          >
            GitHub
          </a>.
        </div>
      </AboutCard>

      {/* Open Source Acknowledgments */}
      <AboutCard title="Open Source" last>
        <div style={{ fontSize: '0.8rem', color: 'var(--text-dim)', lineHeight: 1.8 }}>
          DCENT_OS builds upon the work of many open-source projects:
          <ul style={{ paddingLeft: 20, marginTop: 8 }}>
            <li>Rust / Tokio (MIT/Apache-2.0)</li>
            <li>Buildroot (GPL-2.0)</li>
            <li>Linux Kernel (GPL-2.0)</li>
            <li>React &amp; TypeScript (MIT)</li>
            <li>Zustand (MIT)</li>
            <li>Vite (MIT)</li>
            <li>Lightweight Charts (Apache-2.0)</li>
          </ul>
          <div style={{ marginTop: 8, fontStyle: 'italic' }}>
            Special thanks to the Bitcoin mining open-source community.
          </div>
        </div>
      </AboutCard>
    </div>
  );
}

// ─── Reusable card wrapper ───────────────────────────────────
function AboutCard({ title, children, last }: { title: string; children: React.ReactNode; last?: boolean }) {
  return (
    <div style={{
      background: 'var(--card-bg, #242432)',
      borderRadius: 12,
      padding: 16,
      border: '1px solid var(--border, rgba(255,255,255,0.06))',
      marginBottom: last ? 0 : 16,
    }}>
      <div style={{
        fontSize: '0.75rem',
        color: 'var(--text-dim)',
        textTransform: 'uppercase',
        marginBottom: 12,
        letterSpacing: '0.05em',
      }}>
        {title}
      </div>
      {children}
    </div>
  );
}

// ─── Info row ───────────────────────────────────────────────
function InfoRow({ label, value, accent, mono }: {
  label: string;
  value: string;
  accent?: boolean;
  mono?: boolean;
}) {
  return (
    <div style={{
      display: 'flex',
      justifyContent: 'space-between',
      padding: '6px 0',
      borderBottom: '1px solid rgba(255,255,255,0.05)',
      fontSize: '0.85rem',
    }}>
      <span style={{ color: 'var(--text-dim)' }}>{label}</span>
      <span style={{
        fontFamily: mono ? "'JetBrains Mono', monospace" : "'JetBrains Mono', monospace",
        color: accent ? 'var(--accent, #FAA500)' : 'var(--text, #E8E8E8)',
        fontWeight: accent ? 600 : 400,
      }}>
        {value}
      </span>
    </div>
  );
}

// ─── Link row ───────────────────────────────────────────────
function AboutLink({ href, label, description }: {
  href: string;
  label: string;
  description: string;
}) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      style={{
        display: 'flex',
        justifyContent: 'space-between',
        alignItems: 'center',
        padding: '8px 12px',
        borderRadius: 8,
        background: 'rgba(247, 147, 26, 0.06)',
        border: '1px solid rgba(247, 147, 26, 0.12)',
        textDecoration: 'none',
        transition: 'background 0.15s',
      }}
      onMouseEnter={(e) => { e.currentTarget.style.background = 'rgba(247, 147, 26, 0.12)'; }}
      onMouseLeave={(e) => { e.currentTarget.style.background = 'rgba(247, 147, 26, 0.06)'; }}
    >
      <div>
        <div style={{ fontSize: '0.85rem', fontWeight: 600, color: 'var(--accent, #FAA500)' }}>
          {label}
        </div>
        <div style={{ fontSize: '0.75rem', color: 'var(--text-dim)' }}>
          {description}
        </div>
      </div>
      <span style={{ color: 'var(--text-dim)', fontSize: '0.9rem' }}>{'\u2192'}</span>
    </a>
  );
}
