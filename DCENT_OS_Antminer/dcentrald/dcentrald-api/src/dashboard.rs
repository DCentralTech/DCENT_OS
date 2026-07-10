//! DCENTos Mining Dashboard route stub.
//!
//! W5.1 (2026-05-07) decoupled the dashboard from the daemon binary. The
//! React SPA is no longer embedded into `dcentrald` via `include_str!`;
//! it ships as a static asset served by `server.py` from
//! `/usr/share/dcentos-dashboard/index.html` on the rootfs overlay.
//!
//! Why decouple:
//! - A 700+ KB HTML blob compiled into the binary forced a full Rust
//!   rebuild + sysupgrade cycle for every dashboard tweak. That cycle is
//!   ~10 minutes; serving the file from the overlay drops dashboard
//!   iteration to a 30-second `scp` (`dev_deploy.sh --dashboard-only`).
//! - `server.py` (S80dashboard) stays up even when `dcentrald` is dead,
//!   which is exactly when the operator needs the diagnostic UI. Embedding
//!   the SPA in the daemon meant the SPA was unreachable during the
//!   crash-loop windows it was designed to surface.
//! - `dcentrald-api` no longer carries a `build.rs` artifact-size gate
//!   for an HTML file that isn't part of its compilation unit.
//!
//! This handler is preserved as a small redirect so any cached operator
//! bookmarks pointing at `dcentrald` :8080 / still land on the live
//! dashboard served by `server.py` on :80.
//!
//! Build the dashboard: `cd DCENT_OS_Antminer/dashboard && npm run build`.
//! Deploy the dashboard only: `bash DCENT_OS_Antminer/scripts/dev_deploy.sh
//! <MINER_IP> --dashboard-only` (no Rust rebuild).
//! Self-detection: the React app fetches `/api/dashboard/version` to
//! confirm the daemon's notion of the dashboard build matches what was
//! served.

use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
};

/// Stub handler for `dcentrald`'s `/`.
///
/// In production the dashboard is served on :80 by `server.py` (which
/// reverse-proxies `/api/*` to `dcentrald` on :8080). Hitting
/// `dcentrald`'s `/` directly on :8080 is almost always a misconfiguration
/// or a stale bookmark. Returning 404 with a plain-text body keeps the
/// response cheap, avoids a redirect loop (`/` on this same host:port
/// would re-enter this handler), and gives operators an actionable hint
/// when they curl the wrong port. See `dcentrald-api/src/dashboard.rs`
/// header comment for the W5.1 decoupling rationale.
pub async fn index_handler() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "dcentrald no longer serves the dashboard SPA on `/`.\n\
         \n\
         The dashboard is served by server.py from \
         /usr/share/dcentos-dashboard/index.html on the platform's HTTP \
         port (:80). dcentrald hosts `/api/*` only.\n\
         \n\
         Self-detection: GET /api/dashboard/version returns the on-disk \
         SHA-256 + build timestamp so the React shell can detect drift.\n\
         \n\
         See DCENT_OS_Antminer/dcentrald/dcentrald-api/src/dashboard.rs \
         for the W5.1 decoupling rationale.\n",
    )
}
