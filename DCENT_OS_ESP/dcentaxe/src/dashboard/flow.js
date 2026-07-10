/* ──────────────────────────────────────────────────────────────────────────
   flow.js — flow-graph component + share particle + thermal shifter (Phase 5)
   ──────────────────────────────────────────────────────────────────────────
   <flow-graph> renders a 600×80 SVG ribbon of the last 60 hashrate samples
   as a smooth-Bezier path, with animated stroke-dashoffset for "scrolling"
   feel. Samples buffered in module-level closure (survives re-mounts).

   Public window helpers:
     window.addShareParticle(accepted)  Spawn a green/red dot drifting →
     window.setThermal(elOrId, c)       Set [data-thermal] cool/normal/warm/danger
   ────────────────────────────────────────────────────────────────────────── */

(function () {
  'use strict';

  /* ── flow-graph component ─────────────────────────────────────────── */
  var SAMPLES = [];
  var MAX = 60;

  function pathD(samples, w, h) {
    if (samples.length < 2) return '';
    var max = Math.max.apply(null, samples) || 1;
    var step = w / (samples.length - 1);
    var d = 'M0 ' + (h - samples[0] / max * (h - 4) - 2);
    for (var i = 1; i < samples.length; i++) {
      var x = i * step;
      var y = h - samples[i] / max * (h - 4) - 2;
      var px = (i - 1) * step;
      var py = h - samples[i - 1] / max * (h - 4) - 2;
      var cx = (px + x) / 2;
      d += ' Q' + cx.toFixed(1) + ' ' + py.toFixed(1)
         + ' ' + x.toFixed(1) + ' ' + y.toFixed(1);
    }
    return d;
  }
  function areaD(samples, w, h) {
    return pathD(samples, w, h) + ' L' + w + ' ' + h + ' L0 ' + h + ' Z';
  }

  function flowRender() {
    /* A11y (TD-7): the ribbon is a throughput sparkline — role="img" + a
       descriptive aria-label so AT announces what the graphic represents
       (afterRender refreshes the label with the latest sample). */
    return ''
      + '<svg viewBox="0 0 600 80" preserveAspectRatio="none" role="img" aria-label="Hashrate throughput trend, last 60 samples">'
      +   '<defs>'
      +     '<linearGradient id="flowFill" x1="0" x2="0" y1="0" y2="1">'
      +       '<stop offset="0%" stop-color="var(--accent)" stop-opacity=".32"/>'
      +       '<stop offset="100%" stop-color="var(--accent)" stop-opacity="0"/>'
      +     '</linearGradient>'
      +   '</defs>'
      +   '<path id="flowArea" fill="url(#flowFill)"/>'
      +   '<path id="flowLine" fill="none" stroke="var(--accent)" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" style="filter:drop-shadow(0 0 4px var(--glow))"/>'
      + '</svg>';
  }
  function flowAfterRender(host /*, props */) {
    var d = (window.state && window.state.get('info')) || window._lastInfo || {};
    var hr = d.hashRate_1m || d.hashRate || 0;
    SAMPLES.push(hr);
    if (SAMPLES.length > MAX) SAMPLES.shift();
    var line = host.querySelector('#flowLine');
    var fill = host.querySelector('#flowArea');
    if (line) line.setAttribute('d', pathD(SAMPLES, 600, 80));
    if (fill) fill.setAttribute('d', areaD(SAMPLES, 600, 80));
    /* Keep the role="img" label current with the latest throughput sample. */
    var svg = host.querySelector('svg');
    if (svg) {
      svg.setAttribute(
        'aria-label',
        'Hashrate throughput trend, last ' + SAMPLES.length + ' samples, latest '
          + (typeof hr === 'number' ? hr.toFixed(1) : '0') + ' GH/s'
      );
    }
  }

  if (typeof window.defineComponent === 'function') {
    window.defineComponent('flow-graph', flowRender, {
      boundKeys: ['info'],
      afterRender: flowAfterRender,
    });
  }

  /* ── Share particle strip ─────────────────────────────────────────── */
  window.addShareParticle = function (accepted) {
    var host = document.getElementById('shareStream');
    if (!host) return;
    /* Cap in-flight at 30 to prevent DOM bloat at high share rate */
    if (host.children.length > 30) return;
    var p = document.createElement('span');
    p.className = 'share-p ' + (accepted ? 'ok' : 'bad');
    host.appendChild(p);
    setTimeout(function () { if (p.parentNode) p.parentNode.removeChild(p); }, 4200);
  };

  /* ── Thermal color shifter ────────────────────────────────────────── */
  window.setThermal = function (elOrId, c) {
    var el = (typeof elOrId === 'string') ? document.getElementById(elOrId) : elOrId;
    if (!el) return;
    var band = c < 50 ? 'cool' : c < 75 ? 'normal' : c < 90 ? 'warm' : 'danger';
    el.setAttribute('data-thermal', band);
  };
})();
