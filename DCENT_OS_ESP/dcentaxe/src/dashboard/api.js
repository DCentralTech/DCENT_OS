/* ──────────────────────────────────────────────────────────────────────────
   DCENT_axe — API layer
   ──────────────────────────────────────────────────────────────────────────
   A thin abstraction over the firmware HTTP endpoints. In Phase 1c this
   is a wrapper that delegates to the legacy inline helpers (tfetch,
   authHeaders, etc.) so existing code keeps working. Future phases (2-5)
   will migrate the legacy helpers IN HERE wholesale.

   Public namespace (exposed on window.daxAPI):
     daxAPI.fetch(url, opts)       Timed fetch with abort, returns Promise<Response>
     daxAPI.authHeaders(extra)     Inject CSRF + bearer auth
     daxAPI.poll.start()           Begin the polling loop (delegates to startPoll)
     daxAPI.poll.stop()            Stop the loop (best effort)
     daxAPI.systemInfo()           One-shot GET /api/system/info → JSON
     daxAPI.coredump.probe()       GET /api/system/coredump → {present,size,…}
     daxAPI.coredump.delete()      DELETE /api/system/coredump
     daxAPI.coredump.downloadUrl() URL string for Save-As link
     daxAPI.mining.start()         POST /api/mining/start
     daxAPI.mining.stop()          POST /api/mining/stop
   ──────────────────────────────────────────────────────────────────────────
   The wrapper functions resolve `tfetch` / `authHeaders` at CALL time
   (closure semantics) — so they work even though the legacy implementations
   are defined later in the inline <script> block. */

(function () {
  'use strict';

  const _resolve = (name) => {
    const v = window[name];
    if (typeof v !== 'function') {
      console.warn('[daxAPI] legacy helper not yet defined:', name);
      return null;
    }
    return v;
  };

  const fetchTimed = (url, opts) => {
    const tf = _resolve('tfetch');
    if (tf) return tf(url, opts || {});
    // Fallback: plain fetch with 8 s abort.
    const c = new AbortController();
    const t = setTimeout(() => c.abort(), 8000);
    const o = Object.assign({}, opts || {}, { signal: c.signal });
    return fetch(url, o).finally(() => clearTimeout(t));
  };

  const authHeaders = (extra) => {
    const ah = _resolve('authHeaders');
    if (ah) return ah(extra || {});
    const h = Object.assign({}, extra || {});
    h['X-Requested-With'] = 'XMLHttpRequest';
    return h;
  };

  const _json = (r) => {
    if (!r.ok && r.status !== 200) throw new Error('HTTP ' + r.status);
    return r.json();
  };

  // ── System info ───────────────────────────────────────────────────────
  function systemInfo() {
    return fetchTimed('/api/system/info', { headers: authHeaders() }).then(_json);
  }

  // ── Coredump ──────────────────────────────────────────────────────────
  const coredump = {
    probe()  { return fetchTimed('/api/system/coredump', { headers: authHeaders() }).then(_json); },
    delete() { return fetchTimed('/api/system/coredump', { method: 'DELETE', headers: authHeaders() }).then(_json); },
    downloadUrl() { return '/api/system/coredump?download=1'; },
  };

  // ── Mining control ────────────────────────────────────────────────────
  const mining = {
    start() {
      return fetchTimed('/api/mining/start', {
        method: 'POST',
        headers: Object.assign(authHeaders(), { 'Content-Type': 'application/json' }),
        body: '{}',
      }).then(_json);
    },
    stop() {
      return fetchTimed('/api/mining/stop', {
        method: 'POST',
        headers: Object.assign(authHeaders(), { 'Content-Type': 'application/json' }),
        body: '{}',
      }).then(_json);
    },
    block() {
      return fetchTimed('/api/mining/block', { headers: authHeaders() }).then(_json);
    },
  };

  // ── Polling loop control (delegates to legacy startPoll) ──────────────
  // The CANONICAL polling cadence lives in dashboard.rs:startPoll() —
  // visibility-aware (15s visible / 30s hidden) plus a 10s block-hero
  // timer. In real firmware startPoll always resolves, so the fallback
  // below NEVER runs. It is a DEGRADED last-resort kept only for the case
  // where the inline shell failed to load; keep it a behavioural superset
  // of startPoll so a future refactor that leans on daxAPI.poll does not
  // silently drop the hidden-tab backoff or the block-hero updates
  // (review DASH-6).
  let _pollTimer = null;
  let _blockTimer = null;
  const poll = {
    start() {
      const sp = _resolve('startPoll');
      if (sp) return sp();
      // ── Degraded fallback (inline startPoll absent) ──
      const tick = () => {
        systemInfo()
          .then(d => { if (window.state && typeof window.state.set === 'function') window.state.set('info', d); })
          .catch(e => console.warn('[daxAPI.poll] tick failed', e));
      };
      // Visibility-aware rate, mirroring dashboard.rs:startPoll. Phase Q:
      // /api/system/info builds a ~25 KB JSON payload; rapid polling shreds
      // the heap with intermediate Value-tree allocations, so back off to
      // 30s when the tab is hidden (15s when visible — snappy for a human).
      const arm = () => {
        if (_pollTimer) clearInterval(_pollTimer);
        const rate = (typeof document !== 'undefined' && document.hidden) ? 30000 : 15000;
        _pollTimer = setInterval(tick, rate);
        // Block-hero refresh at 10s, matching the canonical cadence — only
        // if the inline helper exists (it lives in block-tile.js).
        if (_blockTimer) clearInterval(_blockTimer);
        const pbh = _resolve('pollBlockHero');
        if (pbh) _blockTimer = setInterval(pbh, 10000);
      };
      tick();
      arm();
      try { document.addEventListener('visibilitychange', arm); } catch (e) { /* no DOM */ }
    },
    stop() {
      if (_pollTimer) { clearInterval(_pollTimer); _pollTimer = null; }
      if (_blockTimer) { clearInterval(_blockTimer); _blockTimer = null; }
    },
  };

  window.daxAPI = {
    fetch: fetchTimed,
    authHeaders,
    systemInfo,
    coredump,
    mining,
    poll,
  };
})();
