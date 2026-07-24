// DCENT_axe Wi-Fi ⇄ Ethernet failover FSM — PLAN-E Phase 1
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// PURE logic (no esp-idf imports) so the whole decision matrix is host-tested
// via the dcentaxe-core `#[path]` re-include. The esp-idf side (`eth_w5500.rs`)
// only SAMPLES link state into a `LinkSnapshot`; policy lives here.
//
// Honesty posture / division of labor:
// - The MECHANISM that actually steers outbound traffic is lwIP's default-netif
//   selection by `route_prio` (the Ethernet netif is created with a priority
//   above the Wi-Fi STA's 100 when LAN is preferred, and lwIP re-selects the
//   highest-priority UP netif whenever a link gains/loses its address). Inbound
//   services (httpd/stratum/MCP) bind all netifs, so they are reachable over
//   whichever link is up.
// - This FSM is the OBSERVER/REPORTER: it derives the operator-facing "active
//   link" label from sampled link state and logs transitions. It never claims a
//   link that has no IP, and under `EthOnly` with the cable down it reports
//   `None` rather than pretending the Wi-Fi fallback does not exist (Wi-Fi
//   teardown under EthOnly is a documented follow-up, not shipped behavior).

use crate::config::NetworkMode;

/// Which network link the firmware currently treats as active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ActiveLink {
    /// No usable link for the selected mode (boot state, or cable/AP down).
    #[default]
    None,
    /// Wi-Fi STA is the active link.
    Wifi,
    /// The W5500 wired link is active (link up AND an IP is held).
    Ethernet,
}

impl ActiveLink {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Wifi => "wifi",
            Self::Ethernet => "ethernet",
        }
    }
}

/// One tick's sampled link state. Produced on the esp-idf side; consumed here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkSnapshot {
    /// Wi-Fi STA is associated (supplicant-level `is_connected`).
    pub wifi_up: bool,
    /// W5500 PHY reports carrier (esp_eth link status).
    pub eth_link_up: bool,
    /// The Ethernet netif holds a (DHCP) address.
    pub eth_has_ip: bool,
}

impl LinkSnapshot {
    /// Ethernet counts as usable only with BOTH carrier and an address —
    /// mirrors TNA's "auto-use Ethernet if link + IP" rule; a cable with no
    /// DHCP lease must never be labeled the active link.
    pub fn eth_usable(self) -> bool {
        self.eth_link_up && self.eth_has_ip
    }
}

/// Pure link-selection policy for one snapshot under a given mode.
pub fn desired_link(mode: NetworkMode, snap: LinkSnapshot) -> ActiveLink {
    match mode {
        NetworkMode::WifiOnly => {
            if snap.wifi_up {
                ActiveLink::Wifi
            } else {
                ActiveLink::None
            }
        }
        NetworkMode::EthOnly => {
            if snap.eth_usable() {
                ActiveLink::Ethernet
            } else {
                // Honest label: the preferred (only) link is down. Wi-Fi may
                // still route management traffic this increment, but we do not
                // claim it as the selected link under EthOnly.
                ActiveLink::None
            }
        }
        NetworkMode::EthPreferred => {
            if snap.eth_usable() {
                ActiveLink::Ethernet
            } else if snap.wifi_up {
                ActiveLink::Wifi
            } else {
                ActiveLink::None
            }
        }
    }
}

/// Tracks the active link across main-loop ticks and reports transitions.
/// Only the main loop owns this; pass it by `&mut` into [`FailoverState::tick`]
/// (the same single-owner pattern as `wifi::ReconnectState`).
#[derive(Debug, Default)]
pub struct FailoverState {
    active: ActiveLink,
    transitions: u32,
}

impl FailoverState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The link currently considered active.
    ///
    /// Consumed by the host tests today; in the firmware binary it is RESERVED
    /// for the Phase-1 follow-up that surfaces the active link in
    /// `/api/system/info` + the dashboard Network card (the main loop itself
    /// only needs `tick`'s transition edges for logging) — hence the allow.
    #[allow(dead_code)]
    pub fn active(&self) -> ActiveLink {
        self.active
    }

    /// How many link transitions have occurred since boot (flap telemetry).
    pub fn transitions(&self) -> u32 {
        self.transitions
    }

    /// Evaluate one snapshot. Returns `Some((from, to))` exactly when the
    /// active link changed (the caller logs it); `None` on steady state.
    pub fn tick(
        &mut self,
        mode: NetworkMode,
        snap: LinkSnapshot,
    ) -> Option<(ActiveLink, ActiveLink)> {
        let next = desired_link(mode, snap);
        if next == self.active {
            return None;
        }
        let from = self.active;
        self.active = next;
        self.transitions = self.transitions.saturating_add(1);
        Some((from, next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(wifi_up: bool, eth_link_up: bool, eth_has_ip: bool) -> LinkSnapshot {
        LinkSnapshot {
            wifi_up,
            eth_link_up,
            eth_has_ip,
        }
    }

    // ── Full decision matrix: 3 modes × 8 link states ────────────────────────
    #[test]
    fn desired_link_full_matrix() {
        use ActiveLink::*;
        use NetworkMode::*;
        // (mode, wifi, eth_link, eth_ip) → expected
        let matrix = [
            // WifiOnly: only Wi-Fi ever counts; Ethernet state is ignored.
            (WifiOnly, false, false, false, None),
            (WifiOnly, false, true, true, None),
            (WifiOnly, true, false, false, Wifi),
            (WifiOnly, true, true, true, Wifi),
            // EthOnly: Ethernet needs link AND ip; NEVER falls back to Wi-Fi.
            (EthOnly, true, false, false, None),
            (EthOnly, true, true, false, None), // carrier but no DHCP lease
            (EthOnly, true, false, true, None), // stale ip, cable pulled
            (EthOnly, false, true, true, Ethernet),
            (EthOnly, true, true, true, Ethernet),
            // EthPreferred: LAN-first, Wi-Fi fallback, else none.
            (EthPreferred, false, false, false, None),
            (EthPreferred, true, false, false, Wifi),
            (EthPreferred, true, true, false, Wifi), // eth not usable yet
            (EthPreferred, true, false, true, Wifi),
            (EthPreferred, false, true, true, Ethernet),
            (EthPreferred, true, true, true, Ethernet), // eth WINS over wifi
        ];
        for (mode, w, el, ei, expected) in matrix {
            assert_eq!(
                desired_link(mode, snap(w, el, ei)),
                expected,
                "mode={mode:?} wifi={w} eth_link={el} eth_ip={ei}"
            );
        }
    }

    #[test]
    fn eth_usable_requires_link_and_ip() {
        assert!(!snap(false, true, false).eth_usable());
        assert!(!snap(false, false, true).eth_usable());
        assert!(snap(false, true, true).eth_usable());
    }

    // ── Failover both directions under EthPreferred ──────────────────────────
    #[test]
    fn eth_preferred_fails_over_and_back() {
        let mode = NetworkMode::EthPreferred;
        let mut fsm = FailoverState::new();
        assert_eq!(fsm.active(), ActiveLink::None, "boot state");

        // Wi-Fi comes up first (typical boot order).
        assert_eq!(
            fsm.tick(mode, snap(true, false, false)),
            Some((ActiveLink::None, ActiveLink::Wifi))
        );
        // Cable plugged, DHCP completes → Ethernet takes over.
        assert_eq!(fsm.tick(mode, snap(true, true, false)), None, "no ip yet");
        assert_eq!(
            fsm.tick(mode, snap(true, true, true)),
            Some((ActiveLink::Wifi, ActiveLink::Ethernet))
        );
        // Steady state is quiet.
        assert_eq!(fsm.tick(mode, snap(true, true, true)), None);
        // Cable pulled → back to Wi-Fi.
        assert_eq!(
            fsm.tick(mode, snap(true, false, false)),
            Some((ActiveLink::Ethernet, ActiveLink::Wifi))
        );
        // Wi-Fi also lost → none.
        assert_eq!(
            fsm.tick(mode, snap(false, false, false)),
            Some((ActiveLink::Wifi, ActiveLink::None))
        );
        assert_eq!(fsm.transitions(), 4);
    }

    // ── EthOnly never reports the Wi-Fi fallback as active ───────────────────
    #[test]
    fn eth_only_reports_none_not_wifi_when_cable_down() {
        let mode = NetworkMode::EthOnly;
        let mut fsm = FailoverState::new();
        assert_eq!(
            fsm.tick(mode, snap(true, true, true)),
            Some((ActiveLink::None, ActiveLink::Ethernet))
        );
        assert_eq!(
            fsm.tick(mode, snap(true, false, false)),
            Some((ActiveLink::Ethernet, ActiveLink::None)),
            "EthOnly must not relabel Wi-Fi as the active link"
        );
    }

    // ── Transition counting is monotonic flap telemetry ──────────────────────
    #[test]
    fn transitions_count_flaps() {
        let mode = NetworkMode::EthPreferred;
        let mut fsm = FailoverState::new();
        for i in 0..3 {
            assert!(fsm.tick(mode, snap(false, true, true)).is_some(), "up {i}");
            assert!(
                fsm.tick(mode, snap(false, false, false)).is_some(),
                "down {i}"
            );
        }
        assert_eq!(fsm.transitions(), 6);
    }
}
