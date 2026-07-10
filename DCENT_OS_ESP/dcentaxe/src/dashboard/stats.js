/* ──────────────────────────────────────────────────────────────────────────
   stats.js — <dcent-stat> tile component (Phase 4a)
   ──────────────────────────────────────────────────────────────────────────
   Each <div data-component="dcent-stat" data-prop-key="bestDiff|power|
   fan|uptime"> renders a uniform dark-glass stat tile with state-driven
   sub-line color via [data-state] resolving to --state-* tokens.
   ────────────────────────────────────────────────────────────────────────── */

(function () {
  'use strict';

  /* Canonical label resolver (TERM-7): pull operator-facing eyebrow strings
     from the shared window.GLOSSARY table when present, else fall back to the
     literal. NOTE: stats.js is currently an ORPHAN component (not register_static'd
     and not in the dashboard.rs <script src> set — see dashboard.rs "review DASH-1");
     this glossary wiring is a dormant, additive source-consistency edit so that
     IF stats.js is ever re-served its labels are already single-sourced. It adds
     NO handler and changes no runtime behaviour today. */
  function L(key, field, literal) {
    var g = window.gloss;
    if (typeof g === 'function') { var v = g(key, field); if (v) return v; }
    return literal;
  }

  function fU(s) {
    if (!s) return '0m';
    var d = (s / 86400) | 0,
        h = ((s % 86400) / 3600) | 0,
        m = ((s % 3600) / 60) | 0;
    return (d ? d + 'd ' : '') + (h ? h + 'h ' : '') + m + 'm';
  }
  function fD(d) {
    if (d == null) return '--';
    if (d >= 1e12) return (d / 1e12).toFixed(1) + 'T';
    if (d >= 1e9)  return (d / 1e9).toFixed(1)  + 'G';
    if (d >= 1e6)  return (d / 1e6).toFixed(1)  + 'M';
    if (d >= 1e3)  return (d / 1e3).toFixed(1)  + 'K';
    return Math.round(d);
  }

  /* Per-key renderers: each returns { eyebrow, value, unit, sub, state, icon } */
  var RENDERERS = {
    bestDiff: function (d) {
      var sess = d.bestSessionDiff || d.bestDiff || 0;
      var ever = d.bestEverDiff || sess;
      var isNew = sess > 0 && sess >= ever;
      return {
        eyebrow: L('best_diff_session', 'short', 'Best Diff · Session'),
        value:   fD(sess), unit: '',
        sub:     isNew ? '↑ new best' : ('all-time ' + fD(ever)),
        state:   isNew ? 'achievement' : 'good',
        icon:    'bolt',
      };
    },
    power: function (d) {
      var w = d.power || 0;
      var maxW = d.maxPower || 25;
      var state = w > maxW * 0.9 ? 'error' : w > maxW * 0.75 ? 'warn' : w > 0 ? 'good' : 'idle';
      var sub = w > maxW * 0.9 ? '· capped'
              : w > maxW * 0.75 ? '· spiking'
              : w > 0           ? '· stable'
                                : '· idle';
      return {
        eyebrow: L('unit_power', 'label', 'Power'),
        value:   w.toFixed(1), unit: 'W',
        sub:     Math.round(w * 3.412142) + ' BTU/h ' + sub,
        state:   state,
        icon:    'power',
      };
    },
    fan: function (d) {
      var fp = d.fanspeed || 0;
      var rpm = d.fanrpm || 0;
      var auto = !!d.autofanspeed;
      var state = fp >= 100 ? 'warn' : fp > 0 ? 'good' : 'idle';
      var label = (auto ? 'auto' : 'manual') + (rpm ? (' · ' + rpm + ' rpm') : '');
      return {
        eyebrow: 'Fan',
        value:   fp.toFixed(0), unit: '%',
        sub:     label,
        state:   state,
        icon:    'fan',
      };
    },
    uptime: function (d) {
      var u = d.uptimeSeconds || 0;
      var wdt = d.wdtResetCount || 0;
      var state = wdt > 0 ? 'warn' : 'good';
      var sub = wdt > 0 ? (wdt + ' wdt reset' + (wdt === 1 ? '' : 's')) : '↑ no resets';
      return {
        eyebrow: 'Uptime',
        value:   fU(u), unit: '',
        sub:     sub,
        state:   state,
        icon:    'clock',
      };
    },
  };

  /* Inline SVG icons (small, currentColor-driven) */
  var ICONS = {
    bolt:  '<polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/>',
    power: '<path d="M18.36 6.64a9 9 0 1 1-12.73 0"/><line x1="12" y1="2" x2="12" y2="12"/>',
    fan:   '<circle cx="12" cy="12" r="2"/><path d="M12 4c3 0 5 2 5 5 0 2-1 3-2 3"/><path d="M20 12c0 3-2 5-5 5-2 0-3-1-3-2"/><path d="M12 20c-3 0-5-2-5-5 0-2 1-3 2-3"/><path d="M4 12c0-3 2-5 5-5 2 0 3 1 3 2"/>',
    clock: '<circle cx="12" cy="12" r="9"/><polyline points="12 7 12 12 16 14"/>',
  };

  function render(props) {
    var key = props && props.key;
    var d = (props && props.info) || window._lastInfo || {};
    var fn = RENDERERS[key];
    if (!fn) return '<div style="font:11px var(--mono);color:var(--state-error);padding:8px">unknown stat: ' + key + '</div>';
    var r = fn(d);
    var iconSvg = ICONS[r.icon] || ICONS.bolt;
    return ''
      + '<div class="stat-head">'
      +   '<span class="stat-eyebrow">' + r.eyebrow + '</span>'
      +   '<svg class="stat-icon" viewBox="0 0 24 24">' + iconSvg + '</svg>'
      + '</div>'
      + '<div class="stat-value"><span class="v">' + r.value + '</span>'
      + (r.unit ? ('<span class="u">' + r.unit + '</span>') : '')
      + '</div>'
      + '<div class="stat-sub">' + r.sub + '</div>';
  }

  function afterRender(host, props) {
    var key = props && props.key;
    var d = (props && props.info) || window._lastInfo || {};
    var fn = RENDERERS[key];
    if (!fn) return;
    var r = fn(d);
    host.setAttribute('data-state', r.state);
    /* A11y (TD-7): label the whole tile (role="img") so a screen reader
       announces the eyebrow + value + unit + sub-line as one coherent reading
       (e.g. "Power: 14.2 W, 48 BTU/h · stable") and skips the decorative icon.
       NOTE: stats.js is currently an ORPHAN component (not register_static'd —
       see review DASH-1), so this is dormant-additive like the glossary wiring
       above: it changes no served UI today but is correct IF stats.js is re-served. */
    host.setAttribute('role', 'img');
    host.setAttribute(
      'aria-label',
      r.eyebrow + ': ' + r.value + (r.unit ? ' ' + r.unit : '') + (r.sub ? ', ' + r.sub : '')
    );
  }

  if (typeof window.defineComponent === 'function') {
    window.defineComponent('dcent-stat', render, {
      boundKeys: ['info'],
      afterRender: afterRender,
    });
  }
})();
