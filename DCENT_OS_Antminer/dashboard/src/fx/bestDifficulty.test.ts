import { describe, expect, it } from 'vitest';
import { BestDifficultyStore, BEST_DIFFICULTY_STORAGE_KEY, isFiniteDifficulty } from './bestDifficulty';

class MemoryStorage {
  values = new Map<string, string>();
  getItem(key: string) {
    return this.values.get(key) ?? null;
  }
  setItem(key: string, value: string) {
    this.values.set(key, value);
  }
}

describe('BestDifficultyStore', () => {
  it('records only finite locally achieved difficulty values', () => {
    expect(isFiniteDifficulty(1024)).toBe(true);
    expect(isFiniteDifficulty(0)).toBe(false);
    expect(isFiniteDifficulty(Number.NaN)).toBe(false);
    expect(isFiniteDifficulty(null)).toBe(false);
  });

  it('persists a new record only when achieved difficulty improves', () => {
    const storage = new MemoryStorage();
    const store = new BestDifficultyStore(storage);

    expect(store.recordIfBest(512, 1000)).toEqual({ value: 512, at: 1000 });
    expect(store.recordIfBest(256, 2000)).toBeNull();
    expect(store.recordIfBest(2048, 3000)).toEqual({ value: 2048, at: 3000 });
    expect(JSON.parse(storage.getItem(BEST_DIFFICULTY_STORAGE_KEY) ?? '{}')).toEqual({ value: 2048, at: 3000 });
  });
});
