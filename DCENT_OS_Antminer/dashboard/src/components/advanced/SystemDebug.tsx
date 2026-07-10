import React, { useState, useEffect, useCallback, useRef } from 'react';
import { useMinerStore } from '../../store/miner';
import { api } from '../../api/client';
import type { SystemStatsResponse } from '../../api/types';
import { BlockerStatePanel } from './BlockerStatePanel';
import { AccentColorPicker } from '../common/AccentColorPicker';
import { useSystemHealth } from '../common/proxy/SystemHealthContext';
import { unsupportedMetricList } from '../../utils/format';

// S9 NAND layout (verified from live probe)
const NAND_LAYOUT = [
  { name: 'boot', mtd: 'mtd0', size: '512K', desc: 'FSBL (First Stage Boot Loader)' },
  { name: 'uboot', mtd: 'mtd1', size: '2.5M', desc: 'U-Boot bootloader' },
  { name: 'fpga1', mtd: 'mtd2', size: '2M', desc: 'FPGA bitstream (primary)' },
  { name: 'fpga2', mtd: 'mtd3', size: '2M', desc: 'FPGA bitstream (backup)' },
  { name: 'uboot_env', mtd: 'mtd4', size: '512K', desc: 'U-Boot environment (redundant CRC32)' },
  { name: 'miner_cfg', mtd: 'mtd5', size: '512K', desc: 'Miner configuration' },
  { name: 'recovery', mtd: 'mtd6', size: '22M', desc: 'Recovery firmware/ramdisk' },
  { name: 'firmware1', mtd: 'mtd7', size: '95M', desc: 'Firmware slot A (UBI)' },
  { name: 'firmware2', mtd: 'mtd8', size: '95M', desc: 'Firmware slot B (UBI)' },
  { name: 'factory', mtd: 'mtd9', size: '36M', desc: 'Factory calibration data' },
];

// UIO devices (14 for S9)
const UIO_DEVICES = [
  { id: 0, addr: '0x42800000', size: '4K', desc: 'Fan controller (AXI Timer PWM)' },
  { id: 1, addr: '0x43C00000', size: '4K', desc: 'Chain 6 - Common registers' },
  { id: 2, addr: '0x43C01000', size: '4K', desc: 'Chain 6 - Cmd RX FIFO' },
  { id: 3, addr: '0x43C02000', size: '4K', desc: 'Chain 6 - Work RX FIFO' },
  { id: 4, addr: '0x43C03000', size: '4K', desc: 'Chain 6 - Work TX FIFO' },
  { id: 5, addr: '0x43C10000', size: '4K', desc: 'Chain 7 - Common registers' },
  { id: 6, addr: '0x43C11000', size: '4K', desc: 'Chain 7 - Cmd RX FIFO' },
  { id: 7, addr: '0x43C12000', size: '4K', desc: 'Chain 7 - Work RX FIFO' },
  { id: 8, addr: '0x43C13000', size: '4K', desc: 'Chain 7 - Work TX FIFO' },
  { id: 9, addr: '0x43C20000', size: '4K', desc: 'Chain 8 - Common registers' },
  { id: 10, addr: '0x43C21000', size: '4K', desc: 'Chain 8 - Cmd RX FIFO' },
  { id: 11, addr: '0x43C22000', size: '4K', desc: 'Chain 8 - Work RX FIFO' },
  { id: 12, addr: '0x43C23000', size: '4K', desc: 'Chain 8 - Work TX FIFO' },
  { id: 13, addr: '0x43D00000', size: '4K', desc: 'Braiins glitch monitor (Braiins-am2 only — stock hw: address hole)' },
];

const CLOCK_TREE = [
  { name: 'PS_CLK', freq: '33.333 MHz', desc: 'External crystal oscillator' },
  { name: 'ARM_PLL', freq: '1332 MHz', desc: 'ARM CPU PLL (2x 666 MHz Cortex-A9)' },
  { name: 'DDR_PLL', freq: '1066 MHz', desc: 'DDR3 memory controller' },
  { name: 'IO_PLL', freq: '1000 MHz', desc: 'I/O peripherals' },
  { name: 'FCLK_CLK0', freq: '100 MHz', desc: 'PL fabric clock (doubled to 200 MHz by PL PLL)' },
  { name: 'FPGA_FABRIC', freq: '200 MHz', desc: 'Effective FPGA clock (UART baud reference)' },
];

const GPIO_MAP = [
  { pin: '893-895', dir: 'output', desc: 'Hash board enable (chain 6/7/8)' },
  { pin: '902-904', dir: 'input', desc: 'PLUGO detect (HIGH = board connected)' },
  { pin: 'EMIO37', dir: 'output', desc: 'Red LED (front panel)' },
  { pin: 'EMIO38', dir: 'output', desc: 'Green LED (front panel)' },
  { pin: 'EMIO15', dir: 'output', desc: 'Red LED inside (heartbeat)' },
  { pin: 'EMIO47', dir: 'input', desc: 'Reset button (polled 100ms)' },
  { pin: 'EMIO51', dir: 'input', desc: 'IP Report button (polled 100ms)' },
];

interface LiveData {
  systemStats: SystemStatsResponse | null;
  i2cStatus: Record<number, string>;
}

function formatKb(kb?: number | null): string {
  if (typeof kb !== 'number' || !Number.isFinite(kb) || kb <= 0) return '---';
  const mb = kb / 1024;
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${mb.toFixed(0)} MB`;
}

function formatPercent(value?: number | null): string {
  return typeof value === 'number' && Number.isFinite(value) ? `${value.toFixed(0)}%` : '---';
}

function formatTemp(value?: number | null): string {
  return typeof value === 'number' && Number.isFinite(value) ? `${value.toFixed(1)}C` : '---';
}

function formatDuration(seconds?: number | null): string {
  if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds <= 0) return '---';
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  return `${hours}h ${minutes}m`;
}

function Section({ title, children, collapsible = false, defaultOpen = true }: {
  title: string; children: React.ReactNode; collapsible?: boolean; defaultOpen?: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);

  return (
    <div className="sd-section">
      {collapsible ? (
        <button
          type="button"
          onClick={() => setOpen(!open)}
          aria-expanded={open}
          className={`sd-section-head sd-section-btn ${open ? 'is-open' : 'is-closed'}`}
        >
          <span className={`sd-section-caret ${open ? 'is-open' : ''}`}>
            {'\u25B6'}
          </span>
          {title}
        </button>
      ) : (
        <div className={`sd-section-head ${open ? 'is-open' : 'is-closed'}`}>
          {title}
        </div>
      )}
      {open && children}
    </div>
  );
}

export function SystemDebug() {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const status = useMinerStore(s => s.status);
  const { isProxyMode } = useSystemHealth();
  const [liveData, setLiveData] = useState<LiveData | null>(null);
  const [liveRefresh, setLiveRefresh] = useState(true);
  const liveRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const fetchLiveData = useCallback(async () => {
    try {
      const [systemStats, i2cResults] = await Promise.all([
        api.getSystemStats().catch(() => null),
        isProxyMode
          ? Promise.resolve(null)
          : Promise.allSettled([
              api.readI2c(0, '0x55'),
              api.readI2c(0, '0x56'),
              api.readI2c(0, '0x57'),
            ]),
      ]);

      const i2cStatus: Record<number, string> = {};
      if (isProxyMode) {
        [6, 7, 8].forEach(chain => {
          i2cStatus[chain] = 'BOSMINER OWNER';
        });
      } else if (i2cResults) {
        const addrs = [
          { chain: 6, result: i2cResults[0] },
          { chain: 7, result: i2cResults[1] },
          { chain: 8, result: i2cResults[2] },
        ];
        for (const { chain, result } of addrs) {
          if (result?.status === 'fulfilled') {
            const byte = (result.value.data ?? [])[0];
            if (byte === 0x60) i2cStatus[chain] = 'APP (0x60)';
            else if (byte === 0xCC) i2cStatus[chain] = 'BOOT (0xCC)';
            else if (byte === 0x00) i2cStatus[chain] = 'DEAD (0x00)';
            else i2cStatus[chain] = `0x${byte?.toString(16) ?? '??'}`;
          } else {
            i2cStatus[chain] = 'NACK';
          }
        }
      }

      setLiveData({
        systemStats,
        i2cStatus,
      });
    } catch {
      // Silently fail on live data refresh
    }
  }, [isProxyMode]);

  useEffect(() => {
    fetchLiveData();
  }, [fetchLiveData]);

  useEffect(() => {
    if (liveRefresh) {
      liveRef.current = setInterval(fetchLiveData, 5000);
    }
    return () => {
      if (liveRef.current) clearInterval(liveRef.current);
    };
  }, [liveRefresh, fetchLiveData]);

  // Metric display helper
  const Metric = ({ label, value, color }: { label: string; value: string; color?: string }) => (
    <div className="sd-metric">
      <div className="sd-metric-label">
        {label}
      </div>
      <div className="sd-metric-value" style={{ color: color || 'var(--accent)' }}>
        {value}
      </div>
    </div>
  );

  const systemStats = liveData?.systemStats ?? null;
  const uptimeSeconds = systemStats?.uptime_s || status?.uptime_s || 0;
  // P3-8: AxeOS/pyasic-compat fields the daemon reports as 0 are not real
  // telemetry — surface them honestly so a 0 is never mistaken for measured.
  const unsupportedFields = unsupportedMetricList(systemInfo?.unsupported_metrics);

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// system debug</div>
          <h2 className="hacker-inspector-title">Runtime Diagnostics</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${liveRefresh ? '' : 'neutral'}`}>
            {liveRefresh ? 'LIVE' : 'PAUSED'}
          </span>
          <button
            type="button"
            aria-label="Toggle live refresh"
            aria-pressed={liveRefresh ? true : false}
            className="hacker-inspector-help"
            onClick={() => setLiveRefresh(current => !current)}
          >
            {liveRefresh ? '⏸ LIVE' : '▶ LIVE'}
          </button>
          <button
            type="button"
            className="hacker-inspector-refresh"
            onClick={fetchLiveData}
          >
            ⟳ REFRESH
          </button>
        </div>
      </header>

      <div className="hacker-inspector-body">
      <BlockerStatePanel />

      {/* Appearance — accent picker, consistent with Standard/Heater
          Settings. Default keeps Hacker phosphor-green; an explicitly
          chosen accent applies to every mode (design: "affects every
          mode"). Browser-local; no daemon contract. */}
      <div className="register-inspector sd-block">
        <Section title="Appearance">
          <AccentColorPicker />
        </Section>
      </div>

      {/* Live data section (top) */}
      <div className="register-inspector sd-block">
        <Section title="System Status">
          <div className="sd-metric-grid">
            <Metric
              label="Load Avg 1m"
              value={systemStats ? `${systemStats.load_avg_1m.toFixed(2)} (${formatPercent(systemStats.load_percent_1m)})` : '---'}
              color={systemStats?.load_percent_1m && systemStats.load_percent_1m > 90 ? 'var(--red)' : undefined}
            />
            <Metric
              label="Memory"
              value={systemStats ? `${formatKb(systemStats.mem_used_kb)} / ${formatKb(systemStats.mem_total_kb)}` : '---'}
              color={systemStats?.mem_used_percent && systemStats.mem_used_percent > 85 ? 'var(--red)' : undefined}
            />
            <Metric
              label="SoC Temp"
              value={formatTemp(systemStats?.soc_temp_c)}
              color={systemStats?.soc_temp_c && systemStats.soc_temp_c > 70 ? 'var(--red)' : undefined}
            />
            <Metric label="Uptime" value={formatDuration(uptimeSeconds)} />
          </div>

          {/* I2C PIC Status */}
          <div className="adv-mb-12">
            <div className="sd-i2c-head">
              <span id="system-debug-i2c-status-heading">I2C PIC Status</span>
            </div>
            {/* Wave-13: removed the "Reset I2C Controller" button. It issued
                writeRegister({chain:6, offset:0x0040}) — but 0x40 is the AXI
                IIC SOFTR offset, NOT a chain-6 FPGA register, so the write hit
                the wrong hardware location (ineffective + potentially
                disruptive). A correct reset needs a dedicated daemon route. */}
            <div className="adv-empty-note adv-mb-8">
              I2C controller reset is in development. It requires a dedicated
              <code> /api/debug/i2c-reset</code> route (the old control wrote to
              the wrong register offset and was removed).
              {isProxyMode && <> Blocked: bosminer owns I2C in proxy mode.</>}
            </div>
            <div className="sd-i2c-row" role="group" aria-labelledby="system-debug-i2c-status-heading">
              {[6, 7, 8].map(chain => {
                const st = liveData?.i2cStatus[chain] || '---';
                const color = st.includes('APP') ? 'var(--green)'
                  : st.includes('BOOT') ? 'var(--yellow)'
                  : st.includes('DEAD') || st.includes('NACK') ? 'var(--red)'
                  : 'var(--text-dim)';
                return (
                  <div key={chain} className="sd-i2c-cell">
                    <div className="sd-i2c-cell-k">
                      Chain {chain} (0x{(0x54 + chain - 5).toString(16)})
                    </div>
                    <div className="sd-i2c-cell-v" style={{ color }}>
                      {st}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        </Section>
      </div>

      {/* Static reference sections (collapsible) */}
      <div className="adv-grid-2">
        {/* Left column */}
        <div>
          {/* System info */}
          <div className="register-inspector sd-block">
            <Section title="System Information" collapsible defaultOpen={false}>
              <div className="adv-mono-block sd-info-block">
                <div>Firmware:  <span style={{ color: 'var(--text)' }}>{systemInfo?.firmware ?? 'DCENT_OS'}</span></div>
                <div>Version:   <span style={{ color: 'var(--text)' }}>{systemInfo?.version ?? '---'}</span></div>
                <div>Model:     <span style={{ color: 'var(--text)' }}>{systemInfo?.model ?? '---'}</span></div>
                <div>Board:     <span style={{ color: 'var(--text)' }}>{systemInfo?.board ?? 'am1-s9'}</span></div>
                <div>SoC:       <span style={{ color: 'var(--text)' }}>{systemInfo?.soc ?? 'Zynq XC7Z010'}</span></div>
                <div>Chip Type: <span style={{ color: 'var(--text)' }}>{systemInfo?.chip_type ?? '---'}</span></div>
                <div>Hostname:  <span style={{ color: 'var(--text)' }}>{systemInfo?.hostname ?? '---'}</span></div>
                <div>MAC:       <span style={{ color: 'var(--text)' }}>{systemInfo?.mac ?? '---'}</span></div>
                {/* Kernel/CPU/RAM/NAND are not in the systemInfo contract — the
                    daemon does not report them per-device. They were previously
                    hardcoded to S9/Zynq values (wrong on Amlogic/BB), so they are
                    rendered as unavailable rather than fabricated. The S9/Zynq
                    figures live in the relabeled static reference tables below. */}
                <div>Kernel:    <span style={{ color: 'var(--text-dim)' }}>---</span></div>
                <div>CPU:       <span style={{ color: 'var(--text-dim)' }}>---</span></div>
                <div>RAM:       <span style={{ color: 'var(--text-dim)' }}>---</span></div>
                <div>NAND:      <span style={{ color: 'var(--text-dim)' }}>---</span></div>
              </div>
              <div className="adv-empty-note adv-mb-8">
                Kernel / CPU / RAM / NAND are not reported by the daemon for this
                device — shown as <code>---</code>. See the static S9/Zynq reference
                tables below for the reference layout (not this unit's measured config).
              </div>
              {unsupportedFields.length > 0 && (
                <div className="adv-empty-note adv-mb-8">
                  Compatibility-only fields reported as 0 (n/a, not real telemetry):{' '}
                  {unsupportedFields.join(', ')}.
                </div>
              )}
            </Section>
          </div>

          {/* NAND layout */}
          <div className="register-inspector sd-block">
            <Section title="NAND Partition Layout — S9/Zynq reference (static, 256 MB)" collapsible defaultOpen={false}>
              <div className="table-wrap">
                <table className="sd-table">
                  <thead>
                    <tr>
                      <th scope="col">MTD</th>
                      <th scope="col">Name</th>
                      <th scope="col">Size</th>
                      <th scope="col">Description</th>
                    </tr>
                  </thead>
                  <tbody>
                    {NAND_LAYOUT.map(p => (
                      <tr key={p.mtd}>
                        <td className="sd-td-accent">{p.mtd}</td>
                        <td>{p.name}</td>
                        <td className="sd-td-yellow">{p.size}</td>
                        <td className="sd-td-dim">{p.desc}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </Section>
          </div>

          {/* GPIO */}
          <div className="register-inspector">
            <Section title="GPIO Map — S9/Zynq reference (static)" collapsible defaultOpen={false}>
              <div className="table-wrap">
                <table className="sd-table">
                  <thead>
                    <tr>
                      <th scope="col">Pin</th>
                      <th scope="col">Dir</th>
                      <th scope="col">Description</th>
                    </tr>
                  </thead>
                  <tbody>
                    {GPIO_MAP.map(g => (
                      <tr key={g.pin}>
                        <td className="sd-td-accent">{g.pin}</td>
                        <td style={{ color: g.dir === 'output' ? 'var(--yellow)' : 'var(--green)' }}>{g.dir}</td>
                        <td className="sd-td-dim">{g.desc}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </Section>
          </div>
        </div>

        {/* Right column */}
        <div>
          {/* UIO devices */}
          <div className="register-inspector sd-block">
            <Section title="UIO Devices — S9/Zynq reference (static, 14)" collapsible defaultOpen={false}>
              <div className="table-wrap">
                <table className="sd-table">
                  <thead>
                    <tr>
                      <th scope="col">UIO</th>
                      <th scope="col">Address</th>
                      <th scope="col">Size</th>
                      <th scope="col">Description</th>
                    </tr>
                  </thead>
                  <tbody>
                    {UIO_DEVICES.map(dev => (
                      <tr key={dev.id}>
                        <td className="sd-td-accent">uio{dev.id}</td>
                        <td className="sd-td-yellow">{dev.addr}</td>
                        <td>{dev.size}</td>
                        <td className="sd-td-dim">{dev.desc}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </Section>
          </div>

          {/* Clock tree */}
          <div className="register-inspector">
            <Section title="Clock Tree — S9/Zynq reference (static)" collapsible defaultOpen={false}>
              <div className="table-wrap">
                <table className="sd-table">
                  <thead>
                    <tr>
                      <th scope="col">Clock</th>
                      <th scope="col">Frequency</th>
                      <th scope="col">Description</th>
                    </tr>
                  </thead>
                  <tbody>
                    {CLOCK_TREE.map(clk => (
                      <tr key={clk.name}>
                        <td className="sd-td-accent">{clk.name}</td>
                        <td className="sd-td-yellow">{clk.freq}</td>
                        <td className="sd-td-dim">{clk.desc}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </Section>
          </div>
        </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>uptime {Math.floor(uptimeSeconds / 60)}m</span>
          <span>{liveRefresh ? 'live polling' : 'static snapshot'}</span>
        </div>
      </footer>
    </div>
  );
}
