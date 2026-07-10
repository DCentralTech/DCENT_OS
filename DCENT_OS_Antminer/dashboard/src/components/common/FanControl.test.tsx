// @vitest-environment jsdom
//
// HIGH (keyboard dead-control + home-quiet clamp) regression for FanControl.
//
// The custom-mode ("Home Cap") PWM slider is a native range input. Mouse/touch
// users commit their value to the daemon on release (onMouseUp/onTouchEnd →
// onPwmChange → useFanControl → api.setFan). A keyboard-only user can move the
// thumb with arrow keys (onChange updates the displayed value) but, before this
// fix, the change NEVER committed — the safety/comfort control silently did
// nothing for keyboard users. The fix adds an onKeyUp commit using the EXACT
// same ref the mouse/touch paths use, so an arrow-key edit is debounced + sent
// by useFanControl identically to a drag.
//
// This file pins:
//   1. the custom slider only appears in Home Cap mode,
//   2. a keyboard edit commits on keyUp with the new PWM (and matches mouseUp),
//   3. the native control is physically bounded to the 10–30 home cap,
//   4. the home-quiet clamp/classification (clampHomePwm / pwmZone).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';

import { FanControl, clampHomePwm, normalizeFanMode, pwmZone } from './FanControl';

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('FanControl — keyboard commit + home-quiet clamp', () => {
  it('does not highlight a preset when the daemon does not report fan mode', () => {
    render(<FanControl currentPwm={20} currentRpm={2880} modeSource="unknown" onPwmChange={vi.fn()} />);

    for (const button of screen.getAllByRole('button')) {
      expect(button.classList.contains('active')).toBe(false);
      expect(button.getAttribute('aria-pressed')).toBe('false');
    }
    expect(screen.getByText(/fan mode not reported by daemon/i)).toBeTruthy();
  });

  it('keeps the active highlight on the daemon-reported mode when a click is not reconciled', () => {
    const onModeChange = vi.fn();
    render(
      <FanControl
        currentPwm={20}
        currentRpm={2880}
        activeMode="quiet"
        modeSource="daemon"
        onModeChange={onModeChange}
      />,
    );

    const quiet = screen.getByRole('button', { name: /home idle/i });
    const custom = screen.getByRole('button', { name: /home cap/i });
    expect(quiet.classList.contains('active')).toBe(true);

    fireEvent.click(custom);

    expect(onModeChange).toHaveBeenCalledWith('custom');
    expect(quiet.classList.contains('active')).toBe(true);
    expect(custom.classList.contains('active')).toBe(false);
    expect(custom.classList.contains('is-applying')).toBe(true);
  });

  it('does not render the custom PWM slider until Home Cap mode is selected', () => {
    render(<FanControl currentPwm={20} currentRpm={2880} onPwmChange={vi.fn()} />);
    // No daemon-confirmed custom mode, so no manual slider.
    expect(screen.queryByRole('slider')).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: /home cap/i }));
    expect(screen.getByRole('slider')).toBeTruthy();
  });

  it('commits a keyboard arrow-key PWM edit on keyUp, matching the mouse-commit path', () => {
    const onPwmChange = vi.fn();
    render(<FanControl currentPwm={20} currentRpm={2880} onPwmChange={onPwmChange} />);

    fireEvent.click(screen.getByRole('button', { name: /home cap/i }));
    const slider = screen.getByRole('slider');

    // Arrow-key edit: onChange updates the displayed value + the commit ref,
    // but (like a drag in progress) does NOT reach the daemon yet.
    fireEvent.change(slider, { target: { value: '25' } });
    expect(onPwmChange).not.toHaveBeenCalled();

    // Release: keyUp commits the SAME value the mouse/touch release would.
    fireEvent.keyUp(slider, { key: 'ArrowUp' });
    expect(onPwmChange).toHaveBeenCalledTimes(1);
    expect(onPwmChange).toHaveBeenCalledWith(25);

    // Parity check: the existing mouseUp path commits identically.
    fireEvent.change(slider, { target: { value: '15' } });
    fireEvent.mouseUp(slider);
    expect(onPwmChange).toHaveBeenLastCalledWith(15);
  });

  it('bounds the custom PWM slider to the 10–30 home cap (native min/max)', () => {
    render(<FanControl currentPwm={20} currentRpm={2880} onPwmChange={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: /home cap/i }));
    const slider = screen.getByRole('slider');

    // The manual request range is the home cap — never a louder request.
    expect(slider.getAttribute('min')).toBe('10');
    expect(slider.getAttribute('max')).toBe('30');
  });

  it('classifies and clamps a manual PWM request to the 10–30 home cap', () => {
    // A request inside the cap stays "home"; anything above it is a louder
    // override/emergency the dashboard never exposes as a manual request.
    expect(pwmZone(20).zone).toBe('home');
    expect(pwmZone(31).zone).toBe('override');
    expect(pwmZone(70).zone).toBe('emergency');

    // clampHomePwm bounds any value into [10, 30] (and floors junk to 10).
    expect(clampHomePwm(50)).toBe(30);
    expect(clampHomePwm(5)).toBe(10);
    expect(clampHomePwm(20)).toBe(20);
    expect(clampHomePwm(Number.NaN)).toBe(10);
  });

  it('normalizes known daemon fan-mode strings and rejects unknown strings', () => {
    expect(normalizeFanMode('quiet')).toBe('quiet');
    expect(normalizeFanMode('auto')).toBe('balanced');
    expect(normalizeFanMode('cooling_override')).toBe('performance');
    expect(normalizeFanMode('manual')).toBe('custom');
    expect(normalizeFanMode('mystery-mode')).toBeNull();
    expect(normalizeFanMode(null)).toBeNull();
  });
});
