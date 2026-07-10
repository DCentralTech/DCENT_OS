/* ──────────────────────────────────────────────────────────────────────────
   core.js — Mining Core component (Phase 3.2)
   ──────────────────────────────────────────────────────────────────────────
   The "living organism" centerpiece. Renders a 7-node molecular sphere
   inside <div data-component="mining-core" class="core glass glow">.

     1 center "CORE" + 6 hex-arranged outer nodes (asic0/asic1/pool/temp/
     power/net). Two rotating dashed rings + 6 spokes + 6 ring links + a
     hashrate readout overlay + per-poll meta line.

   Re-renders on state.set('info', d). Once mounted, the legacy
   spawnSphereParticle / scheduleSphereHeartbeat / updateSphereNodes /
   updateAmbientLoad helpers (still resident in inline JS) start firing
   because #coreSphere now exists in the DOM.
   ────────────────────────────────────────────────────────────────────────── */

(function () {
  'use strict';

  const NODES = [
    { role: 'center', cx: 0,    cy: 0,    r: 18, center: true },
    { role: 'asic0',  cx: 0,    cy: -110, r: 11 },
    { role: 'asic1',  cx: 95,   cy: -55,  r: 11 },
    { role: 'pool',   cx: 95,   cy: 55,   r: 11 },
    { role: 'temp',   cx: 0,    cy: 110,  r: 11 },
    { role: 'power',  cx: -95,  cy: 55,   r: 11 },
    { role: 'net',    cx: -95,  cy: -55,  r: 11 },
  ];

  function nodeSvg(n) {
    var hl_x = -n.r * 0.35, hl_y = -n.r * 0.4;
    var hl_rx = n.r * 0.45, hl_ry = n.r * 0.22;
    return ''
      + '<g class="sphere-node' + (n.center ? ' sphere-node-center' : '') + '"'
      +   ' data-role="' + n.role + '" data-cx="' + n.cx + '" data-cy="' + n.cy + '"'
      +   ' transform="translate(' + n.cx + ' ' + n.cy + ')">'
      +   '<circle r="' + (n.r + 8) + '" fill="color-mix(in srgb,var(--accent) 18%,transparent)" opacity=".35"/>'
      +   '<circle r="' + n.r + '" fill="var(--accent)" opacity=".85"/>'
      +   '<ellipse cx="' + hl_x + '" cy="' + hl_y + '" rx="' + hl_rx + '" ry="' + hl_ry + '" fill="rgba(255,255,255,.55)"/>'
      + '</g>';
  }

  function linksSvg() {
    var outer = NODES.filter(function (n) { return !n.center; });
    var s = '';
    /* spokes (center → outer) */
    for (var i = 0; i < outer.length; i++) {
      var n = outer[i];
      s += '<line class="sphere-link" x1="0" y1="0" x2="' + n.cx + '" y2="' + n.cy + '"/>';
    }
    /* ring (outer ↔ outer in hex order) */
    for (var j = 0; j < outer.length; j++) {
      var a = outer[j];
      var b = outer[(j + 1) % outer.length];
      s += '<line class="sphere-link" x1="' + a.cx + '" y1="' + a.cy + '" x2="' + b.cx + '" y2="' + b.cy + '"/>';
    }
    return s;
  }

  function fmtHr(v) {
    if (v >= 1000) return { v: (v / 1000).toFixed(2), u: 'TH/s' };
    if (v >= 1)    return { v: v.toFixed(1),          u: 'GH/s' };
    return                  { v: (v * 1000).toFixed(0), u: 'MH/s' };
  }

  function fD(d) {
    if (d == null) return '--';
    if (d >= 1e12) return (d / 1e12).toFixed(2) + 'T';
    if (d >= 1e9)  return (d / 1e9).toFixed(2)  + 'G';
    if (d >= 1e6)  return (d / 1e6).toFixed(2)  + 'M';
    if (d >= 1e3)  return (d / 1e3).toFixed(1)  + 'K';
    return Math.round(d);
  }

  /* Canonical label resolver (TERM-7): pull operator-facing strings from the
     shared window.GLOSSARY table when present, else fall back to the literal
     so a missing key never blanks the UI. Strings are byte-identical to the
     prior hardcoded labels (and to terminology-lexicon canonical spellings). */
  function L(key, field, literal) {
    var g = window.gloss;
    if (typeof g === 'function') { var v = g(key, field); if (v) return v; }
    return literal;
  }

  function render(props) {
    var d = (props && props.info) || window._lastInfo || {};
    /* Hero is captioned "10m Average" — bind the firmware's 600-sample
       hashRate_10m window, falling back to 1m / instantaneous so a chip that
       hasn't filled the 10m buffer yet still shows a number. */
    var hrGH = d.hashRate_10m || d.hashRate_1m || d.hashRate || 0;
    var hr = fmtHr(hrGH);
    var w = d.power || 0;
    var jth = (hrGH > 0 && w > 0) ? (w / (hrGH / 1000)).toFixed(1) : '--';
    var pt = (d.dcentaxe && d.dcentaxe.poolTruth) || {};
    var a = pt.sharesAccepted != null ? pt.sharesAccepted : (d.sharesAccepted || 0);
    var best = d.bestSessionDiff || d.bestDiff || 0;
    var telemetryReady = !!(d && Object.keys(d).length);
    /* miningEnabled is the operator permit; mining = enabled AND positive hashrate. */
    var miningEnabled = telemetryReady ? (d.dcentaxe ? d.dcentaxe.miningEnabled !== false : hrGH > 0) : false;
    var mining = miningEnabled && hrGH > 0;
    /* Status truth-ladder (COMP-PILL §1 / TERM-2): first paint MUST be telemetry_pending,
       never mining. PENDING (pre-telemetry) → MINING (enabled+hr>0) → READY (rung-2:
       permitted but hr==0) → STANDBY (rung-3: mining disabled). STANDBY is neutral, not a
       fault, so it uses pill-muted (not pill-err). Words are the shared contract states;
       UPPERCASE CRT presentation is axe's skin. */
    var miningPill = !telemetryReady
      ? '<span class="pill pill-muted"><span class="dot"></span>PENDING</span>'
      : mining
      ? '<span class="pill pill-ok"><span class="dot"></span>MINING</span>'
      : miningEnabled
      ? '<span class="pill pill-muted"><span class="dot"></span>READY</span>'
      : '<span class="pill pill-muted"><span class="dot"></span>STANDBY</span>';
    /* Capacity readout: hr / expected. Falls back to 'idle' if zero. */
    var expected = d.expectedHashrate || 0;
    var capPct = expected > 0 ? Math.min(150, Math.round(hrGH / expected * 100)) : 0;
    var sub = capPct ? ('Core at ' + capPct + '% capacity') : 'idle';

    /* A11y (TD-7): the 7-node sphere is a data visualization, not decoration —
       give it role="img" + a descriptive, data-bearing aria-label rebuilt every
       render so a screen reader announces the LIVE hashrate (the readout text is
       also visible). role="img" makes AT treat the SVG as a single labeled image
       and ignore its animated internals (was aria-hidden, i.e. invisible to AT). */
    var coreAria = 'Mining core, live hashrate ' + hr.v + ' ' + hr.u
      + (mining ? ', mining' : miningEnabled ? ', ready' : telemetryReady ? ', standby' : ', telemetry pending')
      + (capPct ? ', core at ' + capPct + '% capacity' : '');

    return ''
      + '<div class="core-main">'
      +   '<div class="core-h">'
      +     '<span class="core-eyebrow">DCENT_axe &middot; Mining Core</span>'
      +     miningPill
      +   '</div>'
      +   '<div class="core-row">'
      +     '<div class="core-c">'
      +       '<svg id="coreSphere" viewBox="-160 -160 320 320" preserveAspectRatio="xMidYMid meet" role="img" aria-label="' + coreAria + '">'
      +         '<circle class="sphere-ring" cx="0" cy="0" r="135" stroke-dasharray="2 8"/>'
      +         '<circle class="sphere-ring sphere-ring-rev" cx="0" cy="0" r="118" stroke-dasharray="6 8"/>'
      +         '<g id="sphereLinks">' + linksSvg() + '</g>'
      +         '<g id="sphereNodes">' + NODES.map(nodeSvg).join('') + '</g>'
      +         '<g id="sphereParticles"></g>'
      +       '</svg>'
      +     '</div>'
      +     '<div class="core-readout">'
      +       '<div class="lbl">' + L('window_headline_10m', 'label', 'Hashrate · 10m') + '</div>'
      +       '<div class="val"><span id="coreHr">' + hr.v + '</span><span class="u" id="coreHrUnit"> ' + hr.u + '</span></div>'
      +       '<div class="sub" id="coreLoad">' + sub + '</div>'
      +     '</div>'
      +     '<div class="core-meta">'
      +       '<span><span class="k">' + L('efficiency_jth', 'short', 'Eff') + '</span><b id="coreEff">' + jth + '</b><span class="u">' + L('efficiency_jth', 'unit', 'J/TH') + '</span></span>'
      +       '<span><span class="k">' + L('share_accepted', 'short', 'Pool OK') + '</span><b id="coreShares">' + a.toLocaleString() + '</b></span>'
      +       '<span><span class="k">' + L('best_diff_session', 'tiny', 'Best') + '</span><b id="coreBest">' + (best ? fD(best) : '--') + '</b></span>'
      +     '</div>'
      +   '</div>'
      + '</div>';
  }

  if (typeof window.defineComponent === 'function') {
    window.defineComponent('mining-core', render, { boundKeys: ['info'] });
  }
})();
