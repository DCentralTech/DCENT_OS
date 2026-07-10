// @vitest-environment jsdom
//
// LANE-D regression: the daemon config backup/restore controls (COMP-1) render
// an Export + Import control, and selecting an import file stages an explicit
// confirmation dialog BEFORE anything is sent to the daemon. A daemon-side
// validation rejection is surfaced honestly (never claimed as applied).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';

const getConfigExport = vi.fn();
const importConfig = vi.fn();

vi.mock('../../api/client', () => ({
  api: {
    getConfigExport: (...args: unknown[]) => getConfigExport(...args),
    importConfig: (...args: unknown[]) => importConfig(...args),
  },
}));

import { DaemonConfigBackup } from './DaemonConfigBackup';

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => cleanup());

describe('DaemonConfigBackup — export/import controls', () => {
  it('renders the Export and Import controls with honest redaction copy', () => {
    render(<DaemonConfigBackup />);
    expect(screen.getByRole('button', { name: /export config/i })).toBeTruthy();
    expect(screen.getByRole('button', { name: /import config/i })).toBeTruthy();
    // The redaction + fail-closed validation contract must be surfaced.
    // (redaction is mentioned in both the export blurb and the placeholder note,
    // so allow multiple matches — the contract just must be present.)
    expect(screen.getAllByText(/redacted/i).length).toBeGreaterThan(0);
    expect(screen.getByText(/validated on the daemon/i)).toBeTruthy();
  });

  it('shows the import confirmation dialog after a file is selected', async () => {
    render(<DaemonConfigBackup />);

    const input = screen.getByTestId('daemon-config-import-input') as HTMLInputElement;
    const file = new File(
      [JSON.stringify({ config_toml: '[general]\nschema_version = 1\n' })],
      'dcentos-daemon-config.json',
      { type: 'application/json' },
    );
    fireEvent.change(input, { target: { files: [file] } });

    // The confirm dialog appears (FileReader is async, so wait for it). Nothing
    // is sent to the daemon yet.
    const confirm = await screen.findByRole('button', { name: /validate & import/i });
    expect(confirm).toBeTruthy();
    expect(screen.getByText(/import daemon config\?/i)).toBeTruthy();
    expect(importConfig).not.toHaveBeenCalled();
  });

  it('surfaces a daemon validation rejection verbatim instead of claiming success', async () => {
    importConfig.mockRejectedValueOnce(
      new Error(JSON.stringify({ status: 'error', message: 'thermal.target_temp_c (80) must be less than thermal.hot_temp_c (70)' })),
    );
    render(<DaemonConfigBackup />);

    const input = screen.getByTestId('daemon-config-import-input') as HTMLInputElement;
    const file = new File(
      [JSON.stringify({ config_toml: '[thermal]\ntarget_temp_c = 80\n' })],
      'bad.json',
      { type: 'application/json' },
    );
    fireEvent.change(input, { target: { files: [file] } });

    const confirm = await screen.findByRole('button', { name: /validate & import/i });
    fireEvent.click(confirm);

    await waitFor(() => expect(importConfig).toHaveBeenCalledTimes(1));
    // The exact daemon message is shown; the dialog stays open (not applied).
    const alert = await screen.findByRole('alert');
    expect(alert.textContent ?? '').toMatch(/must be less than thermal\.hot_temp_c/i);
  });
});
