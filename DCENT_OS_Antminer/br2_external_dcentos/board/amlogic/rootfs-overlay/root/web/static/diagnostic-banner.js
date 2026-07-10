// DCENT_OS diagnostic banner — injected into any /static/index.html load.
// On every page, polls /api/dashboard/health (always-local). When dcentrald
// is dead, prepends a red bar with a link to /diagnostic so the user
// always has a way to see what's wrong.
(function () {
  const BAR_ID = "dcent-diag-banner";
  function inject(status, error) {
    if (document.getElementById(BAR_ID)) return;
    const bar = document.createElement("div");
    bar.id = BAR_ID;
    bar.style.cssText = [
      "position:fixed", "top:0", "left:0", "right:0",
      "z-index:99999",
      "background:#a01010", "color:#fff",
      "padding:10px 16px",
      "font-family:system-ui,-apple-system,Segoe UI,Inter,Roboto,sans-serif",
      "font-size:14px", "font-weight:500",
      "display:flex", "align-items:center", "gap:12px",
      "box-shadow:0 2px 6px rgba(0,0,0,0.4)",
    ].join(";");
    const pill = document.createElement("span");
    pill.style.cssText = "background:#fff;color:#a01010;padding:2px 8px;border-radius:999px;font-size:11px;font-weight:700;letter-spacing:1px;";
    pill.textContent = status === "starting" ? "STARTING" : "DEAD";
    const msg = document.createElement("span");
    msg.textContent = status === "starting"
      ? "dcentrald is starting up — API not yet available."
      : "dcentrald is not running — dashboard cannot show live mining data.";
    if (error) {
      const err = document.createElement("span");
      err.style.cssText = "opacity:0.85;font-size:12px;font-family:ui-monospace,monospace;";
      err.textContent = "· " + (error.length > 100 ? error.slice(0, 100) + "…" : error);
      msg.appendChild(document.createTextNode(" "));
      msg.appendChild(err);
    }
    const linkStyle = "color:#fff;background:rgba(0,0,0,0.25);padding:5px 12px;border-radius:4px;text-decoration:none;font-weight:600;";
    // Static recovery page (GROUP B): self-contained SSH/restart/log/rollback
    // guidance that renders with zero JS and zero daemon. Surfaced first so a
    // daemon-down operator always has a non-dead path to recovery steps.
    const recoveryLink = document.createElement("a");
    recoveryLink.href = "/recovery";
    recoveryLink.textContent = "→ Recovery";
    recoveryLink.style.cssText = linkStyle + "margin-left:auto;";
    const link = document.createElement("a");
    link.href = "/diagnostic";
    link.textContent = "→ Diagnostic Mode";
    link.style.cssText = linkStyle + "margin-left:8px;";
    bar.appendChild(pill);
    bar.appendChild(msg);
    bar.appendChild(recoveryLink);
    bar.appendChild(link);
    document.body.insertBefore(bar, document.body.firstChild);
    // Push the page content down so the banner doesn't overlap.
    document.body.style.paddingTop = "44px";
  }
  function remove() {
    const bar = document.getElementById(BAR_ID);
    if (bar) {
      bar.remove();
      document.body.style.paddingTop = "";
    }
  }
  async function check() {
    try {
      const r = await fetch("/api/dashboard/health", { cache: "no-store" });
      if (!r.ok) {
        // P0-6 (C-7): a non-200 means we could NOT confirm a healthy daemon —
        // a 404 from a daemon binary that doesn't register this route, the 503
        // disconnect marker server.py returns when it can't reach dcentrald on
        // :8080, or any 5xx. Treat "couldn't confirm health" as unhealthy and
        // surface the bar instead of silently returning (the original bug:
        // the "dcentrald is DEAD" bar never showed when it was needed).
        inject("dead", "health check failed: HTTP " + r.status);
        return;
      }
      const d = await r.json();
      const status = d.dcentrald_status || (d.alive ? "alive" : "dead");
      if (status === "alive") {
        remove();
      } else {
        // Strip ANSI escape codes from the error message before display.
        let err = d.dcentrald_last_error || "";
        err = err.replace(/\x1b\[[0-9;]*m/g, "").replace(/\x1b\[\dm/g, "");
        // Take the part after "error=" if present
        const m = err.match(/error=([^]+)$/);
        if (m) err = m[1];
        inject(status, err);
      }
    } catch (e) {
      // Network error — show the bar anyway so the user knows something's off.
      inject("dead", "dashboard endpoint unreachable: " + e.message);
    }
  }
  // First check on load, then poll every 5s.
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", check);
  } else {
    check();
  }
  setInterval(check, 5000);
})();
