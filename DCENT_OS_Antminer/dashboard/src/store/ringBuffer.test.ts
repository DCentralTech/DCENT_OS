import { describe, it, expect } from 'vitest';
import { RingBuffer } from './ringBuffer';

// P3-6: the history/log buffers are fixed-size circular buffers. These pin the
// load-bearing invariant — pushing more than `capacity` keeps exactly the last
// `capacity` items, in insertion order — plus the under/at-capacity edges and
// the fresh-reference snapshot semantics Zustand relies on.

describe('RingBuffer', () => {
  it('keeps everything in order while under capacity', () => {
    const rb = new RingBuffer<number>(5);
    rb.push(1);
    rb.push(2);
    rb.push(3);
    expect(rb.size).toBe(3);
    expect(rb.toArray()).toEqual([1, 2, 3]);
  });

  it('holds exactly capacity items at capacity', () => {
    const rb = new RingBuffer<number>(3);
    rb.push(10);
    rb.push(20);
    rb.push(30);
    expect(rb.size).toBe(3);
    expect(rb.toArray()).toEqual([10, 20, 30]);
  });

  it('keeps the last `capacity` items in order when pushed past capacity', () => {
    const cap = 40;
    const n = 1000; // n > cap
    const rb = new RingBuffer<number>(cap);
    for (let i = 0; i < n; i += 1) rb.push(i);

    expect(rb.size).toBe(cap);
    const out = rb.toArray();
    expect(out).toHaveLength(cap);
    // Last `cap` values, oldest→newest: [n-cap, ..., n-1].
    expect(out).toEqual(Array.from({ length: cap }, (_, i) => n - cap + i));
    expect(out[0]).toBe(n - cap);
    expect(out[out.length - 1]).toBe(n - 1);
  });

  it('overwrites the oldest exactly once past capacity (order preserved)', () => {
    const rb = new RingBuffer<string>(3);
    ['a', 'b', 'c', 'd', 'e'].forEach((v) => rb.push(v));
    expect(rb.toArray()).toEqual(['c', 'd', 'e']);
  });

  it('returns a fresh array reference from each toArray() call', () => {
    const rb = new RingBuffer<number>(4);
    rb.push(1);
    rb.push(2);
    const a = rb.toArray();
    const b = rb.toArray();
    expect(a).toEqual(b);
    expect(a).not.toBe(b); // new reference → Zustand/React re-render fires
  });

  it('clear() empties the buffer without breaking subsequent pushes', () => {
    const rb = new RingBuffer<number>(3);
    rb.push(1);
    rb.push(2);
    rb.clear();
    expect(rb.size).toBe(0);
    expect(rb.toArray()).toEqual([]);
    rb.push(9);
    rb.push(8);
    rb.push(7);
    rb.push(6); // wraps after clear
    expect(rb.toArray()).toEqual([8, 7, 6]);
  });

  it('rejects a non-positive or non-integer capacity', () => {
    expect(() => new RingBuffer<number>(0)).toThrow();
    expect(() => new RingBuffer<number>(-1)).toThrow();
    expect(() => new RingBuffer<number>(2.5)).toThrow();
  });
});
