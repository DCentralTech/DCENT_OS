// DCENT_axe Web Dashboard v8 — Sidebar Navigation, AxeOS-Inspired
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// V8 Redesign:
// - Left sidebar navigation (8 pages) replacing top tabs
// - 4-tier dark surface system (void/base/raised/overlay)
// - Dark theme ONLY — no light mode
// - D-Central design: Bitcoin orange earned, data-is-hero, terminal heritage
// - Network page: WiFi SSID/password change + scan
// - Dedicated Logs page with filter/pause/clear
// - Theme page: accent color picker
// - Swarm page: placeholder
// - All existing features preserved: achievements, creature, chart, OTA, autotuner
//
// ════════════════════════════════════════════════════════════════════════════
// MODULAR DASHBOARD CONTRACT (Phase 2.A-3.2 — RALPH loop 11, 2026-04-28)
// ════════════════════════════════════════════════════════════════════════════
// The "good" dashboard logic lives in `dcentaxe/src/dashboard/*.{js,css}` and
// is NOT optional. This file glues it to the firmware HTTP server via three
// rules. Break any one and the user perceives a regression.
//
//   1. SERVE  — every file in `dashboard/` is `include_str!`-baked here and
//               served at `/dashboard/<file>` by `register_static()`.
//   2. LOAD   — `<link rel="stylesheet">` tags load the CSS *after* the
//               inline `<style>` block (so component rules win the cascade).
//               `<script src=>` tags load the JS *before* the inline
//               `<script>` block (so window.state, defineComponent are
//               defined when update(d) fires).
//   3. MOUNT  — every component has a `<div data-component="NAME">` somewhere
//               in DASHBOARD_HTML. Components self-mount on first
//               `state.set('info', d)`.
//
// CRITICAL: do NOT redeclare a function with the same name as a window.*
// global exposed by the modular files (e.g. inline `function renderChips()`
// will shadow `window.renderChips` from asic-chips.js because hoisted
// declarations override window properties). When in doubt, grep:
//   `grep -nH "^\s*window\." dcentaxe/src/dashboard/*.js`
// and ensure no inline function declaration matches.
//
// History: 2026-04-25 the modular files were extracted (Phase 2.A–3.2)
// but never wired. 2026-04-28 the user reported "logo wrong / chips card
// missing / mining core cube gone / block modal missing reward%". RALPH
// loop 11 wired everything and added this contract comment.
// ════════════════════════════════════════════════════════════════════════════

use esp_idf_svc::http::server::EspHttpServer;
use esp_idf_svc::http::Method;
use log::*;

/// Register the dashboard route on the HTTP server.
/// Modular Phase 2.A–3.2 dashboard assets. Each constant is the raw file
/// content baked into the firmware via `include_str!`. Kept ordered so the
/// load order in DASHBOARD_HTML mirrors the dependency graph: framework →
/// api/state bridge → component implementations.
const DASH_CSS_TOKENS: &str = include_str!("dashboard/tokens.css");
const DASH_CSS_CORE: &str = include_str!("dashboard/core.css");
const DASH_CSS_COMPONENTS: &str = include_str!("dashboard/components.css");
// NOTE: stats.css / flow.css are intentionally NOT baked. Their components
// (dcent-stat, flow-graph) have no mount point in DASHBOARD_HTML (the inline
// shell renders the equivalent stat tiles + chart itself), so serving them
// was ~16 KB of OTA payload that rendered nothing. Dropped per review DASH-1.
const DASH_CSS_BLOCK_TILE: &str = include_str!("dashboard/block-tile.css");
const DASH_CSS_ASIC_CHIPS: &str = include_str!("dashboard/asic-chips.css");

const DASH_JS_FRAMEWORK: &str = include_str!("dashboard/framework.js");
const DASH_JS_API: &str = include_str!("dashboard/api.js");
const DASH_JS_CORE: &str = include_str!("dashboard/core.js");
// NOTE: stats.js / flow.js are intentionally NOT baked — see the stats.css /
// flow.css note above. Their dcent-stat / flow-graph components are never
// mounted, and flow.js's window.addShareParticle / window.setThermal helpers
// are unreferenced. Dropped per review DASH-1.
const DASH_JS_BLOCK_TILE: &str = include_str!("dashboard/block-tile.js");
const DASH_JS_ASIC_CHIPS: &str = include_str!("dashboard/asic-chips.js");

fn register_static(
    server: &mut EspHttpServer,
    path: &'static str,
    body: &'static str,
    content_type: &'static str,
) {
    server
        .fn_handler(
            path,
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let mut resp = req.into_response(
                    200,
                    None,
                    &[
                        ("Content-Type", content_type),
                        ("Cache-Control", "public, max-age=300"),
                        ("X-Content-Type-Options", "nosniff"),
                    ],
                )?;
                let _ = resp.write(body.as_bytes());
                Ok(())
            },
        )
        .unwrap_or_else(|_| panic!("Failed to register GET {}", path));
}

pub fn register_dashboard(server: &mut EspHttpServer) {
    server
        .fn_handler(
            "/",
            Method::Get,
            |req| -> Result<(), Box<dyn std::error::Error>> {
                let mut resp = req.into_response(
                    200,
                    None,
                    &[
                        ("Content-Type", "text/html; charset=utf-8"),
                        ("Cache-Control", "no-cache"),
                        ("X-Frame-Options", "DENY"),
                        ("X-Content-Type-Options", "nosniff"),
                    ],
                )?;
                let _ = resp.write(DASHBOARD_HTML.as_bytes());
                // LoRa mesh panel — appended ONLY under the `lora` feature, so a
                // non-LoRa image serves byte-identical HTML (self-injecting inline
                // script; renders nothing until /api/system/info proves the radio).
                #[cfg(feature = "lora")]
                let _ = resp.write(crate::lora_task::LORA_DASHBOARD_PANEL_HTML.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register GET /");

    server
        .fn_handler(
            "/index.html",
            Method::Get,
            |req| -> Result<(), Box<dyn std::error::Error>> {
                let mut resp = req.into_response(
                    200,
                    None,
                    &[
                        ("Content-Type", "text/html; charset=utf-8"),
                        ("Cache-Control", "no-cache"),
                        ("X-Frame-Options", "DENY"),
                        ("X-Content-Type-Options", "nosniff"),
                    ],
                )?;
                let _ = resp.write(DASHBOARD_HTML.as_bytes());
                // LoRa mesh panel — appended ONLY under the `lora` feature (see the
                // GET / handler above); byte-identical HTML on a non-LoRa image.
                #[cfg(feature = "lora")]
                let _ = resp.write(crate::lora_task::LORA_DASHBOARD_PANEL_HTML.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register GET /index.html");

    // ── Modular dashboard assets (Phase 2.A–3.2) ──
    // CSS
    let css = "text/css; charset=utf-8";
    let js = "application/javascript; charset=utf-8";
    register_static(server, "/dashboard/tokens.css", DASH_CSS_TOKENS, css);
    register_static(server, "/dashboard/core.css", DASH_CSS_CORE, css);
    register_static(
        server,
        "/dashboard/components.css",
        DASH_CSS_COMPONENTS,
        css,
    );
    // stats.css / flow.css dropped (orphaned components, review DASH-1)
    register_static(
        server,
        "/dashboard/block-tile.css",
        DASH_CSS_BLOCK_TILE,
        css,
    );
    register_static(
        server,
        "/dashboard/asic-chips.css",
        DASH_CSS_ASIC_CHIPS,
        css,
    );
    // JS
    register_static(server, "/dashboard/framework.js", DASH_JS_FRAMEWORK, js);
    register_static(server, "/dashboard/api.js", DASH_JS_API, js);
    register_static(server, "/dashboard/core.js", DASH_JS_CORE, js);
    // stats.js / flow.js dropped (orphaned components, review DASH-1)
    register_static(server, "/dashboard/block-tile.js", DASH_JS_BLOCK_TILE, js);
    register_static(server, "/dashboard/asic-chips.js", DASH_JS_ASIC_CHIPS, js);

    info!("Web dashboard v8 + modular Phase 2.A-3.2 components registered");
}

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html><head><meta charset=UTF-8><meta name=viewport content="width=device-width,initial-scale=1,viewport-fit=cover">
<meta name=application-name content="AxeOS Compatible">
<title>DCENT_axe | AxeOS-compatible</title>
<!-- Modular dashboard CSS is loaded AFTER the inline <style> below
     (see closing </style> tag) so that componentized Phase 2.A-3.2 rules
     (chip-grid, chip-tile, mining-core, block-tile)
     authoritatively win the cascade over any legacy inline equivalents. -->
<style>
:root{
--s-void:#050709;--s-base:#0a0e14;--s-raised:#101820;--s-overlay:#182030;
--orange:#FAA500;--accent:#FAA500;--orange-bitcoin:#F7931A;
--green:#34d399;--red:#f87171;--yellow:#fbbf24;--cyan:#22d3ee;--tgreen:#00ff41;
--text:#e8ecf2;--dim:#6b7a8d;--muted:#3a4555;--border:rgba(255,255,255,0.07);
--glow:rgba(250,165,0,0.08);--glow-green:rgba(52,211,153,0.08);
--font:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;
--mono:'SF Mono','Fira Code','Cascadia Code',Consolas,monospace;
--jbmono:'JetBrains Mono','SF Mono','Fira Code',Consolas,monospace;
--archivo:'Archivo Black','Impact','Helvetica Neue',sans-serif;
--gothic:'Pirata One','UnifrakturMaguntia','UnifrakturCook','Old English Text MT',serif;
--term-green:#3ddc7a;
--sidebar-w:210px;--sidebar-c:56px;--topbar-h:48px;
--radius:14px;--radius-sm:8px;
--orange-50:#ffc94d;--orange-60:#f59e0b;--orange-70:#d97706;--border-hi:rgba(255,255,255,0.12);--glow-red:rgba(248,113,113,0.10);
--temp-cool:#22d3ee;--temp-normal:#34d399;--temp-warm:#fbbf24;--temp-danger:#f87171;
--blackletter:'Old English Text MT','UnifrakturCook','Luminari','Apple Chancery','Georgia','Times New Roman',serif;
--t-hero:60px;--t-display:42px;--t-title:22px;--t-stat:22px;--t-h3:16px;--t-body:13px;--t-meta:11px;--t-label:10px;--t-micro:9px;
--sp-1:4px;--sp-2:8px;--sp-3:12px;--sp-4:16px;--sp-5:20px;--sp-6:24px;--sp-8:32px;
--radius-pill:999px;--shadow-card:0 4px 24px rgba(0,0,0,0.40),inset 0 1px 0 rgba(255,255,255,0.04);
--shadow-hover:0 12px 40px rgba(0,0,0,0.50),0 0 24px var(--glow),inset 0 1px 0 rgba(255,255,255,0.06);
--shadow-hero:0 8px 40px rgba(0,0,0,0.50),inset 0 1px 0 rgba(255,255,255,0.05);--shadow-float:0 8px 32px rgba(0,0,0,0.50),0 0 0 1px var(--border);
--ease-out:cubic-bezier(.22,1,.36,1);--ease-pop:cubic-bezier(.34,1.56,.64,1);--dur-fast:.15s;--dur-base:.22s;--dur-slow:.55s;
}
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:var(--font);background:var(--s-void);color:var(--text);min-height:100vh;font-size:13px;-webkit-font-smoothing:antialiased;-moz-osx-font-smoothing:grayscale;text-rendering:optimizeLegibility}
body::before{content:'';position:fixed;inset:0;pointer-events:none;z-index:0;background:radial-gradient(ellipse at 20% 50%,var(--glow),transparent 60%),radial-gradient(ellipse at 80% 20%,rgba(255,255,255,0.01),transparent 50%)}
::selection{background:var(--accent);color:#000}
*{scrollbar-width:thin;scrollbar-color:var(--muted) transparent}
::-webkit-scrollbar{width:5px}::-webkit-scrollbar-track{background:transparent}::-webkit-scrollbar-thumb{background:var(--muted);border-radius:3px}::-webkit-scrollbar-thumb:hover{background:var(--dim)}

/* ── Animations ── */
@keyframes fadeIn{from{opacity:0;transform:translateY(6px)}to{opacity:1;transform:none}}
@keyframes pulse{0%,100%{opacity:1}50%{opacity:.5}}
@keyframes slideIn{from{opacity:0;transform:translateX(20px)}to{opacity:1;transform:none}}
@keyframes shimmer{0%{background-position:-400px 0}100%{background-position:400px 0}}
@keyframes logNew{0%{background:rgba(0,255,65,0.1)}100%{background:transparent}}
.loading{animation:pulse 1.5s ease-in-out infinite;opacity:.4}
.loaded{animation:fadeIn .3s ease}
.sparkline{height:24px;margin-top:6px;opacity:.5;transition:opacity .2s}
.stat:hover .sparkline{opacity:.9}
.sparkline svg{display:block;width:100%;height:100%}
.hero-trend{display:inline-block;font-size:16px;margin-left:8px;vertical-align:middle;transition:color .3s}
.hero-trend.up{color:var(--accent)}.hero-trend.down{color:var(--red)}.hero-trend.flat{color:var(--dim)}
.eff-badge{display:inline-block;font-size:9px;padding:2px 8px;border-radius:10px;font-weight:600;text-transform:uppercase;letter-spacing:.5px;vertical-align:middle}
.eff-badge.excellent{background:rgba(52,211,153,0.15);color:var(--green)}.eff-badge.good{background:var(--glow);color:var(--accent)}.eff-badge.average{background:rgba(251,191,36,0.15);color:var(--yellow)}.eff-badge.poor{background:rgba(248,113,113,0.15);color:var(--red)}
.status-badge{display:inline-block;font-size:9px;padding:1px 8px;border-radius:10px;font-weight:600;text-transform:uppercase;letter-spacing:.5px}
.status-badge.ok{background:rgba(52,211,153,0.12);color:var(--green)}.status-badge.warn{background:rgba(251,191,36,0.12);color:var(--yellow)}.status-badge.err{background:rgba(248,113,113,0.12);color:var(--red)}
@keyframes fanSpin{from{transform:rotate(0deg)}to{transform:rotate(360deg)}}

/* ── Scanlines (subtle CRT) ── */
body::after{content:'';position:fixed;inset:0;pointer-events:none;z-index:9999;background:repeating-linear-gradient(0deg,transparent,transparent 2px,rgba(0,0,0,0.018) 2px,rgba(0,0,0,0.018) 4px)}

/* ── Sidebar ── */
.sb{position:fixed;left:0;top:0;bottom:0;width:var(--sidebar-w);background:rgba(10,14,20,0.75);border-right:1px solid var(--border);z-index:100;display:flex;flex-direction:column;backdrop-filter:blur(24px) saturate(1.8);-webkit-backdrop-filter:blur(24px) saturate(1.8);transition:width .2s;box-shadow:1px 0 24px rgba(0,0,0,0.5),inset -1px 0 0 rgba(255,255,255,0.04)}
.sb-head{padding:18px 14px 14px;border-bottom:1px solid var(--border)}
/* ── Terminal lockup logo (canonical design-system, theme-aware) ──
   All orange-coded surfaces use var(--accent) / var(--glow) so the Theme
   page accent picker re-tints the logo (DCENT stays white intentionally;
   the $ prompt stays green by convention). */
.sb-lockup{position:relative;display:flex;align-items:center;gap:8px;padding:9px 10px;background:rgba(10,13,18,0.6);border:1px solid var(--accent);border-radius:3px;box-shadow:inset 0 0 0 1px rgba(255,255,255,0.02),0 0 0 1px rgba(0,0,0,0.4),0 0 24px var(--glow);margin-bottom:10px;border-color:color-mix(in srgb,var(--accent) 35%,transparent)}
.sb-lockup::before,.sb-lockup::after{content:'';position:absolute;width:8px;height:8px;border:1px solid var(--accent);opacity:0.7;pointer-events:none}
.sb-lockup::before{top:-1px;left:-1px;border-right:none;border-bottom:none}
.sb-lockup::after{bottom:-1px;right:-1px;border-left:none;border-top:none}
.sb-prompt{font-family:var(--jbmono);font-size:13px;font-weight:700;color:var(--term-green);text-shadow:0 0 8px rgba(61,220,122,0.5);user-select:none;flex:0 0 auto}
.sb-mark{flex:0 0 auto;width:24px;height:24px;color:var(--accent);filter:drop-shadow(0 0 6px var(--glow))}
.sb-mark .mol-bond{stroke:#0b0e13;stroke-linecap:round}
.sb-mark .mol-orb{fill:currentColor}
.sb-mark .mol-shine{fill:#ffefd0;opacity:0.92;mix-blend-mode:screen}
.sb-word{display:flex;align-items:baseline;gap:0;line-height:1;flex:1;justify-content:flex-end;min-width:0}
.sb-dcent{font-family:var(--archivo);font-weight:900;font-size:20px;letter-spacing:-0.5px;color:#f2f3f5;text-shadow:0 0 10px rgba(242,243,245,0.18);-webkit-text-stroke:0.3px #f2f3f5}
.sb-under{color:var(--accent);font-family:var(--archivo);font-weight:900;font-size:20px;margin:0 1px;text-shadow:0 0 10px var(--accent);animation:sbBlink 1.2s steps(2) infinite}
@keyframes sbBlink{0%,55%{opacity:1}56%,100%{opacity:0.35}}
.sb-axe{font-family:var(--gothic);font-weight:400;font-size:26px;color:var(--accent);letter-spacing:0.5px;text-shadow:0 0 12px var(--glow),0 0 2px var(--accent),0 1px 0 rgba(0,0,0,0.4);position:relative;top:1px;-webkit-text-stroke:0.4px var(--accent)}
.sb-tag{font-family:var(--jbmono);font-size:8px;color:var(--dim);letter-spacing:1.6px;text-transform:uppercase;text-align:center;padding:0 4px}
.sb-tag .br{color:var(--accent);opacity:0.7}
.sb-hr{font-family:var(--mono);font-size:11px;color:var(--accent);margin-top:8px;display:flex;align-items:center;gap:6px}
.sb-dot{width:8px;height:8px;border-radius:50%;background:var(--muted);flex-shrink:0;transition:all .3s}
.sb-dot.on{background:var(--accent);box-shadow:0 0 8px var(--accent);animation:pulse 2s ease infinite}
.sb-nav{flex:1;overflow-y:auto;padding:10px 0}
.sb-nav a{display:flex;align-items:center;gap:11px;padding:10px 16px;color:var(--dim);text-decoration:none;font-size:13px;font-weight:500;letter-spacing:0.2px;border-left:3px solid transparent;transition:all .2s cubic-bezier(.22,1,.36,1);cursor:pointer}
.sb-nav a:hover{color:var(--text);background:rgba(255,255,255,0.035);transform:translateX(3px)}
.sb-nav a.ac{color:var(--text);border-left-color:var(--accent);background:linear-gradient(90deg,rgba(247,147,26,0.08),transparent 80%);box-shadow:inset 3px 0 12px -6px var(--accent)}
.sb-nav svg{width:17px;height:17px;stroke:currentColor;fill:none;stroke-width:1.8;stroke-linecap:round;stroke-linejoin:round;flex-shrink:0;transition:stroke .15s}
.sb-nav a.ac svg{stroke:var(--accent)}
.sb-foot{padding:14px 16px;border-top:1px solid var(--border);font-size:10px;color:var(--muted);line-height:1.6}
.sb-foot b{color:var(--dim);font-weight:600}
.sb-creature{font-size:18px;text-align:center;margin-bottom:6px;text-shadow:0 0 10px currentColor}
.sb-sep{height:1px;background:var(--border);margin:8px 16px}

/* Collapsed sidebar */
body.col .sb{width:var(--sidebar-c)}
body.col .sb-lockup .sb-prompt,body.col .sb-lockup .sb-word,body.col .sb-tag,body.col .sb-nav .nl,body.col .sb-foot,body.col .sb-hr span,body.col .sb-creature{display:none}
body.col .sb-lockup{padding:6px;justify-content:center;margin-bottom:0}
body.col .sb-mark{width:28px;height:28px}
body.col .sb-head{padding:12px 6px;text-align:center}
body.col .sb-nav a{justify-content:center;padding:10px 0;gap:0}
body.col .sb-nav a .nl{display:none}
body.col .main{margin-left:var(--sidebar-c)}

/* ── Main Area ── */
.main{margin-left:var(--sidebar-w);min-height:100vh;transition:margin-left .2s;position:relative;z-index:1}
.topbar{height:var(--topbar-h);background:rgba(10,14,20,0.85);border-bottom:1px solid var(--border);display:flex;align-items:center;justify-content:space-between;padding:0 20px;backdrop-filter:blur(16px);-webkit-backdrop-filter:blur(16px)}
.topbar-left{display:flex;align-items:center;gap:12px;font-size:11px;color:var(--dim)}
.topbar-right{display:flex;align-items:center;gap:8px}
.tb-btn{background:none;border:1px solid var(--border);color:var(--dim);padding:5px 12px;border-radius:var(--radius-sm);font-size:11px;cursor:pointer;font-family:var(--font);transition:all .15s}
.tb-btn:hover{color:var(--text);border-color:var(--muted);background:rgba(255,255,255,0.03)}
.tb-btn.danger{color:var(--red);border-color:rgba(248,113,113,0.2)}
.tb-btn.danger:hover{background:rgba(248,113,113,0.08);border-color:var(--red)}
.tb-btn.tb-pause{display:inline-flex;align-items:center;gap:6px;font-family:var(--mono);font-size:10px;font-weight:700;letter-spacing:1px}
.tb-btn.tb-pause svg{fill:currentColor}
.tb-btn.tb-pause[data-mode="resume"] svg{fill:var(--accent)}
.tb-btn.tb-pause[data-mode="resume"]{color:var(--accent);border-color:rgba(247,147,26,0.25)}
.tb-btn.tb-icon{padding:5px 8px;display:inline-flex;align-items:center;justify-content:center;position:relative}
.tb-btn.tb-icon svg{display:block}
.tb-bell-dot{position:absolute;top:3px;right:4px;width:6px;height:6px;border-radius:50%;background:var(--red);box-shadow:0 0 4px var(--red)}
.tb-mining-pill{display:inline-flex;align-items:center;gap:6px;font-family:var(--mono);font-size:10px;font-weight:700;letter-spacing:1px;text-transform:uppercase;padding:4px 10px;border-radius:12px;background:rgba(34,197,94,0.08);color:var(--green);border:1px solid rgba(34,197,94,0.20)}
.tb-mining-pill .tb-mining-dot{width:6px;height:6px;border-radius:50%;background:var(--green);box-shadow:0 0 6px var(--green);animation:livePulse 1.6s var(--ease-out) infinite}
.tb-mining-pill[data-state="enabled"],.tb-mining-pill[data-state="ready"]{background:var(--glow);color:var(--accent);border-color:rgba(250,165,0,0.28)}
.tb-mining-pill[data-state="enabled"] .tb-mining-dot,.tb-mining-pill[data-state="ready"] .tb-mining-dot{background:var(--accent);box-shadow:0 0 6px var(--accent)}
.tb-mining-pill[data-state="paused"],.tb-mining-pill[data-state="standby"]{background:rgba(248,113,113,0.08);color:var(--red);border-color:rgba(248,113,113,0.22)}
.tb-mining-pill[data-state="paused"] .tb-mining-dot,.tb-mining-pill[data-state="standby"] .tb-mining-dot{background:var(--red);box-shadow:0 0 4px var(--red);animation:none}
.tb-mining-pill[data-state="pending"]{background:rgba(255,255,255,0.04);color:var(--dim);border-color:var(--border)}
.tb-mining-pill[data-state="pending"] .tb-mining-dot{background:var(--dim);box-shadow:none;animation:none}
.tb-alert-panel{position:absolute;top:54px;right:20px;width:300px;max-width:calc(100vw - 40px);background:var(--s-overlay);border:1px solid var(--border-hi);border-radius:var(--radius);box-shadow:var(--shadow-float);z-index:200;font-family:var(--font);animation:fadeIn var(--dur-base) var(--ease-out)}
.tb-alert-head{display:flex;justify-content:space-between;align-items:center;padding:10px 14px;border-bottom:1px solid var(--border);font-family:var(--mono);font-size:11px;font-weight:700;letter-spacing:1px;text-transform:uppercase;color:var(--text)}
.tb-alert-close{background:none;border:none;color:var(--dim);font-size:18px;line-height:1;cursor:pointer;padding:0 4px;border-radius:var(--radius-sm)}
.tb-alert-close:hover{color:var(--text)}
.tb-alert-body{padding:6px 0;max-height:300px;overflow-y:auto}
.tb-alert-empty{padding:18px 16px;text-align:center;color:var(--dim);font-size:11px}
.tb-alert-row{padding:8px 14px;border-bottom:1px dashed var(--border);font-size:11px;color:var(--text);display:flex;flex-direction:column;gap:2px}
.tb-alert-row:last-child{border-bottom:none}
.tb-alert-row .tb-alert-when{font-family:var(--mono);font-size:9px;color:var(--muted);letter-spacing:0.5px}
.hamburger{display:none;background:none;border:none;color:var(--text);font-size:20px;cursor:pointer;padding:4px}
.status-pills{display:flex;gap:8px;flex-wrap:wrap}
.pill{font-size:10px;padding:3px 10px;border-radius:12px;background:rgba(255,255,255,0.04);color:var(--dim);border:1px solid rgba(255,255,255,0.03)}

/* ── Content ── */
.content{padding:24px;max-width:1200px}
.page{display:none;animation:fadeIn .2s ease}
.page.ac{display:block}
.page-title{font-size:22px;font-weight:700;margin-bottom:20px;letter-spacing:-0.3px}
.page-title::before{content:'> ';color:var(--accent);font-family:var(--mono);font-weight:400}

/* ── Cards ── */
.card{background:var(--s-raised);border:1px solid var(--border);border-radius:var(--radius);padding:18px 20px;margin-bottom:16px;transition:border-color .22s ease,box-shadow .28s cubic-bezier(.22,1,.36,1),transform .28s cubic-bezier(.22,1,.36,1);box-shadow:0 4px 24px rgba(0,0,0,0.4),inset 0 1px 0 rgba(255,255,255,0.04)}
.card:hover{border-color:rgba(247,147,26,0.1);box-shadow:0 12px 40px rgba(0,0,0,0.5),0 0 24px var(--glow),inset 0 1px 0 rgba(255,255,255,0.06);transform:translateY(-1px) scale(1.003)}
.card-dense{padding:12px 16px}
.metric-help{font-size:10px;color:var(--muted);margin-top:3px;font-style:italic}
.sb-nav a:focus-visible,.btn:focus-visible,.tb-btn:focus-visible,.color-opt:focus-visible{outline:2px solid var(--accent);outline-offset:2px;box-shadow:0 0 0 4px var(--glow)}
.kv-row{display:flex;justify-content:space-between;padding:5px 0;border-bottom:1px solid var(--border);font-family:var(--mono);font-size:11px}
.kv-row:last-child{border-bottom:none}
.kv-key{color:var(--dim)}.kv-val{color:var(--text)}
.card-title{font-size:10px;text-transform:uppercase;letter-spacing:1.5px;color:var(--dim);margin-bottom:14px;font-weight:700;font-family:var(--mono);display:flex;align-items:center;gap:8px;padding-bottom:10px;border-bottom:1px solid var(--border)}
.card-title::before{content:'>';color:var(--accent);font-weight:400}

/* ── Grid layouts ── */
.grid2{display:grid;grid-template-columns:1fr 1fr;gap:16px;margin-bottom:16px}
.grid3{display:grid;grid-template-columns:1fr 1fr 1fr;gap:16px;margin-bottom:16px}
.grid4{display:grid;grid-template-columns:1fr 1fr 1fr 1fr;gap:16px;margin-bottom:16px}

/* ── Stat cards ── */
.stat{background:var(--s-raised);border:1px solid var(--border);border-radius:var(--radius);padding:16px;position:relative;overflow:hidden;transition:all .25s;animation:fadeIn .3s ease both;box-shadow:0 4px 24px rgba(0,0,0,0.4),inset 0 1px 0 rgba(255,255,255,0.04)}
.stat:nth-child(1){animation-delay:0ms}.stat:nth-child(2){animation-delay:60ms}.stat:nth-child(3){animation-delay:120ms}.stat:nth-child(4){animation-delay:180ms}
.stat:hover{border-color:rgba(255,255,255,0.12);transform:translateY(-1px) scale(1.005);box-shadow:0 12px 32px rgba(0,0,0,0.5),0 0 20px var(--glow),inset 0 1px 0 rgba(255,255,255,0.06)}
.stat::after{content:'';position:absolute;top:12px;right:12px;width:6px;height:6px;border-radius:50%;background:var(--accent);opacity:0.4;animation:pulse 2.5s ease-in-out infinite}
.stat[data-accent="cyan"]::after{background:var(--cyan);box-shadow:0 0 8px rgba(34,211,238,0.4)}
.stat[data-accent="yellow"]::after{background:var(--yellow);box-shadow:0 0 8px rgba(251,191,36,0.4)}
.stat[data-accent="green"]::after{background:var(--green);box-shadow:0 0 8px rgba(52,211,153,0.4)}
.stat[data-accent="orange"]::after{background:var(--accent);box-shadow:0 0 8px var(--glow)}
.stat[data-accent="red"]::after{background:var(--red);box-shadow:0 0 8px var(--glow-red)}
.stat-label{font-size:10px;text-transform:uppercase;letter-spacing:1.2px;color:var(--dim);margin-bottom:6px;font-weight:600}
.stat-val{font-family:var(--mono);font-size:22px;font-weight:700;font-variant-numeric:tabular-nums;color:var(--text);will-change:transform,opacity;transition:transform .35s cubic-bezier(.22,1,.36,1),opacity .35s ease}
.stat-sub{font-size:10px;color:var(--dim);margin-top:4px}
.stat-icon{width:32px;height:32px;border-radius:8px;display:flex;align-items:center;justify-content:center;margin-bottom:8px}
.stat-icon svg{width:16px;height:16px;stroke:currentColor;fill:none;stroke-width:2;stroke-linecap:round;stroke-linejoin:round}
.stat::before{content:'';position:absolute;left:0;top:0;bottom:0;width:3px;border-radius:0 2px 2px 0;background:var(--accent);opacity:0;transition:opacity .25s}
.stat:hover::before{opacity:1}
.stat[data-accent="cyan"]:hover::before{background:var(--cyan)}
.stat[data-accent="yellow"]:hover::before{background:var(--yellow)}
.stat[data-accent="green"]:hover::before{background:var(--green)}
.stat[data-accent="red"]:hover::before{background:var(--red)}
[data-tip]{position:relative;cursor:help}
[data-tip]:hover::after{content:attr(data-tip);position:absolute;bottom:calc(100% + 8px);left:50%;transform:translateX(-50%);background:var(--s-overlay);color:var(--text);padding:6px 12px;border-radius:6px;font-size:11px;font-weight:400;text-transform:none;letter-spacing:0;white-space:nowrap;z-index:50;border:1px solid var(--border);box-shadow:0 4px 16px rgba(0,0,0,0.4);animation:fadeIn .15s ease;pointer-events:none}

/* ── Hero ── */
.hero{text-align:left;padding:28px 28px 22px;background:var(--s-raised);border:1px solid var(--border);border-radius:var(--radius);margin-bottom:16px;position:relative;overflow:hidden;box-shadow:0 8px 40px rgba(0,0,0,0.5),inset 0 1px 0 rgba(255,255,255,0.05);transition:box-shadow .5s ease}
.hero-head{display:flex;justify-content:space-between;align-items:flex-start;gap:20px}
.hero-meta{display:grid;grid-template-columns:repeat(3,1fr);gap:10px;margin-top:16px}
.hero-kpi{background:rgba(255,255,255,.03);border:1px solid rgba(255,255,255,.05);border-radius:10px;padding:10px 14px;transition:border-color .2s}
.hero-kpi:hover{border-color:rgba(255,255,255,.1);transform:translateY(-2px);box-shadow:0 8px 20px rgba(0,0,0,.3),0 0 12px var(--glow)}
.hero-kpi-label{font-size:9px;letter-spacing:1.2px;text-transform:uppercase;color:var(--dim);font-weight:600}
.hero-kpi-val{margin-top:4px;font:700 15px var(--mono);color:var(--text)}
.hero-kpi-sub{font-size:9px;color:var(--muted);margin-top:2px;font-style:italic}
.hero::before{content:'';position:absolute;inset:0;background:radial-gradient(circle at 50% 30%,var(--glow),transparent 60%);pointer-events:none}
.hero::after{content:'';position:absolute;top:0;left:0;right:0;height:1px;background:linear-gradient(90deg,transparent 10%,var(--accent) 50%,transparent 90%);opacity:0.4}
.hero-hr{font-family:var(--mono);font-size:60px;font-weight:800;color:var(--text);text-shadow:0 0 40px rgba(247,147,26,0.2),0 0 80px rgba(247,147,26,0.06);line-height:1;position:relative;letter-spacing:-2px}
.hero-unit{font-size:20px;color:var(--dim);font-weight:400;margin-left:6px;letter-spacing:0}
.hero-sub{font-size:11px;color:var(--dim);margin-top:8px}
.hero-sub span[style*="accent"]{text-shadow:0 0 8px var(--glow)}
.hero-pills{display:flex;justify-content:flex-start;gap:8px;margin-top:10px}
.hero-pill{font-family:var(--mono);font-size:10px;padding:4px 12px;border-radius:12px;background:rgba(255,255,255,0.04);color:var(--dim);border:1px solid rgba(255,255,255,0.04);transition:all .2s}
.hero-pool-pill{font-family:var(--mono);font-size:10px;font-weight:700;letter-spacing:1px;text-transform:uppercase;padding:4px 10px;border-radius:12px;background:var(--glow);color:var(--accent);border:1px solid rgba(247,147,26,0.28)}
.hero-core-caption{font-family:var(--mono);font-size:10px;font-weight:600;letter-spacing:1.5px;text-transform:uppercase;color:var(--dim);padding:0 4px}
.hero-core-caption .hero-core-caption-strong{color:var(--accent);font-weight:700}
.hero-pill:hover{border-color:rgba(255,255,255,0.08);color:var(--text)}

/* ── Gauges ── */
.gauge{height:6px;background:rgba(255,255,255,0.05);border-radius:3px;overflow:hidden;margin:6px 0 2px}
.gauge-fill{height:100%;border-radius:3px;transition:width .55s cubic-bezier(.22,1,.36,1);position:relative}
.fill-green{background:linear-gradient(90deg,var(--green),#10b981)}.fill-orange{background:linear-gradient(90deg,var(--accent),#fb923c)}.fill-red{background:linear-gradient(90deg,var(--red),#ef4444)}.fill-cyan{background:linear-gradient(90deg,var(--cyan),#06b6d4)}
.gauge-row{display:flex;align-items:center;gap:10px;margin:6px 0}
.gauge-label{font-size:10px;color:var(--dim);min-width:50px}
.gauge-val{font-family:var(--mono);font-size:12px;min-width:60px;text-align:right}
.gauge-bar{flex:1}

/* ── Chart ── */
.chart-wrap{position:relative;height:200px;margin:8px 0}
.chart-wrap canvas{position:absolute;top:0;left:0;width:100%;height:100%;cursor:crosshair}
.chart-legend{display:flex;gap:12px;font-size:10px;color:var(--dim);margin-bottom:4px}
.chart-legend label{display:flex;align-items:center;gap:4px;cursor:pointer}
.chart-legend input{accent-color:var(--accent)}

/* ── Forms ── */
.field{margin-bottom:12px}
.field label{display:block;font-size:11px;color:var(--dim);margin-bottom:4px;text-transform:uppercase;letter-spacing:0.5px}
.field input,.field select{width:100%;padding:8px 10px;background:var(--s-void);border:1px solid var(--border);border-radius:var(--radius-sm);color:var(--text);font-family:var(--mono);font-size:12px;outline:none;transition:border-color .2s ease,box-shadow .25s ease,background .2s}
.field input:focus,.field select:focus{border-color:var(--accent);box-shadow:0 0 0 2px var(--accent),0 0 16px var(--glow);background:rgba(255,255,255,0.02);outline:none}
.field:focus-within label{color:var(--accent);transition:color .2s}
.field input::placeholder{color:var(--muted);opacity:0.6}
.field-row{display:flex;gap:10px}
.field-row .field{flex:1}
.btn{padding:8px 16px;border:none;border-radius:var(--radius-sm);font-size:12px;font-weight:600;cursor:pointer;font-family:var(--font);transition:all .15s}
.btn:active{transform:scale(0.97);filter:brightness(0.9)}.btn:disabled{opacity:0.4;cursor:not-allowed}
input[type=range]{accent-color:var(--accent)}
.btn-primary{background:var(--accent);color:#000;box-shadow:0 2px 8px rgba(247,147,26,0.25)}.btn-primary:hover{filter:brightness(1.1);box-shadow:0 4px 16px rgba(247,147,26,0.35);transform:translateY(-1px)}
.btn-danger{background:var(--red);color:#fff}.btn-danger:hover{filter:brightness(1.1)}
.btn-ghost{background:transparent;border:1px solid var(--border);color:var(--dim)}.btn-ghost:hover{color:var(--text);border-color:var(--muted)}
.btn-sm{padding:4px 10px;font-size:11px}
.btn-row{display:flex;gap:8px;margin-top:12px;flex-wrap:wrap}
details{margin-top:8px}
details summary{font-size:11px;color:var(--dim);cursor:pointer;padding:4px 0}

/* ── Shares bar ── */
.share-bar{display:flex;height:8px;border-radius:4px;overflow:hidden;background:rgba(255,255,255,0.04)}
.share-bar .acc{background:var(--accent);transition:width .5s}.share-bar .rej{background:var(--red);transition:width .5s}
.share-dots{display:flex;flex-wrap:wrap;gap:3px;margin-top:8px}
.share-dot{width:6px;height:6px;border-radius:50%;transition:transform .15s}
.share-dot.acc{background:var(--accent);box-shadow:0 0 4px var(--glow)}.share-dot.rej{background:var(--red);box-shadow:0 0 4px rgba(248,113,113,0.4)}
@keyframes sharePop{0%{transform:scale(.3);opacity:0}60%{transform:scale(1.4);opacity:1}100%{transform:scale(1)}}
.share-dot:last-child{animation:sharePop .3s cubic-bezier(.22,1,.36,1)}

/* ── Log terminal (canonical layout) ── */
.log-crumb{font-family:var(--mono);font-size:11px;color:var(--dim);margin-bottom:10px;display:flex;align-items:center;gap:8px;flex-wrap:wrap}
.log-crumb::before{content:'> ';color:var(--accent)}
.log-crumb b{color:var(--text);font-weight:600}
.log-crumb .sep{color:var(--muted)}
.log-crumb .path{color:var(--accent)}
.log-crumb-pills{margin-left:auto;display:flex;gap:6px;align-items:center}
.log-crumb-pill{font-family:var(--mono);font-size:10px;padding:3px 9px;border-radius:10px;border:1px solid var(--border);color:var(--dim);background:rgba(255,255,255,0.02);display:inline-flex;align-items:center;gap:5px;cursor:pointer}
.log-crumb-pill.live{color:var(--green);border-color:rgba(52,211,153,0.25)}
.log-crumb-pill.live::before{content:'';width:6px;height:6px;border-radius:50%;background:var(--green);box-shadow:0 0 6px var(--green);animation:pulse 1.6s ease infinite}
.log-crumb-pill.paused{color:var(--yellow);border-color:rgba(251,191,36,0.25)}
.log-stream-head{display:flex;align-items:center;justify-content:space-between;margin-bottom:10px;font-family:var(--mono);font-size:10px;letter-spacing:1.4px;color:var(--dim);text-transform:uppercase}
.log-stream-head .lhs{color:var(--accent);text-shadow:0 0 8px var(--glow);font-weight:700}
.log-grid{display:grid;grid-template-columns:200px 1fr;gap:14px}
.log-counters-col{display:flex;flex-direction:column;gap:10px}
.log-counter-row{padding:8px 10px;background:var(--s-void);border:1px solid var(--border);border-radius:var(--radius-sm);font-family:var(--mono)}
.log-counter-row .lc-k{font-size:9px;letter-spacing:1.4px;color:var(--dim);text-transform:uppercase}
.log-counter-row .lc-v{font-size:18px;color:var(--text);font-weight:600;margin-top:2px;line-height:1}
.log-counter-row.ok .lc-v{color:var(--green);text-shadow:0 0 8px rgba(52,211,153,0.25)}
.log-counter-row.rej .lc-v{color:var(--red)}
.log-counter-row.hw .lc-v{color:var(--yellow)}
.log-counter-row.stale .lc-v{color:var(--cyan)}
.log-counter-row.rec .lc-v{color:var(--accent)}
.log-counter-bar{height:3px;background:rgba(255,255,255,0.04);border-radius:2px;margin-top:6px;overflow:hidden}
.log-counter-bar i{display:block;height:100%;background:var(--accent);width:0;transition:width .4s ease;box-shadow:0 0 6px var(--glow)}
.log-counter-row.ok .log-counter-bar i{background:var(--green);box-shadow:0 0 6px rgba(52,211,153,0.4)}
.log-counter-row.rej .log-counter-bar i{background:var(--red)}
.log-counter-row.hw .log-counter-bar i{background:var(--yellow)}
.log-counter-row.stale .log-counter-bar i{background:var(--cyan)}
.log-stream-wrap{display:flex;flex-direction:column;min-width:0}
.log-filter-row{display:flex;gap:6px;align-items:center;margin-bottom:8px;flex-wrap:wrap}
.log-filter-row input{flex:1;min-width:140px;padding:6px 10px;background:var(--s-void);border:1px solid var(--border);border-radius:var(--radius-sm);color:var(--text);font-family:var(--mono);font-size:11px}
.log-filter-pills{display:flex;gap:4px;flex-wrap:wrap}
.log-filter-pill{font-family:var(--mono);font-size:10px;letter-spacing:1px;padding:4px 9px;border-radius:10px;background:rgba(255,255,255,0.03);color:var(--dim);cursor:pointer;border:1px solid var(--border);text-transform:uppercase;transition:all .15s}
.log-filter-pill:hover{color:var(--text);border-color:var(--muted)}
.log-filter-pill.ac{background:var(--accent);color:#000;border-color:var(--accent);box-shadow:0 0 10px var(--glow)}
.log-stream{background:var(--s-void);border:1px solid var(--border);border-radius:var(--radius-sm);padding:12px 14px;font-family:var(--mono);font-size:11px;line-height:1.9;overflow-y:auto;height:calc(100vh - 280px);color:var(--text);box-shadow:inset 0 0 40px rgba(0,0,0,0.4)}
.log-line:last-child{animation:logNew .5s ease}
.log-line{white-space:nowrap;overflow:hidden;text-overflow:ellipsis;color:var(--text)}
.log-line .ts{color:var(--dim);margin-right:6px}
.log-line .lvl{display:inline-block;min-width:38px;margin-right:8px;font-weight:700;letter-spacing:0.5px}
.log-line.lvl-sys .lvl{color:var(--dim)}
.log-line.lvl-pool .lvl{color:var(--cyan)}
.log-line.lvl-asic .lvl{color:var(--accent)}
.log-line.lvl-ok .lvl{color:var(--green)}
.log-line.lvl-ok{color:var(--green);text-shadow:0 0 4px rgba(52,211,153,0.15)}
.log-line.lvl-warn .lvl,.log-line.lvl-warn{color:var(--yellow)}
.log-line.lvl-err .lvl,.log-line.lvl-err{color:var(--red)}
.log-line.lvl-auto .lvl{color:#c084fc}
.log-line.lvl-auto{color:#c084fc}
.log-line.lvl-net .lvl{color:#60a5fa}
.log-line.lvl-blk .lvl{color:var(--accent);text-shadow:0 0 8px var(--glow)}
.log-line.lvl-blk{color:var(--accent);font-weight:600;text-shadow:0 0 6px var(--glow)}
@media(max-width:768px){.log-grid{grid-template-columns:1fr}}

/* ── Thermal gauge ── */
.therm-wrap{text-align:center}
.therm-label{font-size:11px;font-weight:600;margin-top:4px}

/* ── Color picker (Theme) ── */
.color-pick{display:flex;gap:16px;flex-wrap:wrap}
.color-opt{width:44px;height:44px;border-radius:50%;cursor:pointer;border:3px solid transparent;transition:all .15s;display:flex;align-items:center;justify-content:center}
.color-opt:hover{transform:scale(1.1)}.color-opt.sel{border-color:var(--text)}
.color-opt svg{width:18px;height:18px;stroke:#fff;fill:none;stroke-width:2.5;display:none}
.color-opt.sel svg{display:block}

/* ── Toast ── */
.toast-container{position:fixed;bottom:20px;right:20px;z-index:999;display:flex;flex-direction:column;gap:8px}
.toast{background:var(--s-overlay);color:var(--text);padding:12px 18px;border-radius:var(--radius-sm);font-size:12px;border-left:3px solid var(--accent);box-shadow:0 8px 32px rgba(0,0,0,0.5),0 0 0 1px var(--border);animation:slideIn .3s cubic-bezier(0.34,1.56,0.64,1);backdrop-filter:blur(16px)}
.toast-warning{border-left-color:var(--yellow)}.toast-error{border-left-color:var(--red)}

/* ── Modal ── */
.modal-overlay{position:fixed;inset:0;background:rgba(0,0,0,0.7);z-index:200;display:none;align-items:center;justify-content:center;backdrop-filter:blur(4px)}
.modal-overlay.show{display:flex}
.modal{background:var(--s-overlay);border:1px solid var(--border);border-radius:var(--radius);padding:24px;max-width:400px;width:90%}
.modal h3{font-size:16px;margin-bottom:8px}.modal p{font-size:12px;color:var(--dim);margin-bottom:16px}

/* ── OTA ── */
.ota-zone{border:2px dashed var(--border);border-radius:var(--radius);padding:24px;text-align:center;cursor:pointer;transition:border-color .2s;color:var(--dim);font-size:12px}
.ota-zone:hover,.ota-zone.dragover{border-color:var(--accent);color:var(--text)}
.ota-meta{display:none;font-size:11px;color:var(--dim);margin-top:6px;line-height:1.45;overflow-wrap:anywhere}

/* ── Alert ── */
.alert{display:none;background:rgba(248,113,113,0.1);border:1px solid rgba(248,113,113,0.3);border-radius:var(--radius-sm);padding:10px;font-size:12px;color:var(--red);margin-bottom:12px}
.alert.show{display:block}
/* Inline spinner for pending-state buttons. Driven by the post() helper below. */
.spinner{display:inline-block;width:10px;height:10px;border:2px solid currentColor;border-top-color:transparent;border-radius:50%;animation:spin .8s linear infinite;vertical-align:middle;margin-right:6px}
@keyframes spin{to{transform:rotate(360deg)}}
/* Reusable layout/typography classes — refactored out of ~70 inline styles. */
.kv-flex-row{display:flex;justify-content:space-between;padding:4px 0;border-bottom:1px solid var(--border)}
.kv-flex-row:last-child{border-bottom:0}
.text-dim{color:var(--dim)}
.text-accent{color:var(--accent)}
.text-red{color:var(--red)}
.text-cyan{color:var(--cyan)}
.text-yellow{color:var(--yellow)}
.text-green{color:var(--green)}
.stack-tight{margin-top:8px}
.help-small{font-size:11px;color:var(--dim);margin-bottom:8px;display:block}
.meta-mono{font-family:var(--mono);font-size:11px}
.gauge-row{display:flex;align-items:center;gap:10px;margin:8px 0}
.icon-accent-bg{background:var(--glow);color:var(--accent)}
/* Show/hide toggle for password inputs. The wrapper reserves padding on the
   right so the eye button doesn't overlap the input text. */
.pw-wrap{position:relative;display:block}
.pw-wrap input{padding-right:32px!important}
.pw-toggle{position:absolute;right:6px;top:50%;transform:translateY(-50%);background:transparent;border:0;color:var(--dim);cursor:pointer;padding:4px 6px;font-size:14px;line-height:1;border-radius:var(--radius-sm)}
.pw-toggle:hover{color:var(--accent)}
.pw-toggle:focus-visible{outline:2px solid var(--accent);outline-offset:2px}

/* ── Offline banner ── */
.offline-banner{display:none;background:var(--red);color:#fff;text-align:center;padding:8px;font-size:12px}

/* ── Achievement grid ── */
.ach-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(84px,1fr));gap:8px}
.ach-item{text-align:center;padding:10px 6px;border-radius:var(--radius-sm);font-size:9px;background:rgba(255,255,255,0.02);border:1px solid var(--border);transition:all .2s}
.ach-item:hover{transform:translateY(-2px);border-color:rgba(255,255,255,0.1)}
.ach-item.unlocked{border-color:var(--accent);background:rgba(247,147,26,0.06);box-shadow:0 0 12px rgba(247,147,26,0.08)}
.ach-item.locked{opacity:0.4}

/* ── Chip visualization ── */
.chip-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(100px,1fr));gap:10px}
.chip-tile{background:var(--s-void);border:1px solid var(--border);border-radius:var(--radius-sm);padding:12px;text-align:center;position:relative;transition:all .2s}
.chip-tile:hover{border-color:rgba(255,255,255,0.1);transform:translateY(-1px)}
.chip-tile .chip-id{font-family:var(--mono);font-size:9px;color:var(--dim);text-transform:uppercase;letter-spacing:1px;margin-bottom:4px}
.chip-tile .chip-temp{font-family:var(--mono);font-size:20px;font-weight:700;line-height:1.2}
.chip-tile .chip-status{font-size:9px;margin-top:4px;display:flex;align-items:center;justify-content:center;gap:4px}
.chip-tile .cs-dot{width:6px;height:6px;border-radius:50%}
.cs-active{background:var(--accent);box-shadow:0 0 4px var(--glow)}.cs-idle{background:var(--dim)}.cs-err{background:var(--red)}
.chip-tile.hottest{border-color:var(--red);box-shadow:0 0 12px rgba(248,113,113,0.1)}

/* duplicate scanline removed */

/* ── Mobile tabs ── */
.mobile-tabs{display:none}

/* ── Responsive ── */
@media(max-width:768px){
.mobile-tabs{display:grid;grid-template-columns:repeat(4,1fr);position:fixed;left:0;right:0;bottom:0;background:rgba(10,14,20,.96);border-top:1px solid var(--border);z-index:120;padding:4px 0}
.mobile-tabs button{background:none;border:none;color:var(--dim);font-size:9px;padding:6px 0;cursor:pointer;display:flex;flex-direction:column;align-items:center;gap:2px;font-family:var(--font)}
.mobile-tabs button.ac{color:var(--accent)}
.mobile-tabs svg{width:18px;height:18px;stroke:currentColor;fill:none;stroke-width:1.8}
.content{padding-bottom:72px!important}
body{grid-template-columns:1fr}
.sb{transform:translateX(-100%);transition:transform .25s;width:260px}
.sb.open{transform:translateX(0)}
.main{margin-left:0!important}
.hamburger{display:block}
.grid4{grid-template-columns:1fr 1fr}.grid3{grid-template-columns:1fr 1fr}.grid2{grid-template-columns:1fr}
.hero-hr{font-size:36px}
.offline-banner{left:0}
.content{padding:14px}
[data-tip]:hover::after{display:none}
}
@media(max-width:480px){.grid4{grid-template-columns:1fr}.hero-hr{font-size:28px}}

/* ── Design handoff layer (2026-04-24) — hero split + block-card + new modal ── */
.hero-inner{display:flex;justify-content:space-between;align-items:stretch;gap:24px;position:relative;z-index:1}
.hero-left{flex:1;min-width:0;display:flex;flex-direction:column;justify-content:center;gap:10px}
.hero-divider-v{width:1px;background:var(--border);align-self:stretch}
.hero-footer{display:flex;gap:24px;padding-top:14px;border-top:1px solid var(--border);margin-top:14px;flex-wrap:wrap;align-items:center}
.hero-footer .hero-kpi{min-width:120px}
.hero-footer .hero-divider{width:1px;height:36px;background:var(--border);flex-shrink:0}
.block-card{background:var(--s-raised);border:1px solid var(--border);border-radius:var(--radius);padding:16px 18px;width:260px;flex-shrink:0;cursor:pointer;transition:all var(--dur-base) var(--ease-out);display:flex;flex-direction:column;gap:8px;text-align:left;outline:none;font:inherit;color:inherit}
.block-card:hover{border-color:rgba(247,147,26,0.28);box-shadow:var(--shadow-hover);transform:translateY(-1px)}
.block-card:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.block-card .block-title{font-family:var(--mono);font-size:10px;font-weight:700;text-transform:uppercase;letter-spacing:1.5px;color:var(--dim);display:flex;align-items:center;gap:6px}
.block-card .block-title::before{content:">";color:var(--accent);font-weight:400}
.block-card .block-height{font-family:var(--mono);font-size:30px;font-weight:800;line-height:1;letter-spacing:-1px;color:var(--text);text-shadow:0 0 12px rgba(247,147,26,0.18);font-variant-numeric:tabular-nums}
.block-card .block-sub{display:flex;justify-content:space-between;align-items:center;font-size:10px;color:var(--dim);gap:8px;margin-top:2px}
.block-card .block-click-hint{font-size:9px;color:var(--muted);letter-spacing:0.5px;text-transform:uppercase;font-family:var(--mono)}
.block-card.new-block{animation:blockFlash 1.2s var(--ease-out)}
@keyframes blockFlash{0%{border-color:var(--accent);box-shadow:0 0 0 3px var(--glow),var(--shadow-hover)}100%{border-color:var(--border);box-shadow:var(--shadow-card)}}
/* ── Hero block card (loop 2026-04-29 inspiration combine) ──
   Extends .block-card with a richer head/hash/grid/foot layout.
   All data ids are wired by block-tile.js + inline update(). */
.block-card .block-card-head{display:flex;justify-content:space-between;align-items:center;gap:8px}
.block-card .pill-live{display:inline-flex;align-items:center;gap:5px;font-family:var(--mono);font-size:9px;font-weight:700;letter-spacing:1px;text-transform:uppercase;color:var(--accent);background:var(--glow);border:1px solid rgba(247,147,26,0.28);padding:3px 8px;border-radius:var(--radius-sm)}
.block-card .pill-live .live-dot{width:6px;height:6px;border-radius:50%;background:var(--accent);box-shadow:0 0 6px var(--accent);animation:livePulse 1.6s var(--ease-out) infinite}
@keyframes livePulse{0%,100%{opacity:1;transform:scale(1)}50%{opacity:0.4;transform:scale(0.85)}}
.block-card .block-hash-row{display:flex;align-items:center;gap:8px;padding:6px 8px;background:var(--s-void);border:1px solid var(--border);border-radius:var(--radius-sm);font-family:var(--mono);font-size:11px;font-variant-numeric:tabular-nums;min-width:0}
.block-card .block-hash-label{font-size:9px;color:var(--dim);letter-spacing:1px;flex-shrink:0}
.block-card .block-hash-val{color:var(--text);overflow:hidden;text-overflow:ellipsis;white-space:nowrap;flex:1;min-width:0}
.block-card .block-grid3{display:grid;grid-template-columns:repeat(3,1fr);gap:6px}
.block-card .block-grid4{display:grid;grid-template-columns:repeat(2,1fr);gap:6px}
.block-card .block-cell{display:flex;flex-direction:column;gap:2px;padding:6px 8px;background:rgba(255,255,255,0.012);border:1px solid var(--border);border-radius:var(--radius-sm);min-width:0}
.block-card .block-cell-label{font-family:var(--mono);font-size:9px;color:var(--dim);letter-spacing:1px;text-transform:uppercase}
.block-card .block-cell-val{font-family:var(--mono);font-size:13px;font-weight:700;color:var(--text);font-variant-numeric:tabular-nums;line-height:1.2;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.block-card .block-cell-unit{font-size:9px;font-weight:500;color:var(--dim);margin-left:1px}
.block-card .block-card-foot{display:flex;justify-content:space-between;align-items:center;font-family:var(--mono);font-size:10px;color:var(--dim);text-transform:uppercase;letter-spacing:0.5px;padding-top:6px;border-top:1px dashed var(--border);margin-top:2px}
.block-card .block-arrow{color:var(--accent);font-weight:700;transition:transform var(--dur-fast) var(--ease-out)}
.block-card:hover .block-arrow{transform:translateX(3px)}
.modal-back{position:fixed;inset:0;background:rgba(5,7,9,0.85);backdrop-filter:blur(8px);-webkit-backdrop-filter:blur(8px);display:none;align-items:center;justify-content:center;z-index:300;animation:fadeIn var(--dur-base) var(--ease-out)}
.modal-back.show{display:flex}
.modal-sm{background:var(--s-overlay);border:1px solid var(--border-hi);border-radius:var(--radius);width:460px;max-width:94vw;max-height:90vh;overflow:hidden;display:flex;flex-direction:column;box-shadow:var(--shadow-float);animation:modalIn var(--dur-base) var(--ease-pop)}
.modal-head{display:flex;justify-content:space-between;align-items:flex-start;padding:16px 18px 12px;border-bottom:1px solid var(--border);gap:12px}
.modal-head h3{font-size:var(--t-h3);font-weight:700;margin:0;color:var(--text)}
.modal-sub{font-size:11px;color:var(--dim);margin-top:3px}
.modal-close{background:none;border:none;color:var(--dim);cursor:pointer;padding:4px 8px;border-radius:var(--radius-sm);font-size:18px;line-height:1;transition:all var(--dur-fast) var(--ease-out)}
.modal-close:hover{color:var(--text);background:rgba(255,255,255,0.05)}
.modal-body{padding:16px 18px;overflow-y:auto;flex:1}
.modal-foot{padding:12px 18px 16px;display:flex;justify-content:flex-end;gap:8px;border-top:1px solid var(--border)}
@keyframes modalIn{from{opacity:0;transform:scale(0.96) translateY(8px)}to{opacity:1;transform:scale(1) translateY(0)}}
.kvdl{margin:0;padding:0;display:flex;flex-direction:column}
.kvdl .kvdl-row{display:flex;justify-content:space-between;align-items:center;padding:8px 0;border-bottom:1px dashed var(--border);gap:12px}
.kvdl .kvdl-row:last-child{border-bottom:none}
.kvdl .kvdl-k{font-size:10px;color:var(--dim);text-transform:uppercase;letter-spacing:0.5px;font-weight:600;flex-shrink:0}
.kvdl .kvdl-v{font-family:var(--mono);font-size:12px;color:var(--text);text-align:right;font-variant-numeric:tabular-nums;display:inline-flex;align-items:center;gap:6px;justify-content:flex-end;min-width:0}
.kvdl .kvdl-v.truncate{max-width:280px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.kvdl .kvdl-v .copy-btn{background:transparent;border:1px solid var(--border);color:var(--dim);cursor:pointer;padding:2px 8px;border-radius:var(--radius-sm);font-size:10px;font-family:var(--mono);transition:all var(--dur-fast) var(--ease-out)}
.kvdl .kvdl-v .copy-btn:hover{color:var(--accent);border-color:var(--accent)}
.pill-ok{color:var(--green);background:rgba(52,211,153,0.08);border:1px solid rgba(52,211,153,0.20)}
.pill-orange{color:var(--accent);background:var(--glow);border:1px solid rgba(247,147,26,0.25)}
.pill-muted{color:var(--muted);font-size:9px}
.block-hash-big{font-family:var(--mono);font-size:14px;color:var(--accent);word-break:break-all;padding:8px 10px;background:var(--s-void);border:1px solid var(--border);border-radius:var(--radius-sm);margin-top:4px}
@media(max-width:900px){.hero-inner{flex-direction:column;align-items:stretch;gap:16px}.block-card{width:auto}.hero-divider-v{display:none}.hero-footer{gap:16px}}

/* ── Topbar breadcrumb ── */
.crumbs{display:flex;align-items:center;gap:8px;font-size:13px;margin-right:16px}
.crumb-root{color:var(--dim);font-weight:500}
.crumb-sep{color:var(--muted)}
.crumb-cur{color:var(--text);font-weight:600}
body.col .crumbs .crumb-root,body.col .crumbs .crumb-sep{display:none}

/* ── Full handoff logo (320×120 scaled to sidebar 180px) ── */
/* legacy logo classes — superseded by .sb-lockup */

/* ── Pool Status card (Dashboard) ── */
.pool-status-row{display:flex;justify-content:space-between;align-items:center;margin-bottom:10px}
.pool-status-row .pill{font-size:10px}

/* ── 6-gauge Power card upgrade ── */
.gauges6{display:grid;grid-template-columns:1fr 1fr;gap:10px 18px}
.gauges6 .gauge-row{display:flex;flex-direction:column;gap:4px;font-size:10px;margin-bottom:0}
.gauges6 .gauge-head{display:flex;justify-content:space-between;align-items:baseline}
.gauges6 .gauge-label{color:var(--dim);text-transform:uppercase;letter-spacing:0.6px;font-weight:600;font-size:9px}
.gauges6 .gauge-val{font-family:var(--mono);color:var(--text);font-weight:700;font-variant-numeric:tabular-nums;font-size:13px}
.gauges6 .gauge-unit{color:var(--dim);font-weight:400;margin-left:3px;font-size:10px}
.gauges6 .gauge-track{height:4px;background:rgba(255,255,255,0.05);border-radius:2px;overflow:hidden}
.gauges6 .gauge-track .gauge-fill{height:100%;border-radius:2px;transition:width .5s var(--ease-out)}
.g-fill-orange{background:linear-gradient(90deg,var(--accent),var(--orange-50))}
.g-fill-green{background:linear-gradient(90deg,var(--green),#10b981)}
.g-fill-cyan{background:linear-gradient(90deg,var(--cyan),#06b6d4)}
.g-fill-yellow{background:linear-gradient(90deg,var(--yellow),#f59e0b)}
.g-fill-red{background:linear-gradient(90deg,var(--red),#ef4444)}
@media(max-width:700px){.gauges6{grid-template-columns:1fr}}

/* ── Chip tiles v2 (handoff chips.css port) ── */
.chip-grid{display:grid;gap:12px}
.chip-grid-1{grid-template-columns:minmax(0,320px);justify-content:center}
.chip-grid-2{grid-template-columns:repeat(2,1fr)}
.chip-grid-4{grid-template-columns:repeat(4,1fr)}
.chip-grid-6{grid-template-columns:repeat(6,1fr)}
@media(max-width:1100px){.chip-grid-4{grid-template-columns:repeat(2,1fr)}.chip-grid-6{grid-template-columns:repeat(3,1fr)}}
.chip-tile{--chip-color:var(--accent);--chip-glow:rgba(247,147,26,0.35);cursor:default;display:flex;flex-direction:column;padding:12px 12px 10px;background:linear-gradient(180deg,rgba(255,255,255,0.015) 0%,rgba(0,0,0,0.25) 100%),var(--s-raised);border:1px solid var(--border);border-radius:var(--radius-sm);position:relative;overflow:hidden;transition:all var(--dur-base) var(--ease-out);box-shadow:0 2px 12px rgba(0,0,0,0.35)}
.chip-tile::before{content:'';position:absolute;top:0;left:12%;right:12%;height:1px;background:linear-gradient(90deg,transparent,var(--chip-color),transparent);opacity:0.5}
.chip-tile::after{content:'';position:absolute;inset:0;background:radial-gradient(ellipse 120% 50% at 50% 120%,var(--chip-glow),transparent 70%);opacity:0.35;pointer-events:none;transition:opacity var(--dur-base)}
.chip-tile:hover{transform:translateY(-2px);border-color:var(--chip-color);box-shadow:0 8px 28px rgba(0,0,0,0.5),0 0 24px var(--chip-glow)}
.chip-tile:hover::after{opacity:0.8}
.chip-tile.hot{border-color:rgba(247,147,26,0.35);--chip-color:var(--yellow);--chip-glow:rgba(251,191,36,0.30)}
.chip-tile.error{border-color:rgba(248,113,113,0.40);--chip-color:var(--red);--chip-glow:rgba(248,113,113,0.30)}
.chip-head{display:flex;justify-content:space-between;align-items:center;margin-bottom:8px}
.chip-id{font-family:var(--mono);font-size:10px;font-weight:700;color:var(--dim);letter-spacing:1.2px}
.chip-dot{width:6px;height:6px;border-radius:50%;background:var(--muted)}
.chip-dot.active{background:var(--chip-color);box-shadow:0 0 8px var(--chip-color);animation:pulse 1.6s ease-in-out infinite}
.chip-dot.error{background:var(--red);box-shadow:0 0 8px var(--red);animation:pulse 0.8s ease-in-out infinite}
.chip-metrics{display:grid;grid-template-columns:1fr 1fr;gap:6px;margin-bottom:8px}
.chip-metric{display:flex;flex-direction:column;align-items:flex-start;line-height:1.1}
.chip-metric:last-child{align-items:flex-end}
.chip-metric-val{font-family:var(--mono);font-size:18px;font-weight:700;font-variant-numeric:tabular-nums;color:var(--text);letter-spacing:-0.4px}
.chip-metric-unit{font-size:11px;color:var(--dim);margin-left:2px;font-weight:400}
.chip-metric-lbl{font-family:var(--mono);font-size:8px;color:var(--dim);letter-spacing:1.3px;font-weight:600;margin-top:1px}
.chip-share-bar{position:relative;height:16px;background:var(--s-void);border:1px solid var(--border);border-radius:4px;overflow:hidden}
.chip-share-fill{position:absolute;inset:0 auto 0 0;border-radius:4px;opacity:0.35;transition:width .8s var(--ease-out);background:var(--chip-color)}
.chip-share-lbl{position:absolute;inset:0;display:flex;align-items:center;justify-content:center;font-family:var(--mono);font-size:9px;font-weight:700;letter-spacing:0.5px;color:var(--text)}
.uart-chain-v2{display:flex;align-items:center;flex-wrap:wrap;gap:4px;padding:10px 12px;background:var(--s-void);border:1px solid var(--border);border-radius:var(--radius-sm);font-family:var(--mono);font-size:10px;margin-top:12px;overflow:hidden;position:relative}
.uart-node{display:inline-flex;align-items:center;gap:5px;padding:3px 10px;border-radius:4px;font-weight:600;letter-spacing:0.8px;white-space:nowrap}
.uart-mcu{color:var(--cyan);border:1px solid rgba(34,211,238,0.3);background:rgba(34,211,238,0.06);box-shadow:0 0 12px rgba(34,211,238,0.12)}
.uart-led{width:6px;height:6px;border-radius:50%;background:var(--cyan);box-shadow:0 0 8px var(--cyan);animation:pulse 1.6s ease-in-out infinite}
.uart-chip{border:1px solid var(--border);color:var(--dim)}
.uart-chip.active{color:var(--accent);border-color:rgba(247,147,26,0.35);background:var(--glow);box-shadow:0 0 12px rgba(247,147,26,0.15)}
.uart-chip.error{color:var(--red);border-color:rgba(248,113,113,0.35);background:rgba(248,113,113,0.08)}
.uart-line{flex:0 0 auto;width:24px;height:1px;background:linear-gradient(90deg,var(--dim),rgba(107,122,141,0.1));position:relative;margin:0 2px}
</style>
<!-- Modular dashboard CSS (Phase 2.A-3.2). Loaded AFTER inline <style>
     so component rules (chip-grid, chip-tile, mining-core, block-tile)
     authoritatively win the cascade. -->
<link rel="stylesheet" href="/dashboard/tokens.css">
<link rel="stylesheet" href="/dashboard/core.css">
<link rel="stylesheet" href="/dashboard/components.css">
<link rel="stylesheet" href="/dashboard/block-tile.css">
<link rel="stylesheet" href="/dashboard/asic-chips.css">
</head>
<body>
<div style="display:none">AxeOS compatibility marker</div>

<!-- ════ Sidebar ════ -->
<div class="sb" id="sidebar">
 <div class="sb-head">
  <div class="sb-lockup" aria-label="DCENT_axe">
   <span class="sb-prompt" aria-hidden="true">$</span>
   <!-- D-Central molecule mark — matches `assets/d-central-mark.png` from
        the design handoff: LARGEST orb top-right (r=7), MEDIUM bottom-center
        (r=5), SMALLEST left (r=3.5), with short bonds forming an L between
        adjacent pairs (left↔middle and middle↔top-right; the small and big
        orb do NOT directly connect). Theme-reactive via solid currentColor:
        `.sb-mark { color: var(--accent) }` → orbs inherit instantly on
        Theme picker change. White cream specular highlight overlays on top
        of each orb stay untouched (intentional 3D shading). -->
   <svg class="sb-mark" viewBox="0 0 32 32" aria-label="D-Central molecule"><line class="mol-bond" x1="7.5" y1="14" x2="14.5" y2="20" stroke-width="2.4" stroke-linecap="round"/><line class="mol-bond" x1="16" y1="19" x2="21" y2="13" stroke-width="2.4" stroke-linecap="round"/><circle class="mol-orb" cx="22" cy="10" r="7"/><circle class="mol-orb" cx="15" cy="22" r="5"/><circle class="mol-orb" cx="6.5" cy="13.5" r="3.5"/><ellipse class="mol-shine" cx="20.4" cy="6.2" rx="3.4" ry="1.4"/><ellipse class="mol-shine" cx="13.6" cy="19.5" rx="2.3" ry="1.0"/><ellipse class="mol-shine" cx="5.7" cy="11.8" rx="1.5" ry="0.7"/></svg>
   <div class="sb-word"><span class="sb-dcent">DCENT</span><span class="sb-under">_</span><span class="sb-axe">axe</span></div>
  </div>
  <div class="sb-tag"><span class="br">[</span>&nbsp;BITCOIN&nbsp;MINING&nbsp;FIRMWARE&nbsp;<span class="br">]</span></div>
  <div class="sb-hr"><span class="sb-dot" id="dot"></span><span id="sbHR">-- GH/s</span></div>
 </div>
 <nav class="sb-nav" id="nav">
  <a onclick="go(0)" class="ac" id="n0"><svg viewBox="0 0 24 24"><rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/></svg><span class="nl">Dashboard</span></a>
  <a onclick="go(1)" id="n1"><svg viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/><circle cx="6" cy="6" r="1"/><circle cx="6" cy="18" r="1"/></svg><span class="nl">Pool</span></a>
  <a onclick="go(2)" id="n2"><svg viewBox="0 0 24 24"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg><span class="nl">Logs</span></a>
  <a onclick="go(3)" id="n3"><svg viewBox="0 0 24 24"><path d="M5 12.55a11 11 0 0 1 14.08 0"/><path d="M1.42 9a16 16 0 0 1 21.16 0"/><path d="M8.53 16.11a6 6 0 0 1 6.95 0"/><circle cx="12" cy="20" r="1"/></svg><span class="nl">Network</span></a>
  <div class="sb-sep"></div>
  <a onclick="go(4)" id="n4"><svg viewBox="0 0 24 24"><rect x="4" y="4" width="16" height="16" rx="2"/><rect x="9" y="9" width="6" height="6"/><line x1="9" y1="1" x2="9" y2="4"/><line x1="15" y1="1" x2="15" y2="4"/><line x1="9" y1="20" x2="9" y2="23"/><line x1="15" y1="20" x2="15" y2="23"/><line x1="20" y1="9" x2="23" y2="9"/><line x1="20" y1="14" x2="23" y2="14"/><line x1="1" y1="9" x2="4" y2="9"/><line x1="1" y1="14" x2="4" y2="14"/></svg><span class="nl">System</span></a>
  <a onclick="go(5)" id="n5"><svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg><span class="nl">Settings</span></a>
  <a onclick="go(6)" id="n6"><svg viewBox="0 0 24 24"><circle cx="6" cy="6" r="3"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="18" r="3"/><line x1="9" y1="6" x2="15" y2="6"/><line x1="6" y1="9" x2="6" y2="15"/><line x1="18" y1="9" x2="18" y2="15"/><line x1="9" y1="18" x2="15" y2="18"/></svg><span class="nl">Swarm</span></a>
 </nav>
 <div class="sb-foot">
  <div class="sb-creature" id="creatureFace">(^_^)</div>
  <b>D-Central Technologies</b><br>
  <span id="fwVer">DCENT_axe v?</span><br>
  <a href="https://d-central.tech/fund/go?source=dcent_axe&placement=dashboard" target="_blank" rel="noopener">Support open firmware &rarr;</a>
 </div>
</div>

<!-- ════ Main ════ -->
<div class="main" id="mainArea">
 <div class="topbar">
  <div class="topbar-left">
   <button class="hamburger" onclick="toggleSB()">&#9776;</button>
   <div class="crumbs"><span class="crumb-root">DCENT_axe</span><span class="crumb-sep">/</span><span class="crumb-cur" id="crumbPage">Dashboard</span></div>
   <div class="status-pills">
    <span class="pill pill-muted" id="tbMining" style="display:flex;align-items:center;gap:4px"><span class="sb-dot" id="tbMiningDot"></span>CONNECTING</span>
    <span class="pill" id="tbPool">Pool: --</span>
    <span class="pill" id="tbIp">IP: --</span>
    <span class="pill" id="tbWifi">WiFi: --</span>
   </div>
  </div>
  <div class="topbar-right">
   <span class="tb-mining-pill" id="tbMiningPill" data-state="pending"><span class="tb-mining-dot"></span>PENDING</span>
   <button class="tb-btn tb-pause" id="tbPauseBtn" onclick="toggleMiningPause()" title="Pause / resume mining" aria-label="Pause mining">
    <svg class="tb-pause-icon" viewBox="0 0 16 16" width="12" height="12"><rect x="3" y="2" width="3.5" height="12" rx="1"/><rect x="9.5" y="2" width="3.5" height="12" rx="1"/></svg>
    <span id="tbPauseLabel">PAUSE</span>
   </button>
   <button class="tb-btn tb-icon" onclick="toggleAlertPanel()" title="Notifications" aria-label="Notifications">
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/></svg>
    <span class="tb-bell-dot" id="tbBellDot" hidden></span>
   </button>
   <button class="tb-btn tb-icon" onclick="go(5)" title="Settings &amp; account" aria-label="Account">
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/></svg>
   </button>
   <button class="tb-btn" onclick="toggleCol()" title="Collapse sidebar">&#9664;</button>
   <button class="tb-btn danger" onclick="doReboot()" title="Reboot (Shift+R)">Reboot</button>
  </div>
 </div>
 <!-- Lightweight alert panel surfaced by tb bell. Read-only, dismissible. -->
 <div class="tb-alert-panel" id="tbAlertPanel" hidden>
  <div class="tb-alert-head"><span>Notifications</span><button class="tb-alert-close" onclick="toggleAlertPanel()">&times;</button></div>
  <div class="tb-alert-body" id="tbAlertBody"><div class="tb-alert-empty">No new alerts.</div></div>
 </div>
 <div id="alertBox" class="alert"></div>
 <div id="safeModeBanner" class="alert" style="display:none;align-items:center;justify-content:space-between;gap:12px">
  <div><b>Safe Mode active.</b> <span id="safeModeDetail">Mining is disabled until you clear the task-watchdog counter.</span></div>
  <button class="btn btn-danger btn-sm" onclick="clearSafeMode()">Clear &amp; Reboot</button>
 </div>
 <div id="coredumpBanner" class="alert" style="display:none;align-items:center;justify-content:space-between;gap:12px;background:rgba(255,165,0,0.08);border-color:rgba(255,165,0,0.3);color:var(--accent)">
  <div><b>Panic coredump stored.</b> Retrieve the ELF before a new crash overwrites it.</div>
  <div style="display:flex;gap:8px">
   <button class="btn btn-primary btn-sm" onclick="downloadCoredump()">Download</button>
   <button class="btn btn-ghost btn-sm" onclick="deleteCoredump()">Delete</button>
  </div>
 </div>
 <div class="offline-banner" id="offlineBanner"><span id="reconnectMsg">Offline</span></div>

 <div class="content">
<!-- ════════════════════ PAGE 0: DASHBOARD ════════════════════ -->
<div class="page ac" id="p0">
 <!-- MERGED HERO (loop 13, 2026-04-28). Mining Core sphere is THE hero;
      the legacy `<div class="hero">` was deleted because it duplicated
      the hashrate readout that core.js renders inside the sphere card.
      Block card sits beside it; KPI strip + meta pills slot below. -->
 <div class="hero-row" style="display:flex;gap:16px;align-items:stretch;margin-bottom:16px;flex-wrap:wrap">
  <!-- mining-core mount moved up here. The "10M AVERAGE" caption sits inside
       the wrapping div so the mining-core sphere fills the rest. -->
  <div class="hero-core-wrap" style="flex:1 1 480px;min-width:0;display:flex;flex-direction:column;gap:6px">
   <div class="hero-core-caption">Hashrate &middot; <span class="hero-core-caption-strong">10m Average</span></div>
   <div data-component="mining-core" class="core glass glow" style="flex:1 1 auto;min-width:0"></div>
  </div>
  <button type="button" class="block-card" id="blockCardHero" onclick="openBlockModal()" aria-label="Open block details" style="flex:0 1 320px;min-width:260px">
   <div class="block-card-head">
    <span class="block-title">Current Block</span>
    <span class="pill-live" id="blockLivePill" style="opacity:.45"><span class="live-dot"></span></span>
   </div>
   <div class="block-height" id="blockHeightBig">#--</div>
   <div class="block-hash-row">
    <span class="block-hash-label">HASH</span>
    <span class="block-hash-val" id="blockHashShort">mining&hellip;</span>
   </div>
   <div class="block-grid4">
    <div class="block-cell"><div class="block-cell-label">AGE</div><div class="block-cell-val" id="blockAgePill">--</div></div>
    <div class="block-cell"><div class="block-cell-label">TXS</div><div class="block-cell-val" id="blockTxsVal">--</div></div>
    <div class="block-cell"><div class="block-cell-label">REWARD</div><div class="block-cell-val"><span id="blockRewardVal">--</span> <span class="block-cell-unit">BTC</span></div></div>
    <div class="block-cell"><div class="block-cell-label">DIFF</div><div class="block-cell-val"><span id="blockDiffVal">--</span> <span class="block-cell-unit" id="blockDiffUnit"></span></div></div>
   </div>
   <div class="block-card-foot">
    <span>Tap for full block details</span>
    <span class="block-arrow">&rarr;</span>
   </div>
  </button>
 </div>

 <!-- KPI strip: efficiency / share rate / heat output. Heat Output shown in every mode. -->
 <div class="grid3" style="margin-bottom:16px">
  <div class="hero-kpi"><div class="hero-kpi-label">Efficiency</div><div class="hero-kpi-val"><span id="heroEffVal">--</span> <span class="eff-badge" id="effBadge"></span></div><div class="hero-kpi-sub">Lower J/TH = more efficient</div></div>
  <div class="hero-kpi"><div class="hero-kpi-label">Pool Share Rate</div><div class="hero-kpi-val" id="heroSatsVal">--</div><div class="hero-kpi-sub">Pool-accepted shares per hour</div></div>
  <div class="hero-kpi" data-tip="Heat output in BTU/h. Shown in every mode (every miner is a heater)."><div class="hero-kpi-label">Heat Output</div><div class="hero-kpi-val"><span id="btuVal" style="color:var(--accent)">--</span> BTU/h</div><div class="hero-kpi-sub" id="btuEquiv">--</div></div>
 </div>

 <!-- Thin meta strip: state badge + multi-window pills + board + hex hash -->
 <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap;margin-bottom:16px;font-family:var(--mono);font-size:10px;color:var(--dim);padding:8px 12px;background:rgba(255,255,255,0.012);border:1px solid var(--border);border-radius:var(--radius-sm)">
  <span class="status-badge warn" id="heroMiningState" style="font-size:10px;padding:4px 12px">Telemetry pending</span>
  <span class="hero-pool-pill" id="heroPoolPill" hidden></span>
  <span class="hero-pill" id="hr1m">1m: --</span>
  <span class="hero-pill" id="hr5m">5m: --</span>
  <span class="hero-pill" id="hr15m">15m: --</span>
  <span id="heroBoardModel" style="color:var(--dim);font-size:10px;margin-left:auto"></span>
  <span style="font-size:9px;color:var(--muted);letter-spacing:1px;width:100%;overflow:hidden;white-space:nowrap;text-overflow:ellipsis;opacity:0.6" id="heroHex">0x0000000000000000...</span>
 </div>

 <!-- Hidden ID-bound spans the inline JS update() still writes via S('hr')/
      S('hrUnit')/S('hrTrend'). The mining-core component owns the visible
      hashrate render; these stay so writes do not crash. -->
 <span id="hr" hidden>--</span><span id="hrUnit" hidden>GH/s</span><span id="hrTrend" hidden></span>

 <div class="grid4">
  <div class="stat" data-accent="green">
   <div class="stat-icon" style="background:var(--glow);color:var(--accent)"><svg viewBox="0 0 24 24"><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg></div>
   <div class="stat-label" data-tip="Overall mining health status">Health</div><div class="stat-val" id="healthQuick">--</div><div class="sparkline" id="hrSpk"></div><div class="stat-sub" id="healthQuickLabel">--</div>
  </div>
  <div class="stat" data-accent="cyan">
   <div class="stat-icon" style="background:rgba(34,211,238,0.1);color:var(--cyan)"><svg viewBox="0 0 24 24"><path d="M14 14.76V3.5a2.5 2.5 0 0 0-5 0v11.26a4.5 4.5 0 1 0 5 0z"/></svg></div>
   <div class="stat-label" data-tip="ASIC die temperature. Firmware throttles above 90\u00B0C and cuts power above 105\u00B0C.">Temperature</div><div class="stat-val" id="tempQuick">--</div><div class="stat-sub" id="tempQuickLabel">--</div>
  </div>
  <div class="stat" data-accent="orange">
   <div class="stat-icon" style="background:rgba(247,147,26,0.1);color:var(--accent)"><svg viewBox="0 0 24 24"><polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2"/></svg></div>
   <div class="stat-label" data-tip="Highest difficulty share found this session">Best Diff</div><div class="stat-val" id="bestDiff">--</div><div class="stat-sub">All-time: <span id="bestEver" style="color:var(--accent)">--</span> | Up: <span id="uptime">--</span></div>
  </div>
  <div class="stat" data-accent="yellow">
   <div class="stat-icon" style="background:rgba(251,191,36,0.1);color:var(--yellow)"><svg viewBox="0 0 24 24"><path d="M13 2L3 14h9l-1 8 10-12h-9l1-8z"/></svg></div>
   <div class="stat-label" data-tip="Total electrical power consumption">Power</div><div class="stat-val" id="powerQuick">--</div><div class="sparkline" id="pwrSpk"></div><div class="stat-sub" id="powerQuickLabel">--</div>
  </div>
 </div>

 <div class="card">
  <div class="card-title">Performance</div>
  <div class="chart-legend"><label><input type=checkbox id="chkHR" checked onchange="toggleSeries()"><span style="color:var(--accent)">Hashrate</span></label><label><input type=checkbox id="chkTemp" checked onchange="toggleSeries()"><span style="color:var(--cyan)">Temp</span></label><label><input type=checkbox id="chkPwr" checked onchange="toggleSeries()"><span style="color:var(--yellow)">Power</span></label></div>
  <div class="chart-wrap"><canvas id="perfChart"></canvas></div>
 </div>

 <div class="grid2">
  <div class="card">
   <div class="card-title">Pool Shares</div>
   <div class="share-bar"><div class="acc" id="accBar" style="width:0"></div><div class="rej" id="rejBar" style="width:0"></div></div>
   <div style="display:flex;justify-content:space-between;font-size:10px;color:var(--dim);margin-top:6px">
    <span><span style="color:var(--accent);font-weight:600" id="accN">0</span> pool accepted</span>
    <span><span style="color:var(--red)" id="rejN">0</span> pool rejected</span>
    <span class="status-badge" id="accBadge">--</span>
   </div>
   <div style="display:flex;justify-content:space-between;margin-top:8px;font-size:11px">
    <span>Session Best: <span style="font-family:var(--mono);color:var(--accent)" id="sessionBestDiff">--</span></span>
    <span style="color:var(--dim)">Rate: <span style="font-family:var(--mono);color:var(--text)" id="sharesPerHr2">--</span>/hr</span>
   </div>
   <div class="share-dots" id="shareDots" data-tip="Recent pool responses — green=accepted, red=rejected"></div>
  </div>
  <div class="card">
   <div class="card-head"><div class="card-title">Power &amp; Thermals</div><span class="card-right-meta" id="pwrSummary">-- V · -- A</span></div>
   <div class="gauges6">
    <div class="gauge-row"><div class="gauge-head"><span class="gauge-label" data-tip="Total electrical power draw">Draw</span><span class="gauge-val"><span id="pw6Draw">--</span><span class="gauge-unit">W</span></span></div><div class="gauge-track"><div class="gauge-fill g-fill-green" id="pw6DrawBar"></div></div></div>
    <div class="gauge-row"><div class="gauge-head"><span class="gauge-label" data-tip="ASIC clock frequency">Freq</span><span class="gauge-val"><span id="pw6Freq">--</span><span class="gauge-unit">MHz</span></span></div><div class="gauge-track"><div class="gauge-fill g-fill-orange" id="pw6FreqBar"></div></div></div>
    <div class="gauge-row"><div class="gauge-head"><span class="gauge-label" data-tip="ASIC core voltage — affects hash and power">Core</span><span class="gauge-val"><span id="pw6Core">--</span><span class="gauge-unit">mV</span></span></div><div class="gauge-track"><div class="gauge-fill g-fill-green" id="pw6CoreBar"></div></div></div>
    <div class="gauge-row"><div class="gauge-head"><span class="gauge-label" data-tip="ASIC die temperature">ASIC</span><span class="gauge-val"><span id="pw6Asic">--</span><span class="gauge-unit">°C</span></span></div><div class="gauge-track"><div class="gauge-fill g-fill-cyan" id="pw6AsicBar"></div></div></div>
    <div class="gauge-row"><div class="gauge-head"><span class="gauge-label" data-tip="Voltage regulator temperature">VR</span><span class="gauge-val"><span id="pw6Vreg">--</span><span class="gauge-unit">°C</span></span></div><div class="gauge-track"><div class="gauge-fill g-fill-cyan" id="pw6VregBar"></div></div></div>
    <div class="gauge-row"><div class="gauge-head"><span class="gauge-label" data-tip="Fan duty cycle (RPM in hover)">Fan</span><span class="gauge-val"><span id="pw6Fan">--</span><span class="gauge-unit">%</span> <span id="pw6FanRpm" style="color:var(--dim);font-size:10px"></span></span></div><div class="gauge-track"><div class="gauge-fill g-fill-orange" id="pw6FanBar"></div></div></div>
   </div>
  </div>
 </div>

 <div class="card">
  <div class="card-head"><div class="card-title">Pool Status</div><span class="pill pill-muted" id="dPoolStatusPill">CONNECTING</span></div>
  <dl class="kvdl">
   <div class="kvdl-row"><span class="kvdl-k">URL</span><span class="kvdl-v truncate" id="dPoolUrl">--</span></div>
   <div class="kvdl-row"><span class="kvdl-k">Port</span><span class="kvdl-v" id="dPoolPort">--</span></div>
   <div class="kvdl-row"><span class="kvdl-k">Worker</span><span class="kvdl-v truncate" id="dPoolWorker">--</span></div>
   <div class="kvdl-row"><span class="kvdl-k">Protocol</span><span class="kvdl-v" id="dPoolProto">Stratum V1</span></div>
   <div class="kvdl-row"><span class="kvdl-k">Failback</span><span class="kvdl-v" id="dFailback">--</span></div>
   <div class="kvdl-row"><span class="kvdl-k">Pool Confirmed</span><span class="kvdl-v"><span class="text-green" id="dPoolAcc">0</span><span class="dim" id="dPoolRejMeta">/ 0 rejected</span></span></div>
   <div class="kvdl-row"><span class="kvdl-k">Submit State</span><span class="kvdl-v" id="dPoolTruth">--</span></div>
   <div class="kvdl-row"><span class="kvdl-k">Quality</span><span class="kvdl-v" id="dShareQuality">--</span></div>
   <div class="kvdl-row"><span class="kvdl-k">Recovery</span><span class="kvdl-v" id="dRecovery">--</span></div>
   <div class="kvdl-row"><span class="kvdl-k" data-tip="Pool Target Difficulty: the pool-required minimum for a share to count (vardiff), not the Achieved Difficulty a share actually hit">Share Target</span><span class="kvdl-v" id="dPoolTarget">--</span></div>
  </dl>
 </div>

 <div class="card">
  <div class="card-title">Difficulty Explorer</div>
  <div style="font-family:var(--mono);font-size:11px">
   <div style="display:flex;align-items:center;gap:10px;margin:8px 0"><span style="color:var(--dim);min-width:55px">Session</span><div class="gauge" style="flex:1;margin:0"><div class="gauge-fill" id="dxBarSession" style="background:var(--accent);width:0"></div></div><span style="min-width:50px;text-align:right" id="dxSession">--</span></div>
   <div style="display:flex;align-items:center;gap:10px;margin:8px 0"><span style="color:var(--dim);min-width:55px">All-time</span><div class="gauge" style="flex:1;margin:0"><div class="gauge-fill" id="dxBarAllTime" style="background:var(--yellow);width:0"></div></div><span style="min-width:50px;text-align:right;color:var(--yellow)" id="dxAllTime">--</span></div>
   <div style="display:flex;align-items:center;gap:10px;margin:8px 0"><span style="color:var(--dim);min-width:55px">Pool</span><div class="gauge" style="flex:1;margin:0"><div class="gauge-fill" id="dxBarPool" style="background:var(--cyan);width:0"></div></div><span style="min-width:50px;text-align:right;color:var(--cyan)" id="dxPool">--</span></div>
  </div>
 </div>

 <!-- ════ Network card (CAP-OS2AXE-3) — INLINE, 0 new register_static handlers ════
      (a) Halving countdown: HalvingTimelineBar math reimplemented client-side
          from the ALREADY-CLIENT-SIDE d.blockHeight (no fetch, no field, no handler).
      (b) Mempool fee radial: MempoolFeeRadial pure-SVG 180° gauge built from a
          cross-origin browser fetch to mempool.space — the SAME pattern
          block-tile.js already uses, so it costs NO firmware handler, only the
          browser fetch. Fails SILENTLY to the em-dash empty state when blocked/
          offline (device may be on an isolated LAN). No new dashboard/network.js. -->
 <div class="card" id="networkCard">
  <div class="card-title">Network</div>
  <div class="grid2" style="align-items:start">
   <div>
    <div style="font-size:9px;text-transform:uppercase;letter-spacing:1px;color:var(--dim);margin-bottom:8px;font-weight:700">Halving Countdown</div>
    <div id="halvingReadout" style="display:flex;justify-content:space-between;align-items:baseline;font-family:var(--mono);margin-bottom:6px">
     <span><span id="halvingBlocksLeft" style="font-size:18px;color:var(--accent)">—</span> <span style="font-size:10px;color:var(--dim)">blocks left</span></span>
     <span id="halvingEta" style="font-size:11px;color:var(--dim)">—</span>
    </div>
    <div class="gauge" style="margin:0"><div class="gauge-fill" id="halvingBar" style="background:var(--accent);width:0"></div></div>
    <div id="halvingEra" style="font-size:10px;color:var(--muted);margin-top:6px;font-family:var(--mono)">—</div>
   </div>
   <div>
    <div style="font-size:9px;text-transform:uppercase;letter-spacing:1px;color:var(--dim);margin-bottom:8px;font-weight:700">Mempool Fees <span style="color:var(--muted)">sat/vB</span></div>
    <div id="mempoolRadial" style="text-align:center;min-height:130px"></div>
   </div>
  </div>
 </div>

 <div class="grid2">
  <div class="card">
   <div class="card-title">Thermal</div>
   <div class="therm-wrap"><canvas id="thermGauge" width="200" height="110"></canvas><div class="therm-label" id="thermLabel">--</div></div>
   <div style="font-family:var(--mono);font-size:16px;text-align:center;margin-top:4px" id="chipTempBig">--</div>
   <div style="display:flex;justify-content:center;gap:12px;margin-top:6px;font-size:10px;color:var(--dim)">
    <span id="boardTempPill">Board: <span id="boardTemp">--</span></span>
    <span id="vregTempPill">VReg: <span id="vregTemp">--</span></span>
    <span>Fan: <span id="fanPct">--</span>% (<span id="fanRpm">--</span>) <span id="fanProof" class="dim" data-tip="cut hash before noise: speed is proven only when a tach RPM reading is present">Unproved</span></span>
   </div>
  </div>
  <div class="card card-dense">
   <div class="card-title">Block Info</div>
   <div style="font-family:var(--mono)">
    <div class="kv-row"><span class="kv-key" data-tip="Current Bitcoin block height">Height</span><span class="kv-val" id="biHeight">--</span></div>
    <div class="kv-row"><span class="kv-key" data-tip="Shares submitted to the pool this session; pending responses are not counted as accepted">Submitted</span><span class="kv-val" id="biNonces">--</span></div>
    <div class="kv-row"><span class="kv-key" data-tip="Pool Target Difficulty: the pool-required minimum for a share to count (vardiff), not the Achieved Difficulty a share actually hit">Share Target</span><span class="kv-val" id="biPoolDiff">--</span></div>
    <div class="kv-row"><span class="kv-key" data-tip="Pool-confirmed accepted shares divided by accepted plus rejected responses">Acceptance</span><span class="kv-val" id="biAccRate">--</span></div>
   </div>
  </div>
 </div>

 <!-- ASIC Chips card — Phase 2.C asic-chips.js component. The component's
      render() replaces the innerHTML of the INNER data-component div only, so
      the persistent title + the summary header (chipCardCount/Model/Active/
      AvgTemp, written imperatively by asic-chips.js render()) survive every
      re-render. #chipCard stays the outer card so render() can still toggle
      the whole card's display:none when there are no chips. -->
 <div class="card" id="chipCard">
  <div class="card-head"><div class="card-title">ASIC Chips</div>
   <span class="card-right-meta" style="display:flex;gap:12px;font-family:var(--mono);font-size:10px;color:var(--dim)">
    <span>Chips <b id="chipCardCount" style="color:var(--text)">--</b></span>
    <span><b id="chipCardModel" style="color:var(--text)">--</b></span>
    <span>Active <b id="chipCardActive" style="color:var(--text)">--</b></span>
    <span>Avg <b id="chipCardAvgTemp" style="color:var(--text)">--</b></span>
   </span>
  </div>
  <div data-component="asic-chips">
   <div class="component-loading" style="font-family:var(--mono);font-size:11px;color:var(--dim);text-align:center;padding:14px">awaiting first telemetry…</div>
  </div>
 </div>
</div>

<!-- ════════════════════ PAGE 2: LOGS ════════════════════ -->
<div class="page" id="p2">
 <div class="log-crumb">Logs <span class="sep">/</span> dcent@<b id="logHost">bitaxe</b> <span class="sep">:</span> <span class="path">~/logs</span>
  <span class="log-crumb-pills">
   <span class="log-crumb-pill paused" id="logStatePill">PENDING</span>
   <span class="log-crumb-pill paused" id="logPausePill" onclick="logPause()" title="Pause / resume stream">&#10074;&#10074; PAUSE</span>
   <span class="log-crumb-pill" onclick="logClear()" title="Clear stream">&#x2715; CLEAR</span>
  </span>
 </div>
 <div class="card">
  <div class="log-stream-head"><span class="lhs">LOG STREAM &middot; LIVE</span><span id="logStreamMeta">events: <span id="logTotalN">0</span></span></div>
  <div class="log-grid">
   <div class="log-counters-col">
    <div class="log-counter-row ok"><div class="lc-k">Accepted</div><div class="lc-v" id="logAccN">0</div><div class="log-counter-bar"><i id="logAccBar"></i></div></div>
    <div class="log-counter-row rej"><div class="lc-k">Rejected</div><div class="lc-v" id="logRejN">0</div><div class="log-counter-bar"><i id="logRejBar"></i></div></div>
    <div class="log-counter-row hw"><div class="lc-k">HW Err</div><div class="lc-v" id="logHwN">0</div><div class="log-counter-bar"><i id="logHwBar"></i></div></div>
    <div class="log-counter-row stale"><div class="lc-k">Stale</div><div class="lc-v" id="logStaleN">0</div><div class="log-counter-bar"><i id="logStaleBar"></i></div></div>
    <div class="log-counter-row rec"><div class="lc-k">Reconnect</div><div class="lc-v"><span id="logRecN">0</span><span style="font-size:11px;color:var(--dim);margin-left:3px">&times;</span></div><div class="log-counter-bar"><i id="logRecBar"></i></div></div>
   </div>
   <div class="log-stream-wrap">
    <div class="log-filter-row">
     <input type=text id="logFilter" placeholder="filter...">
     <div class="log-filter-pills">
      <button class="log-filter-pill ac" data-cat="all" onclick="logCat('all',this)">All</button>
      <button class="log-filter-pill" data-cat="ok" onclick="logCat('ok',this)">Ok</button>
      <button class="log-filter-pill" data-cat="warn" onclick="logCat('warn',this)">Warn</button>
      <button class="log-filter-pill" data-cat="err" onclick="logCat('err',this)">Err</button>
      <button class="log-filter-pill" data-cat="pool" onclick="logCat('pool',this)">Pool</button>
      <button class="log-filter-pill" data-cat="asic" onclick="logCat('asic',this)">Asic</button>
      <button class="log-filter-pill" data-cat="auto" onclick="logCat('auto',this)">Auto</button>
     </div>
    </div>
    <div class="log-stream" id="logBody"></div>
   </div>
  </div>
 </div>
</div>

<!-- ════════════════════ PAGE 1: POOL ════════════════════ -->
<div class="page" id="p1">
 <div class="page-title">Pool Configuration</div>
 <div class="card">
  <div class="card-title">Primary Pool</div>
  <div class="field"><label>Protocol</label><select id="pProtocol"><option value="v1">Stratum V1</option><option value="v2">Stratum V2 (Noise)</option></select></div>
  <div class="field"><label>Pool Host</label><input id="pUrl"></div>
  <div class="field-row">
   <div class="field"><label>Port</label><input id="pPort" type=number></div>
   <div class="field"><label>Password</label><span class="pw-wrap"><input id="pPass" type=password placeholder="x"><button type="button" class="pw-toggle" onclick="togglePasswordVisibility('pPass',this)" aria-label="Show password" aria-pressed="false">&#x1F441;</button></span></div>
  </div>
  <div class="field"><label>Worker (BTC Address)</label><input id="pUser"></div>
  <div class="btn-row"><button class="btn btn-primary" onclick="savePool()">Save Pool</button></div>
 </div>
 <details id="ownTemplatesDetails">
  <summary>Own Block Templates (SV2 Proxy)</summary>
  <div class="card" style="margin-top:8px">
   <div class="field"><label><input type=checkbox id="ownTplEnable" onchange="ownTemplateChanged()"> Use a local Template/JD proxy</label></div>
   <div class="field"><label>SV2 Mining Proxy</label><input id="ownTplProxyUrl" placeholder="stratum2+tcp://dcentos.local:3336" oninput="ownTemplateChanged()"></div>
   <div class="field-row">
    <div class="field"><label>Template Provider</label><input id="ownTplProviderUrl" placeholder="sv2+tcp://127.0.0.1:8442"></div>
    <div class="field"><label>Job Declarator</label><input id="ownTplJdUrl" placeholder="sv2+tcp://pool-jds:34255"></div>
   </div>
   <div class="help-small">DCENT_axe mines through the standard SV2 proxy endpoint. DCENT_OS or another local proxy owns Template Distribution and Job Declaration.</div>
  </div>
 </details>
 <details>
  <summary>Fallback Pool Configuration</summary>
  <div class="card" style="margin-top:8px">
   <div class="field"><label>Protocol</label><select id="fbProtocol"><option value="v1">Stratum V1</option><option value="v2">Stratum V2 (Noise)</option></select></div>
   <div class="field"><label>Fallback Host</label><input id="fbUrl" placeholder="solo.ckpool.org"></div>
   <div class="field-row">
    <div class="field"><label>Port</label><input id="fbPort" type=number value="3333"></div>
    <div class="field"><label>Password</label><span class="pw-wrap"><input id="fbPass" type=password placeholder="x"><button type="button" class="pw-toggle" onclick="togglePasswordVisibility('fbPass',this)" aria-label="Show password" aria-pressed="false">&#x1F441;</button></span></div>
   </div>
   <div class="field"><label>Worker</label><input id="fbUser"></div>
  </div>
 </details>
 <details>
  <summary>Hashrate Splitting</summary>
  <div class="card" style="margin-top:8px">
   <div class="field"><label><input type=checkbox id="splitEnable" onchange="updateSplitPctLabel()"> Enable split pool</label></div>
   <div class="field-row">
    <div class="field"><label>Pool 1</label><div class="kv-val" id="splitPrimaryPct">80%</div></div>
    <div class="field"><label>Pool 2</label><input id="splitPct" type=number min=1 max=99 value=20 oninput="updateSplitPctLabel()"></div>
  </div>
   <div class="field"><label>Split Host</label><input id="splitUrl" placeholder="pool.example.com"></div>
   <div class="field"><label>Protocol</label><select id="splitProtocol"><option value="v1">Stratum V1</option><option value="v2">Stratum V2 (Noise)</option></select></div>
   <div class="field-row">
    <div class="field"><label>Port</label><input id="splitPort" type=number value="3333"></div>
    <div class="field"><label>Password</label><span class="pw-wrap"><input id="splitPass" type=password placeholder="x"><button type="button" class="pw-toggle" onclick="togglePasswordVisibility('splitPass',this)" aria-label="Show password" aria-pressed="false">&#x1F441;</button></span></div>
   </div>
   <div class="field"><label>Worker</label><input id="splitUser"></div>
  </div>
 </details>
 <div class="card" style="margin-top:12px">
  <div class="card-title">Pool Status</div>
  <div style="display:flex;align-items:center;gap:8px;margin-bottom:8px"><span class="sb-dot" id="poolConnDot"></span><span id="poolConnStatus">--</span><span class="pill" id="poolProtoPill" style="display:none;font-size:9px;padding:2px 8px">SV2</span><span style="color:var(--dim);font-size:10px;margin-left:auto" id="poolConnDur">--</span></div>
  <div>
   <div class="kv-row"><span class="kv-key">URL</span><span class="kv-val" id="poolUrl">--</span></div>
   <div class="kv-row"><span class="kv-key">Worker</span><span class="kv-val" id="poolWorker" style="max-width:200px;overflow:hidden;text-overflow:ellipsis">--</span></div>
   <div class="kv-row"><span class="kv-key">Share Target</span><span class="kv-val" id="poolDiff">--</span></div>
   <div class="kv-row"><span class="kv-key">Best Share</span><span class="kv-val" id="poolBestDiff">--</span></div>
   <div class="kv-row"><span class="kv-key">Shares/hr</span><span class="kv-val" id="sharesPerHr">--</span></div>
  </div>
  <div id="splitRuntime" style="display:none;margin-top:10px;border-top:1px solid var(--border);padding-top:10px"></div>
 </div>
</div>

<!-- ════════════════════ PAGE 3: NETWORK ════════════════════ -->
<div class="page" id="p3">
 <div class="page-title">Network Configuration</div>
 <div class="card">
  <div class="card-title">WiFi Settings</div>
  <div class="field"><label>Hostname</label><input id="netHostname" placeholder="DCENTaxe-gt"></div>
  <div class="field"><label>Wi-Fi SSID</label><input id="netSsidInput" placeholder="Network name"></div>
  <div class="field"><label>Wi-Fi Password</label><span class="pw-wrap"><input id="netWifiPass" type=password placeholder="Password"><button type="button" class="pw-toggle" onclick="togglePasswordVisibility('netWifiPass',this)" aria-label="Show password" aria-pressed="false">&#x1F441;</button></span></div>
  <div class="btn-row">
   <button class="btn btn-primary" onclick="saveNetwork()">Save</button>
   <button class="btn btn-danger" onclick="saveNetworkRestart()">Save & Restart</button>
   <button class="btn btn-ghost" onclick="enterSetup()">Setup Mode</button>
  </div>
 </div>
 <div class="card">
  <div class="card-title">Connection Status</div>
  <div>
   <div class="kv-row"><span class="kv-key">SSID</span><span class="kv-val" id="netSsid">--</span></div>
   <div class="kv-row"><span class="kv-key">IP Address</span><span class="kv-val" id="netIp">--</span></div>
   <div class="kv-row"><span class="kv-key">MAC</span><span class="kv-val" id="netMac">--</span></div>
   <div class="kv-row"><span class="kv-key">Signal</span><span class="kv-val" id="netRssi">--</span></div>
   <div class="kv-row"><span class="kv-key">Quality</span><span class="kv-val" id="netSigLabel">--</span></div>
   <div class="kv-row"><span class="kv-key">Uptime</span><span class="kv-val" id="netUptime">--</span></div>
  </div>
  <div class="gauge" style="margin-top:8px"><div class="gauge-fill" id="netSigBar" style="width:0"></div></div>
 </div>
</div>

<!-- ════════════════════ PAGE 5: SETTINGS ════════════════════ -->
<div class="page" id="p5">
 <div class="page-title">Settings</div>
 <div class="card">
  <div class="card-title">Mining Profile</div>
  <div class="field"><label>Preset</label><select id="presetSel" onchange="presetChanged()"><option>Loading...</option></select></div>
  <div style="font-size:10px;color:var(--dim)" id="presetInfo"></div>
  <div class="btn-row"><button class="btn btn-primary btn-sm" onclick="applyPreset()">Apply Preset</button></div>
  <details>
   <summary>Manual Override</summary>
   <div class="field-row" style="margin-top:8px">
    <div class="field"><label>Frequency (MHz)</label><input id="setFreq" type=number></div>
    <div class="field"><label>Voltage (mV)</label><input id="setVolt" type=number></div>
   </div>
   <button class="btn btn-ghost btn-sm" onclick="setHw()">Apply</button>
  </details>
 </div>
 <div class="card">
  <div class="card-title" data-tip="cut hash before noise: hash power is cut before fan noise is raised. Loud airflow is reserved for measured thermal need.">Fan Control</div>
  <div class="field"><label>Mode</label><select id="fanMode" onchange="fanModeChanged()"><option value="manual">Manual</option><option value="auto">Auto (Target Temp)</option></select></div>
  <!-- UXFLOW-SAFETY-1: derived PWM ZONE label next to the slider value. axe keeps its
       OWN floor (min=20) — do NOT import OS's 10-30 home clamp; only the zone VOCABULARY
       (Home cap ≤30 / Loud override ≤60 / Thermal override >60) and a11y aria-valuetext
       are shared. Behavior is unchanged; this is a label. -->
  <div id="fanManualFields"><div class="field"><label>Fan Speed: <span id="fanLabel">100</span>% <span id="fanZone" class="dim" style="font-size:10px"></span></label><input type=range id="fanSlider" min=20 max=100 value=100 aria-valuetext="100% Thermal override" oninput="S('fanLabel',this.value);fanZoneLabel(this.value)" onchange="setFan()"></div></div>
  <div id="fanAutoFields" style="display:none"><div class="field"><label>Target Temperature (C)</label><input id="fanTargetTemp" type=number value=65 min=40 max=80></div><button class="btn btn-ghost btn-sm" onclick="setFanAuto()">Set Auto</button></div>
  <!-- CAP-OS2AXE-2 lite fan-curve editor: vanilla-SVG 3-pt temp->PWM over axe's
       two-scalar fan model (KNEE+FLOOR). KEEP-UNIQUE §7: a richer INPUT, NOT a
       per-board multi-fan firmware curve (DCENT_OS-only). CONTROL write is owner-
       auth-gated via post('/api/system'); PWM clamps to axe's floor 20..100 (NOT OS
       10-30). +0 handlers/schema/register_static. Invariants pinned in
       dcentaxe-core s4_axe_capability_port_guards. -->
  <details id="fanCurveWrap" ontoggle="fanCurveRender()" style="margin-top:8px">
   <summary>Fan Curve (lite)</summary>
   <div style="font-size:10px;color:var(--dim);line-height:1.5;margin:6px 0"><b>Not</b> a per-point firmware curve (that's DCENT_OS-only) &mdash; shapes axe's target-temp + fan floor as a 3-point curve; Apply maps it to one knee + one floor. Clamped to the axe fan floor (20-100%). <span data-tip="cut hash before noise: hash power is cut before fan noise is raised. Loud airflow is reserved for measured thermal need.">cut hash before noise</span>.</div>
   <svg id="fanCurveSvg" viewBox="0 0 320 180" style="width:100%;height:auto;touch-action:none;user-select:none" role="img" aria-label="Fan curve editor: temperature versus PWM, axe floor 20 to 100 percent"></svg>
   <div class="btn-row"><button class="btn btn-ghost btn-sm" onclick="fanCurveReset()">Reset</button><button class="btn btn-primary btn-sm" onclick="fanCurveApply()">Apply Curve</button></div>
  </details>
 </div>
  <div class="card">
   <div class="card-title">Autotuner</div>
   <div class="field"><label><input type=checkbox id="atEnable" onchange="atChanged()"> Enable Autotuner</label></div>
   <div class="field"><label>Mode</label><select id="atMode" onchange="updateAtDesc()"><option value=max_hashrate>Max Hashrate</option><option value=best_efficiency>Best Efficiency</option><option value=target_watts>Target Watts</option><option value=target_temp>Target Temp</option></select></div>
   <div style="font-size:10px;color:var(--dim);margin-bottom:8px" id="atModeDesc"></div>
   <div class="field"><label>Target Value</label><input id="atTarget" type=number></div>
   <div style="font-size:11px;margin-bottom:8px">Status: <span id="atStatus">Unavailable</span></div>
   <button class="btn btn-ghost btn-sm" onclick="setAt()">Apply</button>
   <!-- ════ Autotuner Evidence tiles (CAP-OS2AXE-1) ════
        Read-only port of the OS AutotunerEvidencePanel IA + honesty copy
        (NOT the React panel). Inline rows reading d.dcentaxe.autotuner.* —
        no new file, no new register_static handler, ~0 OTA delta. Truth
        contract (data-model-fields §7.1/§7.2): silicon_grade is DERIVED
        (measured error-rate/nonce), never a factory bin, with an honest
        "unknown" empty state; the last_good_* values are PERSISTED last-
        session evidence, NOT the live setpoint. Do NOT soften this copy. -->
   <div class="at-evidence" style="margin-top:14px;padding-top:12px;border-top:1px solid var(--border)">
    <div style="font-size:9px;text-transform:uppercase;letter-spacing:1px;color:var(--dim);margin-bottom:4px;font-weight:700">Evidence</div>
    <div style="font-size:10px;color:var(--dim);margin-bottom:10px">Receipts from the last tuning run — persisted to NVS, not live-proven.</div>
    <div class="kv-row"><span class="kv-key">Silicon Grade <span class="status-badge" style="background:rgba(110,110,128,.18);color:var(--dim)">derived</span></span><span class="kv-val" id="atEvGrade">&mdash;</span></div>
    <div style="font-size:9px;color:var(--muted);margin:-2px 0 6px">derived (measured error-rate/nonce), not factory bin</div>
    <div class="kv-row"><span class="kv-key">Best Efficiency</span><span class="kv-val"><span class="eff-badge" id="atEvEffBadge" style="display:none"></span> <span id="atEvEff">&mdash;</span></span></div>
    <div style="font-size:9px;color:var(--muted);margin:-2px 0 6px">measured &mdash; J/TH, lower is better</div>
    <div class="kv-row"><span class="kv-key">Last Good Freq <span class="status-badge" style="background:rgba(110,110,128,.18);color:var(--dim)">persisted</span></span><span class="kv-val" id="atEvLgFreq">&mdash;</span></div>
    <div class="kv-row"><span class="kv-key">Last Good Voltage</span><span class="kv-val" id="atEvLgVolt">&mdash;</span></div>
    <div class="kv-row"><span class="kv-key">Last Good J/TH</span><span class="kv-val" id="atEvLgJth">&mdash;</span></div>
    <div class="kv-row"><span class="kv-key">Last Good Error Rate</span><span class="kv-val" id="atEvLgErr">&mdash;</span></div>
    <div style="font-size:9px;color:var(--muted);margin:4px 0 0">persisted, not live-proven &mdash; last-session evidence, not the current setpoint</div>
   </div>
  </div>
  <div class="card">
   <div class="card-title">Home Assistant (MQTT)</div>
   <div style="font-size:11px;color:var(--dim);margin-bottom:8px">Publish hashrate, ASIC temp, power, fan RPM, shares &amp; uptime to an MQTT broker with Home Assistant auto-discovery &mdash; your miner appears in HA automatically. Outbound &amp; publish-only; never affects mining. <b>Implemented + unit-tested; live broker delivery not yet field-proven.</b></div>
   <div class="field"><label><input type=checkbox id="mqttEnable"> Enable MQTT publishing</label></div>
   <div class="field-row">
    <div class="field"><label>Broker Host</label><input id="mqttHost" type=text placeholder="203.0.113.10 or mqtt.local"></div>
    <div class="field"><label>Port</label><input id="mqttPort" type=number min=1 max=65535 placeholder="1883"></div>
   </div>
   <div class="field"><label>Username (optional)</label><input id="mqttUser" type=text placeholder="anonymous if blank"></div>
   <div class="field"><label>Password (optional)</label><span class="pw-wrap"><input id="mqttPass" type="password" placeholder="leave blank to keep current"><button type="button" class="pw-toggle" onclick="togglePasswordVisibility('mqttPass',this)" aria-label="Show password" aria-pressed="false">&#x1F441;</button></span></div>
   <div class="field-row">
    <div class="field"><label><input type=checkbox id="mqttTls"> Use TLS (mqtts)</label></div>
    <div class="field"><label>Publish Interval (s)</label><input id="mqttInterval" type=number min=5 max=3600 placeholder="30"></div>
   </div>
   <div class="btn-row"><button class="btn btn-primary btn-sm" onclick="saveMqtt()">Save MQTT Settings</button></div>
   <div style="font-size:10px;color:var(--muted);margin-top:8px">Enabling/disabling needs a restart to take effect; broker/credential changes apply on the next reconnect. TLS uses a plaintext LAN broker by default &mdash; cert provisioning is operator-gated.</div>
  </div>
  <div class="card">
   <div class="card-title">Daily Schedule</div>
   <div style="font-size:11px;color:var(--dim);margin-bottom:8px">Switch fixed profiles or autotuner policies by local time. Uses NTP when available; otherwise falls back to uptime.</div>
   <div class="field"><label><input type=checkbox id="schedEnable"> Enable Schedule</label></div>
   <div class="field-row">
    <div class="field"><label>UTC Offset (min)</label><input id="schedTz" type=number min=-720 max=840></div>
    <div class="field"><label>Clock</label><div style="font-size:11px;color:var(--dim)" id="schedClock">--</div></div>
   </div>
   <div id="schedRows"></div>
   <div class="btn-row"><button class="btn btn-ghost btn-sm" onclick="addScheduleRow()">Add Slot</button><button class="btn btn-primary btn-sm" onclick="saveSchedule()">Save Schedule</button></div>
  </div>
  <div class="card">
   <div class="card-title">Display</div>
   <div class="field"><label><input type=checkbox id="flipScreen" onchange="setDisplay()"> Flip Screen 180&deg;</label></div>
  </div>
  <!-- Appearance (formerly the top-level Theme page; demoted under Settings
       per nav-ia-spine §7 to free a top-nav slot). The color-pick markup,
       IDs (colorPick, color-opt), and setAccent/colorKeydown handlers are
       moved verbatim so the accent picker keeps working unchanged. -->
  <div class="card">
   <div class="card-title">Appearance</div>
   <div class="field"><label>Accent Color</label></div>
   <div class="color-pick" id="colorPick" role="radiogroup" aria-label="Accent color">
    <div class="color-opt sel" style="background:#FAA500" role="radio" aria-checked="true" aria-label="D-Central Amber accent" tabindex="0" onclick="setAccent('#FAA500',this)" onkeydown="colorKeydown(event,'#FAA500',this)" title="D-Central Amber"><svg viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></svg></div>
    <div class="color-opt" style="background:#F7931A" role="radio" aria-checked="false" aria-label="Bitcoin Orange accent" tabindex="0" onclick="setAccent('#F7931A',this)" onkeydown="colorKeydown(event,'#F7931A',this)" title="Bitcoin Orange"><svg viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></svg></div>
    <div class="color-opt" style="background:#a855f7" role="radio" aria-checked="false" aria-label="Purple accent" tabindex="0" onclick="setAccent('#a855f7',this)" onkeydown="colorKeydown(event,'#a855f7',this)" title="Purple"><svg viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></svg></div>
    <div class="color-opt" style="background:#22d3ee" role="radio" aria-checked="false" aria-label="Cyan accent" tabindex="0" onclick="setAccent('#22d3ee',this)" onkeydown="colorKeydown(event,'#22d3ee',this)" title="Cyan"><svg viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></svg></div>
    <div class="color-opt" style="background:#34d399" role="radio" aria-checked="false" aria-label="Green accent" tabindex="0" onclick="setAccent('#34d399',this)" onkeydown="colorKeydown(event,'#34d399',this)" title="Green"><svg viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></svg></div>
    <div class="color-opt" style="background:#f87171" role="radio" aria-checked="false" aria-label="Red accent" tabindex="0" onclick="setAccent('#f87171',this)" onkeydown="colorKeydown(event,'#f87171',this)" title="Red"><svg viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></svg></div>
   </div>
   <div style="margin-top:16px;font-size:11px;color:var(--dim)">
    Dark theme only. D-Central's signature: data is the hero, orange is earned.
   </div>
   <div class="stat" style="max-width:200px;margin-top:16px">
    <div class="stat-label">Sample Metric</div>
    <div class="stat-val" style="color:var(--accent)">1,234</div>
    <div class="stat-sub">Accent color preview</div>
   </div>
  </div>
</div>

<!-- ════════════════════ PAGE 4: SYSTEM ════════════════════ -->
<div class="page" id="p4">
 <div class="page-title">System</div>
 <div class="card">
  <div class="card-title">Device Info</div>
  <div class="meta-mono" id="sysInfo">
   <div class="kv-flex-row"><span class="text-dim">Variant</span><span id="sysVariant">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">ASIC Model</span><span id="asicModel">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">Chips</span><span id="asicChips">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">Cores</span><span id="asicCores">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">Uptime</span><span id="sysUptime">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">Wi-Fi SSID</span><span id="sysSsid">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">IP Address</span><span id="sysIp">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">MAC Address</span><span id="mac">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">Free Heap</span><span id="heap">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">Firmware</span><span id="sysFw">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">ESP-IDF</span><span id="idfVer">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">Partition</span><span id="partition">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">Build</span><span id="sysBuild">--</span></div>
   <div class="kv-flex-row"><span class="text-dim">Reset Reason</span><span id="resetReason">--</span></div>
  </div>
  <div class="btn-row">
   <button class="btn btn-ghost btn-sm" onclick="copyDevInfo()">Copy Info</button>
   <button class="btn btn-ghost btn-sm" onclick="exportConfig()">Export Config</button>
  </div>
 </div>
 <div class="card">
  <div class="card-title">Self-Test</div>
  <div style="font-size:11px;color:var(--dim);margin-bottom:8px">Runs a 6-step factory diagnostic (I2C scan, core voltage, ASIC chain, temp sensors, fan tach, first share). Mining keeps running.</div>
  <div class="btn-row">
   <button class="btn btn-primary btn-sm" id="selfTestRunBtn" onclick="runSelfTest()">Run Self-Test</button>
   <button class="btn btn-ghost btn-sm" id="selfTestCancelBtn" onclick="cancelSelfTest()" style="display:none">Cancel</button>
  </div>
  <div id="selfTestProgress" style="margin-top:8px;font-size:11px;color:var(--dim)"></div>
  <div id="selfTestBody" style="margin-top:10px;font-family:var(--mono)"></div>
 </div>
  <div class="card">
   <div class="card-title">Owner Access</div>
   <div style="display:flex;align-items:center;gap:8px;margin-bottom:10px">
    <span class="status-badge" id="authStatusBadge">Checking...</span>
    <span style="font-size:11px;color:var(--dim)" id="authStatusText">Loading owner access state...</span>
   </div>
   <div id="authSetupBox" style="display:none">
    <div style="font-size:11px;color:var(--dim);margin-bottom:8px">Set the owner password once to protect write actions, MCP, and optional metrics access.</div>
    <div class="field"><label>New Owner Password</label><span class="pw-wrap"><input id="authSetupPass1" type="password" placeholder="At least 8 characters"><button type="button" class="pw-toggle" onclick="togglePasswordVisibility('authSetupPass1',this)" aria-label="Show password" aria-pressed="false">&#x1F441;</button></span></div>
    <small style="display:block;font-size:11px;color:var(--dim);margin:-4px 0 10px">At least 8 characters. Pick a passphrase you can remember &mdash; recovery requires a factory reset.</small>
    <div class="field"><label>Confirm Password</label><span class="pw-wrap"><input id="authSetupPass2" type="password" placeholder="Repeat password"><button type="button" class="pw-toggle" onclick="togglePasswordVisibility('authSetupPass2',this)" aria-label="Show password" aria-pressed="false">&#x1F441;</button></span></div>
    <div class="btn-row"><button class="btn btn-primary btn-sm" onclick="setupOwnerAccess()">Set Owner Password</button></div>
   </div>
   <div id="authLoginBox" style="display:none">
    <div style="font-size:11px;color:var(--dim);margin-bottom:8px">Sign in to change settings, upload firmware, or use other protected actions.</div>
    <div class="field"><label>Owner Password</label><span class="pw-wrap"><input id="authLoginPass" type="password" placeholder="Enter owner password"><button type="button" class="pw-toggle" onclick="togglePasswordVisibility('authLoginPass',this)" aria-label="Show password" aria-pressed="false">&#x1F441;</button></span></div>
    <div id="authLockout" style="display:none;margin-top:8px;padding:8px 10px;background:rgba(248,113,113,0.1);border:1px solid rgba(248,113,113,0.3);border-radius:var(--radius-sm);font-size:11px;color:var(--red)"></div>
    <div class="btn-row"><button class="btn btn-primary btn-sm" id="authLoginBtn" onclick="loginOwner()">Sign In</button> <button class="btn btn-ghost btn-sm" onclick="resetOwnerAccess()" title="Clear owner password. Keeps WiFi + pool config. Requires an active owner session.">Reset owner access</button></div>
    <div style="font-size:10px;color:var(--dim);margin-top:6px">Forgot the password? Reset clears it and keeps WiFi + pool config. Physical access = authorization.</div>
   </div>
   <div id="authSessionBox" style="display:none">
    <div style="font-size:11px;color:var(--dim);margin-bottom:8px" id="authSessionSummary">Signed in.</div>
    <div class="btn-row"><button class="btn btn-ghost btn-sm" onclick="logoutOwner()">Sign Out</button></div>
   </div>
   <details id="authSecurityBox" style="display:none;margin-top:10px">
    <summary>Security Settings</summary>
    <div class="field"><label><input type="checkbox" id="authMetricsRequireAuth"> Require owner auth for <code>/metrics</code></label></div>
    <div class="field"><label><input type="checkbox" id="authAllowUnsignedOta"> Allow unsigned OTA uploads (developer mode)</label></div>
    <div class="btn-row"><button class="btn btn-ghost btn-sm" onclick="saveSecuritySettings()">Save Security Settings</button></div>
    <div style="font-size:11px;color:var(--dim);margin:8px 0 4px">Change owner password</div>
    <div class="field"><label>Current Password</label><span class="pw-wrap"><input id="authCurrentPass" type="password" placeholder="Current password"><button type="button" class="pw-toggle" onclick="togglePasswordVisibility('authCurrentPass',this)" aria-label="Show password" aria-pressed="false">&#x1F441;</button></span></div>
    <div class="field-row">
     <div class="field"><label>New Password</label><span class="pw-wrap"><input id="authNewPass" type="password" placeholder="At least 8 characters"><button type="button" class="pw-toggle" onclick="togglePasswordVisibility('authNewPass',this)" aria-label="Show password" aria-pressed="false">&#x1F441;</button></span></div>
     <div class="field"><label>Confirm</label><span class="pw-wrap"><input id="authNewPass2" type="password" placeholder="Repeat password"><button type="button" class="pw-toggle" onclick="togglePasswordVisibility('authNewPass2',this)" aria-label="Show password" aria-pressed="false">&#x1F441;</button></span></div>
    </div>
    <div class="btn-row"><button class="btn btn-danger btn-sm" onclick="changeOwnerPassword()">Change Password</button></div>
   </details>
  </div>
  <div class="card">
   <div class="card-title">Integrations</div>
   <div style="font-size:11px;color:var(--dim);display:grid;gap:10px">
    <div>
     <div><b style="color:var(--text)">MCP</b> <span class="pill" style="font-size:9px;padding:2px 6px;margin-left:6px">AI control</span></div>
     <div>JSON-RPC 2.0 at <code style="font-family:var(--mono);color:var(--accent)">http://&lt;ip&gt;/mcp</code>. 14+ tools for status, frequency, pool config, self-test, swarm. Bearer token required.</div>
    </div>
    <div>
     <div><b style="color:var(--text)">Stratum V2</b> <span class="pill" id="sysSv2Pill" style="font-size:9px;padding:2px 6px;margin-left:6px;display:none">active</span></div>
     <div id="sysSv2Copy">Encrypted pool transport via Noise_NX handshake. Use <code style="font-family:var(--mono);color:var(--accent)">stratum2+tcp://</code> in the Pool page to enable.</div>
    </div>
    <div id="sysBapRow" style="display:none">
     <div><b style="color:var(--text)">BitAxe Touch accessory</b></div>
     <div>Touch display over BAP (UART_NUM_2, GPIO 40/39). Connect the stock <code style="font-family:var(--mono)">BAP-GT-TOUCH</code> board. Firmware advertises peers via <code style="font-family:var(--mono)">_dcentaxe._tcp</code>.</div>
    </div>
    <div>
     <div><b style="color:var(--text)">Home Assistant</b> <span class="pill" style="font-size:9px;padding:2px 6px;margin-left:6px">v1.1</span></div>
     <div>MQTT + auto-discovery for climate &amp; sensor entities. Not yet released.</div>
    </div>
   </div>
  </div>
  <div class="card">
   <div class="card-title">Firmware Update (OTA)</div>
  <div class="ota-zone" id="otaZone" onclick="document.getElementById('otaFile').click()">
   Drop .bin here or click to browse<br><span style="font-size:10px;color:var(--muted)">Current: <span id="otaCurrentVer">--</span></span>
  </div>
  <input type=file id="otaFile" accept=".bin" style="display:none" onchange="otaFileSelected()">
  <div class="btn-row" style="margin-top:8px"><button class="btn btn-ghost btn-sm" onclick="document.getElementById('otaManifestFile').click()">Select Manifest</button></div>
  <input type=file id="otaManifestFile" accept=".json" style="display:none" onchange="otaManifestSelected()">
  <div id="otaFileInfo" class="ota-meta"></div>
  <div id="otaManifestInfo" class="ota-meta"></div>
  <progress id="otaProg" value=0 max=100 style="display:none;width:100%;margin-top:8px;accent-color:var(--accent)"></progress>
  <div id="otaMsg" style="font-size:11px;color:var(--dim);margin-top:4px"></div>
  <div class="btn-row"><button class="btn btn-primary btn-sm" onclick="doOta()">Upload Update</button></div>
 </div>
 <div class="card">
  <div class="card-title">Overclock</div>
  <div class="field"><label><input type=checkbox id="ocEnable" onchange="ocToggle()"> Enable Overclock Mode</label></div>
  <div id="ocWarn" style="display:none;background:rgba(248,113,113,0.1);border:1px solid rgba(248,113,113,0.3);border-radius:var(--radius-sm);padding:10px;font-size:11px;color:var(--red);margin-top:8px">
   WARNING: Overclock increases power draw significantly. Ensure your PSU is rated for the higher load. D-Central is not responsible for damage.
   <div class="btn-row"><button class="btn btn-danger btn-sm" onclick="ocAccept()">Accept Risk</button><button class="btn btn-ghost btn-sm" onclick="ocCancel()">Cancel</button></div>
  </div>
 </div>
 <div class="card">
  <div class="card-title">Achievements (<span id="achBadge">0/24</span>)</div>
  <div class="ach-grid" id="achGrid"></div>
 </div>
</div>

<!-- ════════════════════ PAGE 6: SWARM ════════════════════ -->
<div class="page" id="p6">
 <div class="page-title">Swarm</div>
 <div class="card">
  <div class="card-title" style="display:flex;align-items:center;gap:10px">
   <span>Local Node</span>
   <span id="swarmRoleBadge" class="status-badge ok" style="font-size:10px;padding:2px 8px">Standalone</span>
  </div>
  <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(160px,1fr));gap:10px;font-size:12px">
   <div><span class="text-dim">Hostname</span><div id="swarmHost">--</div></div>
   <div><span class="text-dim">IP</span><div id="swarmIp">--</div></div>
   <div><span class="text-dim">Board</span><div id="swarmBoard">--</div></div>
   <div><span class="text-dim">Hashrate</span><div id="swarmHashrate">--</div></div>
   <div><span class="text-dim">Heat</span><div id="swarmHeat">--</div></div>
   <div><span class="text-dim">Queen</span><div id="swarmQueen">--</div></div>
  </div>
 </div>
 <div class="card">
  <div class="card-title">Discovery</div>
  <div class="help-small">mDNS auto-discovery is <b>deferred</b> in this build (peers register themselves via <code>POST /api/swarm/report</code>). Automatic LAN discovery + room-temp sync ship with v1.1.</div>
  <div class="meta-mono text-dim" style="line-height:1.7">
   <div>mDNS: <span id="swarmMdns">--</span></div>
   <div>API: <span id="swarmApi">--</span></div>
   <div>MCP: <span id="swarmMcp">--</span></div>
   <div id="swarmHint"></div>
  </div>
 </div>
 <div class="card">
  <div class="card-title">Peers (<span id="swarmPeerCount">0</span>)</div>
  <div id="swarmPeers" class="help-small">No peers reported yet. Other miners announce themselves via <code>POST /api/swarm/report</code>.</div>
 </div>
 <div class="card">
  <div class="card-title">Room-temp source</div>
  <div class="help-small">How the Space Heater autotuner chooses a target temperature.</div>
  <div role="radiogroup" aria-label="Room-temp source" style="display:flex;gap:12px;flex-wrap:wrap">
   <label><input type="radio" name="roomTempSrc" value="local" checked onchange="setRoomTempSource(this.value)"> Local target (chip temp)</label>
   <label><input type="radio" name="roomTempSrc" value="swarm_average" onchange="setRoomTempSource(this.value)"> Swarm average</label>
   <label><input type="radio" name="roomTempSrc" value="external" onchange="setRoomTempSource(this.value)"> External sensor only</label>
  </div>
  <div id="roomTempStatus" class="help-small" style="margin-top:8px"></div>
 </div>
</div>


 </div><!-- /content -->
 <div class="mobile-tabs" id="mobileNav">
  <button onclick="go(0)" class="ac" id="mn0"><svg viewBox="0 0 24 24"><rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/></svg>Dash</button>
  <button onclick="go(1)" id="mn1"><svg viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/></svg>Pool</button>
  <button onclick="go(2)" id="mn2"><svg viewBox="0 0 24 24"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>Logs</button>
  <button onclick="go(5)" id="mn5"><svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9"/></svg>Settings</button>
 </div>
</div><!-- /main -->

<!-- Modal -->
<div class="modal-overlay" id="modalOverlay" onclick="if(event.target===this)this.classList.remove('show')">
 <div class="modal">
  <h3 id="modalTitle">Confirm</h3>
  <p id="modalMsg"></p>
  <div class="btn-row"><button class="btn btn-primary" id="modalConfirmBtn">OK</button><button class="btn btn-ghost" onclick="E('modalOverlay').classList.remove('show')">Cancel</button></div>
 </div>
</div>

<!-- Block Info Modal -->
<!-- Block Info Modal (Phase 2.A + 2.F).
     DOM contract owned by /dashboard/block-tile.js — NEVER strip a `bm*`
     ID without auditing block-tile.js renderBlockModal first. The component
     looks up: bmHeight, bmMetaHeight, bmSubsidy, bmFees, bmReward,
     bmTimestamp, bmMetaTime, bmAge, bmClean, bmJobId, bmPrevHash,
     bmPrevHashFull, bmMempoolLink, bmShares, bmBestDiff, bmPool,
     bmDifficulty, bmOdds, bmMode, bmPayout, bmRewardPct, bmCoinbaseHint,
     bmCoinbaseOutputs. Also styled via /dashboard/block-tile.css using
     .bm / .bm-head / .bm-reward / .bm-cb / .bm-section / .bm-tbl. -->
<div class="bm-backdrop" id="blockModalBack" onclick="if(event.target===this)closeBlockModal()">
 <div class="bm" role="dialog" aria-labelledby="blockModalTitle">
  <button type="button" class="bm-x" onclick="closeBlockModal()" aria-label="Close">&times;</button>

  <div class="bm-head">
   <div>
    <div class="bm-eyebrow">
     <!-- D-Central molecule mark — same shape as the sidebar `.sb-mark`
          and `assets/d-central-mark.png`: LARGEST top-right (r=7), MEDIUM
          bottom-center (r=5), SMALLEST left (r=3.5), L-shaped bonds.
          Theme-reactive via `.bm-brand { color: var(--accent) }` — the
          orb circles use solid currentColor (no gradient). -->
     <svg class="bm-brand" viewBox="0 0 32 32" aria-hidden="true">
      <line x1="7.5" y1="14" x2="14.5" y2="20" stroke="#0b0e13" stroke-width="2.4" stroke-linecap="round"/>
      <line x1="16" y1="19" x2="21" y2="13" stroke="#0b0e13" stroke-width="2.4" stroke-linecap="round"/>
      <circle cx="22" cy="10" r="7" fill="currentColor"/>
      <circle cx="15" cy="22" r="5" fill="currentColor"/>
      <circle cx="6.5" cy="13.5" r="3.5" fill="currentColor"/>
      <ellipse class="hl" cx="20.4" cy="6.2" rx="3.4" ry="1.4"/>
      <ellipse class="hl" cx="13.6" cy="19.5" rx="2.3" ry="1.0"/>
      <ellipse class="hl" cx="5.7" cy="11.8" rx="1.5" ry="0.7"/>
     </svg>
     <span id="bmMode" class="pill">--</span>
     <span id="bmPayout" style="font-size:10px;color:var(--dim);font-family:var(--mono);letter-spacing:1px">--</span>
    </div>
    <div class="bm-title" id="blockModalTitle">
     <span class="bm-glyph">&#x29C6;</span> BLOCK <b id="bmHeight">--</b>
    </div>
    <div class="bm-full">
     <div class="k">PREV BLOCK HASH</div>
     <code id="bmPrevHashFull">--</code>
     <div style="display:flex;gap:10px;margin-top:6px;align-items:center">
      <button type="button" class="copy-btn" onclick="copyBlockHash()" style="font-size:10px">Copy</button>
      <a id="bmMempoolLink" href="#" target="_blank" rel="noopener" style="font-size:10px;color:var(--accent);text-decoration:none">View on mempool.space &rarr;</a>
     </div>
    </div>
   </div>
   <div class="bm-reward">
    <div class="k">YOUR SHARE OF REWARD</div>
    <div class="v" id="bmRewardPct">--</div>
    <div style="font-size:10px;color:var(--dim);margin-top:6px">SUBSIDY <b id="bmReward">--</b></div>
    <div style="font-size:10px;color:var(--dim)">FEES <b id="bmFees">--</b></div>
    <span id="bmSubsidy" hidden>--</span>
   </div>
  </div>

  <!-- Solo / pool verification hint, populated by block-tile.js once
       coinbase outputs are decoded by firmware. -->
  <div id="bmCoinbaseHint" style="margin-top:18px;padding:12px 14px;background:rgba(255,255,255,0.022);border:1px solid rgba(255,255,255,0.06);border-radius:8px;font-size:11px;line-height:1.55;color:var(--text);display:none"></div>

  <!-- Coinbase outputs table (component renders inside this div). -->
  <div class="bm-section" id="bmCoinbaseOutputs"></div>

  <!-- Block facts grid -->
  <div class="bm-section">
   <div class="bm-section-h">BLOCK FACTS</div>
   <div class="bm-contribution">
    <div class="bm-cb"><div class="k">HEIGHT</div><div class="v" id="bmMetaHeight">--</div></div>
    <div class="bm-cb"><div class="k">AGE</div><div class="v" id="bmAge">--</div></div>
    <div class="bm-cb"><div class="k">TIMESTAMP</div><div class="v" id="bmMetaTime" style="font-size:11px">--</div></div>
    <div class="bm-cb"><div class="k">CLEAN JOB</div><div class="v" id="bmClean" style="font-size:13px">--</div></div>
   </div>
  </div>

  <!-- Mining contribution grid -->
  <div class="bm-section">
   <div class="bm-section-h">YOUR CONTRIBUTION</div>
   <div class="bm-contribution">
    <div class="bm-cb"><div class="k">POOL OK</div><div class="v" id="bmShares">--</div></div>
    <div class="bm-cb"><div class="k">BEST DIFF</div><div class="v" id="bmBestDiff">--</div></div>
    <div class="bm-cb"><div class="k">DIFFICULTY</div><div class="v" id="bmDifficulty">--</div></div>
    <div class="bm-cb"><div class="k">POOL</div><div class="v" id="bmPool" style="font-size:11px">--</div></div>
   </div>
   <div class="bm-cb" style="margin-top:10px;text-align:center">
    <div class="k">PROBABILITY OF SOLVING THIS BLOCK</div>
    <div class="v" id="bmOdds" style="font-size:14px;color:var(--accent);margin-top:4px">--</div>
   </div>
  </div>

  <!-- Stratum trace footer -->
  <div class="bm-full" style="margin-top:18px">
   <div class="k">STRATUM TRACE</div>
   <code id="bmJobId">--</code>
   <code id="bmTimestamp" style="margin-top:4px">--</code>
   <code id="bmPrevHash" hidden>--</code>
  </div>
 </div>
</div>

<div class="toast-container" id="toastContainer"></div>

<!-- CAP-OS2AXE-6 lite first-run wizard: dismissible 3-4 step overlay reusing the OS
     SetupWizard step ORDERING (Welcome+safety-ack -> Pool/worker -> Mode/heater-target
     -> Review) + the freedom-first "recommended, not required" opt-out copy. DROPS the
     OS-only industrial steps (Circuit / PSU override / Calibration / Power source) per
     keep-unique §4.7. Additive: does NOT replace the SoftAP captive-portal Setup Mode
     (provisioning.rs untouched). Apply reuses the auth-gated post('/api/system') +
     autotune writes (+0 handlers/schema). The keep-unique negative guard scopes only
     the marked region below, so this comment (which names the dropped steps) lives
     OUTSIDE those markers on purpose. Invariants pinned in dcentaxe-core S4 guards. -->
<div id="frOverlay" role="dialog" aria-modal="true" aria-label="First-run setup" style="display:none;position:fixed;inset:0;z-index:500;background:rgba(5,7,9,0.78);backdrop-filter:blur(6px);-webkit-backdrop-filter:blur(6px);align-items:center;justify-content:center;padding:20px">
 <div style="width:460px;max-width:calc(100vw - 32px);background:var(--s-overlay);border:1px solid var(--border-hi);border-radius:var(--radius);box-shadow:var(--shadow-float);padding:20px;font-family:var(--font)">
  <!--WIZ-FR-START-->
  <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:6px">
   <div style="font-family:var(--mono);font-size:10px;letter-spacing:1.4px;text-transform:uppercase;color:var(--accent)">&gt; Setup</div>
   <button class="btn btn-ghost btn-sm" onclick="firstRunSkip()">Skip for now</button>
  </div>
  <div style="font-size:10px;color:var(--dim);margin-bottom:14px">Guided setup is <b>recommended, not required</b> &mdash; every step is optional and you can change anything later in Settings.</div>
  <div id="frSteps">
   <div class="fr-step" data-fr-step="0">
    <div style="font-size:16px;font-weight:700;margin-bottom:8px">Welcome to DCENT_axe</div>
    <div style="font-size:12px;color:var(--dim);line-height:1.6;margin-bottom:12px">A lean, MCP-native BitAxe miner and smart space heater. This quick setup gets you mining &mdash; skip any step you like.</div>
    <label style="display:flex;gap:8px;font-size:11px;color:var(--text);align-items:flex-start;line-height:1.5"><input type=checkbox id="frSafetyAck"> <span>I understand DCENT_axe runs a home space heater that <b>cuts hash before raising fan noise</b>, and I'll keep it somewhere safe and ventilated. (Recommended, not required.)</span></label>
   </div>
   <div class="fr-step" data-fr-step="1" style="display:none">
    <div style="font-size:14px;font-weight:700;margin-bottom:10px">Pool / Worker</div>
    <div class="field"><label>Pool URL (stratum)</label><input id="frPoolUrl" placeholder="stratum+tcp://public-pool.io"></div>
    <div class="field-row"><div class="field"><label>Port</label><input id="frPoolPort" type=number placeholder="3333"></div><div class="field"><label>Worker (BTC address)</label><input id="frPoolUser" placeholder="bc1q..."></div></div>
    <div style="font-size:10px;color:var(--muted)">Recommended, not required &mdash; leave blank to configure later on the Pool page.</div>
   </div>
   <div class="fr-step" data-fr-step="2" style="display:none">
    <div style="font-size:14px;font-weight:700;margin-bottom:10px">Mode / Heater Target</div>
    <div class="field"><label>Autotuner Mode</label><select id="frMode"><option value=best_efficiency>Best Efficiency</option><option value=max_hashrate>Max Hashrate</option><option value=target_watts>Target Watts</option><option value=target_temp>Target Temp</option></select></div>
    <div class="field"><label>Target (optional)</label><input id="frTarget" type=number placeholder="optional"></div>
    <div style="font-size:10px;color:var(--muted)">Best Efficiency is the recommended home default &mdash; not required.</div>
   </div>
   <div class="fr-step" data-fr-step="3" style="display:none">
    <div style="font-size:14px;font-weight:700;margin-bottom:10px">Review</div>
    <div id="frReview" style="font-size:11px;color:var(--dim);line-height:1.9"></div>
    <div style="font-size:10px;color:var(--muted);margin-top:10px">Applying writes only the fields you filled, through the same authenticated path as Settings. Nothing here is mandatory &mdash; recommended, not required.</div>
   </div>
  </div>
  <div style="display:flex;justify-content:space-between;align-items:center;margin-top:16px">
   <button class="btn btn-ghost btn-sm" id="frBack" onclick="firstRunPrev()" style="visibility:hidden">Back</button>
   <span id="frDots" style="font-family:var(--mono);font-size:10px;color:var(--dim)"></span>
   <button class="btn btn-primary btn-sm" id="frNext" onclick="firstRunNext()">Next</button>
  </div>
  <!--WIZ-FR-END-->
 </div>
</div>

<!-- Modular dashboard runtime (Phase 2.A-3.2). Loaded BEFORE inline JS so
     window.state / defineComponent are available when update(d) fires. -->
<script src="/dashboard/framework.js"></script>
<script src="/dashboard/api.js"></script>
<script src="/dashboard/core.js"></script>
<script src="/dashboard/block-tile.js"></script>
<script src="/dashboard/asic-chips.js"></script>

<script>
/* ── Globals ── */
var HRH=[],TH=[],PH=[],TSH=[],MAX_PTS=120,_offline=0,_retryT=0,_retryDelay=10;
var _maxFreq=600,_maxVolt=1400,_maxW=25,_prevBlock=0,_hHist=[];
var _cfgFreq=400,_cfgVolt=1200,_fd=0,_ad=0,_dd=0,_oc=0,_miningOn=true;
var _prevAch=0,_lastShares,_lastRej,_offStart=-1,OFF_RANGES=[];
var _lastInfo=null;
var _otaManifest=null;
var FACES=['(x_x)','(;_;)','(-_-)','(._.)','(o_o)','(^_^)','(^o^)','(*_*)','(>v<)','(\\o/)','(\\o/)'];
var E=function(i){return document.getElementById(i)};
var S=function(i,v){var e=E(i);if(e)e.textContent=v};
var _curPage=0,_logPaused=0,_logFilter='all',_logBuf=[],_firstUpdate=1,_logMiningLabel='PENDING',_logMiningLive=0;
var _logCounters={accepted:0,rejected:0,hwErr:0,stale:0,reconnect:0};
/* Legacy class -> new level alias (keep old mlog('x','share') callers working). */
var _logLvlAlias={share:'ok',info:'pool',block:'blk'};
function _logNorm(cls){if(!cls)return'sys';cls=String(cls).toLowerCase();return _logLvlAlias[cls]||cls}
var _lastPoolFetch=0;
/* Animate number drop-in when value changes */
function flashEl(id,newVal){var e=E(id);if(!e)return;var old=e.textContent;if(old!==String(newVal)&&old!=='--'&&old!=='0'){e.style.transition='none';e.style.transform='translateY(-4px)';e.style.opacity='0.3';void e.offsetWidth;e.style.transition='transform .35s cubic-bezier(.22,1,.36,1),opacity .35s ease';e.style.transform='none';e.style.opacity='1'}}
/* Render inline SVG sparkline from data array */
function drawSpk(id,data,color){var el=E(id);if(!el||data.length<3)return;var w=el.clientWidth||100,h=24;var mx=Math.max.apply(null,data)||1;var pts=data.map(function(v,i){return(i/(data.length-1)*w)+','+(h-v/mx*(h-2)+1)}).join(' ');el.innerHTML='<svg viewBox="0 0 '+w+' '+h+'" preserveAspectRatio="none"><polyline points="'+pts+'" fill="none" stroke="'+color+'" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" opacity="0.7"/></svg>'}

/* ── Navigation ── */
var PAGE_NAMES=['Dashboard','Pool','Logs','Network','System','Settings','Swarm'];
function go(n){if(n===_curPage)return;E('n'+_curPage).classList.remove('ac');E('p'+_curPage).classList.remove('ac');_curPage=n;E('n'+n).classList.add('ac');E('p'+n).classList.add('ac');var cp=E('crumbPage');if(cp)cp.textContent=PAGE_NAMES[n]||'';if(n===0&&HRH.length>1)drawChart();closeSBMobile();
 /* Sync mobile nav */
 document.querySelectorAll('.mobile-tabs button').forEach(function(b){b.classList.remove('ac')});
 var mb=E('mn'+n);if(mb)mb.classList.add('ac')}
function toggleSB(){E('sidebar').classList.toggle('open')}
function closeSBMobile(){E('sidebar').classList.remove('open')}
function toggleCol(){document.body.classList.toggle('col');try{localStorage.setItem('sbCol',document.body.classList.contains('col')?'1':'0')}catch(e){}}
(function(){try{if(localStorage.getItem('sbCol')==='1')document.body.classList.add('col')}catch(e){}})();

/* ── Utilities ── */
var _authToken='';
var _authStatus=null,_sharedCfg=null;
(function(){_authToken=''})();
function fU(s){var d=s/86400|0,h=s%86400/3600|0,m=s%3600/60|0,sc=s%60;return(d?d+'d ':'')+(h?h+'h ':'')+(m+'m')+(d?'':(' '+sc+'s'))}
function fD(d){if(d>=1e12)return(d/1e12).toFixed(1)+'T';if(d>=1e9)return(d/1e9).toFixed(1)+'G';if(d>=1e6)return(d/1e6).toFixed(1)+'M';if(d>=1e3)return(d/1e3).toFixed(1)+'K';return Math.round(d)}
function fHR(v){return v>=1000?(v/1000).toFixed(2)+' TH/s':v>=1?v.toFixed(1)+' GH/s':(v*1000).toFixed(0)+' MH/s'}
function pct(v,mx){return Math.min(100,Math.max(0,mx>0?v/mx*100:0))+'%'}
function showModal(title,msg,onConfirm,btnText){S('modalTitle',title);S('modalMsg',msg);var btn=E('modalConfirmBtn');btn.textContent=btnText||'Confirm';btn.onclick=function(){E('modalOverlay').classList.remove('show');if(onConfirm)onConfirm()};E('modalOverlay').classList.add('show')}
function showToast(msg,type){var t=document.createElement('div');t.className='toast'+(type?' toast-'+type:'');t.textContent=msg;E('toastContainer').appendChild(t);setTimeout(function(){t.style.opacity='0';setTimeout(function(){t.remove()},300)},2700)}
/* ── Block Info Modal ── */
/* Legacy openBlockModal/closeBlockModal/renderBlockModal/fetchBlockInfo/
   pollBlockHero/copyBlockHash/subsidyForHeight/fmtBlockAge/fmtUnixUtc/
   truncHash all live in /dashboard/block-tile.js (Phase 2.A + 2.F).
   That file installs window.openBlockModal etc. as back-compat globals
   AND adds the chip drill-down + reward % + solo verification rows. */
var _blockFetchTimer=null;
function saveAuthToken(t){_authToken=t||''}
function authHeaders(base){var h=base||{};h['X-Requested-With']='XMLHttpRequest';if(_authToken)h['Authorization']='Bearer '+_authToken;return h}
function handleReadAuthFailure(r){if(r&&r.status===401){saveAuthToken('');renderAuthUi();loadAuthStatus();go(4);throw new Error('auth-required')}return r}
function extractSessionToken(d){return d&&d.session&&(d.session.token||d.session.session_token||d.session.api_token)||d&&d.session_token||d&&d.api_token||''}
function normalizeAuthStatus(d){return{passwordSet:!!(d&&(d.passwordSet||d.password_set)),activeSessions:(d&&(d.activeSessions!=null?d.activeSessions:d.active_sessions))||0,metricsRequireAuth:!!(d&&(d.metricsRequireAuth!=null?d.metricsRequireAuth:d.metrics_require_auth)),allowUnsignedOta:!!(d&&(d.allowUnsignedOta!=null?d.allowUnsignedOta:d.allow_unsigned_ota)),sessionAuth:d?d.sessionAuth!==false:true}}
function renderAuthUi(){var s=_authStatus||{passwordSet:false,activeSessions:0,metricsRequireAuth:true,allowUnsignedOta:false},loggedIn=!!_authToken,badge=E('authStatusBadge'),text=E('authStatusText');if(badge){badge.className='status-badge '+(loggedIn?'ok':'warn');badge.textContent=loggedIn?'Signed In':s.passwordSet?'Password Set':'Setup Required'}if(text){text.textContent=loggedIn?('Owner access active • '+s.activeSessions+' session'+(s.activeSessions===1?'':'s')):s.passwordSet?'Owner password is configured. Sign in to use protected actions.':'Protected actions are currently open until you set an owner password.'}if(E('authSetupBox'))E('authSetupBox').style.display=s.passwordSet?'none':'';if(E('authLoginBox'))E('authLoginBox').style.display=s.passwordSet&&!loggedIn?'':'none';if(E('authSessionBox'))E('authSessionBox').style.display=s.passwordSet&&loggedIn?'':'none';if(E('authSecurityBox'))E('authSecurityBox').style.display=s.passwordSet&&loggedIn?'':'none';if(E('authSessionSummary'))E('authSessionSummary').textContent=loggedIn?'Signed in with a bearer session stored in this browser tab.':'Not signed in.';if(E('authMetricsRequireAuth'))E('authMetricsRequireAuth').checked=!!(_sharedCfg&&_sharedCfg.auth&&_sharedCfg.auth.metricsRequireAuth);if(E('authAllowUnsignedOta'))E('authAllowUnsignedOta').checked=!!(_sharedCfg&&_sharedCfg.auth&&_sharedCfg.auth.allowUnsignedOta)}
function loadAuthStatus(){return fetch('/api/auth/status',{headers:{'X-Requested-With':'XMLHttpRequest'}}).then(function(r){return r.json()}).then(function(d){_authStatus=normalizeAuthStatus(d);renderAuthUi();return _authStatus}).catch(function(){_authStatus={passwordSet:false,activeSessions:0,metricsRequireAuth:true,sessionAuth:true};renderAuthUi();return _authStatus})}
function loadSharedCfg(){return fetch('/api/config/shared',{headers:authHeaders({})}).then(function(r){if(r.status===401){handleReadAuthFailure(r)} if(!r.ok)return null;return r.json()}).then(function(d){if(d){_sharedCfg=d;renderAuthUi()}return d}).catch(function(){return null})}
function ensureWriteAuth(){if(_authToken)return Promise.resolve(_authToken);return loadAuthStatus().then(function(s){if(!s.passwordSet)return null;return new Promise(function(resolve,reject){showAuthChoiceModal(function(choice){if(choice==='signin'){go(5);if(E('authLoginPass'))E('authLoginPass').focus();reject(new Error('Sign in on the System page to continue'))}else if(choice==='reset'){resetOwnerAccess();reject(new Error('Resetting owner access — device will reboot'))}else{reject(new Error('Cancelled'))}})})})}
function showAuthChoiceModal(cb){var back=E('blockModalBack');/* repurpose modal infra */ if(!E('authChoiceBack')){var d=document.createElement('div');d.className='modal-back';d.id='authChoiceBack';d.innerHTML='<div class="modal-sm" role="dialog"><div class="modal-head"><div><h3>Owner Access Required</h3><div class="modal-sub">This device has an owner password. Sign in to change settings or reset owner access from an active owner session.</div></div><button type="button" class="modal-close" onclick="closeAuthChoice(\'cancel\')">&times;</button></div><div class="modal-body"><p style="font-size:12px;color:var(--dim);margin-bottom:10px">Owner reset no longer accepts unauthenticated LAN requests.</p></div><div class="modal-foot"><button class="btn btn-ghost btn-sm" onclick="closeAuthChoice(\'cancel\')">Cancel</button><button class="btn btn-primary btn-sm" onclick="closeAuthChoice(\'signin\')">Sign In</button></div></div>';document.body.appendChild(d);d.onclick=function(ev){if(ev.target===d)closeAuthChoice('cancel')}}_authChoiceCb=cb;E('authChoiceBack').classList.add('show')}
var _authChoiceCb=null;
function closeAuthChoice(choice){var b=E('authChoiceBack');if(b)b.classList.remove('show');var cb=_authChoiceCb;_authChoiceCb=null;if(cb)cb(choice)}
function resetOwnerAccess(){if(!_authToken){showToast('Sign in before resetting owner access','warning');return}if(!confirm('Reset owner access? WiFi + pool config will be kept. Device will reboot.'))return;fetch('/api/auth/owner-reset',{method:'POST',headers:authHeaders({}),body:'{}'}).then(function(r){return r.json().then(function(d){return{ok:r.ok,data:d}})}).then(function(res){if(!res.ok)throw new Error(res.data&&res.data.detail||'Reset failed');saveAuthToken('');showToast('Owner access cleared — rebooting','ok');setTimeout(function(){location.reload()},4000)}).catch(function(e){showToast(e.message||'Reset failed','error')})}
function setupOwnerAccess(){var a=E('authSetupPass1').value,b=E('authSetupPass2').value;if(a.length<8){showToast('Password must be at least 8 characters','warning');return}if(a!==b){showToast('Passwords do not match','warning');return}fetch('/api/auth/setup',{method:'POST',headers:{'Content-Type':'application/json','X-Requested-With':'XMLHttpRequest'},body:JSON.stringify({password:a,label:'dashboard'})}).then(function(r){return r.json().then(function(d){return{ok:r.ok,data:d}})}).then(function(res){if(!res.ok)throw new Error(res.data&&res.data.detail||res.data&&res.data.error||'Setup failed');saveAuthToken(extractSessionToken(res.data));E('authSetupPass1').value='';E('authSetupPass2').value='';showToast('Owner password configured');return loadAuthStatus().then(loadSharedCfg)}).catch(function(e){showToast(e.message,'error')})}
/* Lockout countdown state shared between loginOwner and its interval. */
var _lockoutTimer=null;
function showLockoutCountdown(secs){
 var banner=E('authLockout');var btn=E('authLoginBtn');
 if(!banner)return;
 if(_lockoutTimer){clearInterval(_lockoutTimer);_lockoutTimer=null}
 var remaining=Math.max(0,Math.ceil(secs));
 var tick=function(){
  if(remaining<=0){banner.style.display='none';if(btn)btn.disabled=false;if(_lockoutTimer){clearInterval(_lockoutTimer);_lockoutTimer=null}return}
  banner.style.display='block';
  banner.textContent='Too many failed attempts. Try again in '+remaining+' s.';
  if(btn)btn.disabled=true;
  remaining--;
 };
 tick();_lockoutTimer=setInterval(tick,1000);
}
function loginOwner(){
 var pw=E('authLoginPass').value;if(!pw){showToast('Enter the owner password','warning');return}
 var btn=document.activeElement;var restore=_startBusy(btn&&btn.tagName==='BUTTON'?btn:null,'Signing in\u2026');
 fetch('/api/auth/session',{method:'POST',headers:{'Content-Type':'application/json','X-Requested-With':'XMLHttpRequest'},body:JSON.stringify({password:pw,label:'dashboard'})}).then(function(r){return r.json().then(function(d){return{ok:r.ok,status:r.status,data:d}})}).then(function(res){
  if(res.status===429){var ra=(res.data&&res.data.retryAfter)||60;showLockoutCountdown(ra);throw new Error(res.data&&res.data.detail||'Temporarily locked out')}
  if(!res.ok)throw new Error(res.data&&res.data.detail||res.data&&res.data.error||'Sign in failed');
  saveAuthToken(extractSessionToken(res.data));E('authLoginPass').value='';showToast('Signed in');
  if(E('authLockout'))E('authLockout').style.display='none';
  return loadAuthStatus().then(loadSharedCfg);
 }).catch(function(e){showToast(e.message,'error')}).finally(function(){if(restore)restore()})
}
function logoutOwner(){if(!_authToken){saveAuthToken('');renderAuthUi();return}fetch('/api/auth/session/current',{method:'DELETE',headers:authHeaders({})}).then(function(){saveAuthToken('');showToast('Signed out');renderAuthUi();return loadAuthStatus()}).catch(function(){saveAuthToken('');renderAuthUi()})}
function changeOwnerPassword(){var cur=E('authCurrentPass').value,np=E('authNewPass').value,np2=E('authNewPass2').value;if(np.length<8){showToast('New password must be at least 8 characters','warning');return}if(np!==np2){showToast('New passwords do not match','warning');return}fetch('/api/auth/password',{method:'POST',headers:authHeaders({'Content-Type':'application/json'}),body:JSON.stringify({currentPassword:cur,newPassword:np,label:'dashboard'})}).then(function(r){return r.json().then(function(d){return{ok:r.ok,data:d}})}).then(function(res){if(!res.ok)throw new Error(res.data&&res.data.detail||res.data&&res.data.error||'Password update failed');saveAuthToken(extractSessionToken(res.data));E('authCurrentPass').value='';E('authNewPass').value='';E('authNewPass2').value='';showToast('Owner password updated');return loadAuthStatus()}).catch(function(e){showToast(e.message,'error')})}
function saveSecuritySettings(){post('/api/config/shared',{auth:{metricsRequireAuth:E('authMetricsRequireAuth').checked,allowUnsignedOta:E('authAllowUnsignedOta').checked}},function(r){if(r&&r.ok){showToast('Security settings saved');loadSharedCfg()}else{showToast('Save failed','error')}})}
/* Password show/hide toggle. Used by the eye button beside every password
   input. Flips `type` between `password` and `text`, toggles aria-pressed,
   and swaps the eye glyph. */
function togglePasswordVisibility(id,btn){
 var el=E(id);if(!el)return;
 var visible=el.type==='text';
 el.type=visible?'password':'text';
 if(btn){btn.setAttribute('aria-pressed',String(!visible));btn.textContent=visible?'\u{1F441}':'\u{1F576}'}
}
/* Capture the button that triggered the current click. Works because
   document.activeElement is still the clicked element when an inline onclick
   handler fires. Callers that change focus before calling post() should pass
   {button: ...} explicitly. */
function _busyBtn(explicit){
 if(explicit)return explicit;
 var a=document.activeElement;
 return a&&(a.tagName==='BUTTON'||(a.tagName==='INPUT'&&(a.type==='button'||a.type==='submit')))?a:null;
}
function _startBusy(btn,label){
 if(!btn||btn._busy)return null;
 btn._busy=1;btn.disabled=true;
 var orig=btn.innerHTML;
 btn.innerHTML='<span class="spinner"></span>'+(label||'Saving\u2026');
 return function(){if(btn._busy){btn.disabled=false;btn.innerHTML=orig;btn._busy=0}};
}
/* Write helper. New opts:
     button     — element to disable while the fetch is in flight (auto-detected if omitted)
     reloadCfg  — on 2xx, call loadCfg()/loadSharedCfg() so form fields reflect saved values
                  (default true for /api/system and /api/config/shared)
     busyLabel  — override the pending-state text */
function post(u,b,cb,opts){
 opts=opts||{};
 var btn=_busyBtn(opts.button);
 var restore=_startBusy(btn,opts.busyLabel);
 var reload=opts.reloadCfg;
 if(typeof reload==='undefined')reload=(u==='/api/system'||u==='/api/config/shared');
 return ensureWriteAuth().then(function(){return fetch(u,{method:'POST',headers:authHeaders({'Content-Type':'application/json'}),body:JSON.stringify(b)})}).then(function(r){
  if(r&&r.status===401){saveAuthToken('');renderAuthUi();go(4);throw new Error('Session expired. Sign in again.')}
  if(r&&r.ok&&reload){if(u==='/api/config/shared'){loadSharedCfg()}else{loadCfg()}}
  if(r&&cb)cb(r);
  return r;
 }).catch(function(e){if(e.message!=='auth-required')showToast('Error: '+e.message,'error')}).finally(function(){if(restore)restore()})}

/* ── Theme / Accent ── */
function setAccent(c,el){var r=parseInt(c.slice(1,3),16),g=parseInt(c.slice(3,5),16),b=parseInt(c.slice(5,7),16);document.documentElement.style.setProperty('--accent',c);document.documentElement.style.setProperty('--glow','rgba('+r+','+g+','+b+',0.08)');document.querySelectorAll('.color-opt').forEach(function(o){o.classList.remove('sel');o.setAttribute('aria-checked','false')});if(el){el.classList.add('sel');el.setAttribute('aria-checked','true');el.focus()}try{localStorage.setItem('accent',c)}catch(e){};if(typeof drawChart==='function')drawChart()}
/* Keyboard support for the radiogroup. Space/Enter activates; arrows move focus. */
function colorKeydown(ev,c,el){
 if(ev.key==='Enter'||ev.key===' '){ev.preventDefault();setAccent(c,el);return}
 if(ev.key==='ArrowRight'||ev.key==='ArrowDown'||ev.key==='ArrowLeft'||ev.key==='ArrowUp'){
  ev.preventDefault();
  var opts=Array.prototype.slice.call(document.querySelectorAll('.color-opt'));
  var i=opts.indexOf(el);if(i<0)return;
  var next=(ev.key==='ArrowRight'||ev.key==='ArrowDown')?(i+1)%opts.length:(i-1+opts.length)%opts.length;
  var t=opts[next];var oc=t&&t.getAttribute('onclick');var m=oc&&oc.match(/setAccent\('(#[0-9A-Fa-f]+)'/);
  if(m)setAccent(m[1],t);
 }
}
(function(){try{var c=localStorage.getItem('accent');if(c){setAccent(c,null);var opts=document.querySelectorAll('.color-opt');opts.forEach(function(o){if(o.style.background===c||o.getAttribute('onclick').indexOf(c)>=0)o.classList.add('sel')})}}catch(e){}})();

/* ── Share Dots ── */
function addShareDot(ok){var c=E('shareDots');if(!c)return;var d=document.createElement('span');d.className='share-dot '+(ok?'acc':'rej');c.appendChild(d);while(c.children.length>50)c.removeChild(c.firstChild)}

/* ── Mining Log ── */
function mTs(){var d=new Date();return String(d.getHours()).padStart(2,'0')+':'+String(d.getMinutes()).padStart(2,'0')+':'+String(d.getSeconds()).padStart(2,'0')}
function mlog(msg,cls){var lvl=_logNorm(cls);_logBuf.push({msg:msg,ts:mTs(),lvl:lvl});if(_logBuf.length>200)_logBuf.shift();if(!_logPaused)renderLog()}
function _logBar(id,n,max){var el=E(id);if(el)el.style.width=Math.min(100,max>0?(n/max)*100:0).toFixed(1)+'%'}
function renderLog(){var el=E('logBody');if(!el)return;var fi=E('logFilter');var filt=fi?fi.value.toLowerCase():'';
 var html=_logBuf.filter(function(e){if(_logFilter!=='all'&&e.lvl!==_logFilter)return false;if(filt&&e.msg.toLowerCase().indexOf(filt)<0)return false;return true}).map(function(e){
  var label=e.lvl.toUpperCase();
  return'<div class="log-line lvl-'+e.lvl+'"><span class="ts">['+e.ts+']</span><span class="lvl">'+label+'</span>'+e.msg+'</div>'}).join('');
 el.innerHTML=html;el.scrollTop=el.scrollHeight;
 /* Counter totals */
 var c=_logCounters;
 S('logAccN',c.accepted.toLocaleString());S('logRejN',c.rejected.toLocaleString());S('logHwN',c.hwErr.toLocaleString());S('logStaleN',c.stale.toLocaleString());S('logRecN',c.reconnect.toLocaleString());
 S('logTotalN',_logBuf.length);
 var mx=Math.max(c.accepted,1);
 _logBar('logAccBar',c.accepted,mx);_logBar('logRejBar',c.rejected,mx);_logBar('logHwBar',c.hwErr,mx);_logBar('logStaleBar',c.stale,mx);_logBar('logRecBar',c.reconnect,Math.max(c.reconnect,5));
}
function logCat(cat,el){_logFilter=cat;document.querySelectorAll('.log-filter-pill').forEach(function(c){c.classList.remove('ac')});if(el)el.classList.add('ac');renderLog()}
function setLogMiningState(label,live){_logMiningLabel=label;_logMiningLive=live?1:0;var s=E('logStatePill');if(!s||_logPaused)return;s.textContent=label;s.classList.toggle('live',!!live);s.classList.toggle('paused',!live)}
function logPause(){_logPaused=!_logPaused;var p=E('logPausePill');if(p){p.innerHTML=_logPaused?'&#9658; RESUME':'&#10074;&#10074; PAUSE';p.classList.toggle('paused',!_logPaused);p.classList.toggle('live',false)}var s=E('logStatePill');if(s){if(_logPaused){s.classList.remove('live');s.classList.add('paused');s.textContent='PAUSED'}else{s.textContent=_logMiningLabel;s.classList.toggle('live',!!_logMiningLive);s.classList.toggle('paused',!_logMiningLive)}}if(!_logPaused)renderLog()}
function logClear(){_logBuf=[];_logCounters={accepted:0,rejected:0,hwErr:0,stale:0,reconnect:0};renderLog()}
(function(){var fi=E('logFilter');if(fi)fi.addEventListener('input',function(){if(!_logPaused)renderLog()})})();
var _prevPoolDiff=0,_prevSharesLog=0,_prevRejLog=0,_prevBlockLog=0,_mlogInit=0;

/* ── Chart ── */
function getAccentHex(){return getComputedStyle(document.documentElement).getPropertyValue('--accent').trim()||'#FAA500'}
function hexToRgba(hex,a){var r=parseInt(hex.slice(1,3),16),g=parseInt(hex.slice(3,5),16),b=parseInt(hex.slice(5,7),16);return'rgba('+r+','+g+','+b+','+a+')'}
function drawChart(){
 var c=E('perfChart'),ctx=c.getContext('2d');
 var W=c.parentElement.clientWidth,H=c.parentElement.clientHeight;
 c.width=W*2;c.height=H*2;ctx.scale(2,2);ctx.clearRect(0,0,W,H);
 var pad={l:45,r:10,t:10,b:25},cw=W-pad.l-pad.r,ch=H-pad.t-pad.b;
 var ac=getAccentHex();
 ctx.strokeStyle='rgba(255,255,255,0.04)';ctx.lineWidth=1;ctx.setLineDash([3,6]);
 var showHR=E('chkHR').checked,showT=E('chkTemp').checked,showP=E('chkPwr').checked;
 var hrMax=HRH.length>1?Math.max.apply(null,HRH)*1.2||1:1;
 var tMax=TH.length>1?Math.max(Math.max.apply(null,TH)*1.1,50):100;
 for(var i=0;i<=4;i++){var y=pad.t+ch*i/4;ctx.beginPath();ctx.moveTo(pad.l,y);ctx.lineTo(pad.l+cw,y);ctx.stroke();
  if(showHR&&HRH.length>1){ctx.fillStyle=ac;ctx.font='10px sans-serif';ctx.textAlign='right';ctx.fillText((hrMax*(1-i/4)/1e3).toFixed(1),pad.l-4,y+4)}}
 ctx.setLineDash([]);
 if(showHR&&HRH.length>1){ctx.strokeStyle=ac;ctx.lineWidth=2;ctx.shadowColor=ac;ctx.shadowBlur=12;ctx.beginPath();
  var pts=[];for(var i=0;i<HRH.length;i++)pts.push([pad.l+i/(HRH.length-1)*cw,pad.t+ch-(HRH[i]/hrMax)*ch]);
  ctx.moveTo(pts[0][0],pts[0][1]);for(var i=1;i<pts.length;i++){var cp=0.2;var dx2=(pts[Math.min(i+1,pts.length-1)][0]-pts[Math.max(i-1,0)][0])*cp;ctx.bezierCurveTo(pts[i-1][0]+dx2,pts[i-1][1],pts[i][0]-dx2,pts[i][1],pts[i][0],pts[i][1])}
  ctx.stroke();ctx.shadowBlur=0;
  /* Peak marker */
  var pk=0;for(var i=1;i<pts.length;i++)if(HRH[i]>HRH[pk])pk=i;
  if(pts.length>3){ctx.beginPath();ctx.arc(pts[pk][0],pts[pk][1],6,0,Math.PI*2);ctx.fillStyle=hexToRgba(ac,0.15);ctx.fill();ctx.beginPath();ctx.arc(pts[pk][0],pts[pk][1],3,0,Math.PI*2);ctx.fillStyle=ac;ctx.fill()}
  /* Gradient fill */
  ctx.lineTo(pad.l+cw,pad.t+ch);ctx.lineTo(pad.l,pad.t+ch);ctx.closePath();
  var grad=ctx.createLinearGradient(0,pad.t,0,pad.t+ch);grad.addColorStop(0,hexToRgba(ac,0.15));grad.addColorStop(0.5,hexToRgba(ac,0.04));grad.addColorStop(1,hexToRgba(ac,0));ctx.fillStyle=grad;ctx.fill()}
 if(showT&&TH.length>1){ctx.strokeStyle='#22d3ee';ctx.lineWidth=1.5;ctx.setLineDash([4,2]);ctx.beginPath();
  for(var i=0;i<TH.length;i++){var x=pad.l+i/(TH.length-1)*cw;var y=pad.t+ch-(TH[i]/tMax)*ch;i===0?ctx.moveTo(x,y):ctx.lineTo(x,y)}ctx.stroke();ctx.setLineDash([])}
 if(showP&&PH.length>1){var pm=Math.max.apply(null,PH)*1.2||25;ctx.strokeStyle='#fbbf24';ctx.lineWidth=1.5;ctx.setLineDash([2,2]);ctx.beginPath();
  for(var i=0;i<PH.length;i++){var x=pad.l+i/(PH.length-1)*cw;var y=pad.t+ch-(PH[i]/pm)*ch;i===0?ctx.moveTo(x,y):ctx.lineTo(x,y)}ctx.stroke();ctx.setLineDash([])}
 /* Time axis labels */
 if(TSH.length>1){ctx.fillStyle='rgba(255,255,255,0.3)';ctx.font='10px sans-serif';ctx.textAlign='center';
  var ticks=[0,Math.floor(TSH.length/4),Math.floor(TSH.length/2),Math.floor(TSH.length*3/4),TSH.length-1];
  for(var ti=0;ti<ticks.length;ti++){var idx=ticks[ti];if(idx<TSH.length){var tx=pad.l+idx/(TSH.length-1)*cw;var d2=new Date(TSH[idx]);var h=d2.getHours()%12||12,m=d2.getMinutes();ctx.fillText(h+':'+(m<10?'0':'')+m+(d2.getHours()>=12?'p':'a'),tx,pad.t+ch+18)}}}
}
function toggleSeries(){drawChart()}
/* Chart hover crosshair */
var _chartHoverIdx=-1;
E('perfChart').addEventListener('mousemove',function(e){if(HRH.length<2)return;var rect=this.getBoundingClientRect();var mx=e.clientX-rect.left;var W=rect.width;var pad_l=45,pad_r=10,cw=W-pad_l-pad_r;var idx=Math.round((mx-pad_l)/cw*(HRH.length-1));if(idx<0||idx>=HRH.length){_chartHoverIdx=-1;drawChart();return}if(idx===_chartHoverIdx)return;_chartHoverIdx=idx;drawChart();var c=this,ctx=c.getContext('2d');var H=rect.height;var pad={l:45,r:10,t:10,b:25},ch=H-pad.t-pad.b;var x=pad.l+idx/(HRH.length-1)*cw;var hrMax=Math.max.apply(null,HRH)*1.2||1;var ac=getAccentHex();ctx.save();ctx.scale(0.5,0.5);ctx.strokeStyle='rgba(255,255,255,0.15)';ctx.lineWidth=1;ctx.setLineDash([3,3]);ctx.beginPath();ctx.moveTo(x*2,pad.t*2);ctx.lineTo(x*2,(pad.t+ch)*2);ctx.stroke();ctx.setLineDash([]);var hy=pad.t+ch-(HRH[idx]/hrMax)*ch;ctx.beginPath();ctx.arc(x*2,hy*2,8,0,Math.PI*2);ctx.fillStyle=ac;ctx.globalAlpha=0.5;ctx.fill();ctx.globalAlpha=1;ctx.beginPath();ctx.arc(x*2,hy*2,4,0,Math.PI*2);ctx.fillStyle='#fff';ctx.fill();ctx.fillStyle='rgba(10,14,20,0.9)';ctx.fillRect((x-50)*2,(pad.t-2)*2,100*2,38*2);ctx.strokeStyle='rgba(255,255,255,0.1)';ctx.strokeRect((x-50)*2,(pad.t-2)*2,100*2,38*2);ctx.fillStyle=ac;ctx.font='bold 20px sans-serif';ctx.textAlign='center';ctx.fillText(fHR(HRH[idx]),x*2,(pad.t+12)*2);ctx.fillStyle='#94a3b8';ctx.font='16px sans-serif';ctx.fillText(TH[idx]?TH[idx].toFixed(0)+'C':'',x*2,(pad.t+28)*2);ctx.restore()});
E('perfChart').addEventListener('mouseleave',function(){_chartHoverIdx=-1;drawChart()});

/* ── Thermal Gauge ── */
function drawThermGauge(temp){
 var c=E('thermGauge'),ctx=c.getContext('2d');ctx.clearRect(0,0,200,110);
 var cx=100,cy=95,r=75,lw=7;
 ctx.beginPath();ctx.arc(cx,cy,r,Math.PI,0);ctx.lineWidth=lw;ctx.strokeStyle='rgba(255,255,255,0.06)';ctx.stroke();
 var angle=Math.PI+Math.PI*(Math.min(temp,120)/120);
 var color=temp>95?'#f87171':temp>80?'#fbbf24':temp>50?getAccentHex():'#22d3ee';
 ctx.beginPath();ctx.arc(cx,cy,r,Math.PI,angle);ctx.lineWidth=lw;ctx.strokeStyle=color;ctx.lineCap='round';ctx.stroke();ctx.lineCap='butt';
 var nx=cx+r*Math.cos(angle),ny=cy+r*Math.sin(angle);
 ctx.beginPath();ctx.arc(nx,ny,4,0,Math.PI*2);ctx.fillStyle='#fff';ctx.fill();
 E('thermLabel').textContent=temp>95?'DANGER':temp>80?'Warning':temp>50?'Normal':'Cool';E('thermLabel').style.color=color;
}

/* ── Network Save ── */
function saveNetwork(){var h=E('netHostname').value,ss=E('netSsidInput').value,wp=E('netWifiPass').value;
 var b={};if(h)b.hostname=h;if(ss)b.ssid=ss;if(wp)b.wifiPass=wp;
 post('/api/system',b,function(r){if(r.ok)showToast('Network settings saved');else showToast('Save failed','error')})}
function saveNetworkRestart(){saveNetwork();setTimeout(function(){post('/api/system/restart',{})},1000);showToast('Saving and restarting...')}
function enterSetup(){showModal('Transfer Ownership?','This will clear owner access, WiFi credentials, pool secrets, and reboot into secure setup mode (DCENTaxe hotspot).',function(){post('/api/system/setup',{},function(){showToast('Entering secure setup mode...')})},'Transfer Ownership')}

/* ── Pool Save ── */
function updateSplitPctLabel(){var p=Math.max(1,Math.min(99,parseInt(E('splitPct').value)||20));if(E('splitPct').value!=p)E('splitPct').value=p;S('splitPrimaryPct',(100-p)+'%')}
function endpointPort(u,d){var s=(u||'').trim().replace(/^[a-z0-9+.-]+:\/\//i,'').split('/')[0].split('@').pop()||'';if(s.charAt(0)==='['){var e=s.indexOf(']'),r=e>=0?s.slice(e+1):'';if(r.charAt(0)===':'){var p=parseInt(r.slice(1));return p>0&&p<65536?p:d}return d}var i=s.lastIndexOf(':');if(i>0){var p=parseInt(s.slice(i+1));return p>0&&p<65536?p:d}return d}
function ownTemplateChanged(){if(!E('ownTplEnable'))return;var en=E('ownTplEnable').checked,proxy=(E('ownTplProxyUrl').value||'').trim();if(en&&proxy){setIfIdle('pProtocol','v2');setIfIdle('pUrl',proxy);E('pPort').value=endpointPort(proxy,3336)}}
function savePool(){if(E('ownTplEnable')&&E('ownTplEnable').checked){var ownProxy=(E('ownTplProxyUrl').value||'').trim();if(!ownProxy){showToast('SV2 mining proxy URL required','warning');return}E('pProtocol').value='v2';E('pUrl').value=ownProxy;E('pPort').value=endpointPort(ownProxy,3336)}
 if(!E('pUrl').value.trim()){showToast('Pool URL required','warning');return}
 var proto=E('pProtocol').value||'v1',defPort=proto==='v2'?3336:3333;
 var b={stratumURL:E('pUrl').value,stratumPort:parseInt(E('pPort').value)||defPort,stratumUser:E('pUser').value,stratumProtocol:proto};
 var p=E('pPass').value;if(p)b.stratumPassword=p;
 if(E('ownTplEnable')){b.sv2OwnTemplatesEnabled=!!E('ownTplEnable').checked;b.sv2TemplateProxyURL=(E('ownTplProxyUrl').value||'').trim();b.sv2TemplateProviderURL=(E('ownTplProviderUrl').value||'').trim();b.sv2JobDeclaratorURL=(E('ownTplJdUrl').value||'').trim()}
 var fbProto=E('fbProtocol').value||'v1',fbUrl=E('fbUrl').value.trim(),fbUser=E('fbUser').value.trim(),fbPort=parseInt(E('fbPort').value)||(fbProto==='v2'?3336:3333),fbPass=E('fbPass').value;
 b.fallbackStratumURL=fbUrl;b.fallbackStratumPort=fbUrl?fbPort:0;b.fallbackStratumUser=fbUser;b.fallbackStratumProtocol=fbProto;
 if(fbPass)b.fallbackStratumPassword=fbPass;
 var spProto=E('splitProtocol').value||'v1',spEn=E('splitEnable').checked,spUrl=E('splitUrl').value.trim(),spUser=E('splitUser').value.trim(),spPort=parseInt(E('splitPort').value)||(spProto==='v2'?3336:3333),spPass=E('splitPass').value,spPct=Math.max(1,Math.min(99,parseInt(E('splitPct').value)||20));
 if(spEn&&!spUrl){showToast('Split pool URL required','warning');return}
 b.splitPoolEnabled=spEn;b.splitPoolURL=spUrl;b.splitPoolPort=spEn?spPort:0;b.splitPoolUser=spUser;b.splitPoolProtocol=spProto;b.splitPoolPct=spPct;
 if(spPass)b.splitPoolPassword=spPass;
 // Pool URL/port/worker change forces a reboot (the Stratum client stack is
 // re-initialised at boot). Confirm first so the miner doesn't drop shares
 // unexpectedly.
 showModal('Save pool &amp; reboot?','Changing the pool stops mining for ~15 s while the device reconnects. Saved shares are not lost.',function(){post('/api/system',b,function(){showToast('Pool saved \u2014 rebooting...')})},'Save &amp; Reboot')}

/* ── Settings ── */
function setHw(){var b={},f=parseFloat(E('setFreq').value),v=parseInt(E('setVolt').value);if(f)b.frequency=f;if(v)b.coreVoltage=v;post('/api/system',b,function(){showToast('Settings applied')})}
function saveMqtt(){var b={mqttEnabled:E('mqttEnable').checked,mqttBrokerHost:E('mqttHost').value.trim(),mqttTls:E('mqttTls').checked};
 var port=parseInt(E('mqttPort').value);if(port>0)b.mqttBrokerPort=port;
 var iv=parseInt(E('mqttInterval').value);if(iv>0)b.mqttPublishInterval=iv;
 b.mqttUsername=E('mqttUser').value;
 var p=E('mqttPass').value;if(p)b.mqttPassword=p;
 if(b.mqttEnabled&&!b.mqttBrokerHost){showToast('Broker host required','warning');return}
 post('/api/system',b,function(r){if(r&&r.ok)showToast('MQTT saved — restart to apply enable/disable');else showToast('Save failed','error')})}
function setFan(){post('/api/system',{fanMode:'manual',autofanspeed:0,fanSpeed:parseInt(E('fanSlider').value)});_fd=0;showToast('Fan updated')}
// UXFLOW-SAFETY-1: derive the canonical PWM ZONE label (Home cap ≤30 / Loud override ≤60
// / Thermal override >60) for the manual fan slider, and mirror it into aria-valuetext for
// a11y parity (component-contract §7). Label/vocabulary only — axe keeps its own floor (20);
// this does NOT clamp the slider to OS's 10-30 home range and does NOT change fan behavior.
function fanZoneLabel(v){v=parseInt(v)||0;var z=v<=30?'Home cap':v<=60?'Loud override':'Thermal override';var el=E('fanZone');if(el)el.textContent=z;var sl=E('fanSlider');if(sl)sl.setAttribute('aria-valuetext',v+'% '+z);return z}
function fanModeChanged(){var m=E('fanMode').value;E('fanManualFields').style.display=m==='manual'?'':'none';E('fanAutoFields').style.display=m==='auto'?'':'none'}
function setFanAuto(){var t=parseInt(E('fanTargetTemp').value);if(t<40||t>80){showToast('Target: 40-80 C','warning');return}post('/api/system',{fanMode:'auto',autofanspeed:1,fanTargetTemp:t});showToast('Fan auto: '+t+'C')}
/* Autotuner mode descriptions — loaded from /api/autotuner/modes at boot so
   labels stay in lockstep with the firmware side. Fallback to sane strings
   so the page isn't blank if the fetch hasn't completed yet. */
var AT_DESCS={max_hashrate:'Max frequency, highest hash power.',best_efficiency:'Lowest J/TH sweet spot.',target_watts:'Hit your power budget.',target_temp:'Keep chip at target temp.'};
function loadAutotunerModes(){fetch('/api/autotuner/modes').then(function(r){return r.json()}).then(function(d){if(!d||!d.modes)return;var map={};d.modes.forEach(function(m){if(m&&m.id)map[m.id]=m.description||''});AT_DESCS=map;updateAtDesc()}).catch(function(){})}
function updateAtDesc(){S('atModeDesc',AT_DESCS[E('atMode').value]||'')}
updateAtDesc();
function setAt(){_ad=0;post('/api/mining/autotune',{enabled:E('atEnable').checked,mode:E('atMode').value,target:parseFloat(E('atTarget').value)||0});showToast('Autotuner updated')}
function atChanged(){_ad=1;var en=E('atEnable').checked;E('presetSel').disabled=en;E('setFreq').disabled=en;E('setVolt').disabled=en}
/* Autotuner Evidence tiles (CAP-OS2AXE-1) — read-only render of the persisted
   last-known-good evidence. Honesty contract (data-model-fields §7.1/§7.2):
   silicon_grade is DERIVED (measured error-rate/nonce), honest "unknown" when
   absent; last_good_* are PERSISTED last-session evidence, never claimed as the
   live setpoint. Uniquely named so it can't shadow any dashboard/*.js window.*
   global (wiring contract). All wire fields are camelCase serde from
   AutotunerView (api_system_info.rs); no fetch, no new field. */
function renderAutotunerEvidence(at){
 var DASH='—';
 /* silicon grade: derived letter; "unknown"/empty -> honest em-dash */
 var g=at.siliconGrade;
 S('atEvGrade',(g&&g!=='unknown'&&g!=='?')?String(g).toUpperCase():DASH);
 /* best efficiency (measured): "N.N J/TH, lower is better" */
 var eff=at.bestEfficiency;
 if(typeof eff==='number'&&eff>0&&isFinite(eff)){
  S('atEvEff',eff.toFixed(1)+' J/TH');
  var eb=E('atEvEffBadge');if(eb){eb.style.display='inline-block';
   var cls=eff<=25?'excellent':eff<=32?'good':eff<=40?'average':'poor';
   eb.className='eff-badge '+cls;eb.textContent=cls}
 }else{S('atEvEff',DASH);var eb2=E('atEvEffBadge');if(eb2)eb2.style.display='none'}
 /* last-known-good operating point (persisted, NOT the live setpoint) */
 var lf=at.lastGoodFrequency;S('atEvLgFreq',(typeof lf==='number'&&lf>0)?lf.toFixed(0)+' MHz':DASH);
 var lv=at.lastGoodVoltageMv;S('atEvLgVolt',(typeof lv==='number'&&lv>0)?lv+' mV':DASH);
 var lj=at.lastGoodJth;S('atEvLgJth',(typeof lj==='number'&&lj>0&&isFinite(lj))?lj.toFixed(1)+' J/TH':DASH);
 var le=at.lastGoodErrorRate;S('atEvLgErr',(typeof le==='number'&&le>=0&&isFinite(le))?(le*100).toFixed(2)+'%':DASH);
}
/* Network card (CAP-OS2AXE-3) — halving countdown bar. Port of the OS
   HalvingTimelineBar math: epoch = floor(h/210000), blocks-left to the next
   halving, ~days ETA at 600s/block. Reads the ALREADY-CLIENT-SIDE block height
   — no fetch, no new wire field, no register_static handler. Uniquely named to
   avoid shadowing any dashboard/*.js window.* global. */
var _HALVING_INTERVAL=210000,_BLOCK_SECONDS=600;
function _fmtHalvingDays(blocks){var days=(blocks*_BLOCK_SECONDS)/86400;if(days<1){var hrs=days*24;if(hrs<1)return'~'+Math.max(1,Math.round(hrs*60))+' min';return'~'+hrs.toFixed(1)+' h'}if(days<10)return'~'+days.toFixed(1)+' days';return'~'+Math.round(days)+' days'}
function renderHalvingCountdown(bh){
 if(!bh||bh<=0||!isFinite(bh)){S('halvingBlocksLeft','—');S('halvingEta','—');S('halvingEra','Waiting for block height');var hb=E('halvingBar');if(hb)hb.style.width='0';return}
 var epoch=Math.floor(bh/_HALVING_INTERVAL);
 var epochStart=epoch*_HALVING_INTERVAL;
 var nextHalving=epochStart+_HALVING_INTERVAL;
 var blocksIn=bh-epochStart;
 var blocksLeft=nextHalving-bh;
 var pct=Math.max(0,Math.min(100,(blocksIn/_HALVING_INTERVAL)*100));
 S('halvingBlocksLeft',blocksLeft.toLocaleString());
 S('halvingEta',_fmtHalvingDays(blocksLeft));
 S('halvingEra','Era '+epoch+' · now → Era '+(epoch+1)+' · next');
 var hb2=E('halvingBar');if(hb2)hb2.style.width=pct+'%';
}
/* Mempool fee radial (CAP-OS2AXE-3) — pure-SVG 180° gauge ported from the OS
   MempoolFeeRadial: green/yellow/red bands 0-20 / 20-80 / 80+ sat/vB, needle at
   `fastest`, FAST/30M/1H summary. Built client-side from a cross-origin browser
   fetch to mempool.space (same cross-origin pattern block-tile.js uses → NO
   firmware handler). Fails SILENTLY to the em-dash empty state when blocked/
   offline. Slow 60s timer; fail-silent. */
var _MEMPOOL_MAX=120,_mempoolTimer=null;
function _mpPolar(cx,cy,angleDeg,r){var a=(angleDeg*Math.PI)/180;return{x:cx-r*Math.cos(a),y:cy-r*Math.sin(a)}}
function _mpFeeToAngle(fee){var n=Math.max(0,Math.min(_MEMPOOL_MAX,fee))/_MEMPOOL_MAX;return 180-n*180}
function _mpArc(cx,cy,startFee,endFee,r){var a1=_mpFeeToAngle(startFee),a2=_mpFeeToAngle(endFee);var p1=_mpPolar(cx,cy,a1,r),p2=_mpPolar(cx,cy,a2,r);var la=Math.abs(a1-a2)>180?1:0;return'M '+p1.x+' '+p1.y+' A '+r+' '+r+' 0 '+la+' 1 '+p2.x+' '+p2.y}
function renderMempoolRadial(fees){
 var DASH='—';var el=E('mempoolRadial');if(!el)return;
 var has=fees!=null&&typeof fees.fastest==='number'&&fees.fastest!==null;
 var fastest=has?fees.fastest:null,half=fees&&typeof fees.halfHour==='number'?fees.halfHour:null,hour=fees&&typeof fees.hour==='number'?fees.hour:null;
 var W=220,H=130,CX=W/2,CY=H-16,R_OUT=92,R_IN=68,STROKE=R_OUT-R_IN,R_MID=(R_OUT+R_IN)/2;
 var op=has?0.95:0.45;
 var s='<svg viewBox="0 0 '+W+' '+H+'" preserveAspectRatio="xMidYMax meet" style="width:100%;max-width:240px" role="img" aria-label="Mempool fee gauge">';
 s+='<path d="'+_mpArc(CX,CY,0,_MEMPOOL_MAX,R_MID)+'" fill="none" stroke="var(--border)" stroke-width="'+STROKE+'"/>';
 s+='<path d="'+_mpArc(CX,CY,0,20,R_MID)+'" fill="none" stroke="var(--green)" stroke-width="'+STROKE+'" opacity="'+op+'"/>';
 s+='<path d="'+_mpArc(CX,CY,20,80,R_MID)+'" fill="none" stroke="var(--yellow)" stroke-width="'+STROKE+'" opacity="'+op+'"/>';
 s+='<path d="'+_mpArc(CX,CY,80,_MEMPOOL_MAX,R_MID)+'" fill="none" stroke="var(--red)" stroke-width="'+STROKE+'" opacity="'+op+'"/>';
 if(has){
  var na=_mpFeeToAngle(fastest),tip=_mpPolar(CX,CY,na,R_OUT-4),b1=_mpPolar(CX,CY,na+90,5),b2=_mpPolar(CX,CY,na-90,5);
  s+='<polygon points="'+tip.x+','+tip.y+' '+b1.x+','+b1.y+' '+b2.x+','+b2.y+'" fill="var(--text)" stroke="#000" stroke-width="0.8"/>';
  s+='<circle cx="'+CX+'" cy="'+CY+'" r="5" fill="var(--accent)" stroke="#000" stroke-width="1"/>';
 }
 s+='<text x="'+CX+'" y="'+(CY-10)+'" text-anchor="middle" fill="var(--text)" font-size="22" font-weight="700">'+(has?Math.round(fastest):DASH)+'</text>';
 s+='<text x="'+CX+'" y="'+(CY+6)+'" text-anchor="middle" fill="var(--dim)" font-size="9">sat/vB</text>';
 s+='</svg>';
 s+='<div style="display:flex;justify-content:center;gap:14px;font-family:var(--mono);font-size:11px;margin-top:4px">';
 s+='<span style="color:var(--dim)">FAST <span style="color:var(--text)">'+(fastest!==null?Math.round(fastest):DASH)+'</span></span>';
 s+='<span style="color:var(--dim)">30M <span style="color:var(--text)">'+(half!==null?Math.round(half):DASH)+'</span></span>';
 s+='<span style="color:var(--dim)">1H <span style="color:var(--text)">'+(hour!==null?Math.round(hour):DASH)+'</span></span>';
 s+='</div>';
 el.innerHTML=s;
}
function fetchMempoolFees(){
 fetch('https://mempool.space/api/v1/fees/recommended').then(function(r){if(!r.ok)throw 0;return r.json()}).then(function(j){
  renderMempoolRadial({fastest:j.fastestFee,halfHour:j.halfHourFee,hour:j.hourFee});
 }).catch(function(){/* fail-silent: leave/show the em-dash empty state */if(!E('mempoolRadial')||!E('mempoolRadial').innerHTML)renderMempoolRadial(null)})
}
function startMempoolPoll(){renderMempoolRadial(null);fetchMempoolFees();if(_mempoolTimer)clearInterval(_mempoolTimer);_mempoolTimer=setInterval(fetchMempoolFees,60000)}
var _schedule=[];
function pad2(n){return String(n).padStart(2,'0')}
function scheduleTime(e){return pad2(e.hour||0)+':'+pad2(e.minute||0)}
function scheduleDefaultEntry(){var f=parseFloat(E('setFreq').value)||(_lastInfo&&_lastInfo.frequency)||525;var v=parseInt(E('setVolt').value)||(_lastInfo&&_lastInfo.coreVoltage)||1150;return {enabled:true,hour:22,minute:0,label:'Night boost',frequency:f,voltage_mv:v,autotune_enabled:false,autotune_mode:'best_efficiency',autotune_target:0}}
function renderScheduleStatus(s){if(!s)return;var m=s.currentMinuteOfDay||0;var a=s.active||null;var txt=(s.enabled?'ON':'OFF')+' · '+pad2(Math.floor(m/60))+':'+pad2(m%60)+' · '+(s.timeSource||'clock');if(a)txt+=' · active: '+(a.label||scheduleTime(a));if(s.nextChangeMinutes!=null)txt+=' · next '+s.nextChangeMinutes+'m';S('schedClock',txt)}
function renderSchedule(){var host=E('schedRows');if(!host)return;if(!_schedule.length){host.innerHTML='<div style="font-size:11px;color:var(--dim);margin:8px 0">No slots yet. Add a daytime low-power slot and a night boost slot.</div>';return}var html='';for(var i=0;i<_schedule.length;i++){var e=_schedule[i]||{};var at=e.autotune_enabled===true;var mode=at?(e.autotune_mode||'best_efficiency'):'fixed';html+='<div style="border:1px solid var(--line);border-radius:8px;padding:8px;margin:8px 0;background:rgba(255,255,255,0.02)">'
+'<div class="field-row"><div class="field"><label><input type="checkbox" id="sEn'+i+'" '+(e.enabled!==false?'checked':'')+'> Active</label></div><div class="field"><label>Start</label><input id="sTime'+i+'" type="time" value="'+scheduleTime(e)+'"></div><div class="field"><label>Label</label><input id="sLab'+i+'" value="'+(e.label||'').replace(/"/g,'&quot;')+'"></div></div>'
+'<div class="field-row"><div class="field"><label>Freq MHz</label><input id="sFreq'+i+'" type="number" value="'+(e.frequency||'')+'"></div><div class="field"><label>Voltage mV</label><input id="sVolt'+i+'" type="number" value="'+(e.voltage_mv||e.voltageMv||'')+'"></div><div class="field"><label>Policy</label><select id="sPolicy'+i+'"><option value="fixed" '+(!at?'selected':'')+'>Fixed profile</option><option value="best_efficiency" '+(mode==='best_efficiency'?'selected':'')+'>Autotune efficiency</option><option value="target_watts" '+(mode==='target_watts'?'selected':'')+'>Autotune watts</option><option value="target_temp" '+(mode==='target_temp'?'selected':'')+'>Autotune temp</option><option value="max_hashrate" '+(mode==='max_hashrate'?'selected':'')+'>Autotune max</option></select></div></div>'
+'<div class="field-row"><div class="field"><label>Autotune Target</label><input id="sTarget'+i+'" type="number" value="'+(e.autotune_target||e.autotuneTarget||0)+'"></div><div class="field"><label>&nbsp;</label><button class="btn btn-ghost btn-sm" onclick="removeScheduleRow('+i+')">Remove</button></div></div></div>'}host.innerHTML=html}
function addScheduleRow(entry){_schedule.push(entry||scheduleDefaultEntry());renderSchedule()}
function removeScheduleRow(i){_schedule.splice(i,1);renderSchedule()}
function collectSchedule(){var out=[];for(var i=0;i<_schedule.length;i++){var t=(E('sTime'+i).value||'00:00').split(':'),pol=E('sPolicy'+i).value;out.push({enabled:E('sEn'+i).checked,hour:parseInt(t[0])||0,minute:parseInt(t[1])||0,label:E('sLab'+i).value||'',frequency:parseFloat(E('sFreq'+i).value)||0,voltage_mv:parseInt(E('sVolt'+i).value)||0,autotune_enabled:pol==='fixed'?false:true,autotune_mode:pol==='fixed'?null:pol,autotune_target:parseFloat(E('sTarget'+i).value)||0})}return out}
function loadSchedule(){fetch('/api/schedule',{headers:authHeaders({})}).then(handleReadAuthFailure).then(function(r){return r.json()}).then(function(d){var entries=Array.isArray(d)?d:(d.entries||[]);_schedule=entries;E('schedEnable').checked=Array.isArray(d)?true:!!d.enabled;var tz=Array.isArray(d)?(-new Date().getTimezoneOffset()):(d.timezoneOffsetMinutes!=null?d.timezoneOffsetMinutes:-new Date().getTimezoneOffset());setIfIdle('schedTz',tz);renderScheduleStatus(Array.isArray(d)?null:d);renderSchedule()}).catch(function(){})}
function saveSchedule(){var body={enabled:E('schedEnable').checked,timezoneOffsetMinutes:parseInt(E('schedTz').value)||0,entries:collectSchedule()};post('/api/schedule',body,function(r){if(r&&r.ok){showToast('Schedule saved');loadSchedule();return}if(r){r.clone().json().then(function(e){showToast(e.error||'Schedule save failed','error')}).catch(function(){showToast('Schedule save failed','error')})}},{reloadCfg:false,busyLabel:'Saving schedule...'})}
function setDisplay(){_dd=0;post('/api/system',{flipscreen:E('flipScreen').checked?1:0},function(){showToast('Display updated')})}

/* ── System ── */
function ocToggle(){_oc=1;if(E('ocEnable').checked)E('ocWarn').style.display='block';else{E('ocWarn').style.display='none';post('/api/system',{overclockEnabled:false},function(){showToast('Overclock disabled')})}}
function ocAccept(){E('ocWarn').style.display='none';_oc=0;post('/api/system',{overclockEnabled:true},function(r){if(r.ok){showToast('Overclock ON! Rebooting...');post('/api/system/restart',{})}else{showToast('Failed');E('ocEnable').checked=false}})}
function ocCancel(){_oc=0;E('ocEnable').checked=false;E('ocWarn').style.display='none'}
function copyDevInfo(){var info=E('sysInfo').innerText;if(navigator.clipboard)navigator.clipboard.writeText(info).then(function(){showToast('Copied!')})}
function exportConfig(){fetch('/api/system',{headers:authHeaders({})}).then(handleReadAuthFailure).then(function(r){return r.json()}).then(function(c){var b=new Blob([JSON.stringify(c,null,2)],{type:'application/json'});var a=document.createElement('a');a.href=URL.createObjectURL(b);a.download='dcentaxe-config.json';a.click()}).catch(function(e){if(e.message!=='auth-required')showToast('Export failed','warning')})}
function doReboot(){showModal('Reboot?','Mining stops for ~30 seconds.',function(){post('/api/system/restart',{});showToast('Rebooting...')})}

/* ── Topbar mining pause/resume (loop 2026-04-29 inspiration combine) ─────
   Talks to /api/mining/{stop,start} which require an auth header. Re-uses
   the existing post() helper which already attaches authHeaders(). */
var _miningEnabledLast=null;
function toggleMiningPause(){
 var enabled=_miningEnabledLast!==false;
 var url=enabled?'/api/mining/stop':'/api/mining/start';
 var verb=enabled?'Pause':'Resume';
 fetch(url,{method:'POST',headers:authHeaders({'Content-Type':'application/json'})}).then(handleReadAuthFailure).then(function(r){
  if(r.ok){showToast((enabled?'Paused':'Resuming')+' mining');poll()}else{showToast(verb+' failed','error')}
 }).catch(function(e){if(e.message!=='auth-required')showToast(verb+' failed','error')})
}

/* ── Notification panel (placeholder; surfaces recent reject / alert state) ── */
function toggleAlertPanel(){var p=E('tbAlertPanel');if(!p)return;p.hidden=!p.hidden;if(!p.hidden)refreshAlertPanel()}
function refreshAlertPanel(){
 var body=E('tbAlertBody');if(!body)return;
 var rows=[];
 var d=window._lastInfo||{};
 var safeBn=E('safeModeBanner');if(safeBn&&safeBn.style.display!=='none')rows.push({when:'now',msg:'Safe mode active — clear watchdog before mining.'});
 var coreBn=E('coredumpBanner');if(coreBn&&coreBn.style.display!=='none')rows.push({when:'now',msg:'Panic coredump stored — retrieve before next crash.'});
 var rej=+d.sharesRejected||0;if(rej>0)rows.push({when:'session',msg:rej+' rejected share'+(rej>1?'s':'')+'. Open Logs for details.'});
 var stale=+d.staleNonces||0;if(stale>5)rows.push({when:'session',msg:stale+' stale nonces — pool latency or work cycle.'});
 var pc=d.poolConnected;if(pc===false||pc===0)rows.push({when:'now',msg:'Pool connection lost. Failback may be active.'});
 var hr=+d.hashRate||0;var miningOn=d.dcentaxe?d.dcentaxe.miningEnabled!==false:hr>0;if(!miningOn)rows.push({when:'now',msg:'Mining is paused. Hit RESUME to start.'});
 if(rows.length===0){body.innerHTML='<div class="tb-alert-empty">All clear.</div>';E('tbBellDot').hidden=true;return}
 body.innerHTML=rows.map(function(r){return '<div class="tb-alert-row"><span>'+r.msg+'</span><span class="tb-alert-when">'+r.when+'</span></div>'}).join('');
 E('tbBellDot').hidden=false;
}

/* ── Safe mode + coredump recovery ── */
function clearSafeMode(){showModal('Clear safe mode?','The watchdog counter is zeroed and the device reboots into normal operation. Make sure you have fixed whatever wedge caused the repeated resets.',function(){fetch('/api/system/clear-safe-mode',{method:'POST',headers:authHeaders({'X-Requested-With':'dcentaxe-dashboard'})}).then(handleReadAuthFailure).then(function(r){if(r.ok){showToast('Rebooting out of safe mode...')}else{showToast('Clear failed','error')}}).catch(function(e){if(e.message!=='auth-required')showToast('Clear failed','error')})},'Clear &amp; Reboot')}
function downloadCoredump(){fetch('/api/system/coredump?download=1',{headers:authHeaders({})}).then(handleReadAuthFailure).then(function(r){if(!r.ok)throw new Error('no-coredump');return r.blob()}).then(function(b){var a=document.createElement('a');a.href=URL.createObjectURL(b);a.download='dcentaxe-coredump.elf';a.click();showToast('Coredump downloaded')}).catch(function(e){if(e.message!=='auth-required')showToast('Coredump download failed','error')})}
function deleteCoredump(){showModal('Delete coredump?','The stored panic trace will be erased. Download it first if you want to keep it.',function(){fetch('/api/system/coredump',{method:'DELETE',headers:authHeaders({'X-Requested-With':'dcentaxe-dashboard'})}).then(handleReadAuthFailure).then(function(r){if(r.ok){showToast('Coredump erased');E('coredumpBanner').style.display='none'}else{showToast('Delete failed','error')}}).catch(function(e){if(e.message!=='auth-required')showToast('Delete failed','error')})},'Delete')}

/* ── Factory self-test ── */
// Self-test step list — kept in lockstep with self_test::STEP_NAMES.
var SELF_TEST_STEPS=['input_voltage','core_voltage','asic_chain','temp_sensor','fan_tach','mining_liveness'];
function setSelfTestRunUi(running){
 var runBtn=E('selfTestRunBtn');var cancelBtn=E('selfTestCancelBtn');
 if(runBtn)runBtn.disabled=!!running;
 if(cancelBtn)cancelBtn.style.display=running?'inline-flex':'none';
}
function runSelfTest(){
 showModal('Start self-test?','Probes I2C, power rails, ASIC comms, temps, fan tach, and waits up to 60 s for a share. Mining stays on throughout.',function(){
  fetch('/api/system/self-test/run',{method:'POST',headers:authHeaders({'X-Requested-With':'dcentaxe-dashboard'})}).then(handleReadAuthFailure).then(function(r){
   if(r.status===202||r.ok){showToast('Self-test running\u2026');setSelfTestRunUi(true);pollSelfTest()}
   else if(r.status===409){showToast('Already running','warning');setSelfTestRunUi(true);pollSelfTest()}
   else{showToast('Start failed','error')}
  }).catch(function(e){if(e.message!=='auth-required')showToast('Start failed','error')})
 },'Start')
}
function cancelSelfTest(){
 fetch('/api/system/self-test/cancel',{method:'POST',headers:authHeaders({'X-Requested-With':'dcentaxe-dashboard'})}).then(handleReadAuthFailure).then(function(r){
  if(r.status===202||r.ok)showToast('Cancelling\u2026');
  else if(r.status===409)showToast('No self-test is running','warning');
  else showToast('Cancel failed','error');
 }).catch(function(e){if(e.message!=='auth-required')showToast('Cancel failed','error')})
}
var _selfTestTimer=null;
function pollSelfTest(){if(_selfTestTimer)clearInterval(_selfTestTimer);_selfTestTimer=setInterval(function(){fetch('/api/system/self-test/status',{headers:authHeaders({})}).then(handleReadAuthFailure).then(function(r){return r.json()}).then(renderSelfTest).catch(function(e){if(e.message==='auth-required'){clearInterval(_selfTestTimer);_selfTestTimer=null}})},2000);renderSelfTestLoading()}
function renderSelfTestLoading(){var el=E('selfTestBody');if(el)el.innerHTML='<div style="color:var(--dim);font-size:11px">Starting\u2026</div>';S('selfTestProgress','')}
function renderSelfTest(s){
 var el=E('selfTestBody');if(!el)return;
 // Progress: show "Step X of 6 — name" while running.
 var prog=E('selfTestProgress');
 if(s&&s.running){
  var next=(s.results&&s.results.length)||0;
  var nextName=SELF_TEST_STEPS[Math.min(next,SELF_TEST_STEPS.length-1)]||'';
  if(prog)prog.textContent='Step '+(next+1)+' of '+SELF_TEST_STEPS.length+' \u2014 '+nextName;
 } else if(prog){prog.textContent=''}
 if(!s||!s.results||s.results.length===0){el.innerHTML='<div style="color:var(--dim);font-size:11px">'+(s&&s.running?'Running\u2026':'No results yet.')+'</div>';if(s&&!s.running)setSelfTestRunUi(false);return}
 var rows=s.results.map(function(r){
  var c=r.status==='pass'?'var(--green)':r.status==='skip'?'var(--dim)':r.status==='aborted'?'var(--yellow)':'var(--red)';
  var mark=r.status==='pass'?'\u2713':r.status==='skip'?'\u2013':r.status==='aborted'?'\u25CB':'\u2717';
  return '<div class="kv-flex-row" style="font-size:11px"><span><b style="color:'+c+'">'+mark+'</b> '+r.name+'</span><span class="text-dim">'+(r.detail||'')+'</span></div>';
 }).join('');
 var hasAbort=s.results.some(function(r){return r.status==='aborted'});
 var verdict=s.completed?(s.passed?'PASSED':(hasAbort?'CANCELLED':'FAILED')):'Running\u2026';
 var summaryColor=s.completed?(s.passed?'var(--green)':(hasAbort?'var(--yellow)':'var(--red)')):'var(--dim)';
 var summary='<div style="margin-top:8px;color:'+summaryColor+';font-size:11px">'+verdict+'</div>';
 el.innerHTML=rows+summary;
 if(s.completed&&_selfTestTimer){clearInterval(_selfTestTimer);_selfTestTimer=null;setSelfTestRunUi(false)}
}

/* ── OTA ── */
(function(){var z=E('otaZone');if(!z)return;z.addEventListener('dragover',function(e){e.preventDefault();z.classList.add('dragover')});z.addEventListener('dragleave',function(){z.classList.remove('dragover')});z.addEventListener('drop',function(e){e.preventDefault();z.classList.remove('dragover');if(e.dataTransfer.files.length){E('otaFile').files=e.dataTransfer.files;otaFileSelected()}})})();
function otaFileSelected(){var f=E('otaFile').files[0];if(f){E('otaFileInfo').style.display='block';E('otaFileInfo').textContent='Selected: '+f.name+' ('+Math.round(f.size/1024)+'KB)'}}
function fmtKb(n){n=Number(n)||0;return n?Math.round(n/1024)+'KB':'--'}
function manifestUpdatePayload(m){return m&&m.payloads?m.payloads.find(function(p){return p.name==='update'}):null}
function otaManifestSelected(){var f=E('otaManifestFile').files[0];if(!f){_otaManifest=null;E('otaManifestInfo').style.display='none';return}var rd=new FileReader();rd.onload=function(){try{_otaManifest=JSON.parse(rd.result);var p=manifestUpdatePayload(_otaManifest),o=_otaManifest.ota||{},parts=['manifest: '+f.name];if(p&&p.size)parts.push('update_size: '+fmtKb(p.size));if(o.appPartition||o.slotSize)parts.push('slot_gate: '+(o.appPartition||'ota')+' '+fmtKb(o.slotSize));if(o.updateFitsSlot===true)parts.push('slot_fit: valid');else if(o.updateFitsSlot===false)parts.push('slot_fit: invalid');if(p&&p.sha256)parts.push('sha_status: present');else parts.push('sha_status: missing');parts.push('target preflight required');E('otaManifestInfo').style.display='block';E('otaManifestInfo').textContent=parts.join(' | ')}catch(e){_otaManifest=null;showToast('Invalid manifest JSON','error')}};rd.readAsText(f)}
function sha256FileHex(file){if(!(window.crypto&&crypto.subtle&&file&&file.arrayBuffer))return Promise.resolve('');return file.arrayBuffer().then(function(buf){return crypto.subtle.digest('SHA-256',buf)}).then(function(hash){return Array.from(new Uint8Array(hash)).map(function(b){return b.toString(16).padStart(2,'0')}).join('')})}
function doOta(){var f=E('otaFile').files[0];if(!f){showToast('Select a .bin file first','warning');return}
 showModal('Upload Update','Upload firmware for device validation? The device may reboot only after the update is accepted.',function(){
    if(!_lastInfo||!_lastInfo.boardTarget){showToast('Board target unavailable. Refresh and try again.','error');return}
    var runtimeModel=_lastInfo&&_lastInfo.dcentaxe?_lastInfo.dcentaxe.runtimeDeviceModel:'';
    if(!runtimeModel){showToast('Device model unavailable. Refresh and try again.','error');return}
    var otaCfg=_lastInfo&&_lastInfo.dcentaxe&&_lastInfo.dcentaxe.ota?_lastInfo.dcentaxe.ota:{};
    var updatePayload=manifestUpdatePayload(_otaManifest),manifestOta=_otaManifest&&_otaManifest.ota?_otaManifest.ota:{};
    var manifestSig=_otaManifest?(_otaManifest.otaSignature||_otaManifest.signature||''):'';
    var manifestKeyId=_otaManifest?(_otaManifest.otaKeyId||_otaManifest.keyId||''):'';
    if(_otaManifest&&_otaManifest.boardTarget&&_otaManifest.boardTarget!==_lastInfo.boardTarget){showToast('Manifest board target does not match this device','error');return}
    if(_otaManifest&&_otaManifest.deviceModel&&String(_otaManifest.deviceModel).toLowerCase()!==String(runtimeModel).toLowerCase()){showToast('Manifest device model does not match this device','error');return}
    if(_otaManifest&&manifestOta.updateFitsSlot===false){showToast('Manifest says update does not fit OTA slot','error');return}
    if(updatePayload&&manifestOta.slotSize&&Number(updatePayload.size)>Number(manifestOta.slotSize)){showToast('Manifest update size exceeds OTA slot','error');return}
    if(otaCfg.signatureRequired){
      if(!_otaManifest||!updatePayload){showToast('Signed OTA requires a matching manifest JSON','error');return}
      if(!manifestSig||!manifestKeyId){showToast('Manifest is missing OTA signature or key ID','error');return}
    }
    if(updatePayload&&updatePayload.size&&Number(updatePayload.size)!==f.size){showToast('Manifest size does not match selected firmware','error');return}
    var preflight=Promise.resolve();
    if(updatePayload&&updatePayload.sha256&&window.crypto&&crypto.subtle){preflight=sha256FileHex(f).then(function(h){if(h&&h!==String(updatePayload.sha256).toLowerCase())throw new Error('Manifest SHA-256 does not match selected firmware')})}
    preflight.then(function(){return ensureWriteAuth()}).then(function(){E('otaProg').style.display='block';S('otaMsg','Uploading...');var x=new XMLHttpRequest();x.open('POST','/api/system/OTA');x.setRequestHeader('X-Requested-With','XMLHttpRequest');if(_authToken)x.setRequestHeader('Authorization','Bearer '+_authToken);x.setRequestHeader('X-DCENT-Board-Target',_lastInfo.boardTarget);x.setRequestHeader('X-DCENT-Device-Model',String((_otaManifest&&_otaManifest.deviceModel)||runtimeModel).toLowerCase());if(_otaManifest&&updatePayload&&manifestKeyId&&manifestSig){x.setRequestHeader('X-DCENT-Payload-SHA256',updatePayload.sha256||'');x.setRequestHeader('X-DCENT-Payload-Size',String(updatePayload.size||f.size));x.setRequestHeader('X-DCENT-Version',_otaManifest.version||_lastInfo.version||'');x.setRequestHeader('X-DCENT-Key-Id',manifestKeyId);x.setRequestHeader('X-DCENT-Signature',manifestSig)}x.upload.onprogress=function(e){if(e.lengthComputable){E('otaProg').value=e.loaded/e.total*100;S('otaMsg','Uploading... '+(e.loaded/e.total*100|0)+'%')}};x.onload=function(){if(x.status===401){saveAuthToken('');S('otaMsg','Authentication required. Retry upload.');return}try{S('otaMsg',JSON.parse(x.responseText).message||'Upload accepted; boot proof pending.')}catch(e){S('otaMsg','Upload response could not be parsed; refresh device status before assuming reboot or update success.')}};x.onerror=function(){S('otaMsg','Upload failed.')};x.send(f)}).catch(function(e){showToast(e.message,'error')})},'Upload')}

/* ── Achievements ── */
/* Achievement names + rarities come from /api/achievements so the UI can
   never drift from nvs_config::achievement_name(). Fallback strings run the
   first render before the fetch lands; loadAchievements() overrides them. */
var ACH=['First Share!','Centurion (100)','Marathon (24h)','Hot Stuff!','Best Day Ever!','Block Witness','Streak Master','Kilohash (1000)','TERAHASH CLUB!','Cool & Collected','Million Diff!','Night Owl (16h)','Half-K (500)','Warm Day (8h)','Perfect Century!','Hash King!','Early Adopter!','Efficiency Expert','Diamond Hands!','Lucky Strike!','Power Miser!','Block Party!','Creature Legend!','Completionist!'];
var ACH_R=['common','common','uncommon','common','uncommon','uncommon','uncommon','uncommon','rare','rare','rare','rare','rare','rare','epic','epic','epic','epic','epic','epic','epic','legendary','legendary','legendary'];
function loadAchievements(){fetch('/api/achievements').then(function(r){return r.json()}).then(function(d){if(!d||!d.entries)return;var names=new Array(d.total||d.entries.length),rarities=new Array(d.total||d.entries.length);d.entries.forEach(function(e){if(typeof e.bit==='number'){names[e.bit]=e.name||('Achievement '+e.bit);rarities[e.bit]=e.rarity||'common'}});ACH=names;ACH_R=rarities}).catch(function(){})}
function renderAch(bits){var g=E('achGrid');if(!g)return;g.innerHTML='';var count=0;
 for(var i=0;i<24;i++){var u=bits&(1<<i);if(u)count++;
  var d=document.createElement('div');d.className='ach-item'+(u?' unlocked':' locked');
  d.innerHTML='<div style="font-size:14px">'+(u?'\u2B50':'\uD83D\uDD12')+'</div><div style="font-size:9px">'+ACH[i]+'</div><div style="font-size:8px;color:'+(ACH_R[i]==='legendary'?'var(--accent)':ACH_R[i]==='epic'?'#a855f7':ACH_R[i]==='rare'?'var(--cyan)':'var(--dim)')+'">'+ACH_R[i]+'</div>';
  g.appendChild(d)}
 S('achBadge',count+'/24')}

/* ── Presets ── */
var _presets=[];
function loadPresets(){fetch('/api/presets',{headers:{'X-Requested-With':'XMLHttpRequest'}}).then(function(r){return r.json()}).then(function(d){
 _presets=d.presets||[];var sel=E('presetSel');sel.innerHTML='<option value="">-- Select --</option>';
 for(var i=0;i<_presets.length;i++){var p=_presets[i];var o=document.createElement('option');o.value=i;o.textContent=p.name+(p.recommended?' [Recommended]':'')+' ('+p.frequency+'MHz)';sel.appendChild(o)}
 E('presetInfo').textContent='Model: '+(d.model||'--')+(d.recommendedPreset?' • Recommended: '+d.recommendedPreset:'')}).catch(function(){})}
function presetChanged(){var i=parseInt(E('presetSel').value);if(isNaN(i)||!_presets[i])return;var p=_presets[i];E('setFreq').value=p.frequency;E('setVolt').value=p.voltage;E('presetInfo').textContent=p.name+': ~'+p.expectedHashrate+' GH/s, ~'+p.expectedPower+'W'+(p.description?' • '+p.description:'')}
function applyPreset(){var i=parseInt(E('presetSel').value);if(isNaN(i)||!_presets[i]){showToast('Select a preset');return}var p=_presets[i];post('/api/system',{frequency:p.frequency,coreVoltage:p.voltage},function(){showToast(p.name+' applied!')})}

/* ── Chip Visualization ── */
/* Chip rendering is owned by /dashboard/asic-chips.js (window.renderChips,
   mounted at <div data-component="asic-chips">). The former inline
   _legacy_renderChips_DISABLED implementation was deleted (review DASH-5):
   it was never called and duplicated the modular component. Do NOT re-add
   an inline renderChips() — a hoisted declaration would shadow
   window.renderChips and hide the silicon SVG. */

/* ── Main Update ── */
function update(d){
 _lastInfo=d;
 if(_offline){_offline=0;clearInterval(_retryT);_retryDelay=10;E('offlineBanner').style.display='none'}
 /* Remove loading state on first data */
 if(_firstUpdate){_firstUpdate=0;document.querySelectorAll('.loading').forEach(function(e){e.classList.remove('loading');e.classList.add('loaded')})}
 E('dot').className='sb-dot on';
 var hr=d.hashRate_1m||d.hashRate||0;
 var ac=getAccentHex();
 // Sidebar hashrate
 S('sbHR',fHR(hr));
 // Title
 document.title='DCENT_axe '+(d.boardVersion||'')+' - '+fHR(hr);
 // Topbar status
 var _ssid=(d.ssid&&String(d.ssid).trim())||'--';var _ip=(d.ipv4&&String(d.ipv4).trim())||(d.hostname&&String(d.hostname).trim())||'--';
 S('tbPool','Pool: '+(d.poolConnectionInfo||'--'));S('tbIp','IP: '+_ip);S('tbWifi','WiFi: '+_ssid);

 // ── Dashboard page ──
 var hrGH=hr;
 /* The hidden #hr span mirrors the hero (core.js), which is captioned
    "10m Average" — prefer hashRate_10m so the hidden value matches the
    visible render. hrGH (1m) is kept below for the mining-state booleans. */
 var heroHr=d.hashRate_10m||d.hashRate_1m||d.hashRate||0;
 var hrStr=heroHr>=1000?(heroHr/1000).toFixed(2):heroHr>=1?heroHr.toFixed(1):(heroHr*1000).toFixed(0);
 flashEl('hr',hrStr);
 if(heroHr>=1000){S('hr',hrStr);S('hrUnit','TH/s')}
 else if(heroHr>=1){S('hr',hrStr);S('hrUnit','GH/s')}
 else{S('hr',hrStr);S('hrUnit','MH/s')}
 var bm=d.boardVersion||'';var bmEl=E('heroBoardModel');if(bmEl){bmEl.textContent=bm;bmEl.style.display=bm?'inline':'none'}
 var w=d.power||0,hrTh=hr/1e3,jth=hrTh>0&&w>0?w/hrTh:0;
 S('heroEffVal',jth>0?jth.toFixed(1)+' J/TH':'--');
 var eb=E('effBadge');if(eb&&jth>0){if(jth<15){eb.textContent='Excellent';eb.className='eff-badge excellent'}else if(jth<25){eb.textContent='Good';eb.className='eff-badge good'}else if(jth<40){eb.textContent='Average';eb.className='eff-badge average'}else{eb.textContent='Poor';eb.className='eff-badge poor'}}else if(eb){eb.textContent=''}
 _hHist.push(hr);if(_hHist.length>7)_hHist.shift();
 var trE=E('hrTrend');if(trE&&_hHist.length>=3){var old=_hHist[0],delta=(hr-old)/Math.max(old,1)*100;if(delta>2){trE.textContent='\u25B2';trE.className='hero-trend up'}else if(delta<-2){trE.textContent='\u25BC';trE.className='hero-trend down'}else{trE.textContent='\u25C6';trE.className='hero-trend flat'}}
 var miningEnabled=d.dcentaxe?d.dcentaxe.miningEnabled!==false:hrGH>0;
 var activeHashrate=hrGH>0;
 var mining=miningEnabled&&activeHashrate;
 E('tbMiningDot').className='sb-dot'+(mining?' on':'');
 var tbm=E('tbMining');if(tbm){tbm.className='pill '+(mining?'pill-ok':miningEnabled?'pill-muted':'pill-err');tbm.innerHTML='<span class="sb-dot'+(mining?' on':'')+'" id="tbMiningDot"></span>'+(mining?'MINING':miningEnabled?'READY':'STANDBY')}
 var ms=E('heroMiningState');if(ms){ms.textContent=mining?'Mining':miningEnabled?'Ready':'Standby';ms.className='status-badge '+(mining?'ok':miningEnabled?'warn':'err');ms.style.fontSize='10px';ms.style.padding='4px 12px'}
 var hpp=E('heroPoolPill');if(hpp){var u=(d.stratumURL||'').trim();if(u){var host=u.replace(/^[a-z]+:\/\//i,'').split(/[\/:]/)[0]||'';var parts=host.split('.');var label=parts.length>=2?(parts[parts.length-2]+' '+parts[parts.length-1]):host;hpp.textContent=label.toUpperCase();hpp.hidden=false}else{hpp.hidden=true}}
 _miningEnabledLast=miningEnabled;
 window._lastInfo=d;
 var tbp=E('tbMiningPill');if(tbp){var st=mining?'mining':miningEnabled?'ready':'standby';var lbl=mining?'MINING':miningEnabled?'READY':'STANDBY';tbp.dataset.state=st;tbp.innerHTML='<span class="tb-mining-dot"></span>'+lbl}
 var tbpb=E('tbPauseBtn');if(tbpb){tbpb.dataset.mode=miningEnabled?'pause':'resume';var pl=E('tbPauseLabel');if(pl)pl.textContent=miningEnabled?'PAUSE':'RESUME';var ic=tbpb.querySelector('.tb-pause-icon');if(ic){ic.innerHTML=miningEnabled?'<rect x="3" y="2" width="3.5" height="12" rx="1"/><rect x="9.5" y="2" width="3.5" height="12" rx="1"/>':'<polygon points="3,2 13.5,8 3,14"/>'}}
 setLogMiningState(mining?'MINING':miningEnabled?'READY':'STANDBY',mining);
 var rt=d.dcentaxe||{},pt=rt.poolTruth||{},dp=rt.dispatcher||{};
 var poolD=d.poolDifficulty||0;var a=pt.sharesAccepted!=null?pt.sharesAccepted:(d.sharesAccepted||0),r=pt.sharesRejected!=null?pt.sharesRejected:(d.sharesRejected||0),submitted=pt.sharesSubmitted!=null?pt.sharesSubmitted:(a+r),confirmed=a+r,t=confirmed||1,upSec2=d.uptimeSeconds||0;
 var nowMs=Date.now(),lastAcceptedMs=pt.lastShareAcceptedUnixMs||0,lastResponseMs=pt.lastShareResponseUnixMs||0,lastRejectedMs=pt.lastShareRejectedUnixMs||0;
 var acceptedFresh=lastAcceptedMs>0&&(nowMs-lastAcceptedMs)<=600000,responseFresh=lastResponseMs>0&&(nowMs-lastResponseMs)<=600000,rejectedFresh=lastRejectedMs>0&&(nowMs-lastRejectedMs)<=600000;
 var lastAcceptedAge=lastAcceptedMs>0?Math.max(0,Math.floor((nowMs-lastAcceptedMs)/1000)):null;
 var shr=upSec2>60?a/(upSec2/3600):0;S('heroSatsVal',shr>0?shr.toFixed(1)+'/hr':'--');
 var btu=w*3.412142;S('btuVal',btu.toFixed(0));
 var eq=btu<30?'Like a phone charger':btu<60?'Like a laptop':btu<100?'Like a desk lamp':btu<200?'Like a small heater':'Like a room heater';S('btuEquiv',eq);

 S('hr1m','1m: '+fHR(d.hashRate_1m||hr));S('hr5m','5m: '+fHR(d.hashRate_5m||hr));S('hr15m','15m: '+fHR(d.hashRate_15m||hr));

 if(typeof _lastShares==='undefined')_lastShares=a;
 if(a>_lastShares){for(var si=0;si<Math.min(a-_lastShares,5);si++)addShareDot(true);_lastShares=a}
 if(typeof _lastRej==='undefined')_lastRej=r;
 if(r>_lastRej){for(var si=0;si<Math.min(r-_lastRej,5);si++)addShareDot(false);_lastRej=r}
 S('accN',a);S('rejN',r);
 var apct=confirmed?(a/t*100).toFixed(1)+'%':'--';
 E('accBar').style.width=(t>0?a/t*100:0)+'%';E('rejBar').style.width=(t>0?r/t*100:0)+'%';
 S('sessionBestDiff',fD(d.bestDiff||0));
 /* Acceptance badge */
 var ab=E('accBadge');if(ab){if(!confirmed){ab.textContent='pending';ab.className='status-badge'}else if(!acceptedFresh){ab.textContent=responseFresh||rejectedFresh?'no fresh accept':'stale';ab.className='status-badge warn'}else{var pctN=a/t*100;if(pctN>=99){ab.textContent=apct;ab.className='status-badge ok'}else if(pctN>=95){ab.textContent=apct;ab.className='status-badge warn'}else{ab.textContent=apct;ab.className='status-badge err'}}}
 /* Shares per hour */
 if(upSec2>60)S('sharesPerHr2',((a/(upSec2/3600))||0).toFixed(1));

 var ct=d.temp||0;
 /* M-dash-2 data-honesty: the headline temp readouts must respect sensor
    provenance. A dead/absent sensor (sensorsOk=false, temp=0) must NOT render as
    a real "0C / Cool", and an EMC2101 ambient PROXY must be qualified, not shown
    as the true ASIC die temp. tempKnown mirrors asic-chips.js's finite-check. */
 var tempKnown=(d.sensorsOk!==false)&&(typeof d.temp==='number')&&isFinite(d.temp);
 var tempProxy=!!(d.dcentaxe&&d.dcentaxe.tempSource==='ambient_proxy');
 /* Mining Health stat card */
  var poolOk=!!d.poolConnected;
 var health=!mining?'Stopped':ct>90?'HOT':!poolOk?'Connecting':!acceptedFresh?(confirmed?'Stale':'Pending'):(a/t*100<95)?'Unstable':'Stable';
 var hc=health==='Stable'?'var(--accent)':health==='HOT'?'var(--red)':(health==='Connecting'||health==='Pending'||health==='Stale')?'var(--yellow)':'var(--dim)';
 S('healthQuick',health);E('healthQuick').style.color=hc;
 // Health sub-line: trend marker \u2191 stable / \u2193 recovering / \u00B7 waiting.
 var hSubGlyph=health==='Stable'?'\u2191':health==='HOT'?'\u26A0':(health==='Pending'||health==='Connecting'||health==='Stale')?'\u00B7':'\u00B7';
 var hSubText=health==='Stable'?'stable':health==='HOT'?'cool the box':health==='Pending'?'accepted proof pending':health==='Stale'?'accepted proof stale':health==='Connecting'?'connecting\u2026':health==='Unstable'?'recovering':health.toLowerCase();
 var acceptedProof=acceptedFresh?('accepted '+(lastAcceptedAge===0?'<1':lastAcceptedAge)+'s ago'):(poolOk?'no fresh accepted proof':'pool offline');
 S('healthQuickLabel',hSubGlyph+' '+hSubText+' \u00B7 '+acceptedProof);
 if(!tempKnown){
  S('tempQuick','--');E('tempQuick').style.color='var(--dim)';
  S('tempQuickLabel','No sensor \u00B7 Fan: '+(d.fanspeed||0)+'%');
 }else{
  S('tempQuick',ct.toFixed(0)+'\u00B0C');
  E('tempQuick').style.color=ct>95?'var(--red)':ct>80?'var(--yellow)':ct>50?'var(--accent)':'var(--cyan)';
  var tLabel=ct>95?'Danger':ct>80?'Warning':ct>50?'Normal':'Cool';
  S('tempQuickLabel',tLabel+(tempProxy?' \u00B7 ambient proxy':'')+' \u00B7 Fan: '+(d.fanspeed||0)+'%');
 }
 S('powerQuick',w.toFixed(1)+'W');
 var iv0=(d.dcentaxe&&d.dcentaxe.inputVoltage||0)/1000;
 // Power sub: J/TH efficiency when active, fallback to freq \u00B7 V at idle.
 var pwrSub;
 if(hrGH>0&&w>0){var jth=(w/(hrGH/1000)).toFixed(1);pwrSub='\u2022 stable \u00B7 '+jth+' J/TH'}
 else{pwrSub='\u00B7 '+(d.frequency||0).toFixed(0)+' MHz @ '+iv0.toFixed(1)+'V'}
 S('powerQuickLabel',pwrSub);
 // Best Diff: show "\u2191 new best" when a fresh best lands (within 60s of session
 // best diff equalling all-time best). Otherwise show all-time / uptime.
 var bd=+d.bestDiff||0,be=+d.bestEverDiff||0;S('bestDiff',fD(bd));
 var bdSubEl=document.querySelector('#p0 .grid4 .stat:nth-child(3) .stat-sub');
 if(bdSubEl){
  var fresh=bd>0&&be>0&&bd>=be*0.999;
  if(fresh){bdSubEl.innerHTML='<span style="color:var(--green)">\u2191 new best</span> \u00B7 All-time: <span id="bestEver" style="color:var(--accent)">'+fD(be)+'</span>'}
  else{bdSubEl.innerHTML='All-time: <span id="bestEver" style="color:var(--accent)">'+fD(be)+'</span> | Up: <span id="uptime">'+fU(d.uptimeSeconds||0)+'</span>'}
 }
 var upSec=d.uptimeSeconds||0;S('uptime',fU(upSec));

 // Chart
 HRH.push(hr);if(HRH.length>MAX_PTS)HRH.shift();
 TH.push(ct);if(TH.length>MAX_PTS)TH.shift();
 PH.push(w);if(PH.length>MAX_PTS)PH.shift();
 TSH.push(Date.now());if(TSH.length>MAX_PTS)TSH.shift();
 if(_curPage===0)drawChart();
 /* Sparklines on stat cards */
 drawSpk('hrSpk',HRH.slice(-20),ac);
 drawSpk('pwrSpk',PH.slice(-20),'#fbbf24');

 // Power & Thermals gauges (6-gauge handoff layout)
 var maxW=d.overclockEnabled?50:_maxW;
 var freq=d.frequency||0;var volt=d.voltage||0;
 var ivRaw=d.dcentaxe&&d.dcentaxe.inputVoltage||0;var iv=ivRaw/1000;
 var amps=iv>0?(w/iv):0;
 S('pwrSummary',(iv>0?iv.toFixed(2)+' V':'-- V')+' · '+(amps>0?amps.toFixed(2)+' A':'-- A'));
 S('pw6Draw',w.toFixed(1));E('pw6DrawBar').style.width=pct(w,maxW)+'%';E('pw6DrawBar').className='gauge-fill '+((w/maxW*100)>90?'g-fill-red':(w/maxW*100)>70?'g-fill-orange':'g-fill-green');
 S('pw6Freq',freq.toFixed(0));E('pw6FreqBar').style.width=pct(freq,_maxFreq)+'%';
 S('pw6Core',volt.toFixed(0));E('pw6CoreBar').style.width=pct(volt,_maxVolt)+'%';
 S('pw6Asic',tempKnown?ct.toFixed(0):'--');E('pw6AsicBar').style.width=pct(tempKnown?ct:0,110)+'%';E('pw6AsicBar').className='gauge-fill '+(!tempKnown?'g-fill-cyan':ct>95?'g-fill-red':ct>80?'g-fill-yellow':ct>50?'g-fill-green':'g-fill-cyan');
 var vrt=(d.vrTemp!=null?d.vrTemp:(d.dcentaxe&&d.dcentaxe.vrTemp)||0);
 S('pw6Vreg',vrt?vrt.toFixed(0):'--');E('pw6VregBar').style.width=pct(vrt||0,110)+'%';E('pw6VregBar').className='gauge-fill '+(vrt>95?'g-fill-red':vrt>80?'g-fill-yellow':'g-fill-cyan');
 var fp=d.fanspeed||0;var fr=d.fanrpm||0;
 S('pw6Fan',fp.toFixed(0));S('pw6FanRpm',fr?'('+fr+' RPM)':'');E('pw6FanBar').style.width=pct(fp,100)+'%';

 // Thermal
 if(!tempKnown){S('chipTempBig','--');E('chipTempBig').style.color='var(--dim)';}
 else{S('chipTempBig',ct.toFixed(0)+'\u00B0C'+(tempProxy?' (ambient proxy)':''));E('chipTempBig').style.color=ct>95?'var(--red)':ct>80?'var(--yellow)':'var(--accent)';}
 drawThermGauge(ct);
 var bt2=d.temp2||d.boardTemp||0;S('boardTemp',bt2.toFixed(1)+'\u00B0C');
 var vrt=d.vrTemp||0;S('vregTemp',vrt.toFixed(1)+'\u00B0C');
 E('boardTempPill').style.display=bt2>0?'':'none';
 E('vregTempPill').style.display=vrt>0?'':'none';
 S('fanPct',d.fanspeed||0);
 // UXFLOW-SAFETY-1 honesty readout: rpm==0 ⇒ literal "No tach" (never a fake 0),
 // and a speed-proof token "RPM" (proven) vs "Unproved" (no tach). Labels only —
 // the cut-hash-before-noise BEHAVIOR lives in main.rs (XPSAFE/HALT guards), not here.
 var _rpm=d.fanrpm;
 if(!_rpm){S('fanRpm','No tach');S('fanProof','Unproved');var _fpEl=E('fanProof');if(_fpEl)_fpEl.className='dim'}
 else{S('fanRpm',_rpm+' RPM');S('fanProof','RPM');var _fpEl2=E('fanProof');if(_fpEl2)_fpEl2.className='text-green'}
 if(!_fd&&d.fanspeed!==undefined&&d.fanspeed!==null){S('fanLabel',d.fanspeed||0);E('fanSlider').value=d.fanspeed||0;if(typeof fanZoneLabel==='function')fanZoneLabel(d.fanspeed||0)}

 // Chip visualization (GT/Hex multi-ASIC) — handled by Phase 2.C
 // asic-chips component. We push the full info payload onto the
 // reactive state bus; mining-core, asic-chips, block-tile, stats,
 // flow components all subscribe to 'info' and re-render.
 if(window.state&&typeof window.state.set==='function'){window.state.set('info',d)}
  var chips=d.dcentaxe&&d.dcentaxe.chips;
  if(typeof renderChips==='function')renderChips(chips,d.asicCount||1,ct);

 // Best ever difficulty (persisted to NVS)
 var bestEver=d.bestEverDiff||d.bestDiff||0;
 var sesD=d.bestDiff||0;
 flashEl('bestEver',fD(bestEver));S('bestEver',fD(bestEver));

 /* Hex hash display (hacker detail) */
 var zeros=Math.min(16,Math.floor(Math.log2(Math.max(sesD,1))/4));
 var hexD=(sesD||0).toString(16).padStart(8,'0').slice(0,16);
 var hx='0x'+'0'.repeat(zeros)+hexD;while(hx.length<42)hx+='0';
 S('heroHex',hx.slice(0,42)+'...');

 // Difficulty Explorer bars
 S('dxSession',fD(sesD));S('dxAllTime',fD(bestEver));S('dxPool',poolD>0?fD(poolD):'--');
 var dMax=Math.max(sesD,bestEver,poolD,1);
 E('dxBarSession').style.width=Math.max(2,sesD/dMax*100)+'%';
 E('dxBarAllTime').style.width=Math.max(2,bestEver/dMax*100)+'%';
 E('dxBarPool').style.width=poolD>0?Math.max(2,poolD/dMax*100)+'%':'0';

 // Block info
 var bh=d.blockHeight||0;
 var bhStr=bh>0?'#'+bh.toLocaleString():'#--';
 S('blockHeightBig',bhStr);
 if(bh>0&&_prevBlock>0&&bh!==_prevBlock){var bc=E('blockCardHero');if(bc){bc.classList.remove('new-block');void bc.offsetWidth;bc.classList.add('new-block')}}
 _prevBlock=bh;
 S('biHeight',bh>0?'#'+bh.toLocaleString():'--');
 S('biNonces',fD(submitted));S('biPoolDiff',poolD>0?fD(poolD):'--');
 S('biAccRate',confirmed?(a/t*100).toFixed(1)+'%':'--');

 // Network card — halving countdown from d.blockHeight (no fetch/field/handler)
 renderHalvingCountdown(bh);

 // ── Pool page ──
 var pu=d.stratumURL||'--';S('poolUrl',pu);S('poolWorker',d.stratumUser||'--');
 S('poolDiff',poolD>0?fD(poolD):'--');S('poolBestDiff',fD(d.bestDiff||0));
  var poolOk=!!d.poolConnected;
 E('poolConnDot').className='sb-dot'+(poolOk?' on':'');
 S('poolConnStatus',poolOk?'Connected':'Connecting...');
 var dps=E('dPoolStatusPill');if(dps){dps.textContent=poolOk?'CONNECTED':'OFFLINE';dps.className='pill '+(poolOk?'pill-ok':'pill-muted')}
 var proto=((pt.protocol||'').indexOf('v2')>=0||d.stratumProtocol==='v2')?'v2':'v1';
 S('dPoolUrl',pu);S('dPoolPort',d.stratumPort!=null?String(d.stratumPort):'--');S('dPoolWorker',d.stratumUser||'--');
 S('dPoolProto',proto==='v2'?'Stratum V2':'Stratum V1');S('dPoolAcc',a);S('dPoolRejMeta',' / '+r+' pool rejected');S('dPoolTarget',poolD>0?fD(poolD):'--');
 S('dFailback',failbackStatusText(pt));
 var pend=pt.sharesPending!=null?pt.sharesPending:Math.max(0,submitted-a-r),unres=pt.sharesUnresolved||0;
 S('dPoolTruth',fD(submitted)+' submitted / '+fD(pend)+' pending'+(unres?' / '+fD(unres)+' unresolved':''));
 S('dShareQuality',(confirmed?(a/confirmed*100).toFixed(1)+'% pool accepted':'share proof pending')+' / '+fD(dp.filteredNonces||0)+' local filtered');
 S('dRecovery',fD(dp.slotRecoveries||d.slotRecoveries||0)+' recovered / '+fD(dp.staleNonces||d.staleNonces||0)+' stale');
 var pill=E('poolProtoPill');if(pill){if(proto==='v2'){pill.style.display='inline-flex';pill.textContent='SV2 \u2022 encrypted';pill.title='Stratum V2 with Noise_NX handshake (encrypted transport)'}else{pill.style.display='none'}}
 var sysSv2=E('sysSv2Pill');if(sysSv2)sysSv2.style.display=(proto==='v2')?'inline-flex':'none';
 /* SV2-AVAIL (H4): gate the "Stratum V2 (Noise)" option on real build capability.
    /api/system/info reports stratumV2Available=false when the firmware was built
    without the stratum-v2 feature; on such a build, selecting V2 routes to a no-op
    client stub that sleeps forever (no mining, no failover). Disable+relabel the
    option in all 3 protocol selects, hide the SV2 own-templates config, and
    neutralize the System-page enable copy so the operator can't pick a dead path. */
 var sv2avail=d.stratumV2Available!==false;
 ['pProtocol','fbProtocol','splitProtocol'].forEach(function(id){
  var sel=E(id);if(!sel)return;var opt=null;
  for(var i=0;i<sel.options.length;i++){if(sel.options[i].value==='v2'){opt=sel.options[i];break}}
  if(!opt)return;opt.disabled=!sv2avail;
  opt.textContent=sv2avail?'Stratum V2 (Noise)':'Stratum V2 (not in this build)';
  if(!sv2avail&&sel.value==='v2'){sel.value='v1'}
 });
 var ownTpl=E('ownTemplatesDetails');if(ownTpl)ownTpl.style.display=sv2avail?'':'none';
 var sysSv2Copy=E('sysSv2Copy');
 if(sysSv2Copy)sysSv2Copy.innerHTML=sv2avail?'Encrypted pool transport via Noise_NX handshake. Use <code style="font-family:var(--mono);color:var(--accent)">stratum2+tcp://</code> in the Pool page to enable.':'Encrypted pool transport via Noise_NX handshake. <b style="color:var(--text)">Not included in this firmware build.</b>';
 var sysBap=E('sysBapRow');if(sysBap)sysBap.style.display=d.hasBap?'block':'none';
 S('poolConnDur',poolOk?fU(upSec):'--');
 if(upSec>60)S('sharesPerHr',((a/(upSec/3600))||0).toFixed(1));
 if(_curPage===1)loadPoolsRuntime(false);

 // ── Network page ──
 var rssi=d.wifiRSSI||0;
 S('netSsid',_ssid);S('netIp',_ip);S('netMac',(d.macAddr&&String(d.macAddr).trim())||'--');
 S('netRssi',rssi+' dBm');
 S('netSigLabel',rssi>-50?'Excellent':rssi>-60?'Good':rssi>-75?'Fair':'Poor');
 S('netUptime',fU(upSec));
 var rp=Math.min(100,Math.max(0,(rssi+90)*2.5));
 E('netSigBar').style.width=rp+'%';
 E('netSigBar').style.background=rssi>-60?'var(--green)':rssi>-75?'var(--yellow)':'var(--red)';

 // ── System page ──
 S('sysVariant',(d.displayName||d.deviceModel||'--')+(d.hasBap?' \u00B7 BAP accessory':''));
 S('asicModel',d.ASICModel||'--');
 // Don't mask zero-chip detection behind ||1 — a newly-booted device with a
 // detached chain should surface "0x" so the operator knows to investigate.
 var _chipN=(typeof d.asicCount==='number'?d.asicCount:-1);
 S('asicChips',(_chipN<0?'?':_chipN)+'x '+(d.ASICModel||'?')+(_chipN===0?' \u2014 no chips detected':''));
 // coreCount = big cores per chip (e.g. 128 for BM1370), smallCoreCount = small cores.
 S('asicCores',(d.coreCount||'?')+' big / '+(d.smallCoreCount||'?')+' small per chip');
 S('sysUptime',fU(upSec));S('sysSsid',_ssid);S('sysIp',_ip);
 S('mac',d.macAddr||'--');S('heap',((d.freeHeap||0)/1024).toFixed(0)+' KB');
 var fwStr='DCENT_axe '+(d.version||'?');
 // Touch variants get a little glyph so operators can tell at a glance the
 // accessory is expected to be present. The accessory itself remains the
 // primary UI — the dashboard just mirrors the device name.
 var modelLabel=(d.displayName||d.deviceModel||'')+(d.hasBap?' \u25A3':'');
 if(modelLabel){fwStr=modelLabel+' \u00B7 '+fwStr}
 S('sysFw',fwStr);S('otaCurrentVer',fwStr);S('fwVer',fwStr);
 S('idfVer',d.idfVersion||'--');S('partition',d.runningPartition||'--');S('resetReason',d.resetReason||'--');
 if(d.safeMode){S('resetReason',(d.resetReason||'?')+' \u00B7 SAFE MODE')}
 // Build stamp — git hash + local build time (seconds since epoch → local date).
 var build='';if(d.gitHash){build=d.gitHash+(d.gitDirty?'+dirty':'')}if(d.buildEpoch){var bd=new Date(d.buildEpoch*1000);build+=(build?' \u00B7 ':'')+bd.toISOString().slice(0,10)}S('sysBuild',build||'--');

 if(d.dcentaxe&&d.dcentaxe.powerLimits){var pl=d.dcentaxe.powerLimits;if(pl.maxFrequency)_maxFreq=pl.maxFrequency;if(pl.maxVoltageMv)_maxVolt=pl.maxVoltageMv;if(pl.maxPowerW)_maxW=pl.maxPowerW}
 if(!_oc){E('ocEnable').checked=!!d.overclockEnabled;E('ocWarn').style.display=d.overclockEnabled?'block':'none'}
 if(!_dd)E('flipScreen').checked=!!(d.invertscreen||d.flipscreen);
  var at=d.dcentaxe&&d.dcentaxe.autotuner;
  if(at){if(!_ad)E('atEnable').checked=at.enabled;S('atStatus',at.status||'Unavailable');renderAutotunerEvidence(at)}
  if(d.dcentaxe&&d.dcentaxe.schedule)renderScheduleStatus(d.dcentaxe.schedule);

  // Creature
 if(d.dcentaxe){var dx=d.dcentaxe;var mood=dx.creatureMood!=null?dx.creatureMood:5;
  var face=FACES[Math.min(Math.max(Math.round(mood),0),10)];
  S('creatureFace',face);E('creatureFace').style.color=mood>=7?'var(--accent)':mood>=4?'var(--dim)':'var(--red)';
  if(dx.achievementCount>_prevAch&&_prevAch>0)showToast('Achievement unlocked!');
  _prevAch=dx.achievementCount||0;
  renderAch(dx.achievements||0)}

 // Mining log events \u2014 populate breadcrumb host + counters too
 var hn=(d.hostname&&String(d.hostname).trim())||'bitaxe';S('logHost',hn);
 if(!_mlogInit){_mlogInit=1;_prevBlockLog=bh;_prevSharesLog=a;_prevRejLog=r;_prevPoolDiff=poolD;
  mlog('system boot \u00b7 DCENT_axe '+(d.version||'')+' \u00b7 '+(d.ASICModel||'BitAxe'),'sys');
  mlog('chain detected \u00b7 '+(d.asicCount||1)+'\u00d7 '+(d.ASICModel||'?')+' \u00b7 '+(d.frequency||0).toFixed(0)+' MHz \u00b7 '+(d.coreVoltageActual||d.coreVoltage||0)+' mV','asic')}
 if(bh>0&&bh!==_prevBlockLog&&_prevBlockLog>0){mlog('NEW BLOCK \u00b7 height '+bh.toLocaleString()+' \u00b7 extranonce reset','blk');var hero=document.querySelector('.hero');if(hero){hero.style.boxShadow='0 0 40px rgba(247,147,26,0.3),inset 0 1px 0 rgba(255,255,255,0.08)';setTimeout(function(){hero.style.boxShadow=''},2500)}}
 _prevBlockLog=bh;
 if(a>_prevSharesLog){var hr=d.hashRate||0;mlog('pool accepted share \u00b7 diff '+fD(d.bestDiff||0)+' \u00b7 '+hr.toFixed(0)+' GH/s','ok');_logCounters.accepted+=(a-_prevSharesLog)}
 if(r>_prevRejLog){mlog('pool rejected share ('+r+' total)','err');_logCounters.rejected+=(r-_prevRejLog)}
 _prevSharesLog=a;_prevRejLog=r;
 if(poolD>0&&poolD!==_prevPoolDiff&&_prevPoolDiff>0)mlog('stratum difficulty \u2192 '+fD(poolD),'pool');
 _prevPoolDiff=poolD;
 /* HW errors / stale shares from API */
 var hwTot=d.sharesRejectedReasons&&(d.sharesRejectedReasons['Job not found']||0)+(d.sharesRejectedReasons['Difficulty too low']||0)||0;
 if(typeof _prevHwLog==='undefined')_prevHwLog=hwTot;if(hwTot>_prevHwLog){_logCounters.hwErr+=(hwTot-_prevHwLog);mlog('hw error counter +'+(hwTot-_prevHwLog),'warn')}_prevHwLog=hwTot;
 var stTot=(d.sharesRejectedReasons&&d.sharesRejectedReasons['Stale share'])||0;
 if(typeof _prevStLog==='undefined')_prevStLog=stTot;if(stTot>_prevStLog){_logCounters.stale+=(stTot-_prevStLog);mlog('stale share +'+(stTot-_prevStLog),'warn')}_prevStLog=stTot;

 // Alert
 if(ct>90){E('alertBox').textContent='THERMAL WARNING: '+ct.toFixed(0)+'\u00B0C';E('alertBox').className='alert show'}else{E('alertBox').className='alert'}
 // Safe-mode + coredump banners — follow the same show/hide pattern as alertBox.
 var sm=E('safeModeBanner');if(d.safeMode){
  var n=d.wdtResetCount||0;
  var detail='Mining disabled after '+n+' task-watchdog reset'+(n===1?'':'s')+' inside a 5-minute window. The firmware is probably wedging on a real bug \u2014 grab the coredump (if present) before clearing.';
  if(d.coredumpPresent)detail+=' ';
  E('safeModeDetail').innerHTML=detail.replace(/&/g,'&amp;').replace(/</g,'&lt;')+(d.coredumpPresent?' <a href="#" onclick="event.preventDefault();downloadCoredump()" style="color:var(--accent);text-decoration:underline">Download coredump first &rarr;</a>':'');
  sm.style.display='flex'
 }else{sm.style.display='none'}
 var cd=E('coredumpBanner');cd.style.display=d.coredumpPresent?'flex':'none';
}

/* ── Offline / Reconnect ── */
// `cause` shows up in the banner so operators can tell a timeout from a 500
// from a real network drop.
// TERM-6 telemetry-state words (terminology-lexicon \u00a76.1, glossary key
// telemetry_stale / telemetry_absent): axe was binary online/offline. The
// canonical lexicon splits HELD-but-stale data ('Telemetry stale') from
// nothing-arriving ('Offline'/telemetry_absent). When a poll fails but a prior
// good /api/system/info is still held in _lastInfo, the banner reads
// 'Telemetry stale' -- the held readings are real PAST values, NEVER extrapolated
// forward (truth-contract). LABEL only: no value is fabricated, and it stays
// distinct from the share-proof 'Stale' health word (share freshness is a
// different axis). Labels resolve from window.GLOSSARY with a literal fallback.
function _glossLbl(key,lit){var g=window.gloss;if(typeof g==='function'){var v=g(key,'label');if(v)return v}return lit}
function _telemetryBanner(cause){
 /* Explicit transport cause (Timeout / HTTP 500 / ...) shown verbatim. With no
    explicit cause: held telemetry -> 'Telemetry stale'; never-arrived -> 'Offline'. */
 if(cause)return cause;
 return _lastInfo?_glossLbl('telemetry_stale','Telemetry stale'):_glossLbl('telemetry_absent','Offline');
}
function goOffline(cause){var msg=_telemetryBanner(cause);if(_offline){if(cause)S('reconnectMsg',msg+' \u2014 retrying...');return}_offline=1;_offStart=HRH.length;_retryDelay=10;E('dot').className='sb-dot';S('reconnectMsg',msg+' \u2014 retrying...');E('offlineBanner').style.display='block';if(typeof _logCounters!=='undefined'){_logCounters.reconnect++;if(typeof mlog==='function')mlog('connection lost \u00b7 '+(cause||(_lastInfo?'telemetry stale':'offline'))+' \u00b7 reconnecting','net')}schedRetry()}
function schedRetry(){var sec=_retryDelay;S('reconnectMsg','Reconnecting in '+sec+'...');_retryT=setInterval(function(){sec--;if(sec<=0){clearInterval(_retryT);S('reconnectMsg','Retrying...');fetch('/api/system/info',{headers:authHeaders({})}).then(handleReadAuthFailure).then(function(r){return r.json()}).then(function(d){_offline=0;_retryDelay=10;E('offlineBanner').style.display='none';update(d)}).catch(function(e){if(e.message==='auth-required'){_offline=0;E('offlineBanner').style.display='none';return} _retryDelay=Math.min(_retryDelay*2,60);schedRetry()})}else{S('reconnectMsg','Reconnecting in '+sec+'...')}},1000)}

/* ── Poll ── */
// Timed fetch — aborts after 8 s so we detect stalled networks before the next
// poll interval fires (browser default is effectively infinite on some
// platforms). Pairs with goOffline() below.
function tfetch(url,opts,ms){var c=new AbortController();var t=setTimeout(function(){c.abort()},ms||8000);opts=opts||{};opts.signal=c.signal;return fetch(url,opts).finally(function(){clearTimeout(t)})}
function poll(){tfetch('/api/system/info',{headers:authHeaders({})}).then(handleReadAuthFailure).then(function(r){if(!r.ok)throw new Error('HTTP '+r.status);return r.json()}).then(function(d){update(d);if(_curPage===6)loadSwarm()}).catch(function(e){if(e.message==='auth-required')return;if(e.name==='AbortError'){goOffline('Timeout');return}goOffline(e.message||'Offline')})}
/* Write to a field only when it is NOT the active element. Prevents
   loadCfg()/poll() from overwriting characters a user is actively typing. */
function setIfIdle(id,v){var el=E(id);if(!el)return;if(document.activeElement===el)return;el.value=v}
function setCheckedIfIdle(id,v){var el=E(id);if(!el)return;if(document.activeElement===el)return;el.checked=!!v}
function hEsc(v){return String(v==null?'':v).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;')}
function failbackStatusText(pt){
 pt=pt||{};
 var state=pt.primaryFailbackState||'',detail=pt.primaryFailbackDetail||'';
 if(!state&&Array.isArray(pt.recentEvents)){
  for(var i=pt.recentEvents.length-1;i>=0;i--){var ev=pt.recentEvents[i]||{},k=ev.kind||'';if(k==='primary_failback_entered'||k==='primary_reprobe_ready'||k==='primary_reprobe_failed'||k==='primary_reprobe_started'||k==='failover_entered'){state=k;detail=ev.detail||'';break}}
 }
 if(!state&&pt.failoverActive)state='fallback_active';
 var labels={fallback_active:'Fallback active',failover_entered:'Fallback active',primary_reprobe_started:'Primary reprobe',primary_reprobe_failed:'Primary reprobe failed',primary_reprobe_ready:'Primary ready (job proof)',primary_failback_entered:'Primary route entered',failback_entered:'Primary route entered',reprobe_started:'Primary reprobe',reprobe_failed:'Primary reprobe failed',reprobe_ready:'Primary ready (job proof)'};
 var label=labels[state]||'--';
 if(label==='--')return label;
 if(detail&&detail.length>72)detail=detail.slice(0,69)+'...';
 return label+(detail?' - '+detail:'');
}
function renderPoolsRuntime(pools){
 var box=E('splitRuntime');if(!box)return;
 if(!pools||pools.length<2){box.style.display='none';box.innerHTML='';return}
 box.style.display='block';
 var rows=pools.map(function(p){
  var tgt=typeof p.target_pct==='number'?p.target_pct:0,act=typeof p.actual_pct==='number'?p.actual_pct:0;
  return '<div class="kv-row"><span class="kv-key">Pool '+(p.index+1)+' '+tgt+'%</span><span class="kv-val">'+act.toFixed(1)+'% actual / '+hEsc(p.connected?'connected':'offline')+'</span></div>'
   +'<div style="height:4px;background:rgba(255,255,255,0.06);border-radius:4px;margin:2px 0 6px"><div style="height:4px;width:'+Math.max(0,Math.min(100,act))+'%;background:var(--accent);border-radius:4px"></div></div>';
 }).join('');
 box.innerHTML='<div class="card-title" style="font-size:11px;margin-bottom:6px">Hashrate Split</div>'+rows;
}
function loadPoolsRuntime(force){var now=Date.now();if(!force&&now-_lastPoolFetch<15000)return;_lastPoolFetch=now;fetch('/api/pools',{headers:authHeaders({})}).then(handleReadAuthFailure).then(function(r){return r.json()}).then(function(d){renderPoolsRuntime(Array.isArray(d)?d:[])}).catch(function(){})}
function loadCfg(){fetch('/api/system',{headers:authHeaders({})}).then(handleReadAuthFailure).then(function(r){return r.json()}).then(function(d){
 setIfIdle('pProtocol',d.stratumProtocol||'v1');setIfIdle('pUrl',d.stratumURL||'');setIfIdle('pPort',d.stratumPort||'');setIfIdle('pUser',d.stratumUser||'');
 setCheckedIfIdle('ownTplEnable',!!d.sv2OwnTemplatesEnabled);setIfIdle('ownTplProxyUrl',d.sv2TemplateProxyURL||((d.sv2OwnTemplatesEnabled&&d.stratumProtocol==='v2')?d.stratumURL:''));setIfIdle('ownTplProviderUrl',d.sv2TemplateProviderURL||'');setIfIdle('ownTplJdUrl',d.sv2JobDeclaratorURL||'');ownTemplateChanged();
 setIfIdle('fbProtocol',d.fallbackStratumProtocol||'v1');setIfIdle('fbUrl',d.fallbackStratumURL||'');setIfIdle('fbPort',d.fallbackStratumPort||3333);setIfIdle('fbUser',d.fallbackStratumUser||'');
 setCheckedIfIdle('splitEnable',!!d.splitPoolEnabled);setIfIdle('splitProtocol',d.splitPoolProtocol||'v1');setIfIdle('splitUrl',d.splitPoolURL||'');setIfIdle('splitPort',d.splitPoolPort||3333);setIfIdle('splitUser',d.splitPoolUser||'');setIfIdle('splitPct',d.splitPoolPct||20);updateSplitPctLabel();
 setIfIdle('setFreq',d.frequency||'');setIfIdle('setVolt',d.coreVoltage||'');
 setIfIdle('fanMode',(d.autofanspeed||0)?'auto':'manual');setIfIdle('fanTargetTemp',d.temptarget||65);setIfIdle('fanSlider',d.manualFanSpeed||d.fanspeed||100);
 if(document.activeElement!==E('fanSlider'))S('fanLabel',E('fanSlider').value);
 if(typeof fanZoneLabel==='function')fanZoneLabel(E('fanSlider').value);
 fanModeChanged();
 if(d.mqtt){setCheckedIfIdle('mqttEnable',!!d.mqtt.enabled);setIfIdle('mqttHost',d.mqtt.brokerHost||'');setIfIdle('mqttPort',d.mqtt.brokerPort||1883);setIfIdle('mqttUser',d.mqtt.username||'');setCheckedIfIdle('mqttTls',!!d.mqtt.tls);setIfIdle('mqttInterval',d.mqtt.publishIntervalS||30);if(E('mqttPass'))E('mqttPass').placeholder=d.mqtt.passwordSet?'••• set (leave blank to keep)':'leave blank for none';}
 setIfIdle('netHostname',d.hostname||'');setIfIdle('netSsidInput',d.ssid||'');
 loadPoolsRuntime(true);
}).catch(function(){})}

function renderSwarm(d){
 if(!d)return;
 var local=d.local||{},disc=d.discovery||{},peers=d.peers||[];
 var role=(d.role||'standalone').toLowerCase();
 var badge=E('swarmRoleBadge');
 if(badge){
  var labels={queen:'\u{1F451} Queen',worker:'\u{1F528} Worker',standalone:'\u{1F3E0} Standalone'};
  badge.textContent=labels[role]||labels.standalone;
  badge.className='status-badge '+(role==='queen'?'ok':role==='worker'?'ok':'');
  badge.style.background=role==='queen'?'rgba(247,147,26,0.15)':role==='worker'?'rgba(34,211,238,0.12)':'rgba(107,122,141,0.12)';
  badge.style.color=role==='queen'?'var(--accent)':role==='worker'?'var(--cyan)':'var(--dim)';
 }
 S('swarmHost',local.hostname||'--');
 S('swarmIp',local.ip||'--');
 S('swarmBoard',(local.board_model||'--')+(local.board_version?(' ('+local.board_version+')'):' '));
 var hr=typeof local.hashrate_ghs==='number'?local.hashrate_ghs:0;
 S('swarmHashrate',hr?hr.toFixed(1)+' GH/s':'--');
 var p=(typeof d.powerWatts==='number'?d.powerWatts:0);
 S('swarmHeat',p?((p*3.412142).toFixed(0)+' BTU/h \u00B7 '+p.toFixed(1)+' W'):'--');
 S('swarmQueen',d.queenId||d.queen_id||'none');
 S('swarmMdns',disc.mdnsHostname||'disabled');
 S('swarmApi',disc.apiUrl||'--');
 S('swarmMcp',disc.mcpUrl||'--');
 S('swarmHint',disc.discoveryHint||'');
 S('swarmPeerCount',String(d.peerCount||peers.length||0));
 var host=E('swarmPeers');if(!host)return;
 if(!peers.length){host.innerHTML='No peers reported yet. Other miners announce themselves via <code>POST /api/swarm/report</code>.';return}
 var now=Date.now();
 host.innerHTML='<div style="display:grid;gap:6px;font-size:12px">'+peers.map(function(p){
  var age=now-(p.last_seen_unix_ms||0);
  var fresh=age<120000;
  var ageStr=age<60000?'<1m':age<3600000?Math.floor(age/60000)+'m':Math.floor(age/3600000)+'h';
  var qTag=(d.queenId===p.id||d.queen_id===p.id)?' <span class="pill" style="font-size:9px;padding:1px 6px">queen</span>':'';
  var hr=typeof p.hashrate_ghs==='number'?p.hashrate_ghs.toFixed(1)+' GH/s':'--';
  return '<div class="kv-flex-row" style="opacity:'+(fresh?'1':'0.55')+'"><span><b style="color:var(--text)">'+hEsc(p.hostname||p.id||'peer')+'</b>'+qTag+' <span class="text-dim meta-mono" style="margin-left:6px">'+hEsc(p.ip||'')+'</span></span><span class="text-dim meta-mono">'+hEsc(p.board_model||'?')+' \u00B7 '+hr+' \u00B7 '+ageStr+' ago</span></div>';
 }).join('')+'</div>';
}
function setRoomTempSource(src){
 fetch('/api/swarm/config',{method:'POST',headers:authHeaders({'Content-Type':'application/json'}),body:JSON.stringify({roomTempSource:src})}).then(handleReadAuthFailure).then(function(r){
  if(r.ok){S('roomTempStatus','Saved: '+src.replace('_',' '));showToast('Room-temp source set')}
  else showToast('Save failed','error');
 }).catch(function(e){if(e.message!=='auth-required')showToast('Save failed','error')})
}
function loadSwarm(){fetch('/api/swarm',{headers:authHeaders({})}).then(handleReadAuthFailure).then(function(r){return r.json()}).then(renderSwarm).catch(function(){})}

/* ── Keyboard shortcuts ── */
document.addEventListener('keydown',function(e){if(e.target.tagName==='INPUT'||e.target.tagName==='SELECT')return;
 if(e.key==='R'&&e.shiftKey)doReboot();
 var k=parseInt(e.key);if(k>=1&&k<=8)go(k-1)});

/* ── Touch swipe ── */
var _tsx=0,_tsEdge=false;
document.addEventListener('touchstart',function(e){_tsx=e.touches[0].clientX;_tsEdge=_tsx<30||_tsx>window.innerWidth-30});
document.addEventListener('touchend',function(e){if(!_tsEdge)return;var dx=e.changedTouches[0].clientX-_tsx;if(Math.abs(dx)>80){if(dx>0&&_curPage>0)go(_curPage-1);if(dx<0&&_curPage<6)go(_curPage+1)}});

/* CAP-OS2AXE-2 lite fan-curve editor (vanilla SVG, zero deps). FanCurvePoint
   {temp,pwm} shape + geometry borrowed from OS FanCurveEditor; PWM clamps to axe's
   floor 20..100 (NOT OS 10-30). Apply derives KNEE+FLOOR and writes via the auth-
   gated post('/api/system') helper. See dcentaxe-core S4 guards. */
var _fanCurvePts=null,_fanCurveDrag=-1;
function _fanCurveDefault(){return [{temp:40,pwm:20},{temp:60,pwm:30},{temp:75,pwm:55}]}
function _fanCurveLoad(){if(_fanCurvePts)return;try{var s=localStorage.getItem('dcentaxe-fan-curve');if(s){var p=JSON.parse(s);if(p&&p.length>=2){_fanCurvePts=p.map(function(q){return{temp:Math.max(20,Math.min(80,Math.round(+q.temp||20))),pwm:Math.max(20,Math.min(100,Math.round(+q.pwm||20)))}});return}}}catch(e){}_fanCurvePts=_fanCurveDefault()}
function _fcX(t){return 34+((t-20)/60)*274}
function _fcY(p){return 154-((p-20)/80)*142}
function _fcToTemp(x){return 20+((x-34)/274)*60}
function _fcToPwm(y){return 20+((154-y)/142)*80}
function fanCurveInit(){var svg=E('fanCurveSvg');if(!svg||svg._fcInit)return;svg._fcInit=1;
 svg.addEventListener('pointerdown',function(e){var t=e.target,a=t&&t.getAttribute&&t.getAttribute('data-fc');if(a==null)return;_fanCurveDrag=parseInt(a);try{svg.setPointerCapture(e.pointerId)}catch(_){}e.preventDefault()});
 svg.addEventListener('pointermove',function(e){if(_fanCurveDrag<0)return;var pt=svg.createSVGPoint();pt.x=e.clientX;pt.y=e.clientY;var m=svg.getScreenCTM();if(!m)return;var l=pt.matrixTransform(m.inverse());var tt=Math.max(20,Math.min(80,Math.round(_fcToTemp(l.x))));var pp=Math.max(20,Math.min(100,Math.round(_fcToPwm(l.y))));_fanCurvePts[_fanCurveDrag]={temp:tt,pwm:pp};fanCurveRender()});
 var end=function(){_fanCurveDrag=-1};svg.addEventListener('pointerup',end);svg.addEventListener('pointercancel',end);
}
function fanCurveRender(){var svg=E('fanCurveSvg');if(!svg)return;_fanCurveLoad();fanCurveInit();
 var arr=_fanCurvePts,sorted=arr.slice().sort(function(a,b){return a.temp-b.temp});
 var d='';for(var i=0;i<sorted.length;i++){d+=(i?'L':'M')+_fcX(sorted[i].temp).toFixed(1)+' '+_fcY(sorted[i].pwm).toFixed(1)+' '}
 var s='<rect x="34" y="12" width="274" height="142" rx="4" fill="var(--s-void)" stroke="var(--border)"/>';
 s+='<line x1="34" y1="'+_fcY(30).toFixed(1)+'" x2="308" y2="'+_fcY(30).toFixed(1)+'" stroke="var(--green)" stroke-width="1" stroke-dasharray="3 3" opacity="0.5"/>';
 s+='<text x="306" y="'+(_fcY(30)-3).toFixed(1)+'" fill="var(--dim)" font-size="8" text-anchor="end">Home cap 30%</text>';
 s+='<path d="'+d+'" fill="none" stroke="var(--accent)" stroke-width="2"/>';
 for(var j=0;j<arr.length;j++){var cx=_fcX(arr[j].temp),cy=_fcY(arr[j].pwm);s+='<circle data-fc="'+j+'" cx="'+cx.toFixed(1)+'" cy="'+cy.toFixed(1)+'" r="6" fill="var(--accent)" stroke="var(--s-void)" stroke-width="2" style="cursor:grab"/>';s+='<text x="'+cx.toFixed(1)+'" y="'+(cy-9).toFixed(1)+'" fill="var(--dim)" font-size="8" text-anchor="middle">'+Math.round(arr[j].temp)+'C/'+Math.round(arr[j].pwm)+'%</text>'}
 s+='<text x="34" y="170" fill="var(--dim)" font-size="8">20C</text><text x="308" y="170" fill="var(--dim)" font-size="8" text-anchor="end">80C</text>';
 svg.innerHTML=s;
}
function fanCurveReset(){_fanCurvePts=_fanCurveDefault();try{localStorage.removeItem('dcentaxe-fan-curve')}catch(e){}fanCurveRender();showToast('Fan curve reset')}
function fanCurveApply(){_fanCurveLoad();var p=_fanCurvePts.slice().sort(function(a,b){return a.temp-b.temp});
 var floor=Math.max(20,Math.min(100,Math.round(p[0].pwm)));
 var knee=Math.max(40,Math.min(80,Math.round(p[Math.min(1,p.length-1)].temp)));
 try{localStorage.setItem('dcentaxe-fan-curve',JSON.stringify(p))}catch(e){}
 post('/api/system',{fanMode:'auto',autofanspeed:1,fanTargetTemp:knee,fanSpeed:floor},function(){showToast('Fan curve applied — knee '+knee+'C, floor '+floor+'%')});
}

/* CAP-OS2AXE-6 lite first-run wizard (3-4 step overlay). Gating = dismissible
   localStorage flag + a "no configured pool" heuristic (existing installs never see
   it). Apply reuses the auth-gated post('/api/system') + autotune writes. See
   dcentaxe-core S4 guards. */
var _frStep=0;
function firstRunMaybe(){try{if(localStorage.getItem('dcentaxe-firstrun-done'))return}catch(e){}
 var pu=E('pUrl');if(pu&&pu.value&&pu.value.trim()){try{localStorage.setItem('dcentaxe-firstrun-done','1')}catch(e){}return}
 var o=E('frOverlay');if(!o)return;_frStep=0;o.style.display='flex';firstRunShow()}
function firstRunShow(){var steps=document.querySelectorAll('#frSteps .fr-step');for(var i=0;i<steps.length;i++)steps[i].style.display=(i===_frStep?'':'none');
 var back=E('frBack');if(back)back.style.visibility=_frStep===0?'hidden':'visible';
 var nxt=E('frNext');if(nxt)nxt.textContent=(_frStep>=steps.length-1)?'Apply & Finish':'Next';
 S('frDots','Step '+(_frStep+1)+' / '+steps.length);
 if(_frStep>=steps.length-1)firstRunReview()}
function firstRunNext(){var steps=document.querySelectorAll('#frSteps .fr-step');if(_frStep>=steps.length-1){firstRunApply();return}_frStep++;firstRunShow()}
function firstRunPrev(){if(_frStep>0){_frStep--;firstRunShow()}}
function firstRunReview(){var url=((E('frPoolUrl')&&E('frPoolUrl').value)||'').trim(),port=((E('frPoolPort')&&E('frPoolPort').value)||'').trim(),user=((E('frPoolUser')&&E('frPoolUser').value)||'').trim();
 var mode=E('frMode')?E('frMode').value:'',tgt=((E('frTarget')&&E('frTarget').value)||'').trim();
 var un='<span style="color:var(--muted)">unchanged</span>';
 var rows=['Pool: '+(url?hEsc(url)+(port?(':'+hEsc(port)):''):un),'Worker: '+(user?hEsc(user):un),'Mode: '+hEsc(mode||'unchanged')+(tgt?(' @ '+hEsc(tgt)):'')];
 var e=E('frReview');if(e)e.innerHTML=rows.join('<br>')}
function firstRunSkip(){try{localStorage.setItem('dcentaxe-firstrun-done','1')}catch(e){}var o=E('frOverlay');if(o)o.style.display='none'}
function firstRunApply(){var url=((E('frPoolUrl')&&E('frPoolUrl').value)||'').trim(),port=parseInt((E('frPoolPort')&&E('frPoolPort').value))||0,user=((E('frPoolUser')&&E('frPoolUser').value)||'').trim();
 var mode=E('frMode')?E('frMode').value:'',tgt=parseFloat((E('frTarget')&&E('frTarget').value))||0;
 var finish=function(){try{localStorage.setItem('dcentaxe-firstrun-done','1')}catch(e){}var o=E('frOverlay');if(o)o.style.display='none';showToast('Setup complete')};
 var doPool=function(){if(url){var b={stratumURL:url,stratumPort:port||3333,stratumProtocol:'v1'};if(user)b.stratumUser=user;post('/api/system',b,function(){finish()})}else{finish()}};
 if(mode){post('/api/mining/autotune',{enabled:true,mode:mode,target:tgt},function(){doPool()})}else{doPool()}}

/* ── Boot ── */
var _pollInterval;
function startPoll(){if(_pollInterval)clearInterval(_pollInterval);var rate=document.hidden?30000:15000;_pollInterval=setInterval(poll,rate);if(_blockFetchTimer)clearInterval(_blockFetchTimer);_blockFetchTimer=setInterval(pollBlockHero,10000)}
renderAuthUi();poll();loadCfg();loadSharedCfg();loadAuthStatus();loadPresets();loadAutotunerModes();loadSchedule();loadAchievements();loadSwarm();pollBlockHero();startMempoolPoll();startPoll();
try{fanCurveInit();fanCurveRender();}catch(e){}
setTimeout(function(){try{firstRunMaybe();}catch(e){}},800);
document.addEventListener('visibilitychange',startPoll);
var _resizeT;window.addEventListener('resize',function(){clearTimeout(_resizeT);_resizeT=setTimeout(drawChart,100)});
</script>
</body></html>"##;
