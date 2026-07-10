export interface BestDifficultyRecord {
  value: number;
  at: number;
}

export interface BestDifficultyStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export const BEST_DIFFICULTY_STORAGE_KEY = 'dcentos_best_difficulty';

function getDefaultStorage(): BestDifficultyStorage | null {
  try {
    return typeof window !== 'undefined' ? window.localStorage : null;
  } catch {
    return null;
  }
}

export function isFiniteDifficulty(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value > 0;
}

function isFiniteTimestamp(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

export class BestDifficultyStore {
  constructor(
    private readonly storage: BestDifficultyStorage | null = getDefaultStorage(),
    private readonly key = BEST_DIFFICULTY_STORAGE_KEY,
  ) {}

  read(): BestDifficultyRecord | null {
    if (!this.storage) return null;
    try {
      const raw = this.storage.getItem(this.key);
      if (!raw) return null;
      const parsed = JSON.parse(raw) as Partial<BestDifficultyRecord>;
      if (!isFiniteDifficulty(parsed.value) || !isFiniteTimestamp(parsed.at)) {
        return null;
      }
      return { value: parsed.value, at: parsed.at };
    } catch {
      return null;
    }
  }

  recordIfBest(difficulty: unknown, at: number): BestDifficultyRecord | null {
    if (!isFiniteDifficulty(difficulty)) return null;
    const previous = this.read();
    if (previous && previous.value >= difficulty) {
      return null;
    }

    const next = { value: difficulty, at };
    if (this.storage) {
      try {
        this.storage.setItem(this.key, JSON.stringify(next));
      } catch {
        return null;
      }
    }
    return next;
  }
}
