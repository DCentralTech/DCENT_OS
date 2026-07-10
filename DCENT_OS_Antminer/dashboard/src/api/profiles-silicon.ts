// Silicon profile-import API client (W8-D backend, wave 8)
//
// Endpoints registered in dcentrald-api (rest.rs build_router merge):
//   GET    /api/profiles/silicon                   list summaries
//   GET    /api/profiles/silicon/:id               full bundle
//   POST   /api/profiles/silicon/import            multipart upload (`profile` field)
//   POST   /api/profiles/silicon/import-json       JSON-body twin {bundle}
//   PUT    /api/profiles/silicon/active            {model, hashboard, profile_id}
//   DELETE /api/profiles/silicon/:id               LiveConfirmed -> 403
//   POST   /api/profiles/silicon/reload            re-read disk
//
// W8-D path namespacing decision: silicon profiles live under
// /api/profiles/silicon/* to avoid colliding with the existing
// autotuner saved-profile /api/profiles endpoint.

import { apiFetch } from './client';

export type SiliconSourceClass =
  | 'live_confirmed'
  | 'operator_confirmed'
  | 'vendor_extracted'
  | 'baked';

export type SiliconChip =
  | 'bm1387'
  | 'bm1397'
  | 'bm1398'
  | 'bm1362'
  | 'bm1366'
  | 'bm1368'
  | 'bm1370'
  | 'bm1360'
  | 'bm1491'
  | 'bm1485'
  | (string & {});

export interface SiliconProfileSummary {
  id: string;
  miner_model: string;
  hashboard: string;
  chip: SiliconChip;
  source_class: SiliconSourceClass;
  preset_count: number;
}

export interface SiliconPresetRow {
  step: number;
  freq_mhz: number;
  voltage_v: number;
  // optional fields the UI may render — server returns flexible shape
  notes?: string | null;
}

export interface SiliconProfileBundle {
  id?: string;
  schema_version: 1;
  miner_model: string;
  hashboard: string;
  chip: SiliconChip;
  source_class: SiliconSourceClass;
  presets: SiliconPresetRow[];
  metadata?: {
    secure_boot_set_seen?: boolean;
    hashcore_root_hash_seen?: boolean;
    captured_at?: string;
    notes?: string;
    [k: string]: unknown;
  } | null;
}

export interface SiliconImportResponse {
  id: string;
  path: string;
  loaded: number;
}

export interface SiliconReloadResponse {
  loaded: number;
  skipped: number;
  errors: string[];
}

export interface SiliconActiveResponse {
  status: string;
  model: string;
  hashboard: string;
  profile_id: string;
  note?: string;
  // CC-1: the backend reports whether the activation reached the RUNNING miner
  // (`applied_runtime`) vs was only persisted for the next autotuner cycle
  // (`status` = ack_timeout/closed/unavailable/closed_before_ack), so the UI can
  // say "applied live" vs "applies next cycle" instead of an ambiguous ack.
  runtime?: {
    applied_runtime: boolean;
    status: string;
    message?: string;
  };
}

async function unpackError(res: Response): Promise<never> {
  let body: unknown = null;
  try {
    body = await res.json();
  } catch {
    // ignore
  }
  const reason = (body && typeof body === 'object' && 'error' in body && typeof (body as { error: unknown }).error === 'string')
    ? (body as { error: string }).error
    : `HTTP ${res.status}`;
  const err = new Error(reason);
  (err as Error & { status?: number }).status = res.status;
  (err as Error & { body?: unknown }).body = body;
  throw err;
}

export const siliconProfilesApi = {
  list: async (): Promise<SiliconProfileSummary[]> => {
    const res = await apiFetch('/api/profiles/silicon');
    if (!res.ok) await unpackError(res);
    return res.json();
  },

  get: async (id: string): Promise<SiliconProfileBundle> => {
    const res = await apiFetch(`/api/profiles/silicon/${encodeURIComponent(id)}`);
    if (!res.ok) await unpackError(res);
    return res.json();
  },

  // Multipart upload — same wire shape `dcent import` would produce.
  importMultipart: async (file: File): Promise<SiliconImportResponse> => {
    const fd = new FormData();
    fd.append('profile', file, file.name);
    const res = await apiFetch('/api/profiles/silicon/import', {
      method: 'POST',
      body: fd,
    });
    if (!res.ok) await unpackError(res);
    return res.json();
  },

  // JSON-body twin — used after operator overrides source_class /
  // chip / hashboard during the wizard (we patch the bundle in
  // memory and POST the whole thing).
  importJson: async (bundle: SiliconProfileBundle): Promise<SiliconImportResponse> => {
    const res = await apiFetch('/api/profiles/silicon/import-json', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ bundle }),
    });
    if (!res.ok) await unpackError(res);
    return res.json();
  },

  setActive: async (model: string, hashboard: string, profileId: string): Promise<SiliconActiveResponse> => {
    const res = await apiFetch('/api/profiles/silicon/active', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model, hashboard, profile_id: profileId }),
    });
    if (!res.ok) await unpackError(res);
    return res.json();
  },

  remove: async (id: string): Promise<{ status: string }> => {
    const res = await apiFetch(`/api/profiles/silicon/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    });
    if (res.status === 403) {
      // LiveConfirmed -> immutable. Surface the canonical error.
      const err = new Error('live_confirmed_immutable');
      (err as Error & { status?: number }).status = 403;
      throw err;
    }
    if (!res.ok) await unpackError(res);
    return res.json();
  },

  reload: async (): Promise<SiliconReloadResponse> => {
    const res = await apiFetch('/api/profiles/silicon/reload', { method: 'POST' });
    if (!res.ok) await unpackError(res);
    return res.json();
  },
};
