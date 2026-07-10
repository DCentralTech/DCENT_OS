// @vitest-environment jsdom
//
// INST-1 truth-contract regression: a fully-mining unit must NOT show a
// permanent false "Booting" strip.
//
// Backend fix: `/api/boot/phase` now returns 404 when the boot-phase tracker
// was never started (the common case on a healthy already-running S9 / S19j
// Pro). The dashboard maps that 404 to `null` (`api.getBootPhase()`), then
// synthesizes the TRUE state from `/api/status` and HIDES the banner when the
// synthesized state is `mining`.
//
// These tests pin that the banner:
//   1. hides on 404 + mining status (no false "Booting"),
//   2. still surfaces a degrade banner when the daemon is dead with no status,
//   3. still renders a real published phase (cold-boot orchestration intact).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';

import type { DaemonHealth } from '../../hooks/useDaemonHeartbeat';
import type { BootPhaseResponse, StatusResponse } from '../../api/types';

// ─── Mocks ─────────────────────────────────────────────────────────────
// Mutable holders the tests drive; the mocked hooks/api read from them.
let mockBootPhase: BootPhaseResponse | null = null;
let mockDaemon: DaemonHealth;
let mockStatus: StatusResponse | null = null;

vi.mock('../../api/client', () => ({
  api: {
    getBootPhase: vi.fn(async () => mockBootPhase),
  },
}));

vi.mock('../../hooks/useDaemonHeartbeat', () => ({
  useDaemonHeartbeat: () => mockDaemon,
}));

vi.mock('../../store/miner', () => ({
  // The component calls useMinerStore(s => s.status).
  useMinerStore: (selector: (s: { status: StatusResponse | null }) => unknown) =>
    selector({ status: mockStatus }),
}));

import { BootPhaseBanner } from './BootPhaseBanner';

function daemon(state: DaemonHealth['state']): DaemonHealth {
  return {
    state,
    pidAlive: state === 'alive',
    pid: state === 'alive' ? 1 : null,
    uptimeSec: null,
    lastSeenSec: null,
    lastLogLines: [],
    lastError: null,
    lastProbeTs: null,
    lastApiSuccessTs: null,
    serverPyAlive: state !== 'dead',
  };
}

function miningStatus(): StatusResponse {
  // Only the `accepted` field is read by the banner's synthesize path.
  return { accepted: 12 } as unknown as StatusResponse;
}

beforeEach(() => {
  mockBootPhase = null;
  mockDaemon = daemon('alive');
  mockStatus = null;
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('BootPhaseBanner — INST-1 truth contract', () => {
  it('hides the banner on /api/boot/phase 404 when the unit is mining (no false "Booting")', async () => {
    // 404 from the daemon -> api.getBootPhase() resolves null.
    mockBootPhase = null;
    mockDaemon = daemon('alive');
    mockStatus = miningStatus();

    const { container } = render(<BootPhaseBanner pollMs={5} />);

    // After the synthesize path resolves to "mining", the banner returns null.
    await waitFor(() => {
      expect(screen.queryByTestId('boot-phase-banner')).toBeNull();
    });
    // Belt-and-suspenders: never paints a "booting" substate chip.
    expect(container.querySelector('[data-phase="booting"]')).toBeNull();
    expect(screen.queryByTestId('boot-substate-booting')).toBeNull();
  });

  it('still shows a degrade banner when the daemon is dead with no status', async () => {
    mockBootPhase = null;
    mockDaemon = daemon('dead');
    mockStatus = null;

    render(<BootPhaseBanner pollMs={5} />);

    // Dead daemon + no status synthesizes "booting" and must remain visible
    // (this is the genuinely-booting / unreachable case, not a false alarm).
    await waitFor(() => {
      expect(screen.getByTestId('boot-phase-banner')).toBeTruthy();
    });
  });

  it('renders the real published phase when the tracker IS active (200 response)', async () => {
    mockBootPhase = {
      phase: { kind: 'cv1835', phase: 'boot_asic_enum' },
      started_at_unix_ms: 1_700_000_000_000,
      is_live: true,
    } as BootPhaseResponse;
    mockDaemon = daemon('alive');
    mockStatus = null;

    render(<BootPhaseBanner pollMs={5} />);

    await waitFor(() => {
      const banner = screen.getByTestId('boot-phase-banner');
      expect(banner.getAttribute('data-phase')).toBe('boot_asic_enum');
    });
  });
});
