// Fixed-size circular buffer (P3-6).
//
// The dashboard is an always-open 512 MB kiosk surface. The old history/log
// helpers rebuilt the WHOLE array on every telemetry tick (`[...arr, x]` then
// `.slice(-cap)` = two transient allocations of ~cap each, plus an N+1 spill
// array), churning the GC for the life of the session.
//
// This buffer allocates its backing store ONCE at construction and never grows.
// `push()` overwrites the oldest slot in place (O(1), zero allocation) once the
// buffer is full. The only allocation we keep is `toArray()`, which materialises
// an ordered oldest→newest snapshot — required so Zustand sees a fresh reference
// and so consumers keep receiving a plain array (the existing data contract is
// unchanged; nothing downstream sees the buffer itself).

export class RingBuffer<T> {
  private readonly buf: (T | undefined)[];
  private start = 0; // index of the oldest element
  private count = 0; // number of valid elements (0..capacity)
  readonly capacity: number;

  constructor(capacity: number) {
    if (!Number.isInteger(capacity) || capacity <= 0) {
      throw new Error(`RingBuffer capacity must be a positive integer, got ${String(capacity)}`);
    }
    this.capacity = capacity;
    this.buf = new Array<T | undefined>(capacity);
  }

  /** Number of valid elements currently held (never exceeds capacity). */
  get size(): number {
    return this.count;
  }

  /** Append one item in place, overwriting the oldest element when full. */
  push(item: T): void {
    const end = (this.start + this.count) % this.capacity;
    this.buf[end] = item;
    if (this.count < this.capacity) {
      this.count += 1;
    } else {
      // Full: the slot we just wrote was the oldest; advance start so it
      // becomes the newest and the next-oldest is exposed.
      this.start = (this.start + 1) % this.capacity;
    }
  }

  /**
   * Ordered oldest→newest snapshot. Returns a fresh array on every call so the
   * caller can hand it to Zustand / React as a new reference.
   */
  toArray(): T[] {
    const out = new Array<T>(this.count);
    for (let i = 0; i < this.count; i += 1) {
      out[i] = this.buf[(this.start + i) % this.capacity] as T;
    }
    return out;
  }

  /** Drop all elements (backing store is retained — no reallocation). */
  clear(): void {
    this.start = 0;
    this.count = 0;
  }
}
