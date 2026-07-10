import type { FirmwareUploadResponse } from '../api/types';

const STAGED_UPGRADE_KEY = 'dcentos-staged-upgrade';

export interface StagedUpgradeRecord {
  filename?: string;
  stagedPath: string;
  bytesWritten?: number;
}

export function loadStagedUpgrade(): StagedUpgradeRecord | null {
  try {
    const raw = localStorage.getItem(STAGED_UPGRADE_KEY);
    if (!raw) {
      return null;
    }

    const parsed = JSON.parse(raw) as Partial<StagedUpgradeRecord> | null;
    if (!parsed?.stagedPath || typeof parsed.stagedPath !== 'string') {
      return null;
    }

    return {
      stagedPath: parsed.stagedPath,
      filename: typeof parsed.filename === 'string' ? parsed.filename : undefined,
      bytesWritten: typeof parsed.bytesWritten === 'number' ? parsed.bytesWritten : undefined,
    };
  } catch {
    return null;
  }
}

export function saveStagedUpgrade(response: FirmwareUploadResponse) {
  if (!response.staged_path) {
    return;
  }

  const record: StagedUpgradeRecord = {
    stagedPath: response.staged_path,
    filename: response.filename,
    bytesWritten: response.bytes_written,
  };
  localStorage.setItem(STAGED_UPGRADE_KEY, JSON.stringify(record));
}

export function clearStagedUpgrade() {
  localStorage.removeItem(STAGED_UPGRADE_KEY);
}
