/* ──────────────────────────────────────────────────────────────────────────
   asic-chips.js — PROTECTED ASIC chip card component (Phase 2.C)
   ──────────────────────────────────────────────────────────────────────────
   Componentizes the chip rendering as <div data-component="asic-chips">.
   The framework re-renders on every state.set('info', d) — fired from
   update(d) once per poll. The card-head metrics (#chipCardCount, etc.)
   live OUTSIDE the component's render root so they're mutated imperatively.

   Phase 2.C ships with legacy `.hot` / `.error` class emission preserved
   so the existing CSS bug-for-bug matches. Phase 2.D switches to
   [data-state] attribute and removes the legacy class emission.

   The silicon SVG, beam sweep, share bar, UART daisy chain are all
   PROTECTED — DO NOT trim.
   ────────────────────────────────────────────────────────────────────────── */

(function () {
  'use strict';
  function E(i) { return document.getElementById(i); }

  /* Canonical empty-value glyph (TERM-7 / terminology-lexicon §6.2 empty_value).
     Pulled from the shared window.GLOSSARY so the absent/unknown placeholder is
     a single source-of-truth; falls back to the literal '--' (axe's CRT rendering
     of the empty_value role) so a missing table never blanks the card. The chip
     physics labels (TEMP/FREQ/VOLT/HW ERR) are per-silicon units, not shared
     glossary terms, so they stay literal. */
  function emptyGlyph() {
    var g = window.gloss;
    if (typeof g === 'function') { var v = g('empty_value', 'label'); if (v) return v; }
    return '--';
  }

  /* PROTECTED: per-chip animated silicon die. Identical to legacy
     dashboard.rs `chipSiliconSvg(id)`. The .chip-beam rect at the
     bottom is what gives the "swept" feel. */
  function chipSiliconSvg(id) {
    var pins = '';
    var pos = [18, 24.4, 30.8, 37.2, 43.6, 50, 56.4, 62.8, 69.2, 75.6, 82];
    for (var i = 0; i < pos.length; i++) {
      var p = pos[i];
      pins += '<rect x="' + p + '" y="6" width="3" height="6" rx="1" fill="#2a3340" opacity="0.9"/>'
            + '<rect x="' + p + '" y="88" width="3" height="6" rx="1" fill="#2a3340" opacity="0.9"/>'
            + '<rect x="6" y="' + p + '" width="6" height="3" rx="1" fill="#2a3340" opacity="0.9"/>'
            + '<rect x="88" y="' + p + '" width="6" height="3" rx="1" fill="#2a3340" opacity="0.9"/>';
    }
    /* A11y (TD-7): the animated silicon die is a decorative-but-meaningful glyph.
       Give it role="img" + a per-chip aria-label so a screen reader announces it
       as one labeled image (and ignores the internal pins/traces) instead of an
       unlabeled graphic. The numeric metrics (TEMP/SHARES/FREQ/…) remain visible
       text in the tile, so they are already announced. */
    return '<svg viewBox="0 0 100 100" class="chip-silicon" preserveAspectRatio="xMidYMid meet" role="img" aria-label="ASIC chip ' + id + ' silicon die">'
      + '<defs>'
      +   '<radialGradient id="die-' + id + '" cx="50%" cy="45%" r="55%">'
      +     '<stop offset="0%" stop-color="var(--chip-color)" stop-opacity="0.55"/>'
      +     '<stop offset="60%" stop-color="var(--chip-color)" stop-opacity="0.10"/>'
      +     '<stop offset="100%" stop-color="var(--chip-color)" stop-opacity="0"/>'
      +   '</radialGradient>'
      +   '<linearGradient id="bevel-' + id + '" x1="0" y1="0" x2="1" y2="1">'
      +     '<stop offset="0%" stop-color="#1a2230"/>'
      +     '<stop offset="100%" stop-color="#0a0e14"/>'
      +   '</linearGradient>'
      + '</defs>'
      + pins
      + '<rect x="14" y="14" width="72" height="72" rx="6" fill="url(#bevel-' + id + ')" stroke="var(--chip-color)" stroke-width="1" opacity="0.95"/>'
      + '<rect x="26" y="26" width="48" height="48" rx="3" fill="#050709" stroke="var(--chip-color)" stroke-width="0.5" opacity="0.9"/>'
      + '<rect x="10" y="10" width="80" height="80" rx="10" fill="url(#die-' + id + ')" opacity="0.9"/>'
      + '<g stroke="var(--chip-color)" stroke-width="0.35" fill="none" opacity="0.55">'
      +   '<path d="M 30 38 L 44 38 L 44 48 L 56 48"/>'
      +   '<path d="M 62 32 L 62 44 L 70 44"/>'
      +   '<path d="M 30 58 L 38 58 L 38 68 L 50 68"/>'
      +   '<path d="M 58 62 L 68 62 L 68 70"/>'
      + '</g>'
      + '<circle cx="50" cy="50" r="1.8" fill="var(--chip-color)" opacity="0.9"/>'
      + '<rect x="26" y="26" width="48" height="2" rx="1" fill="var(--chip-color)" opacity="0.7" class="chip-beam"/>'
      + '</svg>';
  }

  /* State derivation per chip — used in Phase 2.D. Phase 2.C still emits
     legacy classes alongside data-state for one-step overlap. */
  function chipState(c) {
    var st = (c.status || '').toString().toLowerCase();
    if (st === 'error') return 'error';
    var t = (typeof c.temp === 'number' && isFinite(c.temp)) ? c.temp : null;
    if (st === 'unknown' || t == null) return 'idle';
    if (t >= 70 || st === 'hot') return 'hot';
    if (t >= 55) return 'warm';
    if (t >= 40 || st === 'active') return 'active';
    return 'idle';
  }

  /* Module-level history ring buffer (for future drill-down sparkline) */
  var _hist = {};
  function pushHist(id, sh) {
    var a = _hist[id] = _hist[id] || [];
    a.push(sh | 0);
    if (a.length > 30) a.shift();
  }

  function tileHtml(c, i, n, totalN, hotIdx, info) {
    var st = (c.status || 'unknown').toString();
    var hasTemp = (typeof c.temp === 'number' && isFinite(c.temp));
    /* Legacy-compatible hot detection (single hottest chip > 80) */
    var isHot = (i === hotIdx && hotIdx !== -1 && hasTemp && c.temp > 80);
    var isErr = (st === 'error');
    var derivedState = chipState(c);
    var dotCls = isErr ? ' error' : (derivedState === 'active' || derivedState === 'warm' || derivedState === 'hot' ? ' active' : '');
    var cid = (c.id != null) ? (c.id + 1) : (i + 1);
    var sharePct = totalN > 0 ? Math.round((c.shares || 0) / totalN * 100) : 0;
    /* Phase 2.D: HOT badge driven by derivedState rather than the
       legacy single-hottest-chip rule — every chip in 'hot' state
       gets the badge. */
    var hotBadge = derivedState === 'hot' ? '<span class="chip-badge-hot">HOT</span>' : '';
    var fmtUnix = (typeof window.fmtUnixUtc === 'function') ? window.fmtUnixUtc : function(){return '--'};
    var freq = (c.frequency != null) ? (c.frequency + ' MHz') : emptyGlyph();
    var volt = (c.voltage != null) ? (c.voltage + ' mV') : emptyGlyph();
    var hwErr = (c.hwErrors != null) ? c.hwErrors : 0;
    var lastShare = c.lastShareUnix ? fmtUnix(c.lastShareUnix) : emptyGlyph();
    pushHist(cid, c.shares || 0);
    return '<div class="chip-tile" data-state="' + derivedState + '" data-chip-idx="' + i + '">'
      + hotBadge
      + '<div class="chip-head"><span class="chip-id">#' + (cid < 10 ? '0' + cid : cid) + '</span><span class="chip-dot' + dotCls + '"></span></div>'
      + '<div class="chip-silicon-wrap">' + chipSiliconSvg(cid) + '</div>'
      + '<div class="chip-metrics">'
      +   '<div class="chip-metric"><div class="chip-metric-val">' + (hasTemp ? c.temp.toFixed(0) : '--') + '<span class="chip-metric-unit">&deg;C</span></div><div class="chip-metric-lbl">TEMP</div></div>'
      +   '<div class="chip-metric"><div class="chip-metric-val">' + (c.shares || 0) + '</div><div class="chip-metric-lbl">SHARES</div></div>'
      + '</div>'
      + '<div class="chip-share-bar"><div class="chip-share-fill" style="width:' + sharePct + '%"></div><span class="chip-share-lbl">' + sharePct + '% contrib</span></div>'
      + '<div class="chip-pop">'
      +   '<div class="row"><span class="k">FREQ</span><span class="v">' + freq + '</span></div>'
      +   '<div class="row"><span class="k">VOLT</span><span class="v">' + volt + '</span></div>'
      +   '<div class="row"><span class="k">HW ERR</span><span class="v">' + hwErr + '</span></div>'
      +   '<div class="row"><span class="k">LAST</span><span class="v">' + lastShare + '</span></div>'
      + '</div>'
      + '</div>';
  }

  function uartChainHtml(chips) {
    var ch = '<span class="uart-node uart-mcu"><span class="uart-led"></span>ESP32</span>';
    for (var i = 0; i < chips.length; i++) {
      var cs = (chips[i].status || '').toString().toLowerCase();
      var cls = cs === 'active' ? 'active' : cs === 'error' ? 'error' : 'idle';
      var uid = (chips[i].id != null) ? (chips[i].id + 1) : (i + 1);
      ch += '<span class="uart-line"></span><span class="uart-node uart-chip ' + cls + '">#' + uid + '</span>';
    }
    return ch;
  }

  function render(props) {
    var info = (props && props.info) || window._lastInfo || {};
    var chips = info.dcentaxe && info.dcentaxe.chips;
    var card = E('chipCard');
    if (!chips || chips.length < 1) {
      if (card) card.style.display = 'none';
      return '';
    }
    if (card) card.style.display = 'block';
    var n = chips.length;
    var totalN = chips.reduce(function (s, c) { return s + (c.shares || 0); }, 0);
    var hotIdx = -1;
    for (var i = 0; i < n; i++) {
      if (typeof chips[i].temp === 'number' && isFinite(chips[i].temp)) {
        if (hotIdx === -1 || chips[i].temp > chips[hotIdx].temp) hotIdx = i;
      }
    }
    /* Card-head metrics — model from API, count from chips.length.
       Mutated imperatively because they live OUTSIDE the component's render root. */
    var activeN = 0, tempSum = 0, tempN = 0;
    for (var ci = 0; ci < n; ci++) {
      var cc = chips[ci];
      if ((cc.status || '') === 'active') activeN++;
      if (typeof cc.temp === 'number' && isFinite(cc.temp)) { tempSum += cc.temp; tempN++; }
    }
    var asicModel = info.ASICModel || 'BM????';
    function S(id, v) { var e = E(id); if (e) e.textContent = v; }
    S('chipCardCount', n);
    S('chipCardModel', asicModel);
    S('chipCardActive', activeN + '/' + n);
    S('chipCardAvgTemp', tempN > 0 ? (tempSum / tempN).toFixed(0) + '°' : '--');

    var tiles = '';
    for (var ti = 0; ti < n; ti++) tiles += tileHtml(chips[ti], ti, n, totalN, hotIdx, info);
    var gridCls = 'chip-grid chip-grid-' + (n === 1 ? 1 : n <= 2 ? 2 : n <= 4 ? 4 : 6);
    return '<div class="' + gridCls + '" id="chipGrid">' + tiles + '</div>'
      + '<div class="uart-chain-v2" id="uartChain">' + uartChainHtml(chips) + '</div>';
  }

  function afterRender(host, props) {
    /* Hover popover — 200 ms delay (Phase 2.E) */
    host.querySelectorAll('.chip-tile').forEach(function (tile) {
      var hoverT = null;
      tile.addEventListener('mouseenter', function () {
        if (hoverT) clearTimeout(hoverT);
        hoverT = setTimeout(function () { tile.classList.add('pop-on'); }, 200);
      });
      tile.addEventListener('mouseleave', function () {
        if (hoverT) { clearTimeout(hoverT); hoverT = null; }
        tile.classList.remove('pop-on');
      });
      /* Click drill-down (Phase 2.F) */
      tile.addEventListener('click', function () {
        var idx = +tile.getAttribute('data-chip-idx');
        var info = (window.state && window.state.get('info')) || window._lastInfo || {};
        var chip = info.dcentaxe && info.dcentaxe.chips && info.dcentaxe.chips[idx];
        if (chip && window.daxModal && typeof window.daxModal.openChipDetail === 'function') {
          window.daxModal.openChipDetail(chip, info);
        }
      });
    });
    /* Particle bridge HOOK — Phase 2.G.
       Phase 5 flow.js will addEventListener('dax:chips-updated', …). */
    if (props && props.info && props.info.dcentaxe && props.info.dcentaxe.chips) {
      try {
        window.dispatchEvent(new CustomEvent('dax:chips-updated', {
          detail: { chips: props.info.dcentaxe.chips },
        }));
      } catch (e) { /* CustomEvent unsupported — silent */ }
    }
  }

  /* Register the component */
  if (typeof window.defineComponent === 'function') {
    window.defineComponent('asic-chips', render, {
      boundKeys: ['info'],
      afterRender: afterRender,
    });
  }

  /* Back-compat globals — anything in legacy inline JS that still calls
     these continues to work during the migration overlap. */
  window.daxChips = { chipSiliconSvg: chipSiliconSvg, chipState: chipState };
  window.chipSiliconSvg = chipSiliconSvg;
  /* renderChips becomes a thin shim that just pokes the state bus —
     the component's render() will be re-invoked because boundKeys contains 'info'. */
  window.renderChips = function (chips, asicCount, ct) {
    var info = window._lastInfo || (window.state && window.state.get('info')) || {};
    if (window.state && typeof window.state.set === 'function') {
      window.state.set('info', info);
    }
  };
})();
