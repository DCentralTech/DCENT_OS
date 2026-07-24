//! Platform subtype detection + voltage-controller classification.
//!
//! W2A.2 / W10 PIC1704 wire-up (2026-05-09); W11.3 marker expansion
//! (RE2 §2.5 + §6.1); W12 T19 expansion (RE3 §2.5 + §5.1):
//! Bitmain BraiinsOS+ and the DCENT_OS dev-kit both ship `/etc/subtype` to
//! distinguish hashboard / control-board pairings. The canonical strings
//! observed in the dev-kit rootfs trees:
//!
//! | `/etc/subtype` value | Carrier      | Hashboard family | Voltage controller |
//! |----------------------|--------------|------------------|--------------------|
//! | `CVCtrl_BHB42XXX`    | CV1835/CV183x| BHB42XXX (S19j Pro / S19 / S19i / S19 XP / **T19**) | PIC1704 |
//! | `BBCtrl_BHB42XXX`    | AM335x BB    | BHB42XXX (S19j Pro) | PIC1704 (this wave) |
//! | `AMLCtrl_BHB42XXX`   | Amlogic A113D| BHB42XXX (S19j Pro AML) | PIC1704 (this wave) |
//! | `AMLCtrl_BHB56xxx`   | Amlogic A113D| BHB569xx (S19k Pro / S21) | dsPIC33EP (existing) |
//! | `AMLCtrl_BHB68xxx`   | Amlogic A113D| BHB68xxx (S21 NoPic) | NoPic |
//!
//! W11.3 marker expansion (2026-05-09): RE2 hardware catalog §6.1 confirms
//! S19, S19i, and S19 XP all use the **same** PIC1704 register map at
//! I²C 0x20 as S19j Pro. RE2 also confirms all CVCtrl-family hashboards
//! share the `CVCtrl_BHB42XXX` `/etc/subtype` string — there is no
//! per-SKU subtype string. The classifier therefore needs no new arms;
//! only the compile-time `Pic1704Authorized` whitelist in
//! `dcentrald_asic::pic1704::service::platforms` grew (Cv1835S19,
//! Cv1835S19i, Cv1835S19XP). Subtype + 0x20 ACK probe still both gate
//! routing —  invariants intact.
//!
//! W12 T19 expansion (2026-05-10): RE3 hardware catalog §2.5 (Antminer
//! T19) lists ONLY Cvitek CV183x as the T19 carrier (no AM335x BB or
//! Amlogic alternates). RE3 §5.1 (PSU table) confirms T19 ships APW12
//! (the SMBus-opcode PSU paired with PIC1704). The T19 hashboard reuses
//! the BM1362-family `CVCtrl_BHB42XXX` subtype family, so the classifier
//! decision table needs no new arms — T19 routes through the existing
//! `CVCtrl_BHB42XXX → Pic1704` branch. Only the compile-time
//! `Pic1704Authorized` whitelist grew (`Cv1835T19`). The two-stage
//! subtype + 0x20 ACK probe still gates routing.
//!
//! S21 Amlogic note: RE2 §2.6 lists S21 with PIC1704 at I²C 0x20, but
//! the live S21 unit at .135 has been proven NoPic (TAS5782M kernel-
//! managed DAC).  root corruption-prevention guarantee #2
//! and  lock S21 to NoPic. The
//! classifier therefore catches *any* unknown `AMLCtrl_*` string in the
//! NoPic catch-all arm — which is the correct conservative posture for
//! S21 SKUs whose subtype string we have not ground-truthed.
//!
//! Source-of-truth files used for the canonical strings:
//! - `DCENT_OS_DEVELOPMENT_KIT_FROMRE1/.../ROOTFS_AM335x/BBCtrl_rootfs/etc/subtype`
//!   → `BBCtrl_BHB42XXX`
//! - `DCENT_OS_DEVELOPMENT_KIT_FROMRE1/.../ROOTFS_CV1835/CVCtrl_rootfs/etc/subtype`
//!   → `CVCtrl_BHB42XXX`
//!
//! The classifier is paired with a runtime `i2cdetect 0x20` ACK probe so
//! a unit whose `/etc/subtype` lies (or is missing entirely) cannot
//! accidentally route into the PIC1704 path. A misclassification on a
//! production am2 / am3-aml unit would attempt to talk PIC1704 protocol
//! to a dsPIC and corrupt MSSP — the probe is the defense in depth.
//!
//! This module is host-safe: on Windows / non-Linux the file read returns
//! `None` and the I²C probe returns `false`, so unit tests run without
//! touching `/dev/`.

use crate::i2c::I2cServiceHandle;
use crate::platform::config::VoltageControllerKind;

/// Opaque authority to construct a voltage-controller service at one
/// discovery-bound I2C endpoint.
///
/// The family, bus and address are readable for diagnostics, but the fields
/// and constructor are private. A caller therefore cannot turn a config file,
/// model guess, or arbitrary address into permission to issue energizing wire
/// commands. Obtain this value only through
/// [`discover_system_voltage_controller_endpoint`], which reads the system
/// subtype and performs the required non-payload presence probe itself.
///
/// ```compile_fail
/// use dcentrald_hal::platform::{VoltageControllerEndpoint, VoltageControllerKind};
///
/// // Private fields prevent caller-asserted protocol/address capabilities.
/// let _forged = VoltageControllerEndpoint {
///     kind: VoltageControllerKind::Dspic33Ep,
///     bus: 0,
///     address: 0x20,
/// };
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct VoltageControllerEndpoint {
    kind: VoltageControllerKind,
    bus: u8,
    address: u8,
    observed_firmware: Option<u8>,
}

impl VoltageControllerEndpoint {
    /// Confirmed protocol family. This is observational metadata, not a
    /// constructor input.
    pub fn kind(&self) -> VoltageControllerKind {
        self.kind
    }

    /// I2C bus on which the family-presence observation was made.
    pub fn bus(&self) -> u8 {
        self.bus
    }

    /// Topology-validated controller address bound to this capability.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Firmware byte observed while issuing this endpoint, when the discovery
    /// route performed a protocol-specific version transaction. `None` means
    /// family/address presence was proven without firmware-revision evidence.
    pub fn observed_firmware(&self) -> Option<u8> {
        self.observed_firmware
    }

    pub(super) fn from_observed_am2(address: u8, firmware: u8) -> Self {
        Self {
            kind: VoltageControllerKind::Dspic33Ep,
            bus: 0,
            address,
            observed_firmware: Some(firmware),
        }
    }

    #[cfg(feature = "sim-hal")]
    pub(in crate::platform) fn from_simulated_pic16(bus: u8, address: u8) -> Self {
        Self {
            kind: VoltageControllerKind::Pic16f1704,
            bus,
            address,
            observed_firmware: None,
        }
    }
}

/// Why system discovery could not issue an endpoint capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoltageControllerEndpointError {
    /// Discovery was unknown, contradictory, or positively found no
    /// controller. These states can never be projected into authority.
    DiscoveryNotConfirmed(VoltageControllerDiscoveryStatus),
    /// The exact family is known, but this discovery route does not yet have a
    /// presence-validated topology from which an endpoint can be issued.
    TopologyNotPresenceValidated(VoltageControllerKind),
    /// The caller-selected bus is not the bus pinned by the exact carrier +
    /// hashboard identity table.
    BusOutsideTopology {
        kind: VoltageControllerKind,
        bus: u8,
        expected_bus: u8,
    },
    /// The requested address is outside the documented topology for the
    /// confirmed family.
    AddressOutsideTopology {
        kind: VoltageControllerKind,
        address: u8,
    },
    /// The exact requested endpoint did not ACK the non-payload probe. A
    /// family anchor elsewhere on the bus is not enough.
    EndpointPresenceNotObserved { bus: u8, address: u8 },
}

impl std::fmt::Display for VoltageControllerEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DiscoveryNotConfirmed(status) => {
                write!(
                    f,
                    "voltage-controller discovery is not confirmed: {status:?}"
                )
            }
            Self::TopologyNotPresenceValidated(kind) => write!(
                f,
                "voltage-controller topology is not presence-validated for {}",
                kind.as_str()
            ),
            Self::BusOutsideTopology {
                kind,
                bus,
                expected_bus,
            } => write!(
                f,
                "I2C bus {bus} is outside the confirmed {} topology (expected bus {expected_bus})",
                kind.as_str()
            ),
            Self::AddressOutsideTopology { kind, address } => write!(
                f,
                "I2C address 0x{address:02X} is outside the confirmed {} topology",
                kind.as_str()
            ),
            Self::EndpointPresenceNotObserved { bus, address } => write!(
                f,
                "no voltage-controller presence observed on I2C bus {bus} address 0x{address:02X}"
            ),
        }
    }
}

impl std::error::Error for VoltageControllerEndpointError {}

/// Evidence-level outcome of voltage-controller discovery.
///
/// This is intentionally not a capability: callers cannot construct a bound
/// I2C endpoint from it. The follow-up endpoint API will consume the private
/// evidence held by [`VoltageControllerDiscovery`] inside the HAL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoltageControllerDiscoveryStatus {
    /// Exact platform evidence and any required presence probe agree.
    Confirmed(VoltageControllerKind),
    /// Exact, documented hardware identity positively specifies no controller.
    NoController,
    /// Evidence is missing or not in the versioned identity table.
    Unknown,
    /// Two observations disagree; no controller protocol may be selected.
    Contradictory,
}

/// Read-only discovery result. Fields and construction remain private so this
/// cannot become a caller-asserted family capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoltageControllerDiscovery {
    status: VoltageControllerDiscoveryStatus,
    detail: &'static str,
}

impl VoltageControllerDiscovery {
    pub fn status(&self) -> VoltageControllerDiscoveryStatus {
        self.status
    }

    /// True only when the HAL has enough agreeing evidence to bind a concrete
    /// controller protocol. Unknown, contradictory and known-NoController
    /// outcomes are always non-energizing.
    pub fn energization_eligible(&self) -> bool {
        matches!(
            self.status,
            VoltageControllerDiscoveryStatus::Confirmed(
                VoltageControllerKind::Pic1704
                    | VoltageControllerKind::Dspic33Ep
                    | VoltageControllerKind::Pic16f1704
            )
        )
    }

    pub fn detail(&self) -> &'static str {
        self.detail
    }

    /// Non-breaking projection for existing platform configuration. All
    /// unresolved outcomes map to `NoPic`, which is the only compatibility
    /// value that cannot select PIC/dsPIC wire bytes.
    pub fn compatibility_kind(&self) -> VoltageControllerKind {
        match self.status {
            VoltageControllerDiscoveryStatus::Confirmed(kind) => kind,
            VoltageControllerDiscoveryStatus::NoController
            | VoltageControllerDiscoveryStatus::Unknown
            | VoltageControllerDiscoveryStatus::Contradictory => VoltageControllerKind::NoPic,
        }
    }

    fn confirmed(kind: VoltageControllerKind, detail: &'static str) -> Self {
        Self {
            status: VoltageControllerDiscoveryStatus::Confirmed(kind),
            detail,
        }
    }

    fn no_controller(detail: &'static str) -> Self {
        Self {
            status: VoltageControllerDiscoveryStatus::NoController,
            detail,
        }
    }

    fn unknown(detail: &'static str) -> Self {
        Self {
            status: VoltageControllerDiscoveryStatus::Unknown,
            detail,
        }
    }

    fn contradictory(detail: &'static str) -> Self {
        Self {
            status: VoltageControllerDiscoveryStatus::Contradictory,
            detail,
        }
    }
}

/// Canonical `/etc/subtype` path on Linux. Static for ground-truthing.
pub const SUBTYPE_PATH: &str = "/etc/subtype";

/// Canonical runtime board identity written by DCENT_OS images.
const BOARD_TARGET_PATH: &str = "/etc/dcentos/board_target";

fn read_board_target() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(BOARD_TARGET_PATH)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Read `/etc/subtype` and return its trimmed contents.
///
/// On non-Linux hosts (e.g. Windows test harness), returns `None` so
/// tests can run without touching the filesystem.
pub fn read_subtype() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(SUBTYPE_PATH) {
            Ok(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    tracing::warn!(
                        path = SUBTYPE_PATH,
                        "platform subtype: file present but empty"
                    );
                    None
                } else {
                    tracing::info!(
                        path = SUBTYPE_PATH,
                        subtype = %trimmed,
                        "platform subtype: read"
                    );
                    Some(trimmed.to_string())
                }
            }
            Err(e) => {
                tracing::debug!(
                    path = SUBTYPE_PATH,
                    error = %e,
                    "platform subtype: not present (legacy / non-Bitmain firmware)"
                );
                None
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Host (Windows/macOS) test path — no /etc/subtype.
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubtypeExpectation {
    ControllerAt0x20(VoltageControllerKind),
    ControllerElsewhere(VoltageControllerKind),
    NoController,
    Unknown,
}

fn subtype_expectation(subtype: Option<&str>) -> SubtypeExpectation {
    let Some(subtype) = subtype.map(str::trim).filter(|value| !value.is_empty()) else {
        return SubtypeExpectation::Unknown;
    };
    let upper = subtype.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CVCTRL_BHB42XXX" | "BBCTRL_BHB42XXX" | "AMLCTRL_BHB42XXX"
    ) {
        return SubtypeExpectation::ControllerAt0x20(VoltageControllerKind::Pic1704);
    }
    if upper.starts_with("AMLCTRL_BHB56") {
        return SubtypeExpectation::ControllerAt0x20(VoltageControllerKind::Dspic33Ep);
    }
    if upper.starts_with("AMLCTRL_BHB68") {
        return SubtypeExpectation::NoController;
    }
    if matches!(upper.as_str(), "S9" | "S9J" | "S9K" | "S9_BHB09001") {
        return SubtypeExpectation::ControllerElsewhere(VoltageControllerKind::Pic16f1704);
    }
    SubtypeExpectation::Unknown
}

fn presence_validated_topology_bus(subtype: Option<&str>) -> Option<u8> {
    // Every identity currently represented by ControllerAt0x20 is documented
    // on hashboard bus 0 across CV1835, AM335x BB, and Amlogic carriers. Keep
    // this next to the identity table so a future carrier must add its bus
    // explicitly instead of inheriting a caller-selected value.
    matches!(
        subtype_expectation(subtype),
        SubtypeExpectation::ControllerAt0x20(_)
    )
    .then_some(0)
}

fn discover_from_observations(
    subtype: Option<&str>,
    address_0x20_acked: bool,
) -> VoltageControllerDiscovery {
    match subtype_expectation(subtype) {
        SubtypeExpectation::ControllerAt0x20(kind) if address_0x20_acked => {
            VoltageControllerDiscovery::confirmed(
                kind,
                "exact subtype and address-0x20 presence evidence agree",
            )
        }
        SubtypeExpectation::ControllerAt0x20(_) => VoltageControllerDiscovery::contradictory(
            "exact subtype requires a controller at address 0x20 but the presence probe failed",
        ),
        SubtypeExpectation::ControllerElsewhere(kind) => VoltageControllerDiscovery::confirmed(
            kind,
            "exact subtype identifies a controller family outside the 0x20 topology",
        ),
        SubtypeExpectation::NoController if address_0x20_acked => {
            VoltageControllerDiscovery::contradictory(
                "exact NoController subtype conflicts with a device ACK at address 0x20",
            )
        }
        SubtypeExpectation::NoController => VoltageControllerDiscovery::no_controller(
            "exact subtype identifies a documented NoController hashboard family",
        ),
        SubtypeExpectation::Unknown if address_0x20_acked => VoltageControllerDiscovery::unknown(
            "a device ACKed at address 0x20 but no evidence identifies its protocol family",
        ),
        SubtypeExpectation::Unknown => {
            VoltageControllerDiscovery::unknown("subtype evidence is missing or unrecognized")
        }
    }
}

/// Classify a `/etc/subtype` string into an informational compatibility kind.
///
/// This pure helper performs no presence validation and grants no energization
/// authority. New platform code should use [`discover_voltage_controller`].
///
/// Pure function: no I/O. Logging side-effects only — `tracing::debug!` for a
/// matched arm (and for an absent `/etc/subtype`, which is normal on some
/// units), and `tracing::warn!` for a present-but-unrecognized subtype string
/// (visible in a beta, conservative default preserved). Safe to call from unit
/// tests.
///
/// Decision table:
/// - `*Ctrl_BHB42XXX` (CV / BB / AML) → `Pic1704`
/// - `AMLCtrl_BHB56xxx` (S19k Pro / S21 with framed dsPIC) → `Dspic33Ep`
/// - other `AML*` strings (catch-all for S21 NoPic variants) → `NoPic`
/// - `S9` / Bitmain stock S9 → `Pic16f1704`
/// - missing → `NoPic` compatibility result (identity unknown; non-energizing)
/// - present but unknown → `NoPic` (fail closed: issue no PIC/dsPIC voltage commands)
#[cfg(test)]
pub(crate) fn classify_voltage_controller(subtype: Option<&str>) -> VoltageControllerKind {
    let s = match subtype {
        Some(s) => s,
        None => {
            tracing::warn!("subtype: absent → NoPic compatibility result (identity unknown)");
            return VoltageControllerKind::NoPic;
        }
    };

    // Pre-compute lower-case-or-as-is for case-tolerant matching of the
    // BHB-family suffix. The `Ctrl_` prefix is already case-stable across
    // the dev-kit rootfs tree.
    let upper = s.to_ascii_uppercase();

    // BHB42XXX hashboard family — PIC1704 across all 3 carriers.
    if upper.contains("BHB42XXX")
        && (upper.starts_with("CVCTRL_")
            || upper.starts_with("BBCTRL_")
            || upper.starts_with("AMLCTRL_"))
    {
        tracing::debug!(subtype = %s, "subtype: BHB42XXX → Pic1704");
        return VoltageControllerKind::Pic1704;
    }

    // BHB56xxx hashboard family on Amlogic — framed dsPIC33EP path
    // (S19k Pro at .78, S21 framed-dsPIC variants).
    if upper.starts_with("AMLCTRL_") && upper.contains("BHB56") {
        tracing::debug!(subtype = %s, "subtype: AMLCtrl_BHB56xxx → Dspic33Ep");
        return VoltageControllerKind::Dspic33Ep;
    }

    // Catch-all for other Amlogic carrier strings — treat as S21-class
    // NoPic until a more specific subtype lands. The historical S21 unit
    // at .135 boots without a `/etc/subtype` at all, so this branch is
    // mostly future-proofing for fresh BraiinsOS+ images.
    if upper.starts_with("AMLCTRL_") {
        tracing::debug!(subtype = %s, "subtype: AMLCtrl_* (non-BHB42/56) → NoPic");
        return VoltageControllerKind::NoPic;
    }

    // S9 stock Bitmain (`S9`, `S9j`, `S9k`, …) — PIC16F1704.
    if upper.starts_with("S9") || upper == "S9_BHB09001" {
        tracing::debug!(subtype = %s, "subtype: S9 family → Pic16f1704");
        return VoltageControllerKind::Pic16f1704;
    }

    // A subtype string was present but matched no known family. Fail closed to
    // NoPic so a fresh, missing, or mistyped controller marker never silently
    // opts into a PIC/dsPIC voltage command family.
    tracing::warn!(
        subtype = %s,
        "subtype: UNKNOWN string → NoPic fail-closed \
         (no PIC/dsPIC voltage commands; report this subtype so it can be classified)"
    );
    VoltageControllerKind::NoPic
}

/// Probe `/dev/i2c-{bus}` for an ACK at I²C address 0x20.
///
/// Mirrors `i2cdetect -y -q {bus} 0x20 0x20`: opens the bus, performs a
/// zero-byte write, returns true if the slave ACK'd. Used as the runtime
/// gate: even when `/etc/subtype` says PIC1704 is present, the daemon
/// will not construct a `Pic1704Service` until this returns true.
///
/// On non-Linux hosts (test harness), returns `false`.
///
/// Note on safety: this MUST go through the in-crate `I2cBus::open` path
/// rather than a raw `/dev/i2c-N` open, because production am2 / am3-aml
/// builds enforce the SINGLE-I2C-OWNER architecture. The `pub(crate)`
/// `I2cBus::open` is reachable from this module because `subtype.rs`
/// lives inside the `dcentrald-hal` crate. The probe is a one-shot
/// performed BEFORE the long-running I²C service is spawned.
pub fn probe_pic1704_at_0x20(bus: u8) -> bool {
    #[cfg(target_os = "linux")]
    {
        const PIC1704_PROBE_ADDR: u8 = 0x20;
        let mut i2c = match crate::i2c::I2cBus::open(bus) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    bus,
                    error = %e,
                    "PIC1704 probe: /dev/i2c-{} open failed → false", bus
                );
                return false;
            }
        };
        if let Err(e) = i2c.set_slave(PIC1704_PROBE_ADDR) {
            tracing::warn!(
                bus,
                addr = format_args!("0x{:02X}", PIC1704_PROBE_ADDR),
                error = %e,
                "PIC1704 probe: set_slave failed → false"
            );
            return false;
        }
        // Zero-byte write to test ACK. `i2cdetect` uses SMBus quick-write
        // for safe-list addresses; 0x20 is on its read list, so writing
        // an empty byte slice issues a START + addr-w + STOP only — no
        // payload byte is sent. This matches the safe `i2cdetect`
        // convention and never modifies a slave register.
        match i2c.write(&[]) {
            Ok(_) => {
                tracing::info!(
                    bus,
                    addr = format_args!("0x{:02X}", PIC1704_PROBE_ADDR),
                    "PIC1704 probe: ACK observed → PIC1704 candidate confirmed"
                );
                true
            }
            Err(e) => {
                tracing::info!(
                    bus,
                    addr = format_args!("0x{:02X}", PIC1704_PROBE_ADDR),
                    error = %e,
                    "PIC1704 probe: NACK / write error → PIC1704 not present"
                );
                false
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = bus;
        false
    }
}

/// Non-payload ACK probe for an already identity-validated controller
/// address. This is private so callers cannot use an ACK alone to select a
/// protocol family.
fn probe_voltage_controller_address(bus: u8, address: u8) -> bool {
    #[cfg(target_os = "linux")]
    {
        let mut i2c = match crate::i2c::I2cBus::open(bus) {
            Ok(i2c) => i2c,
            Err(error) => {
                tracing::warn!(bus, %error, "controller endpoint probe could not open I2C bus");
                return false;
            }
        };
        if let Err(error) = i2c.set_slave(address) {
            tracing::warn!(
                bus,
                address = format_args!("0x{address:02X}"),
                %error,
                "controller endpoint probe could not select address"
            );
            return false;
        }
        match i2c.write(&[]) {
            Ok(_) => true,
            Err(error) => {
                tracing::info!(
                    bus,
                    address = format_args!("0x{address:02X}"),
                    %error,
                    "controller endpoint did not ACK non-payload probe"
                );
                false
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (bus, address);
        false
    }
}

/// Classify from a DCENT_OS *board-target* name / PSU-kind string when
/// there is no `/etc/subtype` to read (Phase B, am3-bb on
/// `S19J_IO_BOARD_V2_0`).
///
/// LuxOS units do not ship `/etc/subtype` — the BeagleBone platform reads
/// `/etc/dcentos/board_target` instead and (on `am3-bb-s19jpro`) loads a
/// per-board-target TOML whose `[platform].voltage_controller` can declare
/// `"dspic33ep-fw89"` and whose legacy `[i2c].psu_kind` still declares
/// `"apw12-uart-tunnel"`. The APW121215f PSU is upstream on bus 1, but the
/// 2026-05-13 LuxOS ftrace proves fw=0x89 dsPIC33EP controllers at
/// 0x20/0x21/0x22 on hashboard bus 0. So both the explicit dsPIC string and
/// the legacy APW tunnel string classify to [`VoltageControllerKind::Dspic33Ep`].
///
/// Returns `None` for any string this helper doesn't recognize, so callers
/// can fall back to their static default.
///
/// # Why NOT a new `VoltageControllerKind` variant
///
/// A dedicated `Apw12UartTunnel` variant would mix PSU transport with
/// hashboard voltage-controller identity. The reusable controller identity is
/// still `Dspic33Ep`; the AM3 BB mining path owns the LuxOS-trace fw=0x89
/// sequence and heartbeat timing.
pub(crate) fn discover_from_board_target(s: &str) -> VoltageControllerDiscovery {
    let lower = s.trim().to_ascii_lowercase();
    match lower.as_str() {
        "dspic33ep" | "dspic33ep-fw89" | "dspic33ep16gs202" | "dspic-fw89" => {
            tracing::info!(
                board_target_voltage_controller = %s,
                "board-target classification: explicit dsPIC33EP voltage controller",
            );
            VoltageControllerDiscovery::confirmed(
                VoltageControllerKind::Dspic33Ep,
                "exact board-target voltage-controller identity",
            )
        }
        // Legacy AM3 BB board-targets used the PSU transport string as the
        // voltage-controller hint. Preserve compatibility, but route to the
        // dsPIC path now that the live LuxOS trace proved the controllers.
        "apw12-uart-tunnel" | "apw12_uart_tunnel" | "apw-uart-tunnel" => {
            tracing::info!(
                board_target_psu_kind = %s,
                "board-target classification: apw12-uart-tunnel → Dspic33Ep \
                 (am3-bb S19J_IO_BOARD_V2_0; APW12 upstream PSU plus hashboard fw=0x89 dsPICs)"
            );
            VoltageControllerDiscovery::confirmed(
                VoltageControllerKind::Dspic33Ep,
                "exact legacy board-target plus retained hardware-trace identity",
            )
        }
        _ => {
            tracing::debug!(
                board_target_psu_kind = %s,
                "board-target classification: unrecognized PSU-kind string → no override"
            );
            VoltageControllerDiscovery::unknown("board-target evidence is unrecognized")
        }
    }
}

/// Compatibility projection for existing BeagleBone configuration code.
pub(crate) fn classify_from_board_target(s: &str) -> Option<VoltageControllerKind> {
    let discovery = discover_from_board_target(s);
    match discovery.status() {
        VoltageControllerDiscoveryStatus::Confirmed(kind) => Some(kind),
        VoltageControllerDiscoveryStatus::NoController
        | VoltageControllerDiscoveryStatus::Unknown
        | VoltageControllerDiscoveryStatus::Contradictory => None,
    }
}

/// Discover a voltage controller from exact subtype evidence plus a
/// non-payload presence observation. ACK is corroboration only: without an
/// exact identity-table match it never selects a protocol family.
pub(crate) fn discover_voltage_controller(
    subtype: Option<&str>,
    i2c_bus: u8,
) -> VoltageControllerDiscovery {
    discover_voltage_controller_with_probe(subtype, || probe_pic1704_at_0x20(i2c_bus))
}

fn bind_presence_validated_endpoint<F>(
    subtype: Option<&str>,
    discovery: VoltageControllerDiscovery,
    i2c_bus: u8,
    address: u8,
    probe_exact_address: F,
) -> Result<VoltageControllerEndpoint, VoltageControllerEndpointError>
where
    F: FnOnce() -> bool,
{
    let kind = match discovery.status() {
        VoltageControllerDiscoveryStatus::Confirmed(kind) => kind,
        status @ (VoltageControllerDiscoveryStatus::NoController
        | VoltageControllerDiscoveryStatus::Unknown
        | VoltageControllerDiscoveryStatus::Contradictory) => {
            return Err(VoltageControllerEndpointError::DiscoveryNotConfirmed(
                status,
            ));
        }
    };

    // Only ControllerAt0x20 identities ran a presence probe. S9's exact text
    // identity remains useful discovery evidence, but cannot yet issue an
    // endpoint because its 0x55-0x57 topology was not probed by this route.
    if !matches!(
        subtype_expectation(subtype),
        SubtypeExpectation::ControllerAt0x20(expected) if expected == kind
    ) {
        return Err(VoltageControllerEndpointError::TopologyNotPresenceValidated(kind));
    }
    let expected_bus = presence_validated_topology_bus(subtype)
        .ok_or(VoltageControllerEndpointError::TopologyNotPresenceValidated(kind))?;
    if i2c_bus != expected_bus {
        return Err(VoltageControllerEndpointError::BusOutsideTopology {
            kind,
            bus: i2c_bus,
            expected_bus,
        });
    }

    let address_is_valid = match kind {
        // BHB42 controllers are selected per hashboard bus at the common 0x20
        // endpoint. Do not generalize the dsPIC multi-address topology to PIC.
        VoltageControllerKind::Pic1704 => address == 0x20,
        // Live S19-family evidence pins dsPIC board endpoints to 0x20-0x22;
        // the 0x20 anchor probe establishes family presence on this bus.
        VoltageControllerKind::Dspic33Ep => (0x20..=0x22).contains(&address),
        VoltageControllerKind::Pic16f1704 | VoltageControllerKind::NoPic => false,
    };
    if !address_is_valid {
        return Err(VoltageControllerEndpointError::AddressOutsideTopology { kind, address });
    }
    if !probe_exact_address() {
        return Err(
            VoltageControllerEndpointError::EndpointPresenceNotObserved {
                bus: i2c_bus,
                address,
            },
        );
    }

    Ok(VoltageControllerEndpoint {
        kind,
        bus: i2c_bus,
        address,
        observed_firmware: None,
    })
}

/// Discover and bind one controller endpoint from system-owned evidence.
///
/// This is deliberately the only public endpoint issuer. It reads
/// `/etc/subtype` internally and performs the safe address-0x20 presence
/// probe, so callers cannot inject identity text or an ACK result. The
/// requested address is accepted only when it belongs to the exact confirmed
/// family topology. Unknown, contradictory and NoController outcomes fail.
pub fn discover_system_voltage_controller_endpoint(
    i2c_bus: u8,
    address: u8,
) -> Result<VoltageControllerEndpoint, VoltageControllerEndpointError> {
    let subtype = read_subtype();
    if let SubtypeExpectation::ControllerAt0x20(kind) = subtype_expectation(subtype.as_deref()) {
        let expected_bus = presence_validated_topology_bus(subtype.as_deref())
            .ok_or(VoltageControllerEndpointError::TopologyNotPresenceValidated(kind))?;
        if i2c_bus != expected_bus {
            return Err(VoltageControllerEndpointError::BusOutsideTopology {
                kind,
                bus: i2c_bus,
                expected_bus,
            });
        }
    }
    let discovery = discover_voltage_controller(subtype.as_deref(), i2c_bus);
    bind_presence_validated_endpoint(subtype.as_deref(), discovery, i2c_bus, address, || {
        // Confirmation already includes the 0x20 ACK. Other documented dsPIC
        // addresses must independently ACK before their capability is issued.
        address == 0x20 || probe_voltage_controller_address(i2c_bus, address)
    })
}

fn standard_pic16_discovery(
    board_target: Option<&str>,
    subtype: Option<&str>,
) -> VoltageControllerDiscovery {
    let exact_board_target = board_target
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case("am1-s9"));
    if !exact_board_target {
        return VoltageControllerDiscovery::unknown(
            "exact DCENT_OS am1-s9 board target is missing",
        );
    }

    match subtype.map(str::trim).filter(|value| !value.is_empty()) {
        None => VoltageControllerDiscovery::confirmed(
            VoltageControllerKind::Pic16f1704,
            "exact am1-s9 board target identifies the standard PIC16 topology",
        ),
        Some(value)
            if matches!(
                subtype_expectation(Some(value)),
                SubtypeExpectation::ControllerElsewhere(VoltageControllerKind::Pic16f1704)
            ) =>
        {
            VoltageControllerDiscovery::confirmed(
                VoltageControllerKind::Pic16f1704,
                "exact am1-s9 board target and S9 subtype agree",
            )
        }
        Some(_) => VoltageControllerDiscovery::contradictory(
            "exact am1-s9 board target conflicts with the observed subtype",
        ),
    }
}

fn bind_standard_pic16_endpoint(
    board_target: Option<&str>,
    subtype: Option<&str>,
    bus: u8,
    address: u8,
) -> Result<VoltageControllerEndpoint, VoltageControllerEndpointError> {
    let discovery = standard_pic16_discovery(board_target, subtype);
    let kind = match discovery.status() {
        VoltageControllerDiscoveryStatus::Confirmed(VoltageControllerKind::Pic16f1704) => {
            VoltageControllerKind::Pic16f1704
        }
        status => {
            return Err(VoltageControllerEndpointError::DiscoveryNotConfirmed(
                status,
            ));
        }
    };
    if bus != 0 {
        return Err(VoltageControllerEndpointError::BusOutsideTopology {
            kind,
            bus,
            expected_bus: 0,
        });
    }
    if !(0x55..=0x57).contains(&address) {
        return Err(VoltageControllerEndpointError::AddressOutsideTopology { kind, address });
    }
    Ok(VoltageControllerEndpoint {
        kind,
        bus,
        address,
        observed_firmware: None,
    })
}

/// Bind one standard-daemon S9 PIC16 topology capability without mutating the
/// controller before cold-boot admission observes its raw state.
///
/// This issuer is deliberately narrower than the subtype/ACK issuer above.
/// It independently reads the exact DCENT_OS `am1-s9` board target, rejects a
/// contradictory `/etc/subtype`, pins bus 0 and addresses 0x55..=0x57, and only
/// returns a topology capability only after the standard daemon has completed
/// cooling and positive slot-presence admission. Controller presence and
/// application eligibility are proven later by the atomic cold-boot request;
/// this issuer deliberately performs no I2C transaction.
pub fn discover_system_pic16_endpoint(
    service: &I2cServiceHandle,
    address: u8,
) -> Result<VoltageControllerEndpoint, VoltageControllerEndpointError> {
    let board_target = read_board_target();
    let subtype = read_subtype();
    let discovery = standard_pic16_discovery(board_target.as_deref(), subtype.as_deref());
    if !matches!(
        discovery.status(),
        VoltageControllerDiscoveryStatus::Confirmed(VoltageControllerKind::Pic16f1704)
    ) {
        return Err(VoltageControllerEndpointError::DiscoveryNotConfirmed(
            discovery.status(),
        ));
    }
    if service.bus() != 0 {
        return Err(VoltageControllerEndpointError::BusOutsideTopology {
            kind: VoltageControllerKind::Pic16f1704,
            bus: service.bus(),
            expected_bus: 0,
        });
    }
    if !(0x55..=0x57).contains(&address) {
        return Err(VoltageControllerEndpointError::AddressOutsideTopology {
            kind: VoltageControllerKind::Pic16f1704,
            address,
        });
    }

    bind_standard_pic16_endpoint(
        board_target.as_deref(),
        subtype.as_deref(),
        service.bus(),
        address,
    )
}

fn discover_voltage_controller_with_probe<F>(
    subtype: Option<&str>,
    probe_address_0x20: F,
) -> VoltageControllerDiscovery
where
    F: FnOnce() -> bool,
{
    let address_0x20_acked = match subtype_expectation(subtype) {
        SubtypeExpectation::ControllerAt0x20(_) => probe_address_0x20(),
        SubtypeExpectation::ControllerElsewhere(_)
        | SubtypeExpectation::NoController
        | SubtypeExpectation::Unknown => false,
    };
    let discovery = discover_from_observations(subtype, address_0x20_acked);
    tracing::info!(
        subtype = %subtype.unwrap_or("<missing>"),
        status = ?discovery.status(),
        energization_eligible = discovery.energization_eligible(),
        detail = discovery.detail(),
        "voltage-controller evidence evaluated"
    );
    discovery
}

/// Non-breaking compatibility projection for existing platform configuration.
/// Unknown, contradictory and positively NoController outcomes all map to
/// `NoPic`; this wrapper never guesses another controller family.
pub(crate) fn classify_with_probe(subtype: Option<&str>, i2c_bus: u8) -> VoltageControllerKind {
    discover_voltage_controller(subtype, i2c_bus).compatibility_kind()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_voltage_controller_full_table() {
        // Canonical PIC1704 carriers (BHB42XXX hashboard family).
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("BBCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );

        // Existing dsPIC33EP path (S19k Pro / S21 framed-dsPIC).
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB56902")),
            VoltageControllerKind::Dspic33Ep,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB56999")),
            VoltageControllerKind::Dspic33Ep,
        );

        // Other Amlogic strings → NoPic catch-all.
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB68900")),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_S21Pro")),
            VoltageControllerKind::NoPic,
        );

        // S9 family.
        assert_eq!(
            classify_voltage_controller(Some("S9")),
            VoltageControllerKind::Pic16f1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("S9_BHB09001")),
            VoltageControllerKind::Pic16f1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("S9j")),
            VoltageControllerKind::Pic16f1704,
        );

        // Missing and present-unknown subtype evidence both fail closed.
        assert_eq!(
            classify_voltage_controller(None),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_voltage_controller(Some("")),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_voltage_controller(Some("some-unknown-string")),
            VoltageControllerKind::NoPic,
        );
    }

    #[test]
    fn unknown_subtype_string_takes_nopic_fail_closed_default() {
        // Present-but-unrecognized subtype is visible at `warn!` and must
        // fail closed to NoPic so a new controller marker never silently routes
        // into a PIC/dsPIC voltage command family. Missing subtype is tested
        // separately because old images may not ship `/etc/subtype`.
        for unknown in ["totally-bogus", "XYZCtrl_BHB99999", "bhb42xxx-no-prefix"] {
            assert_eq!(
                classify_voltage_controller(Some(unknown)),
                VoltageControllerKind::NoPic,
                "unknown subtype {unknown:?} must fail closed to NoPic",
            );
        }
    }

    #[test]
    fn evidence_table_is_fail_closed_and_ack_never_selects_a_family_alone() {
        let cases = [
            (
                Some("CVCtrl_BHB42XXX"),
                true,
                VoltageControllerDiscoveryStatus::Confirmed(VoltageControllerKind::Pic1704),
                true,
            ),
            (
                Some("CVCtrl_BHB42XXX"),
                false,
                VoltageControllerDiscoveryStatus::Contradictory,
                false,
            ),
            (
                Some("AMLCtrl_BHB56902"),
                true,
                VoltageControllerDiscoveryStatus::Confirmed(VoltageControllerKind::Dspic33Ep),
                true,
            ),
            (
                Some("AMLCtrl_BHB56902"),
                false,
                VoltageControllerDiscoveryStatus::Contradictory,
                false,
            ),
            (
                Some("AMLCtrl_BHB68900"),
                false,
                VoltageControllerDiscoveryStatus::NoController,
                false,
            ),
            (
                Some("AMLCtrl_BHB68900"),
                true,
                VoltageControllerDiscoveryStatus::Contradictory,
                false,
            ),
            (
                None,
                false,
                VoltageControllerDiscoveryStatus::Unknown,
                false,
            ),
            (None, true, VoltageControllerDiscoveryStatus::Unknown, false),
            (
                Some("FutureCtrl_0x20"),
                true,
                VoltageControllerDiscoveryStatus::Unknown,
                false,
            ),
        ];

        for (subtype, acked, expected_status, eligible) in cases {
            let discovery = discover_from_observations(subtype, acked);
            assert_eq!(discovery.status(), expected_status, "subtype={subtype:?}");
            assert_eq!(
                discovery.energization_eligible(),
                eligible,
                "subtype={subtype:?}"
            );
            if !eligible {
                assert_eq!(discovery.compatibility_kind(), VoltageControllerKind::NoPic);
            }
        }
    }

    #[test]
    fn arbitrary_unknown_strings_never_become_energization_eligible() {
        for index in 0..512u16 {
            let subtype = format!("FUTURE_UNKNOWN_CONTROLLER_{index:04X}");
            for acked in [false, true] {
                let discovery = discover_from_observations(Some(&subtype), acked);
                assert_eq!(
                    discovery.status(),
                    VoltageControllerDiscoveryStatus::Unknown
                );
                assert!(!discovery.energization_eligible());
                assert_eq!(discovery.compatibility_kind(), VoltageControllerKind::NoPic);
            }
        }
    }

    #[test]
    fn only_exact_address_0x20_identities_invoke_the_presence_probe() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        for subtype in [
            None,
            Some(""),
            Some("unknown"),
            Some("S9"),
            Some("AMLCtrl_BHB68900"),
            Some("AMLCtrl_S21Pro"),
        ] {
            let calls = AtomicUsize::new(0);
            let _ = discover_voltage_controller_with_probe(subtype, || {
                calls.fetch_add(1, Ordering::SeqCst);
                true
            });
            assert_eq!(calls.load(Ordering::SeqCst), 0, "subtype={subtype:?}");
        }

        for subtype in ["CVCtrl_BHB42XXX", "AMLCtrl_BHB56902"] {
            let calls = AtomicUsize::new(0);
            let _ = discover_voltage_controller_with_probe(Some(subtype), || {
                calls.fetch_add(1, Ordering::SeqCst);
                true
            });
            assert_eq!(calls.load(Ordering::SeqCst), 1, "subtype={subtype:?}");
        }
    }

    #[test]
    fn text_evidence_entry_points_are_not_public_authority_surfaces() {
        let source = include_str!("subtype.rs");
        for signature in [
            "pub(crate) fn classify_voltage_controller(",
            "pub(crate) fn discover_voltage_controller(",
            "pub(crate) fn classify_with_probe(",
            "pub(crate) fn discover_from_board_target(",
            "pub(crate) fn classify_from_board_target(",
        ] {
            assert!(
                source
                    .lines()
                    .any(|line| line.trim_start().starts_with(signature)),
                "crate-private discovery signature disappeared: {signature}"
            );
        }
    }

    #[test]
    fn endpoint_binding_requires_confirmed_presence_validated_topology() {
        let confirmed_pic =
            discover_voltage_controller_with_probe(Some("CVCtrl_BHB42XXX"), || true);
        let pic = bind_presence_validated_endpoint(
            Some("CVCtrl_BHB42XXX"),
            confirmed_pic,
            0,
            0x20,
            || true,
        )
        .expect("exact PIC identity plus ACK should bind 0x20");
        assert_eq!(pic.kind(), VoltageControllerKind::Pic1704);
        assert_eq!(pic.bus(), 0);
        assert_eq!(pic.address(), 0x20);

        let confirmed_dspic =
            discover_voltage_controller_with_probe(Some("AMLCtrl_BHB56902"), || true);
        let dspic = bind_presence_validated_endpoint(
            Some("AMLCtrl_BHB56902"),
            confirmed_dspic,
            0,
            0x22,
            || true,
        )
        .expect("confirmed dsPIC identity plus exact-address ACK should bind");
        assert_eq!(dspic.kind(), VoltageControllerKind::Dspic33Ep);
        assert_eq!(dspic.address(), 0x22);
    }

    #[test]
    fn standard_pic16_endpoint_consumes_exact_identity_and_topology() {
        for subtype in [None, Some("S9"), Some("S9J"), Some("S9_BHB09001")] {
            let endpoint = bind_standard_pic16_endpoint(Some("am1-s9"), subtype, 0, 0x56)
                .expect("exact S9 identity and topology should issue a capability");
            assert_eq!(endpoint.kind(), VoltageControllerKind::Pic16f1704);
            assert_eq!(endpoint.bus(), 0);
            assert_eq!(endpoint.address(), 0x56);
        }
    }

    #[test]
    fn standard_pic16_endpoint_refuses_guesses_and_contradictory_topologies() {
        for (board_target, subtype, bus, address) in [
            (None, None, 0, 0x55),
            (Some(""), None, 0, 0x55),
            (Some("am1-s9-extra"), None, 0, 0x55),
            (Some("am2-s19j"), Some("S9"), 0, 0x55),
            (Some("am1-s9"), Some("AMLCtrl_BHB56902"), 0, 0x55),
            (Some("am1-s9"), None, 1, 0x55),
            (Some("am1-s9"), None, 0, 0x20),
            (Some("am1-s9"), None, 0, 0x58),
        ] {
            assert!(
                bind_standard_pic16_endpoint(board_target, subtype, bus, address).is_err(),
                "board_target={board_target:?} subtype={subtype:?} bus={bus} address={address:#04x}"
            );
        }
    }

    #[test]
    fn standard_pic16_issuer_is_non_mutating() {
        let source = include_str!("subtype.rs");
        let start = source
            .find("pub fn discover_system_pic16_endpoint(")
            .expect("PIC16 system issuer");
        let body = &source[start..];
        let body = body
            .split("fn discover_voltage_controller_with_probe")
            .next()
            .expect("bounded PIC16 issuer body");
        assert!(!body.contains("service.heartbeat("));
        assert!(!body.contains("probe_voltage_controller_address("));
        assert!(!body.contains("service.write_bytes("));
        assert!(!body.contains("service.read_bytes("));
    }

    #[test]
    fn endpoint_binding_rejects_unknown_contradictory_and_unprobed_topologies() {
        for (subtype, acked) in [
            (None, false),
            (Some("FutureCtrl"), false),
            (Some("FutureCtrl"), true),
            (Some("CVCtrl_BHB42XXX"), false),
            (Some("AMLCtrl_BHB68900"), false),
        ] {
            let discovery = discover_voltage_controller_with_probe(subtype, || acked);
            assert!(
                bind_presence_validated_endpoint(subtype, discovery, 0, 0x20, || true).is_err(),
                "subtype={subtype:?}, acked={acked}"
            );
        }

        let s9 = discover_voltage_controller_with_probe(Some("S9"), || {
            panic!("S9 must not use the 0x20 probe")
        });
        assert_eq!(
            bind_presence_validated_endpoint(Some("S9"), s9, 0, 0x55, || true),
            Err(
                VoltageControllerEndpointError::TopologyNotPresenceValidated(
                    VoltageControllerKind::Pic16f1704
                )
            )
        );
    }

    #[test]
    fn endpoint_binding_rejects_addresses_outside_exact_family_topology() {
        for (subtype, address) in [
            ("CVCtrl_BHB42XXX", 0x21),
            ("AMLCtrl_BHB56902", 0x1F),
            ("AMLCtrl_BHB56902", 0x23),
        ] {
            let discovery = discover_voltage_controller_with_probe(Some(subtype), || true);
            assert!(matches!(
                bind_presence_validated_endpoint(Some(subtype), discovery, 0, address, || true),
                Err(VoltageControllerEndpointError::AddressOutsideTopology { .. })
            ));
        }
    }

    #[test]
    fn endpoint_binding_requires_the_requested_address_to_ack() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let exact_probe_calls = AtomicUsize::new(0);
        let discovery = discover_voltage_controller_with_probe(Some("AMLCtrl_BHB56902"), || true);
        let result =
            bind_presence_validated_endpoint(Some("AMLCtrl_BHB56902"), discovery, 0, 0x22, || {
                exact_probe_calls.fetch_add(1, Ordering::SeqCst);
                false
            });
        assert_eq!(exact_probe_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            result,
            Err(
                VoltageControllerEndpointError::EndpointPresenceNotObserved {
                    bus: 0,
                    address: 0x22,
                }
            )
        );

        let invalid_probe_calls = AtomicUsize::new(0);
        let discovery = discover_voltage_controller_with_probe(Some("AMLCtrl_BHB56902"), || true);
        let _ =
            bind_presence_validated_endpoint(Some("AMLCtrl_BHB56902"), discovery, 0, 0x23, || {
                invalid_probe_calls.fetch_add(1, Ordering::SeqCst);
                true
            });
        assert_eq!(
            invalid_probe_calls.load(Ordering::SeqCst),
            0,
            "out-of-topology addresses must be rejected before any bus probe"
        );

        let wrong_bus_probe_calls = AtomicUsize::new(0);
        let discovery = discover_voltage_controller_with_probe(Some("CVCtrl_BHB42XXX"), || true);
        let wrong_bus =
            bind_presence_validated_endpoint(Some("CVCtrl_BHB42XXX"), discovery, 1, 0x20, || {
                wrong_bus_probe_calls.fetch_add(1, Ordering::SeqCst);
                true
            });
        assert!(matches!(
            wrong_bus,
            Err(VoltageControllerEndpointError::BusOutsideTopology { .. })
        ));
        assert_eq!(
            wrong_bus_probe_calls.load(Ordering::SeqCst),
            0,
            "out-of-topology buses must be rejected before any endpoint probe"
        );
    }

    #[test]
    fn board_target_discovery_preserves_exact_evidence_and_unknown_state() {
        let known = discover_from_board_target("dspic33ep-fw89");
        assert_eq!(
            known.status(),
            VoltageControllerDiscoveryStatus::Confirmed(VoltageControllerKind::Dspic33Ep)
        );
        assert!(known.energization_eligible());

        let unknown = discover_from_board_target("future-controller");
        assert_eq!(unknown.status(), VoltageControllerDiscoveryStatus::Unknown);
        assert!(!unknown.energization_eligible());
    }

    #[test]
    fn classify_is_case_tolerant_for_bhb_suffix() {
        // The dev-kit ships `BHB42XXX` (uppercase X). Production VNish
        // images have shipped `BHB42xxx` historically; classifier must
        // handle both without re-routing to the dsPIC path.
        assert_eq!(
            classify_voltage_controller(Some("BBCtrl_BHB42xxx")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("cvctrl_bhb42xxx")),
            VoltageControllerKind::Pic1704,
        );
    }

    #[test]
    fn read_subtype_returns_none_on_windows_host() {
        // Host-test invariant: Linux-only file path, host returns None.
        // This is what allows the table-driven test above to run on the
        // Windows dev box without touching /etc.
        #[cfg(not(target_os = "linux"))]
        assert_eq!(read_subtype(), None);
    }

    #[test]
    fn probe_pic1704_at_0x20_returns_false_on_non_linux_host() {
        // Test-harness invariant: never opens a real I2C bus on Windows.
        #[cfg(not(target_os = "linux"))]
        assert!(!probe_pic1704_at_0x20(0));
    }

    #[test]
    fn classify_with_probe_fails_closed_when_probe_misses() {
        // A BHB42 identity contradicted by a failed presence probe must never
        // guess dsPIC merely because that was the historical fallback.
        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(
                classify_with_probe(Some("BBCtrl_BHB42XXX"), 0),
                VoltageControllerKind::NoPic,
            );
            assert_eq!(
                classify_with_probe(Some("CVCtrl_BHB42XXX"), 0),
                VoltageControllerKind::NoPic,
            );
            assert_eq!(
                classify_with_probe(Some("AMLCtrl_BHB42XXX"), 0),
                VoltageControllerKind::NoPic,
            );
        }
    }

    // -----------------------------------------------------------------
    //  W11.3 marker expansion regression tests (RE2 §2.5 + §6.1).
    //
    //  All CVCtrl-family hashboards report `CVCtrl_BHB42XXX` regardless
    //  of the SKU (S19 / S19i / S19 XP / S19j Pro). The classifier MUST
    //  route every CVCtrl_BHB42XXX subtype to Pic1704 — and the
    //  S19j Pro path must continue to work unchanged.
    // -----------------------------------------------------------------

    #[test]
    fn s19_cvctrl_classifies_pic1704() {
        // RE2 §2.5: Antminer S19 (Standard) → Cvitek CV183x + PIC1704.
        // Subtype string is the same `CVCtrl_BHB42XXX` family identifier
        // shared by every CVCtrl-class hashboard.
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
    }

    #[test]
    fn s19i_cvctrl_classifies_pic1704() {
        // RE2 §2.5: Antminer S19i → Cvitek CV183x + PIC1704.
        // Same subtype family as S19 / S19j Pro / S19 XP.
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
    }

    #[test]
    fn s19xp_cvctrl_classifies_pic1704() {
        // RE2 §2.5: Antminer S19 XP (Hydra) → Cvitek CV183x + PIC1704.
        // Note RE2 also lists an Amlogic S905 hydro variant of S19 XP;
        // when its subtype string is captured live it will be tested
        // separately. For now we only assert the CVCtrl path.
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
    }

    #[test]
    fn s21_aml_classifies_nopic() {
        //  root corruption-prevention guarantee #2 +
        // : S21 Amlogic is NoPic
        // (TAS5782M kernel-managed DAC). The classifier MUST NOT route
        // any plausible S21 subtype string into Pic1704.
        //
        // The known S21 hashboard family identifier candidates from RE2
        // (BHB68xxx, BHB68606, BHB68603) all fall through to the
        // `AMLCtrl_*` catch-all arm → NoPic. Verify the contract.
        for s21_sub in [
            "AMLCtrl_BHB68xxx",
            "AMLCtrl_BHB68606",
            "AMLCtrl_BHB68603",
            "AMLCtrl_S21",
            "AMLCtrl_S21Pro",
            "AMLCtrl_S21XP",
        ] {
            let kind = classify_voltage_controller(Some(s21_sub));
            assert_ne!(
                kind,
                VoltageControllerKind::Pic1704,
                "S21 subtype `{}` MUST NOT classify as Pic1704 — see \
                 corruption-prevention guarantee #2",
                s21_sub,
            );
            assert_eq!(
                kind,
                VoltageControllerKind::NoPic,
                "S21 subtype `{}` should classify as NoPic",
                s21_sub,
            );
        }
    }

    // -----------------------------------------------------------------
    //  W12 T19 expansion regression tests (RE3 §2.5 + §5.1).
    //
    //  RE3 lists ONLY Cvitek CV183x as the T19 carrier (no AM335x BB
    //  or Amlogic alternates). T19 hashboards share the same
    //  `CVCtrl_BHB42XXX` subtype family + APW12 SMBus PSU as the rest
    //  of the BM1362-family CV183x line, so the classifier MUST route
    //  T19 through the existing CVCtrl_BHB42XXX → Pic1704 path.
    // -----------------------------------------------------------------

    #[test]
    fn t19_bhb42_subtype_returns_pic1704_with_probe() {
        // RE3 §2.5: Antminer T19 ships on Cvitek CV183x only with the
        // BM1362-family BHB42XXX hashboard pattern → PIC1704 gate.
        // The subtype-only classification is Pic1704; on host the
        // probe fails closed to NoPic (no /dev/i2c-0).
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        // On non-Linux hosts the probe always returns false, proving
        // the no-regression contract: subtype says Pic1704 but probe
        // misses → non-energizing NoPic compatibility result.
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            classify_with_probe(Some("CVCtrl_BHB42XXX"), 0),
            VoltageControllerKind::NoPic,
        );
    }

    #[test]
    fn t19_bhb56_subtype_returns_dspic() {
        // Defense-in-depth: if a T19 SKU were ever discovered with a
        // BHB56-family hashboard (RE3 does NOT document this — purely
        // hypothetical), the classifier MUST route it to the existing
        // dsPIC path, not Pic1704. Mirrors the S19k Pro / S21 framed
        // dsPIC contract enforced by the BHB56xxx → Dspic33Ep arm.
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB56999")),
            VoltageControllerKind::Dspic33Ep,
        );
    }

    #[test]
    fn t19_unknown_subtype_returns_nopic() {
        // Any unrecognized T19-flavored subtype string must fail closed to
        // NoPic, never silently route to Pic1704 or dsPIC. Until a live T19
        // unit ground-truths a per-SKU subtype string, the only known-safe T19
        // string is the generic `CVCtrl_BHB42XXX` family identifier.
        assert_eq!(
            classify_voltage_controller(Some("T19_unknown")),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_T19")),
            VoltageControllerKind::NoPic,
        );
    }

    #[test]
    fn s19jpro_cvctrl_still_pic1704() {
        //  regression — the S19j Pro CV1835 path that landed in
        // 2026-05-09 must remain on the Pic1704 route. If this fails,
        // the W11.3 expansion broke an existing platform.
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("BBCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
    }

    #[test]
    fn classify_with_probe_requires_presence_for_address_0x20_families() {
        // BHB56 is exact dsPIC identity evidence, but still requires presence
        // at its documented 0x20 endpoint before compatibility can energize.
        assert_eq!(
            classify_with_probe(Some("AMLCtrl_BHB56902"), 0),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_with_probe(Some("AMLCtrl_S21NoPic"), 0),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_with_probe(Some("S9"), 0),
            VoltageControllerKind::Pic16f1704,
        );
        assert_eq!(classify_with_probe(None, 0), VoltageControllerKind::NoPic,);
    }

    // -----------------------------------------------------------------
    //  Phase B (2026-05-12) — board-target classification for am3-bb on
    //  S19J_IO_BOARD_V2_0 (LuxOS units have no /etc/subtype).
    // -----------------------------------------------------------------

    #[test]
    fn classify_from_board_target_recognizes_apw12_uart_tunnel_as_dspic() {
        // The .79 unit (AM335x BB on S19J_IO_BOARD_V2_0) uses an APW121215f
        // upstream PSU and fw=0x89 dsPIC hashboard controllers. Stale TOMLs may
        // still use the PSU transport string as their voltage-controller hint.
        for s in [
            "apw12-uart-tunnel",
            "apw12_uart_tunnel",
            "apw-uart-tunnel",
            "  APW12-UART-TUNNEL  ", // case + whitespace tolerant
        ] {
            assert_eq!(
                classify_from_board_target(s),
                Some(VoltageControllerKind::Dspic33Ep),
                "`{}` must classify to Dspic33Ep",
                s
            );
        }
    }

    #[test]
    fn classify_from_board_target_returns_none_for_unknown_strings() {
        // Unrecognized PSU-kind strings give None so the platform falls
        // back to its static default — never a silent misroute.
        for s in ["", "apw12-smbus", "pic1704", "some-future-psu"] {
            assert_eq!(
                classify_from_board_target(s),
                None,
                "`{}` must NOT be recognized by classify_from_board_target",
                s
            );
        }
        assert_eq!(
            classify_from_board_target("dspic33ep-fw89"),
            Some(VoltageControllerKind::Dspic33Ep)
        );
    }

    #[test]
    fn classify_from_board_target_does_not_change_existing_subtype_paths() {
        // Defense-in-depth: the Phase B board-target helper is a separate
        // entry point. It MUST NOT affect what `classify_voltage_controller`
        // or `classify_with_probe` return for the existing /etc/subtype
        // strings — those are byte-identical to pre-Phase-B behavior.
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("BBCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB56902")),
            VoltageControllerKind::Dspic33Ep,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB68xxx")),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_voltage_controller(Some("S9")),
            VoltageControllerKind::Pic16f1704,
        );
        assert_eq!(
            classify_voltage_controller(None),
            VoltageControllerKind::NoPic,
        );
        // And the board-target helper recognizes ONLY the apw12-uart-tunnel
        // family — it must not also start accepting a subtype string.
        assert_eq!(classify_from_board_target("CVCtrl_BHB42XXX"), None);
        assert_eq!(classify_from_board_target("BBCtrl_BHB42XXX"), None);
        assert_eq!(classify_from_board_target("S9"), None);
    }
}
