/* ──────────────────────────────────────────────────────────────────────────
   block-tile.js — Rich Block Modal + Chip Drill-Down (Phase 2.A + 2.F)
   ──────────────────────────────────────────────────────────────────────────
   Ports the legacy block-modal helpers verbatim and adds:
     - daxModal.openChipDetail(chip, info)  Phase 2.F chip drill-down
   Uses the same #blockModalBack shell — injects sibling .bm.bm-chip
   variant on first chip-detail open, hides the original .bm.

   Public namespace: window.daxModal
   Back-compat globals exposed for legacy inline JS callers.
   ────────────────────────────────────────────────────────────────────────── */

(function () {
  'use strict';

  let _blockCache = null;
  let _blockAgeTimer = null;
  let _blockKeydown = null;

  function E(i) { return document.getElementById(i); }
  function S(i, v) { var e = E(i); if (e) e.textContent = v; }

  /* Canonical empty-value glyph (TERM-7 / terminology-lexicon §6.2 empty_value).
     Pulled from the shared window.GLOSSARY so the block modal's absent-value
     placeholder is single-sourced; falls back to '--' (axe's CRT rendering of
     the empty_value role). The chip-detail physics labels (TEMP/FREQ/VOLTAGE)
     are per-silicon units, not shared glossary terms, so they stay literal. */
  function emptyGlyph() {
    var g = window.gloss;
    if (typeof g === 'function') { var v = g('empty_value', 'label'); if (v) return v; }
    return '--';
  }

  // ── Helpers (verbatim from legacy inline JS) ─────────────────────────
  function subsidyForHeight(h) {
    if (!h) return null;
    var era = Math.floor(h / 210000);
    if (era >= 33) return 0;
    var sats = 5000000000 >> era;
    return sats / 1e8;
  }
  function fmtBlockAge(ms) {
    if (!ms) return '--';
    var s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
    if (s < 2) return 'just now';
    if (s < 60) return s + ' s ago';
    var m = Math.floor(s / 60);
    if (m < 60) return m + ' min ago';
    var h = Math.floor(m / 60);
    return h + ' h ' + (m % 60) + ' min ago';
  }
  function fmtUnixUtc(u) {
    if (!u) return '--';
    try { var d = new Date(u * 1000); return d.toISOString().replace('T', ' ').replace('.000Z', ' UTC'); }
    catch (e) { return '--'; }
  }
  function truncHash(h) {
    if (!h || h.length < 20) return h || '--';
    return h.slice(0, 12) + '…' + h.slice(-12);
  }
  function fmtDiffShort(d) {
    if (d == null) return '--';
    if (d >= 1e12) return (d / 1e12).toFixed(2) + ' T';
    if (d >= 1e9)  return (d / 1e9).toFixed(2) + ' G';
    if (d >= 1e6)  return (d / 1e6).toFixed(2) + ' M';
    if (d >= 1e3)  return (d / 1e3).toFixed(1) + ' K';
    return Math.round(d);
  }

  // ── Network: GET /api/mining/block ───────────────────────────────────
  function fetchBlockInfo(cb) {
    var headers = (typeof window.authHeaders === 'function') ? window.authHeaders({}) : { 'X-Requested-With': 'XMLHttpRequest' };
    fetch('/api/mining/block', { headers: headers })
      .then(function (r) {
        if (r.status === 401 && typeof window.handleReadAuthFailure === 'function') window.handleReadAuthFailure(r);
        if (!r.ok) return null;
        return r.json();
      })
      .then(function (d) {
        if (d && d.blockHeight) { _blockCache = d; if (cb) cb(d); }
        else { _blockCache = null; if (cb) cb(null); }
      })
      .catch(function () { if (cb) cb(null); });
  }

  // ── Block modal: open / close / render ───────────────────────────────
  function openBlockModal() {
    var back = E('blockModalBack'); if (!back) return;
    // Restore the original .bm if a chip-detail call previously hid it.
    back.querySelectorAll('.bm').forEach(function (el) { el.style.display = ''; });
    var chipPane = back.querySelector('.bm.bm-chip'); if (chipPane) chipPane.style.display = 'none';
    document.body.style.overflow = 'hidden';
    fetchBlockInfo(function (d) {
      renderBlockModal(d);
      back.classList.add('show');
      _blockKeydown = function (ev) { if (ev.key === 'Escape') closeModal(); };
      document.addEventListener('keydown', _blockKeydown);
      /* A11y (TD-7): move focus into the dialog (the close button) on open so
         keyboard/AT users land inside the modal. The dialog markup itself
         (role="dialog" + aria-labelledby + labeled close) already lives in the
         inline shell — this completes the focus behaviour. */
      var blkCloseBtn = back.querySelector('.bm:not(.bm-chip) .bm-x');
      if (blkCloseBtn && typeof blkCloseBtn.focus === 'function') blkCloseBtn.focus();
      if (_blockAgeTimer) clearInterval(_blockAgeTimer);
      _blockAgeTimer = setInterval(function () {
        if (_blockCache && _blockCache.receivedUnixMs) S('bmAge', fmtBlockAge(_blockCache.receivedUnixMs));
      }, 1000);
    });
  }
  function closeModal() {
    var back = E('blockModalBack'); if (!back) return;
    back.classList.remove('show');
    document.body.style.overflow = '';
    if (_blockAgeTimer) { clearInterval(_blockAgeTimer); _blockAgeTimer = null; }
    if (_blockKeydown) { document.removeEventListener('keydown', _blockKeydown); _blockKeydown = null; }
  }
  function renderBlockModal(d) {
    if (!d) {
      S('bmHeight', '#--'); S('bmMetaHeight', '--'); S('bmSubsidy', '--'); S('bmFees', '--');
      S('bmReward', 'no block yet'); S('bmTimestamp', '--'); S('bmMetaTime', '--');
      S('bmAge', '--'); S('bmClean', '--'); S('bmJobId', '--');
      S('bmPrevHash', 'waiting…'); S('bmPrevHashFull', '--');
      var ml = E('bmMempoolLink'); if (ml) ml.removeAttribute('href');
      return;
    }
    var hStr = '#' + Number(d.blockHeight).toLocaleString();
    S('bmHeight', hStr); S('bmMetaHeight', hStr);
    var sub = subsidyForHeight(d.blockHeight);
    S('bmSubsidy', sub != null ? sub.toFixed(4) + ' BTC' : '--');
    S('bmFees', '—');
    S('bmReward', sub != null ? sub.toFixed(4) + ' BTC' : '--');
    S('bmTimestamp', fmtUnixUtc(d.ntimeUnix));
    S('bmMetaTime', fmtUnixUtc(d.ntimeUnix));
    S('bmAge', fmtBlockAge(d.receivedUnixMs));
    var clean = E('bmClean');
    if (clean) clean.innerHTML = d.cleanJobs ? '<span class="pill pill-ok"><span class="dot"></span>NEW BLOCK</span>' : '<span class="pill" style="color:var(--dim)">extension</span>';
    S('bmJobId', d.jobId || '--');
    S('bmPrevHash', d.prevHash || '--');
    S('bmPrevHashFull', d.prevHash || '--');
    var ml = E('bmMempoolLink'); if (ml && d.prevHash) ml.href = 'https://mempool.space/block/' + d.prevHash;
    /* contribution panel from latest cached info */
    var inf = window._lastInfo || {};
    var pt = (inf.dcentaxe && inf.dcentaxe.poolTruth) || {};
    var shares = pt.sharesAccepted != null
      ? Number(pt.sharesAccepted).toLocaleString()
      : (inf.sharesAccepted != null ? Number(inf.sharesAccepted).toLocaleString() : emptyGlyph());
    S('bmShares', shares);
    var best = inf.bestSessionDiff || inf.bestDiff || inf.bestEverDiff;
    S('bmBestDiff', best ? String(best) : emptyGlyph());
    var pool = (inf.stratumURL || '').replace(/^stratum\+tcp:\/\//, '').replace(/^stratum:\/\//, '');
    S('bmPool', pool || emptyGlyph());
    var pd = inf.poolDifficulty || inf.stratumSuggestedDifficulty;
    var diff = inf.networkDifficulty || inf.difficulty;
    if (diff) S('bmDifficulty', fmtDiffShort(diff));
    if (pd && diff) { var oddsN = diff / pd; S('bmOdds', '1 in ' + fmtDiffShort(oddsN)); }
    else if (pd)    S('bmOdds', 'share diff ' + pd);
    else            S('bmOdds', '--');

    /* ── Coinbase & Payout section ───────────────────────────────────── */
    renderCoinbasePayout(inf);
  }

  /* Heuristics for solo vs pool detection + payout method inference.
     Detection inputs: stratumURL, stratumUser, scriptsig, coinbaseOutputs.
     Payout methods are inferred from the pool host. When the firmware
     populates coinbaseOutputs the section also lists addresses + reward. */
  function detectMode(inf) {
    var url  = (inf.stratumURL || '').toLowerCase();
    var user = (inf.stratumUser || '');
    var outs = (inf.coinbaseOutputs || []);

    /* If we have outputs decoded, single-output (or user as sole recipient)
       is solo; multi-output pool. */
    var soloByOutputs = null;
    if (outs.length === 1) soloByOutputs = true;
    else if (outs.length > 1) {
      var totalSats = inf.coinbaseValueTotalSatoshis || 0;
      var userSats  = inf.coinbaseValueUserSatoshis || 0;
      /* > 95% to user → effectively solo with pool fee */
      soloByOutputs = (totalSats > 0 && userSats / totalSats > 0.95);
    }

    /* Pool-name heuristics for payout method when outputs aren't decoded yet. */
    var POOL_HINTS = [
      // [match, mode, payout, note]
      [/solo\.ckpool|public-pool/, 'solo',   'Solo (100% to user)',    null],
      [/ckpool\.org/,             'pool',   'PPLNS',                  null],
      [/ocean\.xyz|ocean\.io/,    'pool',   'TIDES',                  null],
      [/braiins|slushpool/,       'pool',   'Score (PPLNS-like)',     null],
      [/antpool/,                 'pool',   'PPS+',                   null],
      [/f2pool/,                  'pool',   'PPS+',                   null],
      [/viabtc/,                  'pool',   'PPS+ / PPLNS / SOLO',    null],
      [/nicehash/,                'pool',   'PPS',                    null],
      [/luxor/,                   'pool',   'FPPS',                   null],
      [/foundry/,                 'pool',   'FPPS',                   null],
      [/bitcoin\.com|btc\.com/,   'pool',   'FPPS',                   null],
      [/poolin/,                  'pool',   'FPPS',                   null],
      [/d-central|dcent/,         'pool',   'D-Central pool',         null],
      [/127\.0\.0\.1|localhost/,  'unknown','Local proxy / dev',      'Pool detection skipped — local relay'],
    ];
    var hint = null;
    for (var i = 0; i < POOL_HINTS.length; i++) {
      if (POOL_HINTS[i][0].test(url)) { hint = POOL_HINTS[i]; break; }
    }
    var mode    = (soloByOutputs === true)  ? 'solo'
                : (soloByOutputs === false) ? 'pool'
                : (hint ? hint[1] : 'unknown');
    var payout  = hint ? hint[2] : (mode === 'solo' ? 'Solo (100% to user)' : 'Unknown');
    var note    = hint ? hint[3] : null;
    return { mode: mode, payout: payout, note: note, url: url, user: user, outs: outs };
  }

  function renderCoinbasePayout(inf) {
    if (!inf) inf = {};
    var det = detectMode(inf);
    var modeEl   = E('bmMode');
    var payoutEl = E('bmPayout');
    var rewardEl = E('bmRewardPct');
    var hintEl   = E('bmCoinbaseHint');
    var outsEl   = E('bmCoinbaseOutputs');

    /* === Verifier — derive user's expected scripthex and tally matches === */
    var userAddr = inf.stratumUser || '';
    var userScriptHex = userAddr ? (addressToScriptHex(userAddr) || '').toLowerCase() : '';
    var outs = inf.coinbaseOutputs || [];
    var totalSats = inf.coinbaseValueTotalSatoshis || 0;
    var userSats = 0;
    /* If Rust hasn't filled total yet but outs are there, sum locally */
    if (totalSats === 0 && outs.length > 0) {
      for (var i = 0; i < outs.length; i++) totalSats += (outs[i].valueSats || outs[i].value || 0);
    }
    /* Sum outputs whose scriptpubkey matches the user-derived scripthex */
    if (userScriptHex && outs.length > 0) {
      for (var i = 0; i < outs.length; i++) {
        var sh = (outs[i].scriptHex || outs[i].script || '').toLowerCase();
        if (sh === userScriptHex) userSats += (outs[i].valueSats || outs[i].value || 0);
      }
    }
    var hasReal = totalSats > 0 && outs.length > 0;

    /* === Mode rendering === */
    if (modeEl) {
      var modeFromCoinbase = null;
      if (hasReal) {
        var pctReal = userSats / totalSats;
        if (pctReal === 0) modeFromCoinbase = 'pool';
        else if (pctReal > 0.50) modeFromCoinbase = 'solo';
        else modeFromCoinbase = 'pool';
      }
      var finalMode = modeFromCoinbase || det.mode;
      var label = finalMode === 'solo'  ? '<span class="pill pill-ac"><span class="dot"></span>SOLO</span>'
                : finalMode === 'pool'  ? '<span class="pill pill-ok"><span class="dot"></span>POOLED</span>'
                :                          '<span class="pill" style="color:var(--dim)"><span class="dot"></span>UNKNOWN</span>';
      var sourceTag = hasReal
        ? '<span style="color:var(--state-active);font-size:9px;margin-left:6px;letter-spacing:1px">[verified ✓]</span>'
        : '<span style="color:var(--dim);font-size:9px;margin-left:6px;letter-spacing:1px">[url heuristic]</span>';
      modeEl.innerHTML = label + sourceTag + '<div style="color:var(--dim);font-size:10px;margin-top:4px">' + escText(det.url || '--') + '</div>';
    }
    if (payoutEl) payoutEl.textContent = det.payout;

    /* === Reward % rendering — coinbase-verified vs unverified === */
    if (rewardEl) {
      if (hasReal && userScriptHex) {
        var pct = userSats / totalSats * 100;
        rewardEl.textContent = pct.toFixed(2) + '%';
        rewardEl.style.cursor = 'help';
        if (userSats === 0) {
          rewardEl.style.color = 'var(--state-warn)';
          rewardEl.title = 'Your address is NOT in the coinbase outputs of this block. If solo mining, this is a red flag — the pool may be misdirecting block rewards.';
        } else if (pct >= 97) {
          rewardEl.style.color = 'var(--state-active)';
          rewardEl.title = 'Verified directly from coinbase outputs. Your address receives ' + pct.toFixed(2) + '% of this block reward.';
        } else if (pct >= 90) {
          rewardEl.style.color = 'var(--state-warn)';
          rewardEl.title = 'Solo-style payout below 97% — pool fee appears to be ' + (100 - pct).toFixed(2) + '%. Verify this matches your pool agreement.';
        } else if (pct > 0) {
          rewardEl.style.color = 'var(--accent)';
          rewardEl.title = 'Pool mining — your address receives ' + pct.toFixed(2) + '% of this block (pool typically distributes the rest via PPLNS/PPS/etc.).';
        }
      } else if (!userScriptHex) {
        rewardEl.textContent = '?';
        rewardEl.style.color = 'var(--dim)';
        rewardEl.title = 'Could not derive scriptpubkey from stratumUser ("' + escText(userAddr) + '"). Non-standard address format.';
      } else {
        rewardEl.textContent = 'decoding…';
        rewardEl.style.color = 'var(--dim)';
        rewardEl.title = 'Coinbase outputs not yet exposed by firmware. Awaiting Rust-side parser.';
      }
    }

    /* === Hint line === */
    if (hintEl) {
      if (det.note) {
        hintEl.textContent = det.note;
        hintEl.style.display = '';
      } else if (hasReal) {
        var pctReal = userSats / totalSats * 100;
        if (userSats === 0) {
          hintEl.innerHTML = '⚠ <b>Your address is NOT in this block\'s coinbase.</b> Either you\'re pool mining (PPLNS/PPS/etc. — payout is off-chain) or the pool isn\'t directing solo rewards to your stratum address. Verify your pool agreement.';
        } else if (pctReal >= 97) {
          hintEl.innerHTML = '✓ <b>Verified solo.</b> Your address <code style="font-size:10px">' + escText(userAddr) + '</code> receives <b>' + pctReal.toFixed(2) + '%</b> of this block. Pool fee: ' + (100 - pctReal).toFixed(2) + '%.';
        } else if (pctReal >= 90) {
          hintEl.innerHTML = '⚠ <b>Solo with elevated pool fee.</b> Expected near-100% to your address but observed <b>' + pctReal.toFixed(2) + '%</b>. Verify ' + escText(det.url) + ' fee schedule.';
        } else {
          hintEl.innerHTML = '<b>Pooled.</b> Your address receives <b>' + pctReal.toFixed(2) + '%</b> of the coinbase directly; the remainder goes to other addresses for distribution per ' + escText(det.payout) + '.';
        }
        hintEl.style.display = '';
      } else if (det.mode === 'solo' && !hasReal) {
        hintEl.innerHTML = 'Coinbase outputs not yet decoded by firmware. Once decoded, this section will show your <b>actual</b> percentage of the block reward (verified directly against the on-chain coinbase tx).';
        hintEl.style.display = '';
      } else if (det.mode === 'pool') {
        hintEl.innerHTML = '<b>Pooled mining</b> (' + escText(det.payout) + '). Your individual payout is settled off-chain by the pool — the on-chain coinbase outputs will not match your address. Use the pool dashboard for accurate earnings.';
        hintEl.style.display = '';
      } else {
        hintEl.style.display = 'none';
      }
    }

    /* === Outputs table === */
    if (outsEl) {
      if (outs.length === 0) {
        outsEl.innerHTML = '';
      } else {
        var rows = '';
        for (var i = 0; i < outs.length; i++) {
          var o = outs[i];
          var sh = (o.scriptHex || o.script || '').toLowerCase();
          var dec = scriptHexToAddress(sh);
          var addr = dec.address || ('(' + dec.type + ')');
          var sats = o.valueSats || o.value || 0;
          var btc  = (sats / 1e8).toFixed(8);
          var pct  = (totalSats > 0) ? (sats / totalSats * 100).toFixed(1) + '%' : '--';
          var isUser = sh === userScriptHex && userScriptHex !== '';
          var tdStyle = isUser ? ' style="color:var(--accent);font-weight:700"' : '';
          var rowMarker = isUser ? ' ◀ you' : '';
          rows += '<tr>'
            + '<td' + tdStyle + '><code style="font-size:10px;background:rgba(0,0,0,.3);padding:1px 5px;border-radius:3px">' + escText(addr) + '</code> <span style="color:var(--dim);font-size:9px">' + escText(dec.type) + '</span>' + (isUser ? ' <span style="color:var(--state-active);font-size:9px">' + rowMarker + '</span>' : '') + '</td>'
            + '<td style="text-align:right' + (isUser ? ';color:var(--accent);font-weight:700' : '') + '">' + btc + ' BTC</td>'
            + '<td style="text-align:right;font-weight:700' + (isUser ? ';color:var(--accent)' : '') + '">' + pct + '</td>'
            + '</tr>';
        }
        outsEl.innerHTML = '<div style="font-family:var(--mono);font-size:9px;color:var(--dim);letter-spacing:1.4px;font-weight:700;text-transform:uppercase;margin-bottom:6px">COINBASE OUTPUTS · ' + outs.length + '</div>'
          + '<table class="bm-tbl" style="font-size:11px"><thead><tr>'
          + '<td>address / type</td><td style="text-align:right">amount</td><td style="text-align:right">%</td>'
          + '</tr></thead><tbody>' + rows + '</tbody></table>';
      }
    }
  }

  function copyBlockHash() {
    var toast = (typeof window.showToast === 'function') ? window.showToast : function () {};
    if (!_blockCache || !_blockCache.prevHash) { toast('No hash to copy', 'warning'); return; }
    var h = _blockCache.prevHash;
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(h).then(function () { toast('Hash copied', 'ok'); }, function () { toast('Copy failed', 'error'); });
    } else {
      try {
        var t = document.createElement('textarea');
        t.value = h; document.body.appendChild(t); t.select();
        document.execCommand('copy'); document.body.removeChild(t);
        toast('Hash copied', 'ok');
      } catch (e) { toast('Copy failed', 'error'); }
    }
  }

  function pollBlockHero() {
    fetchBlockInfo(function (d) {
      if (!d) return;
      var ap = E('blockAgePill'); if (ap) ap.textContent = fmtBlockAge(d.receivedUnixMs);
      var bh = d.prevHash || '';
      var bs = E('blockHashShort');
      if (bs) bs.textContent = bh.length >= 12 ? (bh.slice(0, 10) + '…' + bh.slice(-8)) : 'mining…';
      // Freshness gate (review DASH-7): block-derived economic numbers
      // (REWARD, user%) must carry the same staleness honesty as the LIVE
      // pill — otherwise an operator can read a several-minute-old '100%'
      // reward as current. 120s threshold, shared with the pill below.
      var stale = !d.receivedUnixMs || (Date.now() - d.receivedUnixMs) > 120000;
      var totalSats = +d.coinbaseValueTotalSatoshis || 0;
      var rv = E('blockRewardVal');
      if (rv) {
        rv.textContent = totalSats > 0 ? (totalSats / 1e8).toFixed(4) : '--';
        rv.style.opacity = stale ? '0.45' : '1';
        rv.title = stale ? 'Stale (>120s since last block update)' : '';
      }
      // (review) blockUserPctVal dead write removed — the block hero's
      // block-grid4 is a fixed 2-col (2x2) grid with no cell for the
      // operator reward %, so this write had no element. The reward % stays
      // surfaced in the block modal (bmRewardPct), which is unchanged.
      var lp = E('blockLivePill');
      if (lp) {
        lp.style.opacity = stale ? '0.45' : '1';
        // BlockCard freshness rule (COMP-BLOCKTILE, component-contract §5):
        // live=true ⇒ LIVE pill; else STALE pill. The OS CurrentBlockCard
        // flips the pill LABEL to 'STALE' (liveStatus() label swap); axe must
        // match that honesty — opacity-dim alone let an operator read a
        // minutes-old 'LIVE'. Swap the pill's TEXT NODE only (preserve the
        // leading <span class="live-dot"> affordance) and toggle a 'stale'
        // class. The stale tint references the token ROLE var(--yellow), never
        // a literal hex, so the S1 token drift-validators stay green and no
        // dashboard.rs CSS edit is required.
        var lpTxt = lp.lastChild;
        var lpLabel = stale ? 'STALE' : 'LIVE';
        if (lpTxt && lpTxt.nodeType === 3) lpTxt.nodeValue = lpLabel;
        else lp.appendChild(document.createTextNode(lpLabel));
        if (stale) {
          lp.classList.add('stale');
          lp.style.color = 'var(--yellow)';
          lp.style.borderColor = 'var(--yellow)';
        } else {
          lp.classList.remove('stale');
          lp.style.color = '';
          lp.style.borderColor = '';
        }
      }
      // Network difficulty derived from nbits compact target.
      var dv = E('blockDiffVal'); var du = E('blockDiffUnit');
      if (dv) {
        var diff = nbitsToDifficulty(d.nbits);
        if (diff > 0) {
          var fmt = fmtDifficulty(diff);
          dv.textContent = fmt.val;
          if (du) du.textContent = fmt.unit;
        } else {
          dv.textContent = '--';
          if (du) du.textContent = '';
        }
      }
      // Approximate tx count from merkle branch depth: 2^N is the upper bound,
      // so we display "~2^N" to be honest about precision (Stratum doesn't
      // send the actual count). At depth 0 the block has just the coinbase.
      var tv = E('blockTxsVal');
      if (tv) {
        var mc = +d.merkleBranchCount || 0;
        if (mc > 0) {
          var approx = Math.pow(2, mc);
          tv.textContent = approx >= 1000 ? '~' + (approx / 1000).toFixed(1) + 'K' : '~' + approx;
        } else {
          tv.textContent = '1';
        }
      }
    });
  }
  /* nbitsToDifficulty(hex) — convert Bitcoin compact target to network difficulty.
     Uses the canonical formula: diff = max_target / current_target where
     max_target = 0x00ffff * 2^208 (genesis nbits 0x1d00ffff). */
  function nbitsToDifficulty(hex) {
    if (!hex || typeof hex !== 'string') return 0;
    var n = parseInt(hex, 16);
    if (!n || isNaN(n)) return 0;
    var exp = (n >>> 24) & 0xff;
    var mant = n & 0xffffff;
    if (mant === 0) return 0;
    return (0xffff / mant) * Math.pow(2, 8 * (0x1d - exp));
  }
  /* fmtDifficulty(d) — render network diff with K/M/G/T/P units. */
  function fmtDifficulty(d) {
    if (d >= 1e12) return { val: (d / 1e12).toFixed(2), unit: 'T' };
    if (d >= 1e9)  return { val: (d / 1e9).toFixed(2),  unit: 'G' };
    if (d >= 1e6)  return { val: (d / 1e6).toFixed(2),  unit: 'M' };
    if (d >= 1e3)  return { val: (d / 1e3).toFixed(2),  unit: 'K' };
    return { val: d.toFixed(0), unit: '' };
  }

  // ── Chip drill-down modal (Phase 2.F) — reuses .bm-backdrop shell ────
  function openChipDetail(chip, info) {
    var back = E('blockModalBack'); if (!back) return;
    var pane = back.querySelector('.bm.bm-chip');
    if (!pane) {
      pane = document.createElement('div');
      pane.className = 'bm bm-chip';
      back.appendChild(pane);
    }
    /* A11y (TD-7): the chip drill-down is a modal dialog (was role="document").
       Set role="dialog" + aria-modal on every open (also fixes a cached pane that
       was created as role="document" before) so AT treats the rest of the page as
       inert; the aria-label is set below once the chip number is known. */
    pane.setAttribute('role', 'dialog');
    pane.setAttribute('aria-modal', 'true');
    // Hide the block .bm sibling, show the chip one.
    back.querySelectorAll('.bm').forEach(function (el) { el.style.display = 'none'; });
    pane.style.display = 'block';

    var cid = (chip.id != null) ? (chip.id + 1) : '?';
    pane.setAttribute('aria-label', 'ASIC chip ' + cid + ' detail');
    var st = (chip.status || 'unknown').toString();
    var temp = (typeof chip.temp === 'number' && isFinite(chip.temp)) ? chip.temp.toFixed(1) + '°C' : '--';
    var freq = (chip.frequency != null) ? chip.frequency + ' MHz' : '--';
    var volt = (chip.voltage != null) ? chip.voltage + ' mV' : '--';
    var hwErr = (chip.hwErrors != null) ? chip.hwErrors : 0;
    var shares = chip.shares || 0;
    var lastShare = chip.lastShareUnix ? fmtUnixUtc(chip.lastShareUnix) : '--';
    var model = (info && info.ASICModel) || 'BM????';
    var siliconSvg = (window.daxChips && typeof window.daxChips.chipSiliconSvg === 'function')
      ? window.daxChips.chipSiliconSvg(cid)
      : '<div style="color:var(--dim);font-family:var(--mono)">silicon glyph unavailable</div>';
    var asicCount = (info && info.asicCount) || '?';

    pane.innerHTML = ''
      + '<button type="button" class="bm-x" aria-label="Close" data-dax-close="1">&times;</button>'
      + '<div class="bm-head"><div>'
        + '<div class="bm-eyebrow"><span class="pill pill-ac"><span class="dot"></span>CHIP DETAIL</span><span class="badge">' + escAttr(model) + '</span></div>'
        + '<div class="bm-title"><span class="bm-glyph">&#9670;</span> Chip <b>#' + (cid < 10 ? '0' + cid : cid) + '</b></div>'
      + '</div>'
      + '<div class="bm-reward"><div class="k">STATE</div><div class="v" style="font-size:18px">' + escText(st.toUpperCase()) + '</div></div></div>'
      + '<div class="bm-chip-svg-wrap">' + siliconSvg + '</div>'
      + '<div class="bm-chip-meta">'
        + '<div class="bm-cb"><div class="k">TEMP</div><div class="v lg hot">' + escText(temp) + '</div></div>'
        + '<div class="bm-cb"><div class="k">FREQ</div><div class="v lg">' + escText(freq) + '</div></div>'
        + '<div class="bm-cb"><div class="k">VOLTAGE</div><div class="v lg">' + escText(volt) + '</div></div>'
        + '<div class="bm-cb"><div class="k">LOCAL NONCES</div><div class="v">' + escText(shares) + '</div></div>'
        + '<div class="bm-cb"><div class="k">HW ERR</div><div class="v">' + escText(hwErr) + '</div></div>'
        + '<div class="bm-cb"><div class="k">LAST SHARE</div><div class="v sm">' + escText(lastShare) + '</div></div>'
      + '</div>'
      + '<div class="bm-chip-board-note">UART chain position ' + escText(cid) + ' of ' + escText(asicCount) + '. Daisy-chain RX/TX shared on GPIO 17/18.</div>'
      + '<div class="bm-foot"><button class="btn primary" type="button" data-dax-close="1">Close</button></div>';

    pane.querySelectorAll('[data-dax-close]').forEach(function (el) { el.onclick = closeModal; });

    document.body.style.overflow = 'hidden';
    back.classList.add('show');
    _blockKeydown = function (ev) { if (ev.key === 'Escape') closeModal(); };
    document.addEventListener('keydown', _blockKeydown);
    /* A11y (TD-7): move keyboard focus into the dialog (the close button) on
       open so keyboard/AT users land inside the modal, not behind it. */
    var chipCloseBtn = pane.querySelector('.bm-x');
    if (chipCloseBtn && typeof chipCloseBtn.focus === 'function') chipCloseBtn.focus();
  }

  function escText(s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return ({ '&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;' }[c]); }); }
  function escAttr(s) { return escText(s); }

  /* ── Bech32 / Base58 — address ↔ scriptpubkey for coinbase-share verify ─
     BIP-173 (bech32, witness v0 P2WPKH/P2WSH) + BIP-350 (bech32m, witness v1+
     P2TR). Base58Check for legacy P2PKH/P2SH. Implementations vendored —
     ~120 LOC total — to avoid CDN dep on a firmware-served dashboard. */
  var BECH32_ALPHA = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
  function bech32Polymod(values) {
    var GEN = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
    var chk = 1;
    for (var i = 0; i < values.length; i++) {
      var top = chk >>> 25;
      chk = ((chk & 0x1ffffff) << 5) ^ values[i];
      for (var j = 0; j < 5; j++) if ((top >>> j) & 1) chk ^= GEN[j];
    }
    return chk;
  }
  function bech32HrpExpand(hrp) {
    var ret = [];
    for (var i = 0; i < hrp.length; i++) ret.push(hrp.charCodeAt(i) >>> 5);
    ret.push(0);
    for (var i = 0; i < hrp.length; i++) ret.push(hrp.charCodeAt(i) & 31);
    return ret;
  }
  function bech32Decode(addr) {
    var lower = addr.toLowerCase();
    var pos = lower.lastIndexOf('1');
    if (pos < 1 || pos + 7 > lower.length) return null;
    var hrp = lower.substring(0, pos);
    var data = [];
    for (var i = pos + 1; i < lower.length; i++) {
      var d = BECH32_ALPHA.indexOf(lower.charAt(i));
      if (d === -1) return null;
      data.push(d);
    }
    var chk = bech32Polymod(bech32HrpExpand(hrp).concat(data));
    var encoding = chk === 1 ? 'bech32' : chk === 0x2bc830a3 ? 'bech32m' : null;
    if (!encoding) return null;
    return { hrp: hrp, data: data.slice(0, data.length - 6), encoding: encoding };
  }
  function convertBits(data, fromBits, toBits, pad) {
    var acc = 0, bits = 0, ret = [], maxv = (1 << toBits) - 1;
    for (var i = 0; i < data.length; i++) {
      var v = data[i];
      if (v < 0 || (v >>> fromBits) !== 0) return null;
      acc = (acc << fromBits) | v;
      bits += fromBits;
      while (bits >= toBits) { bits -= toBits; ret.push((acc >>> bits) & maxv); }
    }
    if (pad) { if (bits > 0) ret.push((acc << (toBits - bits)) & maxv); }
    else if (bits >= fromBits || ((acc << (toBits - bits)) & maxv)) return null;
    return ret;
  }
  function toHex(bytes) {
    var s = '';
    for (var i = 0; i < bytes.length; i++) {
      var h = (bytes[i] & 0xff).toString(16);
      s += (h.length < 2 ? '0' : '') + h;
    }
    return s;
  }
  /* Base58 alphabet for legacy P2PKH/P2SH. */
  var B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
  function base58Decode(s) {
    var bytes = [0];
    for (var i = 0; i < s.length; i++) {
      var c = B58.indexOf(s.charAt(i));
      if (c === -1) return null;
      for (var j = 0; j < bytes.length; j++) bytes[j] *= 58;
      bytes[0] += c;
      var carry = 0;
      for (var j = 0; j < bytes.length; j++) {
        bytes[j] += carry;
        carry = bytes[j] >>> 8;
        bytes[j] &= 0xff;
      }
      while (carry) { bytes.push(carry & 0xff); carry >>>= 8; }
    }
    /* Leading '1's → leading 0x00 bytes */
    for (var k = 0; k < s.length && s.charAt(k) === '1'; k++) bytes.push(0);
    bytes.reverse();
    return bytes;
  }
  /* SHA-256 (sync, small). Used only for base58check verification. */
  function sha256(bytes) {
    /* Browsers expose async crypto.subtle, but for short payload and sync API
       we use a tiny pure-JS impl. ~50 LOC. */
    var H = [0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19];
    var K = [0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2];
    var L = bytes.length, bitLen = L * 8;
    var msg = bytes.slice(); msg.push(0x80);
    while ((msg.length % 64) !== 56) msg.push(0);
    for (var i = 7; i >= 0; i--) msg.push((bitLen / Math.pow(2, i*8)) & 0xff);
    var W = new Array(64);
    function ROTR(n, x) { return (x >>> n) | (x << (32 - n)); }
    for (var c = 0; c < msg.length; c += 64) {
      for (var i = 0; i < 16; i++) W[i] = (msg[c+i*4]<<24)|(msg[c+i*4+1]<<16)|(msg[c+i*4+2]<<8)|msg[c+i*4+3];
      for (var i = 16; i < 64; i++) {
        var s0 = ROTR(7, W[i-15]) ^ ROTR(18, W[i-15]) ^ (W[i-15] >>> 3);
        var s1 = ROTR(17, W[i-2]) ^ ROTR(19, W[i-2]) ^ (W[i-2] >>> 10);
        W[i] = (W[i-16] + s0 + W[i-7] + s1) | 0;
      }
      var a=H[0],b=H[1],e=H[2],f=H[3],g=H[4],h=H[5],ii=H[6],jj=H[7];
      for (var i = 0; i < 64; i++) {
        var S1 = ROTR(6, g) ^ ROTR(11, g) ^ ROTR(25, g);
        var ch = (g & h) ^ (~g & ii);
        var t1 = (jj + S1 + ch + K[i] + W[i]) | 0;
        var S0 = ROTR(2, a) ^ ROTR(13, a) ^ ROTR(22, a);
        var mj = (a & b) ^ (a & e) ^ (b & e);
        var t2 = (S0 + mj) | 0;
        jj = ii; ii = g; g = f; f = e; e = (h + t1) | 0; h = b; b = a; a = (t1 + t2) | 0;
      }
      H[0]=(H[0]+a)|0;H[1]=(H[1]+b)|0;H[2]=(H[2]+e)|0;H[3]=(H[3]+f)|0;H[4]=(H[4]+g)|0;H[5]=(H[5]+h)|0;H[6]=(H[6]+ii)|0;H[7]=(H[7]+jj)|0;
    }
    var out = [];
    for (var i = 0; i < 8; i++) for (var j = 3; j >= 0; j--) out.push((H[i] >>> (j*8)) & 0xff);
    return out;
  }
  function base58CheckDecode(s) {
    var raw = base58Decode(s);
    if (!raw || raw.length < 5) return null;
    var payload = raw.slice(0, raw.length - 4);
    var checksum = raw.slice(raw.length - 4);
    var hashHash = sha256(sha256(payload));
    for (var i = 0; i < 4; i++) if (checksum[i] !== hashHash[i]) return null;
    return payload;
  }

  /* address → scriptpubkey hex. Returns null on unrecognized format. */
  function addressToScriptHex(addr) {
    if (!addr || typeof addr !== 'string') return null;
    var lower = addr.toLowerCase();
    /* SegWit (bech32 / bech32m) */
    if (lower.indexOf('bc1') === 0 || lower.indexOf('tb1') === 0) {
      var dec = bech32Decode(addr);
      if (!dec) return null;
      var ver = dec.data[0];
      var prog = convertBits(dec.data.slice(1), 5, 8, false);
      if (!prog) return null;
      /* v0 must be bech32; v1+ must be bech32m */
      if (ver === 0 && dec.encoding !== 'bech32') return null;
      if (ver !== 0 && dec.encoding !== 'bech32m') return null;
      if (ver < 0 || ver > 16) return null;
      if (ver === 0 && prog.length !== 20 && prog.length !== 32) return null;
      var op = ver === 0 ? 0x00 : (0x50 + ver);  /* OP_0 = 0x00, OP_N = 0x50+N */
      var script = [op, prog.length].concat(prog);
      return toHex(script);
    }
    /* Legacy P2PKH (1...) / P2SH (3...) */
    if (addr.charAt(0) === '1' || addr.charAt(0) === '3') {
      var p = base58CheckDecode(addr);
      if (!p || p.length !== 21) return null;
      var ver = p[0];
      var hash = p.slice(1);
      if (ver === 0x00) {  /* P2PKH */
        return toHex([0x76, 0xa9, 0x14].concat(hash).concat([0x88, 0xac]));
      } else if (ver === 0x05) {  /* P2SH */
        return toHex([0xa9, 0x14].concat(hash).concat([0x87]));
      }
    }
    return null;
  }

  /* Decode a known scripthex back to an address (best effort). Returns
     {address, type} or {address: null, type} if not standard. */
  function scriptHexToAddress(hex) {
    if (!hex || typeof hex !== 'string') return { address: null, type: 'unknown' };
    var b = [];
    for (var i = 0; i + 1 < hex.length; i += 2) b.push(parseInt(hex.substr(i, 2), 16));
    /* P2WPKH: 0x00 0x14 + 20 bytes (22 bytes total) */
    if (b.length === 22 && b[0] === 0x00 && b[1] === 0x14) {
      return { address: encodeBech32('bc', 0, b.slice(2), 'bech32'), type: 'P2WPKH' };
    }
    /* P2WSH: 0x00 0x20 + 32 bytes */
    if (b.length === 34 && b[0] === 0x00 && b[1] === 0x20) {
      return { address: encodeBech32('bc', 0, b.slice(2), 'bech32'), type: 'P2WSH' };
    }
    /* P2TR: 0x51 0x20 + 32 bytes */
    if (b.length === 34 && b[0] === 0x51 && b[1] === 0x20) {
      return { address: encodeBech32('bc', 1, b.slice(2), 'bech32m'), type: 'P2TR' };
    }
    /* P2PKH: 0x76 0xa9 0x14 + 20 + 0x88 0xac */
    if (b.length === 25 && b[0] === 0x76 && b[1] === 0xa9 && b[2] === 0x14 && b[23] === 0x88 && b[24] === 0xac) {
      return { address: encodeBase58Check([0x00].concat(b.slice(3, 23))), type: 'P2PKH' };
    }
    /* P2SH: 0xa9 0x14 + 20 + 0x87 */
    if (b.length === 23 && b[0] === 0xa9 && b[1] === 0x14 && b[22] === 0x87) {
      return { address: encodeBase58Check([0x05].concat(b.slice(2, 22))), type: 'P2SH' };
    }
    /* OP_RETURN */
    if (b.length > 0 && b[0] === 0x6a) return { address: null, type: 'OP_RETURN' };
    return { address: null, type: 'non-standard' };
  }

  function encodeBech32(hrp, ver, prog, enc) {
    var data = [ver].concat(convertBits(prog, 8, 5, true));
    var values = bech32HrpExpand(hrp).concat(data).concat([0,0,0,0,0,0]);
    var chkConst = enc === 'bech32m' ? 0x2bc830a3 : 1;
    var pm = bech32Polymod(values) ^ chkConst;
    var checksum = [];
    for (var i = 0; i < 6; i++) checksum.push((pm >>> (5 * (5 - i))) & 31);
    var combined = data.concat(checksum);
    var out = hrp + '1';
    for (var i = 0; i < combined.length; i++) out += BECH32_ALPHA.charAt(combined[i]);
    return out;
  }
  function encodeBase58Check(payload) {
    var hh = sha256(sha256(payload));
    var full = payload.concat(hh.slice(0, 4));
    /* Convert to base58 */
    var num = full.slice();
    var out = '';
    while (num.length > 0) {
      var rem = 0, q = [];
      for (var i = 0; i < num.length; i++) {
        var acc = rem * 256 + num[i];
        var d = (acc / 58) | 0;
        rem = acc % 58;
        if (q.length > 0 || d > 0) q.push(d);
      }
      out = B58.charAt(rem) + out;
      num = q;
    }
    /* Leading 0x00 bytes → leading '1's */
    for (var k = 0; k < full.length && full[k] === 0; k++) out = '1' + out;
    return out;
  }

  // ── Public namespace + back-compat globals ───────────────────────────
  window.daxModal = {
    openBlockModal: openBlockModal,
    closeModal: closeModal,
    closeBlockModal: closeModal,
    openChipDetail: openChipDetail,
    copyBlockHash: copyBlockHash,
    pollBlockHero: pollBlockHero,
    fetchBlockInfo: fetchBlockInfo,
    renderBlockModal: renderBlockModal,
  };
  // Legacy globals — preserve symbol-for-symbol compatibility with the
  // pre-extraction inline JS (`onclick="openBlockModal()"` markup, etc.).
  window.openBlockModal = openBlockModal;
  window.closeBlockModal = closeModal;
  window.copyBlockHash = copyBlockHash;
  window.pollBlockHero = pollBlockHero;
  window.fetchBlockInfo = fetchBlockInfo;
  window.renderBlockModal = renderBlockModal;
  window.subsidyForHeight = subsidyForHeight;
  window.fmtBlockAge = fmtBlockAge;
  window.fmtUnixUtc = fmtUnixUtc;
  window.truncHash = truncHash;
  window.fmtDiffShort = fmtDiffShort;
  // _blockCache mirror so legacy reads/writes still resolve.
  Object.defineProperty(window, '_blockCache', {
    get: function () { return _blockCache; },
    set: function (v) { _blockCache = v; },
    configurable: true,
  });
})();
