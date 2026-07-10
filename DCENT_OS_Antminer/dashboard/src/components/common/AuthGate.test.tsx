// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';

import { api, ApiError } from '../../api/client';
import { clearCredentials } from '../../api/credentials';
import { useMinerStore } from '../../store/miner';
import { AuthGate } from './AuthGate';

function protectedSetupStatus() {
  return {
    needs_setup: false,
    resume_requires_auth: true,
    auth: { password_set: true, token_issued: false, password_opt_out: false },
  };
}

function renderGate() {
  render(
    <AuthGate>
      <div>Advanced content</div>
    </AuthGate>,
  );
}

beforeEach(() => {
  clearCredentials();
  useMinerStore.setState({
    authenticated: false,
    setupStatus: protectedSetupStatus() as never,
    settings: { ...useMinerStore.getState().settings, password: null, apiToken: null },
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  clearCredentials();
});

describe('AuthGate daemon-backed session check', () => {
  it('renders the daemon rejection instead of comparing passwords locally', async () => {
    const createSession = vi
      .spyOn(api, 'createSession')
      .mockRejectedValue(new ApiError(401, 'owner password rejected by daemon'));

    renderGate();

    fireEvent.change(screen.getByLabelText(/password/i), { target: { value: 'wrong-password' } });
    fireEvent.click(screen.getByRole('button', { name: /authenticate/i }));

    expect(await screen.findByText('owner password rejected by daemon')).toBeTruthy();
    expect(screen.queryByText('Advanced content')).toBeNull();
    expect(useMinerStore.getState().authenticated).toBe(false);
    expect(createSession).toHaveBeenCalledWith('wrong-password');
  });

  it('unlocks only after the daemon returns a session token', async () => {
    vi.spyOn(api, 'createSession').mockResolvedValue('session-token');

    renderGate();

    fireEvent.change(screen.getByLabelText(/password/i), { target: { value: 'correct-password' } });
    fireEvent.click(screen.getByRole('button', { name: /authenticate/i }));

    await waitFor(() => expect(screen.getByText('Advanced content')).toBeTruthy());
    expect(useMinerStore.getState().authenticated).toBe(true);
  });
});
