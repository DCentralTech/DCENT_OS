import { describe, it, expect } from 'vitest';
import { picToVoltage, voltageToPic, isS9PicDacBoard } from './format';

// P3-4: the PIC DAC <-> voltage math is the PIC16F1704 transfer function on the
// S9 (am1 / BM1387) control board ONLY. format.ts ships in the single fleet-wide
// bundle, so it must NOT auto-apply the S9 formula on S17/S19/S21/Amlogic boards
// (dsPIC / NoPic) — those would get a fabricated voltage. When the caller knows
// the board and it is not S9, the helpers return null ("n/a") instead.

describe('isS9PicDacBoard', () => {
  it('recognizes S9 / BM1387 / am1 targets', () => {
    for (const b of [
      's9', 'S9', 's9i', 'Antminer S9', 'Antminer S9 Pro',
      'bm1387', 'BM1387',
      'am1-s9', 'zynq-am1-bm1387',
    ]) {
      expect(isS9PicDacBoard(b)).toBe(true);
    }
  });

  it('rejects non-S9 boards (dsPIC / NoPic platforms)', () => {
    for (const b of [
      'am2-s19jpro', 'am1-s17', 's17', 's19', 's21',
      'bm1362', 'bm1368', 'bm1397', 'bm1398',
      'amlogic', 'Antminer S19j Pro', 'Antminer S21',
    ]) {
      expect(isS9PicDacBoard(b)).toBe(false);
    }
  });

  it('rejects null / undefined / empty (unknown board)', () => {
    expect(isS9PicDacBoard(null)).toBe(false);
    expect(isS9PicDacBoard(undefined)).toBe(false);
    expect(isS9PicDacBoard('')).toBe(false);
  });
});

describe('picToVoltage / voltageToPic — S9 path unchanged (no board arg)', () => {
  it('pins the documented S9 PIC16F1704 DAC points', () => {
    expect(picToVoltage(0)).toBeCloseTo(9.438, 3);   // max
    expect(picToVoltage(6)).toBeCloseTo(9.403, 3);   // init
    expect(picToVoltage(57)).toBeCloseTo(9.103, 3);  // operating
    expect(picToVoltage(92)).toBeCloseTo(8.898, 3);  // low
    expect(picToVoltage(255)).toBeCloseTo(7.9415, 4); // min
  });

  it('inverts back to the PIC code', () => {
    expect(voltageToPic(9.4)).toBe(6);
    expect(voltageToPic(picToVoltage(120))).toBe(120);
    expect(voltageToPic(picToVoltage(57))).toBe(57);
  });

  it('an explicit S9 board produces the identical S9 math', () => {
    for (const b of ['s9', 'BM1387', 'am1-s9', 'zynq-am1-bm1387']) {
      expect(picToVoltage(57, b)).toBeCloseTo(picToVoltage(57), 9);
      expect(voltageToPic(9.1, b)).toBe(voltageToPic(9.1));
    }
  });
});

describe('picToVoltage / voltageToPic — non-S9 boards do NOT get S9 math', () => {
  it('returns null (n/a) for every non-S9 board instead of the wrong S9 voltage', () => {
    for (const b of ['am2-s19jpro', 'bm1362', 'bm1368', 'amlogic', 's21', 'am1-s17']) {
      const v = picToVoltage(57, b);
      expect(v).toBeNull();
      // and crucially NOT the S9 conversion of the same PIC code
      expect(v).not.toBe(picToVoltage(57));
    }
  });

  it('voltageToPic also refuses non-S9 boards', () => {
    for (const b of ['am2-s19jpro', 'bm1362', 'amlogic', 's19']) {
      const p = voltageToPic(9.1, b);
      expect(p).toBeNull();
      expect(p).not.toBe(voltageToPic(9.1));
    }
  });

  it('an explicitly-unknown (null) board is honest, not S9', () => {
    expect(picToVoltage(57, null)).toBeNull();
    expect(voltageToPic(9.1, null)).toBeNull();
  });
});
