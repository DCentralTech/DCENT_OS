/* ──────────────────────────────────────────────────────────────────────────
   DCENT_axe — Tiny Reactive Component Framework
   ──────────────────────────────────────────────────────────────────────────
   A ~200-LOC inline framework for building modular dashboard components
   without React, Vue, or a node toolchain. Designed for embedded firmware
   delivery via include_str! macros.

   Pattern:
     defineComponent('asic-chips', renderFn, { boundKeys: ['chips','asicModel'] });
     // renderFn(props) returns HTML string
     // Every <div data-component="asic-chips"> auto-mounts on first state.set
     // Re-renders only when one of boundKeys changes (or on full refresh)

   Public API (exposed on window):
     defineComponent(name, render, opts)  Register a component
     state.get(key)                       Read current state
     state.set(key, val)                  Write + notify subscribers
     state.subscribe(key, fn)             Listen for changes
     mount()                              Hydrate all [data-component] in DOM
     refreshAll()                         Force re-render every mounted component
   ──────────────────────────────────────────────────────────────────────────
   NOTE: the modular window.flashEl helper was removed (review DASH-4). The
   inline dashboard.rs flashEl(id,newVal) is the single source of truth for
   the value-change pulse; exposing a same-named window global here only
   created a shadow conflict (the inline hoisted declaration always won) and
   shipped a never-triggered .dax-flash keyframe. Do NOT re-add window.flashEl
   without first removing the inline one and migrating its 3 callers.
   ────────────────────────────────────────────────────────────────────────── */

(function () {
  'use strict';

  // ── Canonical glossary string-table (TERM-7) ──────────────────────────
  // axe's compact inline equivalent of the OS `glossary.ts` STRUCTURAL MODEL
  // (terminology-lexicon §0 / §11). Same KEYS + same canonical STRINGS as the
  // OS registry, but a flat literal table instead of a typed record set — the
  // physically-forbidden-to-share asymmetry (OS hosts ~70 typed GlossaryEntry
  // records; axe hosts this string mirror). Each entry tags its cross-firmware
  // scope so the two emissions can never silently drift:
  //   shared      — both firmwares render this exact word
  //   os-only     — OS-only-industrial (listed here only for source-consistency)
  //   axe-only    — axe-only-solo flavour
  // This object lives at framework.js top-level (script-src #1, loaded before
  // every component IIFE and before the inline dashboard.rs JS) and is exposed
  // as `window.GLOSSARY`, so consumers resolve labels with a per-key fallback
  // (`GLOSSARY[k] || literal`) and a missing key can never blank the UI.
  // Strings are byte-identical to the contract spellings (terminology-lexicon
  // §3.1/§4.x/§5.3/§6.1) so the S1 token+label drift-validators stay green.
  // Handler-free: NO register_static, MAX_URI_HANDLERS budget untouched (71/96).
  const GLOSSARY = {
    // — Windows / headline (TERM-5 §5.3) —
    window_headline_10m:   { t: 'shared', label: 'Hashrate · 10m', caption: '10m Average' },
    // — Units (TERM-3) —
    efficiency_jth:        { t: 'shared', label: 'Efficiency', short: 'Eff', unit: 'J/TH', help: 'Lower J/TH = more efficient' },
    unit_power:            { t: 'shared', label: 'Power', unit: 'W' },
    btu_per_hour:          { t: 'shared', label: 'Heat Output', unit: 'BTU/h' },
    // — Mining truth-ladder (TERM-2 §2.1) —
    state_telemetry_pending: { t: 'shared', label: 'Telemetry pending', pill: 'PENDING' },
    state_mining:          { t: 'shared', label: 'Mining',  pill: 'MINING' },
    state_ready:           { t: 'shared', label: 'Ready',   pill: 'READY' },
    state_standby:         { t: 'shared', label: 'Standby', pill: 'STANDBY' },
    state_stopped:         { t: 'shared', label: 'Stopped', pill: 'STOPPED' },
    // — No-data / stale lexicon (TERM-6 §6.1) —
    telemetry_stale:       { t: 'shared', label: 'Telemetry stale' },
    telemetry_absent:      { t: 'shared', label: 'Offline' },
    empty_value:           { t: 'shared', label: '--' },
    // — Pool ladder (TERM-2 §2.3) —
    pool_connecting:       { t: 'shared', label: 'Connecting', pill: 'CONNECTING' },
    pool_connected:        { t: 'shared', label: 'Connected',  pill: 'CONNECTED' },
    // — Share / difficulty vocabulary (TERM-4 §4.2/§4.4) —
    share_accepted:        { t: 'shared', label: 'pool accepted', short: 'Pool OK' },
    share_rejected:        { t: 'shared', label: 'pool rejected' },
    best_diff_session:     { t: 'shared', label: 'Best Diff (session)', short: 'Best Diff · Session', tiny: 'Best' },
    best_diff_all_time:    { t: 'shared', label: 'Best Ever (all-time)', short: 'All-time' },
    pool_target_difficulty:{ t: 'shared', label: 'Pool Target Difficulty', short: 'Pool' },
    achieved_difficulty:   { t: 'shared', label: 'Achieved Difficulty' },
    // — Disclosure-run / scoped-word register (TERM-4? §9; UINAV-4) —
    // 'Advanced' is a SHARED WORD with axe-only scope (inline <details>
    // disclosure) vs OS (the Hacker tool-tier). axe renders the depth-rung
    // naming Basic/Standard/Advanced as inline disclosure — NO OS tier machinery.
    disclosure_basic:      { t: 'shared', label: 'Basic' },
    disclosure_standard:   { t: 'shared', label: 'Standard' },
    advanced_axe_disclosure: { t: 'axe-only', label: 'Advanced' },
  };

  /**
   * Resolve a canonical label by glossary key with a fail-safe fallback.
   * `gloss(key)` → the .label; `gloss(key, 'short')` → a named variant
   * (short/caption/unit/help/pill/tiny), falling back to .label then ''.
   * A typo or missing key returns '' rather than throwing — consumers
   * additionally pass `|| literal` so the UI never blanks.
   */
  function gloss(key, field) {
    const e = GLOSSARY[key];
    if (!e) return '';
    if (field && e[field] != null) return e[field];
    return e.label != null ? e.label : '';
  }

  // ── State bus ─────────────────────────────────────────────────────────
  const _state = {};
  const _subs = {};

  const state = {
    get(k) { return _state[k]; },
    set(k, v) {
      const prev = _state[k];
      _state[k] = v;
      // Shallow-equal short-circuit for primitives only — objects always notify.
      if (prev !== v) (_subs[k] || []).forEach(fn => { try { fn(v, prev); } catch (e) { console.error('[framework] subscriber error', k, e); } });
    },
    subscribe(k, fn) {
      (_subs[k] = _subs[k] || []).push(fn);
      // Return an unsubscribe handle.
      return () => { const arr = _subs[k] || []; const i = arr.indexOf(fn); if (i >= 0) arr.splice(i, 1); };
    },
    /** Read multiple keys as one snapshot object — convenient for renderers. */
    snapshot(keys) {
      const out = {};
      for (const k of keys) out[k] = _state[k];
      return out;
    },
  };

  // ── Component registry ────────────────────────────────────────────────
  const _components = {};
  const _mounted = new Map(); // host element → { name, opts, unsubs[] }

  /**
   * Register a component renderer.
   * @param {string} name              tag name (used in data-component="name")
   * @param {(props: object) => string} render  pure renderer returning HTML string
   * @param {object} [opts]
   * @param {string[]} [opts.boundKeys]   state keys to subscribe to
   * @param {(host: Element) => void} [opts.afterRender]  called after each render
   *                                  (use to attach event listeners to children)
   */
  function defineComponent(name, render, opts) {
    _components[name] = { render, opts: opts || {} };
    /* Auto-hydrate any matching host already in the DOM. Idempotent — _hydrate
       skips elements already in _mounted. This protects against the case where
       mount() ran before the per-component IIFE registered (script-load
       ordering). */
    try {
      var hosts = document.querySelectorAll('[data-component="' + name + '"]');
      for (var i = 0; i < hosts.length; i++) {
        if (!_mounted.has(hosts[i])) _hydrate(hosts[i]);
      }
    } catch (e) { /* DOM not ready yet — mount() will catch up later */ }
  }

  function _hydrate(host) {
    const name = host.getAttribute('data-component');
    const def = _components[name];
    if (!def) {
      console.warn('[framework] no component registered for', name);
      return;
    }
    const boundKeys = (def.opts && def.opts.boundKeys) || [];
    const unsubs = [];

    const doRender = () => {
      const props = state.snapshot(boundKeys);
      // Allow extra props from host attributes (e.g. data-prop-foo)
      for (const a of host.attributes) {
        if (a.name.startsWith('data-prop-')) {
          props[a.name.slice('data-prop-'.length)] = a.value;
        }
      }
      try {
        const html = def.render(props) || '';
        host.innerHTML = html;
        if (def.opts.afterRender) def.opts.afterRender(host, props);
      } catch (e) {
        console.error('[framework] render error in', name, e);
        host.innerHTML = '<div style="font:11px var(--mono);color:var(--red);padding:8px">component error: ' + (e.message || e) + '</div>';
      }
    };

    // Subscribe to all bound keys.
    for (const k of boundKeys) unsubs.push(state.subscribe(k, doRender));
    _mounted.set(host, { name, opts: def.opts, unsubs });
    doRender();
  }

  function _unhydrate(host) {
    const m = _mounted.get(host);
    if (!m) return;
    m.unsubs.forEach(fn => fn());
    _mounted.delete(host);
  }

  /** Scan the DOM and hydrate every [data-component] — call once on boot. */
  function mount(root) {
    (root || document).querySelectorAll('[data-component]').forEach(host => {
      if (!_mounted.has(host)) _hydrate(host);
    });
  }

  /** Force re-render every mounted component (e.g. after a theme change). */
  function refreshAll() {
    for (const [host] of _mounted) {
      const m = _mounted.get(host);
      if (!m) continue;
      const def = _components[m.name];
      if (!def) continue;
      const props = state.snapshot((def.opts && def.opts.boundKeys) || []);
      for (const a of host.attributes) if (a.name.startsWith('data-prop-')) props[a.name.slice(10)] = a.value;
      try { host.innerHTML = def.render(props) || ''; if (def.opts.afterRender) def.opts.afterRender(host, props); }
      catch (e) { console.error('[framework] refresh error in', m.name, e); }
    }
  }

  // flashEl removed (review DASH-4): the inline dashboard.rs flashEl is the
  // single source of truth; a same-named window global here only shadowed.

  // ── Tag function for HTML strings ─────────────────────────────────────
  // Usage in renderers: html`<div class="${state ? 'on' : 'off'}">${value}</div>`
  // Auto-escapes interpolated values (except for ${{__html: '<safe/>'}} marker).
  function html(strings) {
    let out = strings[0];
    for (let i = 1; i < arguments.length; i++) {
      const v = arguments[i];
      if (v == null) out += '';
      else if (typeof v === 'object' && v.__html != null) out += v.__html;
      else if (Array.isArray(v)) out += v.join('');
      else out += String(v).replace(/[&<>"']/g, c => ({ '&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;' }[c]));
      out += strings[i];
    }
    return out;
  }
  /** Mark a string as already-safe HTML (skip escaping). */
  function raw(s) { return { __html: s == null ? '' : String(s) }; }

  // ── Bootstrap (idempotent — safe to call multiple times) ──────────────
  let _booted = false;
  function bootFramework() {
    if (_booted) return;
    _booted = true;
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', mount);
    } else {
      /* Defer to after the current synchronous task — by then any sibling
         component IIFEs in the same <script> block will have called
         defineComponent and registered themselves. Otherwise mount() runs
         BEFORE those IIFEs and finds no registered components. */
      setTimeout(mount, 0);
    }
  }

  // Expose globals.
  window.state = state;
  window.defineComponent = defineComponent;
  window.mount = mount;
  window.refreshAll = refreshAll;
  window.html = html;
  window.htmlRaw = raw;
  window.bootFramework = bootFramework;
  // Canonical label table (TERM-7) + resolver (TERM-6/TERM-7). Exposed on
  // window so every component IIFE and the inline dashboard.rs JS can pull
  // labels from the single source rather than hardcoding scattered strings.
  window.GLOSSARY = GLOSSARY;
  window.gloss = gloss;

  // Auto-boot on script load.
  bootFramework();
})();
/* The dax-flash keyframe CSS injection was removed with window.flashEl
   (review DASH-4) — it was never triggered (no caller used the modular
   flashEl). The inline dashboard.rs flashEl animates via inline styles. */
