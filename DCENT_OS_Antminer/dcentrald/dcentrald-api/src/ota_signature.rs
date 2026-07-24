//! In-Rust Ed25519 verification of sysupgrade OTA bundles.
//!
//! SECURITY (wave 8, 2026-04-28): Ported from `DCENT_OS_ESP/dcentaxe/
//! src/ota_signature.rs`. The previous DCENT_OS upgrade flow ran `sysupgrade
//! --test` then `sysupgrade -f` on the uploaded `.tar` without an in-Rust
//! signature check — defense-in-depth was provided only by the shell script
//! (which calls `openssl pkeyutl -verify` on `MANIFEST.sig` against
//! `/etc/dcentos/release_ed25519.pub`). A malicious or corrupted bundle would
//! at minimum reach the shell script, get extracted to /tmp, and exercise the
//! shell parser before being rejected. With this module, the daemon rejects
//! bad bundles at the API boundary.
//!
//! Two verification entry points are provided:
//!
//! 1. `verify_signed_metadata()` — pure ed25519 verify over the canonical
//!    metadata string used by DCENT_axe (BitAxe). Useful for OTA flows that
//!    surface metadata as separate fields (HTTP headers, JSON body, etc.).
//!
//! 2. `verify_sysupgrade_bundle()` — opens the staged `.tar`, locates
//!    a single safe payload directory containing `MANIFEST.json` +
//!    `MANIFEST.sig` + `release_ed25519.pub`,
//!    re-verifies that the embedded pubkey matches the compile-time pin (or
//!    the on-disk pinned key), and verifies the Ed25519 signature over the
//!    raw manifest bytes (matching the existing shell `openssl pkeyutl
//!    -verify -rawin` semantics).
//!
//! The compile-time pin uses `option_env!("DCENT_OTA_PUBLIC_KEY_HEX")`. If the
//! env var is absent at build time, `signature_required()` returns `false` and
//! the API caller logs a startup warning.
//!
//! IMPORTANT — the bundle verifier does NOT fail open when the compile-time pin
//! is absent. `verify_sysupgrade_bundle()` never consults `signature_required()`;
//! it establishes its trust anchor from EITHER the compile-time pin OR the
//! on-disk `/etc/dcentos/release_ed25519.pub` (shipped in every rootfs overlay).
//! When a `MANIFEST.sig` is present but NEITHER trust anchor is available, the
//! verifier returns `Err` (see `verify_sysupgrade_signature_bytes()` — "no
//! trusted OTA public key is available"). The only path that accepts a bundle
//! without a signature is the explicit `allow_unsigned = true` lab override; the
//! production browser-upload caller (`rest.rs::system_upgrade`) hardcodes
//! `allow_unsigned = false`, so an unsigned or untrusted bundle is rejected on
//! the production path. The fail-closed contract is regression-pinned by the
//! `bundle_*` tests in this module. `config.allow_unsigned_ota` is the lab-only
//! escape hatch and is NOT wired into the production call site.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAX_SYSUPGRADE_TAR_ENTRIES: usize = 32;
const MAX_SYSUPGRADE_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_SYSUPGRADE_IMAGE_PAYLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_SYSUPGRADE_TOTAL_PAYLOAD_BYTES: u64 = 1024 * 1024 * 1024;
pub const SYSUPGRADE_AUTHORITY_PROFILE: &str = "dcentos.sysupgrade-authority/v1";
pub const SYSUPGRADE_UNSIGNED_LAB_PROFILE: &str = "dcentos.sysupgrade-unsigned-lab/v1";

use dcentrald_common::{
    ArtifactKind, ArtifactMaturity, BoardDesc, InstallAuthorization, StorageTopology,
    UpdateMechanism,
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};

#[derive(Debug, PartialEq, Eq)]
enum ManifestJsonError {
    Malformed(String),
    DuplicateObjectKey(String),
}

/// Recursive JSON seed that refuses duplicate object keys before a manifest is
/// represented as `serde_json::Value`. `Value`'s normal deserializer silently
/// keeps one duplicate (currently the last), which is unsafe for signed policy:
/// different consumers could authorize different occurrences of the same key.
struct UniqueJsonValueSeed<'a> {
    duplicate_key: &'a mut Option<String>,
}

struct UniqueJsonValueVisitor<'a> {
    duplicate_key: &'a mut Option<String>,
}

impl<'de> DeserializeSeed<'de> for UniqueJsonValueSeed<'_> {
    type Value = serde_json::Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonValueVisitor {
            duplicate_key: self.duplicate_key,
        })
    }
}

impl<'de> Visitor<'de> for UniqueJsonValueVisitor<'_> {
    type Value = serde_json::Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value with unique object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| E::custom("JSON numbers must be finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        UniqueJsonValueSeed {
            duplicate_key: self.duplicate_key,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));
        while let Some(value) = sequence.next_element_seed(UniqueJsonValueSeed {
            duplicate_key: self.duplicate_key,
        })? {
            values.push(value);
        }
        Ok(serde_json::Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = serde_json::Map::with_capacity(map.size_hint().unwrap_or(0));
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                *self.duplicate_key = Some(key.clone());
                return Err(serde::de::Error::custom(format!(
                    "duplicate object key '{key}'"
                )));
            }
            let value = map.next_value_seed(UniqueJsonValueSeed {
                duplicate_key: self.duplicate_key,
            })?;
            values.insert(key, value);
        }
        Ok(serde_json::Value::Object(values))
    }
}

fn parse_unique_manifest_json(
    manifest_bytes: &[u8],
) -> Result<serde_json::Value, ManifestJsonError> {
    let mut duplicate_key = None;
    let mut deserializer = serde_json::Deserializer::from_slice(manifest_bytes);
    let parsed = UniqueJsonValueSeed {
        duplicate_key: &mut duplicate_key,
    }
    .deserialize(&mut deserializer);

    match parsed {
        Ok(value) => {
            deserializer
                .end()
                .map_err(|error| ManifestJsonError::Malformed(error.to_string()))?;
            Ok(value)
        }
        Err(error) => match duplicate_key {
            Some(key) => Err(ManifestJsonError::DuplicateObjectKey(key)),
            None => Err(ManifestJsonError::Malformed(error.to_string())),
        },
    }
}

fn parse_authority_manifest_json(manifest_bytes: &[u8]) -> Result<serde_json::Value, String> {
    parse_unique_manifest_json(manifest_bytes).map_err(|error| match error {
        ManifestJsonError::Malformed(message) => {
            format!("OTA bundle: malformed MANIFEST.json: {message}")
        }
        ManifestJsonError::DuplicateObjectKey(key) => {
            format!("OTA bundle: MANIFEST.json contains duplicate object key '{key}'")
        }
    })
}

/// Returns true when this build was compiled with a pinned OTA public key.
pub fn signature_required() -> bool {
    option_env!("DCENT_OTA_PUBLIC_KEY_HEX").is_some()
}

/// Optional opaque key id pinned at build time alongside the public key.
pub fn compiled_key_id() -> Option<&'static str> {
    option_env!("DCENT_OTA_KEY_ID")
}

/// Canonical on-disk OTA verification key path shipped in every rootfs
/// overlay. The shell `sysupgrade` script `openssl pkeyutl -verify`s
/// `MANIFEST.sig` against this file, and `verify_sysupgrade_bundle` accepts it
/// as a trust anchor in addition to the compile-time pin.
pub const ON_DISK_RELEASE_KEY_PATH: &str = "/etc/dcentos/release_ed25519.pub";

/// WAVE 0 STABILIZE (2026-06-05) — honest OTA signature state.
///
/// The audit found the firmware advertising `signatureRequired: true` while
/// **no ed25519 pubkey is compiled in AND the on-disk key file does not
/// exist** — a signature gate that, if honored, would reject every update
/// (`verify_sysupgrade_bundle` returns "no trusted OTA public key is
/// available" the moment a `MANIFEST.sig` is present), and which on the
/// production `allow_unsigned = false` path means OTA is effectively inert.
///
/// This enum is the single source of truth for what the daemon HONESTLY
/// reports, derived from the actual trust anchors present at runtime — never
/// hardcoded. A variant name maps 1:1 to the REST `state` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaSignatureState {
    /// A trust anchor IS available (compile-time pin and/or the on-disk
    /// `/etc/dcentos/release_ed25519.pub`). Signed bundles verify; the
    /// production path rejects unsigned/untrusted bundles.
    Enforced,
    /// NO trust anchor is available anywhere. The daemon cannot verify a
    /// signature, so it must NOT claim a signature gate. OTA is
    /// unsigned-only / inert on the production path — honestly reported as
    /// such rather than as a gate that would reject every update.
    InertNoKey,
}

impl OtaSignatureState {
    /// Stable wire string for the REST/dashboard contract.
    pub fn as_str(self) -> &'static str {
        match self {
            OtaSignatureState::Enforced => "enforced",
            OtaSignatureState::InertNoKey => "inert_no_key",
        }
    }

    /// True only when a real signature gate is in force. This is what the
    /// honest `signature_required` field should report (NOT a hardcoded
    /// `true`).
    pub fn is_enforced(self) -> bool {
        matches!(self, OtaSignatureState::Enforced)
    }
}

/// Pure helper: derive the honest signature state from the two trust-anchor
/// inputs (compile-time pin present? on-disk key present?). Split out so the
/// honesty contract is host-testable without a build-time env var or a real
/// `/etc` file.
pub fn ota_signature_state_from(
    has_compiled_key: bool,
    on_disk_key_present: bool,
) -> OtaSignatureState {
    if has_compiled_key || on_disk_key_present {
        OtaSignatureState::Enforced
    } else {
        OtaSignatureState::InertNoKey
    }
}

/// Runtime honest signature state. Consults the compile-time pin AND probes
/// the on-disk `/etc/dcentos/release_ed25519.pub`. Used by the REST update
/// metadata / update-capability builders so they report a gate ONLY when a
/// trust anchor actually exists.
pub fn ota_signature_state() -> OtaSignatureState {
    let on_disk = Path::new(ON_DISK_RELEASE_KEY_PATH).is_file();
    ota_signature_state_from(compiled_public_key_hex().is_some(), on_disk)
}

/// The honest `keyId` to surface: the compiled key id when a key is actually
/// pinned, otherwise `None`. NEVER claims a key id when OTA is inert.
pub fn honest_key_id() -> Option<&'static str> {
    if compiled_public_key_hex().is_some() {
        compiled_key_id()
    } else {
        None
    }
}

/// Compile-time pinned public key (hex). None when no key was pinned.
pub fn compiled_public_key_hex() -> Option<&'static str> {
    option_env!("DCENT_OTA_PUBLIC_KEY_HEX")
}

fn decode_hex(input: &str) -> Result<Vec<u8>, String> {
    if !input.len().is_multiple_of(2) {
        return Err("hex input has odd length".to_string());
    }
    let mut bytes = Vec::with_capacity(input.len() / 2);
    let mut chars = input.as_bytes().chunks_exact(2);
    for pair in &mut chars {
        let byte = u8::from_str_radix(
            std::str::from_utf8(pair).map_err(|e| format!("invalid utf8 hex: {}", e))?,
            16,
        )
        .map_err(|e| format!("invalid hex byte: {}", e))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

/// Canonical message format — MUST stay byte-identical to the BitAxe build so
/// signing tools produce signatures that verify on both fleets.
pub fn canonical_message(
    board_target: &str,
    version: &str,
    payload_size: usize,
    payload_sha256: &str,
) -> String {
    format!(
        "schema=1\nboard_target={}\nversion={}\nsize={}\nsha256={}\n",
        board_target, version, payload_size, payload_sha256
    )
}

/// Verify an Ed25519 signature over the canonical metadata string using the
/// compile-time-pinned public key.
///
/// Returns `Err` if no public key was pinned at build time.
pub fn verify_signed_metadata(
    board_target: &str,
    version: &str,
    payload_size: usize,
    payload_sha256: &str,
    key_id: &str,
    signature_hex: &str,
) -> Result<(), String> {
    let public_key_hex = compiled_public_key_hex()
        .ok_or_else(|| "No OTA public key compiled into this firmware build".to_string())?;
    if let Some(compiled_key_id) = compiled_key_id() {
        if compiled_key_id != key_id {
            return Err(format!(
                "OTA key id mismatch: got '{}', expected '{}'",
                key_id, compiled_key_id
            ));
        }
    }
    let public_key_bytes = decode_hex(public_key_hex)?;
    let public_key_array: [u8; 32] = public_key_bytes
        .try_into()
        .map_err(|_| "OTA public key must decode to 32 bytes".to_string())?;
    let verifying_key = VerifyingKey::from_bytes(&public_key_array)
        .map_err(|e| format!("Invalid OTA public key: {}", e))?;
    let signature_bytes = decode_hex(signature_hex)?;
    let signature = Signature::try_from(signature_bytes.as_slice())
        .map_err(|e| format!("Invalid OTA signature: {}", e))?;
    let message = canonical_message(board_target, version, payload_size, payload_sha256);
    verifying_key
        .verify(message.as_bytes(), &signature)
        .map_err(|e| format!("OTA signature verification failed: {}", e))
}

/// Lower-level helper: verify raw bytes with an explicit public key. Used by
/// `verify_sysupgrade_bundle()` so we can run the same ed25519 check the shell
/// `sysupgrade` script runs via `openssl pkeyutl -verify -rawin`.
pub fn verify_raw(
    public_key_bytes: &[u8],
    message: &[u8],
    signature_bytes: &[u8],
) -> Result<(), String> {
    let public_key_array: [u8; 32] = public_key_bytes
        .try_into()
        .map_err(|_| "public key must decode to 32 bytes".to_string())?;
    let verifying_key = VerifyingKey::from_bytes(&public_key_array)
        .map_err(|e| format!("Invalid public key: {}", e))?;
    let signature =
        Signature::try_from(signature_bytes).map_err(|e| format!("Invalid signature: {}", e))?;
    verifying_key
        .verify(message, &signature)
        .map_err(|e| format!("Ed25519 verification failed: {}", e))
}

/// Signed, schema-validated authority carried by a sysupgrade manifest.
///
/// This is deliberately separate from payload paths: a valid Ed25519
/// signature and matching payload hashes do not by themselves authorize a
/// persistent write.  The manifest must also state an exact target, typed
/// artifact kind and maturity, explicit installability, and version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifact {
    pub board_target: String,
    pub artifact_kind: ArtifactKind,
    pub artifact_maturity: ArtifactMaturity,
    pub installable: bool,
    pub version: String,
    pub payload_prefix: String,
}

/// Opaque proof that a verified artifact and the running board's declarative
/// enablement policy agree. Only this type may cross the browser OTA admission
/// boundary into preflight/scheduling.
#[derive(Debug, Clone)]
pub struct AuthorizedSysupgrade {
    bundle: SysupgradeBundle,
    board_target: &'static str,
}

impl AuthorizedSysupgrade {
    pub fn board_target(&self) -> &'static str {
        self.board_target
    }

    pub fn version(&self) -> &str {
        self.bundle
            .verified_artifact
            .as_ref()
            .map(|artifact| artifact.version.as_str())
            .unwrap_or_default()
    }

    pub fn bundle(&self) -> &SysupgradeBundle {
        &self.bundle
    }
}

/// Outcome of inspecting a staged sysupgrade `.tar`.
#[derive(Debug, Clone)]
pub struct SysupgradeBundle {
    /// Exact board target read from the Ed25519-authenticated manifest.
    ///
    /// `None` means the manifest genuinely omitted the field (legacy signed
    /// bundle) or the explicit unsigned lab path was used. Callers that can
    /// flash production hardware must require an exact match rather than
    /// inferring identity from the archive name or payload directory.
    pub authenticated_board_target: Option<String>,
    /// Complete signed authority contract. `None` is possible only for the
    /// explicit unsigned laboratory inspection path and can never authorize a
    /// browser/public update.
    pub verified_artifact: Option<VerifiedArtifact>,
    pub manifest_path: PathBuf,
    pub signature_path: PathBuf,
    pub release_key_path: PathBuf,
    pub kernel_path: PathBuf,
    pub rootfs_path: PathBuf,
}

impl Default for SysupgradeBundle {
    fn default() -> Self {
        Self {
            authenticated_board_target: None,
            verified_artifact: None,
            manifest_path: PathBuf::new(),
            signature_path: PathBuf::new(),
            release_key_path: PathBuf::new(),
            kernel_path: PathBuf::new(),
            rootfs_path: PathBuf::new(),
        }
    }
}

impl SysupgradeBundle {
    /// Require authenticated manifest authority for exactly `expected`.
    ///
    /// This intentionally rejects legacy signed manifests that omit
    /// `board_target`; production release/install paths are fail closed. A
    /// read-only or explicitly lab-scoped caller may inspect the optional
    /// field without calling this method.
    pub fn require_authenticated_board_target(&self, expected: &str) -> Result<(), String> {
        if expected.is_empty() || expected != expected.trim() {
            return Err(
                "OTA board-target check: expected board target must be non-empty with no surrounding whitespace"
                    .to_string(),
            );
        }

        match self.authenticated_board_target.as_deref() {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => Err(format!(
                "OTA board-target mismatch: signed manifest targets '{actual}', expected '{expected}'"
            )),
            None => Err(format!(
                "OTA board-target check: bundle has no authenticated board_target; expected '{expected}'"
            )),
        }
    }

    /// Consume this verified bundle and bind it to the exact running target's
    /// public update policy.
    pub fn authorize_public_update(
        self,
        expected_board_target: &str,
    ) -> Result<AuthorizedSysupgrade, String> {
        self.require_authenticated_board_target(expected_board_target)?;
        let artifact = self.verified_artifact.as_ref().ok_or_else(|| {
            "OTA authorization: bundle has no authenticated artifact contract".to_string()
        })?;
        if artifact.board_target != expected_board_target {
            return Err(format!(
                "OTA authorization: verified artifact targets '{}', expected '{}'",
                artifact.board_target, expected_board_target
            ));
        }

        let descriptor = require_public_update_policy(expected_board_target)?;
        if artifact.artifact_kind != descriptor.enablement.artifact_kind {
            return Err(format!(
                "OTA authorization: artifact kind '{}' conflicts with target policy '{}'",
                artifact.artifact_kind.as_str(),
                descriptor.enablement.artifact_kind.as_str()
            ));
        }
        if artifact.artifact_maturity != descriptor.enablement.artifact_maturity {
            return Err(format!(
                "OTA authorization: artifact maturity '{}' conflicts with target policy '{}'",
                artifact.artifact_maturity.as_str(),
                descriptor.enablement.artifact_maturity.as_str()
            ));
        }
        if !artifact.installable {
            return Err(
                "OTA authorization: artifact explicitly declares installable=false".to_string(),
            );
        }

        Ok(AuthorizedSysupgrade {
            bundle: self,
            board_target: descriptor.board_target,
        })
    }
}

/// Admit the exact running board target to the public browser update API.
///
/// Laboratory-only writers remain accessible through their explicit guarded
/// workflows; this public endpoint is restricted to production/public-beta,
/// redundant-slot Zynq sysupgrade policies.
pub fn require_public_update_policy(board_target: &str) -> Result<&'static BoardDesc, String> {
    let descriptor = BoardDesc::lookup(board_target)
        .ok_or_else(|| format!("OTA policy: unknown canonical board target '{board_target}'"))?;
    let policy = descriptor.enablement;
    if !matches!(
        policy.install_authorization,
        InstallAuthorization::PublicBeta | InstallAuthorization::Production
    ) || !policy.allows_persistent_update()
    {
        return Err(format!(
            "OTA policy: target '{board_target}' is not authorized for public persistent updates"
        ));
    }
    if policy.storage_topology != StorageTopology::RedundantSlots
        || policy.update_mechanism != UpdateMechanism::ZynqUbiFwSetenv
        || policy.artifact_kind != ArtifactKind::SysupgradeBundle
    {
        return Err(format!(
            "OTA policy: target '{board_target}' is not a redundant-slot Zynq sysupgrade target"
        ));
    }
    Ok(descriptor)
}

pub const MAX_BOARD_TARGET_MARKER_BYTES: usize = 128;

/// Parse `/etc/dcentos/board_target` without trimming arbitrary whitespace or
/// accepting multiple lines. One conventional trailing LF (or CRLF) is
/// allowed; every other byte is part of the identity and must satisfy the
/// canonical lowercase target grammar.
pub fn parse_board_target_marker(bytes: &[u8]) -> Result<String, String> {
    if bytes.is_empty() || bytes.len() > MAX_BOARD_TARGET_MARKER_BYTES {
        return Err(format!(
            "board_target marker must contain 1..={MAX_BOARD_TARGET_MARKER_BYTES} bytes"
        ));
    }
    let without_lf = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let value_bytes = without_lf.strip_suffix(b"\r").unwrap_or(without_lf);
    if value_bytes.is_empty()
        || value_bytes
            .iter()
            .any(|byte| matches!(*byte, b'\r' | b'\n'))
    {
        return Err("board_target marker must contain exactly one identity line".to_string());
    }
    let value = std::str::from_utf8(value_bytes)
        .map_err(|_| "board_target marker must be valid UTF-8".to_string())?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
    }) {
        return Err(format!(
            "board_target marker contains non-canonical identity '{value}'"
        ));
    }
    Ok(value.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayloadRole {
    Kernel,
    Rootfs,
    Other,
}

#[derive(Debug, Clone, Copy)]
struct SupportedPayloadKind {
    manifest_kind: &'static str,
    accepted_leaves: &'static [&'static str],
    role: PayloadRole,
}

// Keep this registry deliberately small. A new hardware family may add a new
// payload kind here, but an authenticated manifest cannot turn an arbitrary tar
// member into an accepted image payload merely by naming it.
const SUPPORTED_PAYLOAD_KINDS: &[SupportedPayloadKind] = &[
    SupportedPayloadKind {
        manifest_kind: "kernel",
        accepted_leaves: &["kernel"],
        role: PayloadRole::Kernel,
    },
    SupportedPayloadKind {
        manifest_kind: "rootfs",
        accepted_leaves: &["root"],
        role: PayloadRole::Rootfs,
    },
    SupportedPayloadKind {
        manifest_kind: "metadata",
        accepted_leaves: &["METADATA"],
        role: PayloadRole::Other,
    },
    SupportedPayloadKind {
        manifest_kind: "bitstream",
        accepted_leaves: &["fpga_bitstream.bit"],
        role: PayloadRole::Other,
    },
    SupportedPayloadKind {
        manifest_kind: "verification_key",
        accepted_leaves: &["release_ed25519.pub"],
        role: PayloadRole::Other,
    },
];

fn supported_payload_kind(kind: &str) -> Option<&'static SupportedPayloadKind> {
    SUPPORTED_PAYLOAD_KINDS
        .iter()
        .find(|candidate| candidate.manifest_kind == kind)
}

#[derive(Debug, Clone)]
struct ObservedPayload {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Debug)]
struct VerifiedPayloadContract {
    kernel_path: PathBuf,
    rootfs_path: PathBuf,
}

/// Verify a staged sysupgrade tar against the compile-time-pinned OTA key
/// and (optionally) an on-disk pinned `release_ed25519.pub`.
///
/// On success returns the resolved bundle paths so the caller can hand them to
/// the existing shell `sysupgrade` invocation. On failure, returns a
/// human-readable error suitable for surfacing in an HTTP 400.
///
/// `allow_unsigned`: when true and no signature/manifest is present, the
/// bundle is accepted and the caller is expected to have already logged a
/// loud warning. When false, missing-signature is a hard error.
///
/// `pinned_release_key_path`: optional second pin (typically
/// `/etc/dcentos/release_ed25519.pub`). If provided AND present on disk, the
/// bundle's embedded `release_ed25519.pub` MUST match it byte-for-byte.
pub fn verify_sysupgrade_bundle(
    bundle_path: &Path,
    allow_unsigned: bool,
    pinned_release_key_path: Option<&Path>,
) -> Result<SysupgradeBundle, String> {
    if bundle_path.is_file() {
        return verify_sysupgrade_tar_bundle(bundle_path, allow_unsigned, pinned_release_key_path);
    }

    verify_sysupgrade_extracted_bundle(bundle_path, allow_unsigned, pinned_release_key_path)
}

/// Read the `version` string from a staged sysupgrade bundle's MANIFEST.json.
///
/// W24-OTA-2 / W24-OTA-1: the OTA write path needs the candidate version to run
/// `dcentrald_api_types::ota_rollback_protection::assess_rollback` BEFORE it
/// schedules the flash, so a signed-but-older package is refused. This reads the
/// manifest bytes the same way `verify_sysupgrade_bundle` does (either a `.tar`
/// file or an already-extracted directory) and extracts `version`.
///
/// Returns `Ok(Some(version))` when a non-empty `version` is present,
/// `Ok(None)` when the manifest has no `version` field (older manifests), and
/// `Err` only on a malformed/unreadable manifest.
pub fn read_manifest_version_from_bundle(bundle_path: &Path) -> Result<Option<String>, String> {
    let manifest_bytes: Vec<u8> = if bundle_path.is_file() {
        let mut file = std::fs::File::open(bundle_path).map_err(|e| {
            format!(
                "OTA rollback check: failed to open '{}': {}",
                bundle_path.display(),
                e
            )
        })?;
        let archive = read_sysupgrade_tar(&mut file)?;
        archive.manifest
    } else {
        // Extracted bundle: find the sysupgrade-* payload dir, read MANIFEST.json.
        let entries = std::fs::read_dir(bundle_path).map_err(|e| {
            format!(
                "OTA rollback check: failed to read extracted root '{}': {}",
                bundle_path.display(),
                e
            )
        })?;
        let mut manifest_path: Option<PathBuf> = None;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("sysupgrade-") {
                        manifest_path = Some(path.join("MANIFEST.json"));
                        break;
                    }
                }
            }
        }
        let manifest_path = manifest_path
            .ok_or_else(|| "OTA rollback check: missing sysupgrade-* payload dir".to_string())?;
        std::fs::read(&manifest_path)
            .map_err(|e| format!("OTA rollback check: failed to read MANIFEST.json: {}", e))?
    };

    if manifest_bytes.is_empty() {
        return Err("OTA rollback check: MANIFEST.json is empty".to_string());
    }

    #[derive(serde::Deserialize)]
    struct ManifestVersion {
        #[serde(default)]
        version: Option<String>,
    }
    let manifest = parse_unique_manifest_json(&manifest_bytes).map_err(|error| match error {
        ManifestJsonError::Malformed(message) => {
            format!("OTA rollback check: malformed MANIFEST.json: {message}")
        }
        ManifestJsonError::DuplicateObjectKey(key) => {
            format!("OTA rollback check: MANIFEST.json contains duplicate object key '{key}'")
        }
    })?;
    let parsed: ManifestVersion = serde_json::from_value(manifest)
        .map_err(|e| format!("OTA rollback check: malformed MANIFEST.json: {e}"))?;
    Ok(parsed
        .version
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty()))
}

/// CE-183: manifest-declared package status. Missing/unparseable status is
/// treated as release (fail-closed), matching the target sysupgrade script.
fn manifest_declares_release_status(manifest_bytes: &[u8]) -> bool {
    let status = parse_unique_manifest_json(manifest_bytes)
        .ok()
        .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "release".to_string());
    matches!(status.trim(), "release" | "production" | "stable")
}

/// Read the board target exactly as declared by a manifest.
///
/// A missing field is the only legacy-compatible absence. `null`, a non-string
/// value, surrounding whitespace, and an empty string are malformed identity
/// claims and must not silently degrade to `None`.
fn manifest_board_target(manifest_bytes: &[u8]) -> Result<Option<String>, String> {
    let manifest = parse_authority_manifest_json(manifest_bytes)?;
    let Some(value) = manifest.get("board_target") else {
        return Ok(None);
    };
    let target = value.as_str().ok_or_else(|| {
        "OTA bundle: MANIFEST.json board_target must be a non-empty string when present".to_string()
    })?;
    if target.is_empty() || target != target.trim() {
        return Err(
            "OTA bundle: MANIFEST.json board_target must be non-empty with no surrounding whitespace"
                .to_string(),
        );
    }
    Ok(Some(target.to_string()))
}

fn required_manifest_string(manifest: &serde_json::Value, field: &str) -> Result<String, String> {
    let value = manifest
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("OTA bundle: MANIFEST.json {field} must be a non-empty string"))?;
    if value.is_empty() || value != value.trim() {
        return Err(format!(
            "OTA bundle: MANIFEST.json {field} must be non-empty with no surrounding whitespace"
        ));
    }
    Ok(value.to_string())
}

/// Parse the signed, write-authorizing manifest fields independently from the
/// payload registry. Missing fields are not legacy-compatible on a mutating
/// path: an artifact that does not explicitly say what it is and whether it is
/// installable cannot authorize a persistent update.
fn verified_artifact_contract(
    manifest_bytes: &[u8],
    payload_prefix: &str,
) -> Result<VerifiedArtifact, String> {
    let manifest = parse_authority_manifest_json(manifest_bytes)?;
    let schema = manifest
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "OTA bundle: MANIFEST.json schema must be integer 1".to_string())?;
    if schema != 1 {
        return Err(format!(
            "OTA bundle: unsupported MANIFEST.json schema {schema}; expected 1"
        ));
    }

    let manifest_profile = required_manifest_string(&manifest, "manifest_profile")?;
    if manifest_profile != SYSUPGRADE_AUTHORITY_PROFILE {
        return Err(format!(
            "OTA bundle: unsupported MANIFEST.json manifest_profile '{manifest_profile}'; expected '{SYSUPGRADE_AUTHORITY_PROFILE}'"
        ));
    }

    // Authority-v1 is deliberately bound to a direct signature by the
    // long-lived release root.  The older two-level verifier uses wall-clock
    // validity windows, but Zynq recovery/first-flash environments do not have
    // an authenticated RTC or a guaranteed NTP fix before update admission.
    // Treating that clock as authority would make the same signed bundle
    // accept or reject according to mutable boot-time state.  Keep the
    // low-level certificate parser available for read-only research and a
    // future profile with trusted-time evidence, but never let its fields
    // authorize a profile-v1 persistent write.
    for unsupported_chain_field in ["ota_intermediate_cert", "ota_revoked_intermediates"] {
        if manifest.get(unsupported_chain_field).is_some() {
            return Err(format!(
                "OTA bundle: {SYSUPGRADE_AUTHORITY_PROFILE} requires a direct release-root signature and forbids '{unsupported_chain_field}'; certificate validity has no trusted-time authority on Zynq"
            ));
        }
    }

    let status = required_manifest_string(&manifest, "status")?;
    if status == "lab_unsigned" {
        return Err(format!(
            "OTA bundle: {SYSUPGRADE_AUTHORITY_PROFILE} forbids status 'lab_unsigned'"
        ));
    }

    let product = required_manifest_string(&manifest, "product")?;
    if product != "DCENT_OS" {
        return Err(format!(
            "OTA bundle: MANIFEST.json product must be DCENT_OS, found '{product}'"
        ));
    }

    let package_type = required_manifest_string(&manifest, "package_type")?;
    let artifact_kind = ArtifactKind::parse(&package_type).ok_or_else(|| {
        format!("OTA bundle: unsupported MANIFEST.json package_type '{package_type}'")
    })?;
    if artifact_kind != ArtifactKind::SysupgradeBundle {
        return Err(format!(
            "OTA bundle: package_type '{}' is not a persistent sysupgrade artifact",
            artifact_kind.as_str()
        ));
    }

    match manifest
        .get("installable")
        .and_then(serde_json::Value::as_bool)
    {
        Some(true) => {}
        Some(false) => {
            return Err(
                "OTA bundle: MANIFEST.json explicitly declares installable=false".to_string(),
            )
        }
        None => {
            return Err(
                "OTA bundle: MANIFEST.json must explicitly declare installable=true".to_string(),
            )
        }
    }

    let artifact_maturity_value = manifest.get("artifact_maturity").ok_or_else(|| {
        "OTA bundle: MANIFEST.json must declare typed artifact_maturity".to_string()
    })?;
    let artifact_maturity: ArtifactMaturity =
        serde_json::from_value(artifact_maturity_value.clone()).map_err(|_| {
            "OTA bundle: MANIFEST.json artifact_maturity must match the current 'experimental' authority policy"
                .to_string()
        })?;
    if artifact_maturity != ArtifactMaturity::Experimental {
        return Err(
            "OTA bundle: MANIFEST.json artifact_maturity must match the current 'experimental' authority policy"
                .to_string(),
        );
    }

    let board_target = manifest_board_target(manifest_bytes)?.ok_or_else(|| {
        "OTA bundle: MANIFEST.json must declare an exact board_target".to_string()
    })?;
    let board = required_manifest_string(&manifest, "board")?;
    if board != board_target {
        return Err(format!(
            "OTA bundle: MANIFEST.json board '{board}' conflicts with board_target '{board_target}'"
        ));
    }

    let expected_prefix = format!("sysupgrade-{board_target}");
    if payload_prefix != expected_prefix {
        return Err(format!(
            "OTA bundle: payload directory '{payload_prefix}' does not match signed target prefix '{expected_prefix}'"
        ));
    }

    Ok(VerifiedArtifact {
        board_target,
        artifact_kind,
        artifact_maturity,
        installable: true,
        version: required_manifest_string(&manifest, "version")?,
        payload_prefix: payload_prefix.to_string(),
    })
}

/// Validate the deliberately non-authoritative unsigned laboratory profile.
///
/// This function returns only `()` by design. Even a structurally valid lab
/// manifest is unauthenticated input and must never mint `VerifiedArtifact`,
/// an authenticated board target, or `AuthorizedSysupgrade`.
fn validate_unsigned_lab_artifact_contract(
    manifest_bytes: &[u8],
    payload_prefix: &str,
) -> Result<(), String> {
    let manifest = parse_authority_manifest_json(manifest_bytes)?;
    let schema = manifest
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            "OTA bundle: unsigned lab MANIFEST.json schema must be integer 1".to_string()
        })?;
    if schema != 1 {
        return Err(format!(
            "OTA bundle: unsupported unsigned lab MANIFEST.json schema {schema}; expected 1"
        ));
    }

    let manifest_profile = required_manifest_string(&manifest, "manifest_profile")?;
    if manifest_profile != SYSUPGRADE_UNSIGNED_LAB_PROFILE {
        return Err(format!(
            "OTA bundle: unsigned lab MANIFEST.json manifest_profile must be '{SYSUPGRADE_UNSIGNED_LAB_PROFILE}', found '{manifest_profile}'"
        ));
    }

    let status = required_manifest_string(&manifest, "status")?;
    if status != "lab_unsigned" {
        return Err(format!(
            "OTA bundle: {SYSUPGRADE_UNSIGNED_LAB_PROFILE} requires exact status 'lab_unsigned', found '{status}'"
        ));
    }

    for forbidden_authority_field in ["ota_intermediate_cert", "ota_revoked_intermediates"] {
        if manifest.get(forbidden_authority_field).is_some() {
            return Err(format!(
                "OTA bundle: {SYSUPGRADE_UNSIGNED_LAB_PROFILE} forbids authority field '{forbidden_authority_field}'"
            ));
        }
    }

    let payloads = manifest
        .get("payloads")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            "OTA bundle: unsigned lab MANIFEST.json must contain an object-valued payloads registry"
                .to_string()
        })?;
    if payloads.contains_key("verification_key") {
        return Err(format!(
            "OTA bundle: {SYSUPGRADE_UNSIGNED_LAB_PROFILE} forbids payload kind 'verification_key'"
        ));
    }

    let product = required_manifest_string(&manifest, "product")?;
    if product != "DCENT_OS" {
        return Err(format!(
            "OTA bundle: unsigned lab MANIFEST.json product must be DCENT_OS, found '{product}'"
        ));
    }

    let package_type = required_manifest_string(&manifest, "package_type")?;
    if package_type != ArtifactKind::SysupgradeBundle.as_str() {
        return Err(format!(
            "OTA bundle: unsigned lab package_type must be '{}', found '{package_type}'",
            ArtifactKind::SysupgradeBundle.as_str()
        ));
    }

    if manifest
        .get("installable")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err(
            "OTA bundle: unsigned lab MANIFEST.json must explicitly declare installable=true"
                .to_string(),
        );
    }

    let artifact_maturity_value = manifest.get("artifact_maturity").ok_or_else(|| {
        "OTA bundle: unsigned lab MANIFEST.json must declare typed artifact_maturity".to_string()
    })?;
    let artifact_maturity: ArtifactMaturity =
        serde_json::from_value(artifact_maturity_value.clone()).map_err(|_| {
            "OTA bundle: unsigned lab artifact_maturity must be 'experimental'".to_string()
        })?;
    if artifact_maturity != ArtifactMaturity::Experimental {
        return Err(
            "OTA bundle: unsigned lab artifact_maturity must be 'experimental'".to_string(),
        );
    }

    let board_target = manifest_board_target(manifest_bytes)?.ok_or_else(|| {
        "OTA bundle: unsigned lab MANIFEST.json must declare an exact board_target".to_string()
    })?;
    let board = required_manifest_string(&manifest, "board")?;
    if board != board_target {
        return Err(format!(
            "OTA bundle: unsigned lab board '{board}' conflicts with board_target '{board_target}'"
        ));
    }

    let expected_prefix = format!("sysupgrade-{board_target}");
    if payload_prefix != expected_prefix {
        return Err(format!(
            "OTA bundle: unsigned lab payload directory '{payload_prefix}' does not match target prefix '{expected_prefix}'"
        ));
    }

    required_manifest_string(&manifest, "version")?;
    Ok(())
}

fn require_extracted_entry_absent(path: &Path, leaf: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err(format!(
            "OTA bundle: unsigned lab profile forbids archive member '{leaf}'"
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "OTA bundle: failed to inspect extracted file '{}': {error}",
            path.display()
        )),
    }
}

fn is_direct_regular_file(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "OTA bundle: failed to inspect extracted file '{}': {error}",
            path.display()
        )),
    }
}

fn read_bounded_extracted_file(path: &Path, max_size: u64, label: &str) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "OTA bundle: failed to inspect extracted {label} '{}': {error}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "OTA bundle: extracted {label} '{}' must be a direct regular file",
            path.display()
        ));
    }
    if metadata.len() > max_size {
        return Err(format!(
            "OTA bundle: extracted {label} exceeds safety ceiling ({} > {})",
            metadata.len(),
            max_size
        ));
    }
    std::fs::read(path).map_err(|error| {
        format!(
            "OTA bundle: failed to read extracted {label} '{}': {error}",
            path.display()
        )
    })
}

fn verify_sysupgrade_extracted_bundle(
    extracted_root: &Path,
    allow_unsigned: bool,
    pinned_release_key_path: Option<&Path>,
) -> Result<SysupgradeBundle, String> {
    // Find the single safe manifest-bearing payload subdir. Authorization
    // later binds it exactly to `sysupgrade-{signed board_target}`.
    let entries = std::fs::read_dir(extracted_root).map_err(|e| {
        format!(
            "OTA bundle: failed to read extracted root '{}': {}",
            extracted_root.display(),
            e
        )
    })?;
    let mut payload_dir: Option<PathBuf> = None;
    for entry in entries {
        let entry =
            entry.map_err(|e| format!("OTA bundle: failed reading extracted root entry: {e}"))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| {
            format!(
                "OTA bundle: failed to inspect extracted root entry '{}': {e}",
                path.display()
            )
        })?;
        if !file_type.is_dir() {
            return Err(format!(
                "OTA bundle extracted root contains unexpected non-directory entry '{}'",
                path.display()
            ));
        }
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| {
                "OTA bundle extracted payload directory name is not valid UTF-8".to_string()
            })?;
        if !is_safe_payload_prefix(&name) || !path.join("MANIFEST.json").exists() {
            return Err(format!(
                "OTA bundle extracted root contains unexpected payload directory '{}'",
                path.display()
            ));
        }
        if payload_dir.replace(path).is_some() {
            return Err(
                "OTA bundle contains multiple manifest-bearing payload directories".to_string(),
            );
        }
    }
    let payload_dir = payload_dir
        .ok_or_else(|| "OTA bundle is missing a manifest-bearing payload directory".to_string())?;

    let manifest_path = payload_dir.join("MANIFEST.json");
    let signature_path = payload_dir.join("MANIFEST.sig");
    let release_key_path = payload_dir.join("release_ed25519.pub");
    let payload_prefix = payload_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "OTA bundle payload directory name is not valid UTF-8".to_string())?;

    if !is_direct_regular_file(&manifest_path)? {
        return Err("OTA bundle is missing MANIFEST.json".to_string());
    }
    let signature_present = is_direct_regular_file(&signature_path)?;
    if !signature_present {
        if !allow_unsigned {
            return Err(
                "OTA bundle is missing MANIFEST.sig — refusing unsigned upgrade. \
                 Set [ota] allow_unsigned_ota = true in /etc/dcentrald.toml only \
                 for controlled lab recovery."
                    .to_string(),
            );
        }
        // CE-183: the allow_unsigned lab override must NOT apply to a bundle
        // that declares release status — mirror the target sysupgrade:507 rule.
        let manifest_bytes = read_bounded_extracted_file(
            &manifest_path,
            MAX_SYSUPGRADE_METADATA_BYTES,
            "MANIFEST.json",
        )?;
        if manifest_declares_release_status(&manifest_bytes) {
            return Err(
                "OTA bundle declares release status but has no MANIFEST.sig — \
                        the allow_unsigned lab override does not apply to release-status \
                        packages (CE-183)"
                    .to_string(),
            );
        }
        require_extracted_entry_absent(&signature_path, "MANIFEST.sig")?;
        require_extracted_entry_absent(&release_key_path, "release_ed25519.pub")?;
        validate_unsigned_lab_artifact_contract(&manifest_bytes, payload_prefix)?;
        let observed = observe_extracted_payloads(&payload_dir, payload_prefix)?;
        let contract =
            verify_manifest_payload_contract(&manifest_bytes, payload_prefix, &observed)?;
        return Ok(SysupgradeBundle {
            authenticated_board_target: None,
            verified_artifact: None,
            manifest_path,
            signature_path,
            release_key_path,
            kernel_path: extracted_contract_path(&payload_dir, &contract.kernel_path)?,
            rootfs_path: extracted_contract_path(&payload_dir, &contract.rootfs_path)?,
        });
    }

    if !is_direct_regular_file(&release_key_path)? {
        return Err("Signed OTA bundle is missing release_ed25519.pub".to_string());
    }

    // Read the embedded release pubkey (raw 32 bytes for ed25519-dalek).
    let embedded_key_bytes = read_bounded_extracted_file(
        &release_key_path,
        MAX_SYSUPGRADE_METADATA_BYTES,
        "release_ed25519.pub",
    )?;
    let embedded_key_bytes = strip_pem_if_present(&embedded_key_bytes);

    // (Optional) compare the embedded key against an on-disk pin.
    if let Some(pinned_path) = pinned_release_key_path {
        if pinned_path.is_file() {
            let pinned_bytes = std::fs::read(pinned_path).map_err(|e| {
                format!(
                    "OTA bundle: failed to read pinned release key '{}': {}",
                    pinned_path.display(),
                    e
                )
            })?;
            let pinned_bytes = strip_pem_if_present(&pinned_bytes);
            if pinned_bytes != embedded_key_bytes {
                return Err(
                    "OTA bundle: embedded release_ed25519.pub does not match pinned \
                     /etc/dcentos/release_ed25519.pub — rejecting bundle"
                        .to_string(),
                );
            }
        }
    }

    // Compare the embedded key against the compile-time pin (if any).
    if let Some(compiled_hex) = compiled_public_key_hex() {
        let compiled_bytes = decode_hex(compiled_hex)?;
        if compiled_bytes != embedded_key_bytes {
            return Err(
                "OTA bundle: embedded release_ed25519.pub does not match the OTA \
                 public key compiled into this firmware build — rejecting bundle"
                    .to_string(),
            );
        }
    }

    // Verify Ed25519 signature over raw manifest bytes (matches the shell
    // sysupgrade's `openssl pkeyutl -verify -rawin` invocation).
    let manifest_bytes = read_bounded_extracted_file(
        &manifest_path,
        MAX_SYSUPGRADE_METADATA_BYTES,
        "MANIFEST.json",
    )?;
    let signature_bytes = read_bounded_extracted_file(
        &signature_path,
        MAX_SYSUPGRADE_METADATA_BYTES,
        "MANIFEST.sig",
    )?;

    verify_sysupgrade_signature_bytes(
        &manifest_bytes,
        &signature_bytes,
        &embedded_key_bytes,
        pinned_release_key_path,
    )?;

    let verified_artifact = verified_artifact_contract(&manifest_bytes, payload_prefix)?;
    let authenticated_board_target = Some(verified_artifact.board_target.clone());

    let observed = observe_extracted_payloads(&payload_dir, payload_prefix)?;
    let contract = verify_manifest_payload_contract(&manifest_bytes, payload_prefix, &observed)?;

    Ok(SysupgradeBundle {
        authenticated_board_target,
        verified_artifact: Some(verified_artifact),
        manifest_path,
        signature_path,
        release_key_path,
        kernel_path: extracted_contract_path(&payload_dir, &contract.kernel_path)?,
        rootfs_path: extracted_contract_path(&payload_dir, &contract.rootfs_path)?,
    })
}

fn extracted_contract_path(payload_dir: &Path, contract_path: &Path) -> Result<PathBuf, String> {
    let leaf = contract_path
        .file_name()
        .ok_or_else(|| "OTA bundle manifest resolved an invalid payload path".to_string())?;
    Ok(payload_dir.join(leaf))
}

fn is_auxiliary_payload_leaf(leaf: &str) -> bool {
    matches!(leaf, "MANIFEST.json" | "MANIFEST.sig" | "SHA256SUMS")
}

fn observe_extracted_payloads(
    payload_dir: &Path,
    payload_prefix: &str,
) -> Result<std::collections::BTreeMap<String, ObservedPayload>, String> {
    let entries = std::fs::read_dir(payload_dir).map_err(|e| {
        format!(
            "OTA bundle: failed to enumerate extracted payload directory '{}': {e}",
            payload_dir.display()
        )
    })?;
    let mut observed = std::collections::BTreeMap::new();
    let mut entry_count = 0usize;
    let mut total_payload_bytes = 0u64;

    for entry in entries {
        let entry = entry.map_err(|e| format!("OTA bundle: failed reading payload entry: {e}"))?;
        entry_count = entry_count
            .checked_add(1)
            .ok_or_else(|| "OTA bundle payload entry count overflow".to_string())?;
        if entry_count > MAX_SYSUPGRADE_TAR_ENTRIES {
            return Err(format!(
                "OTA bundle contains too many payload entries ({} > {})",
                entry_count, MAX_SYSUPGRADE_TAR_ENTRIES
            ));
        }

        let file_type = entry.file_type().map_err(|e| {
            format!(
                "OTA bundle: failed to inspect extracted payload '{}': {e}",
                entry.path().display()
            )
        })?;
        if !file_type.is_file() {
            return Err(format!(
                "OTA bundle extracted payload '{}' must be a direct regular file",
                entry.path().display()
            ));
        }
        let leaf = entry
            .file_name()
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| "OTA bundle extracted payload name is not valid UTF-8".to_string())?;
        if !is_safe_payload_leaf(&leaf) {
            return Err(format!(
                "OTA bundle extracted payload leaf '{leaf}' is unsafe"
            ));
        }

        let size = entry
            .metadata()
            .map_err(|e| format!("OTA bundle: failed to stat payload '{leaf}': {e}"))?
            .len();
        total_payload_bytes = total_payload_bytes
            .checked_add(size)
            .ok_or_else(|| "OTA bundle declared payload size overflow".to_string())?;
        if total_payload_bytes > MAX_SYSUPGRADE_TOTAL_PAYLOAD_BYTES {
            return Err(format!(
                "OTA bundle payload bytes exceed safety ceiling ({} > {})",
                total_payload_bytes, MAX_SYSUPGRADE_TOTAL_PAYLOAD_BYTES
            ));
        }

        if is_auxiliary_payload_leaf(&leaf) {
            if size > MAX_SYSUPGRADE_METADATA_BYTES {
                return Err(format!(
                    "OTA bundle metadata '{leaf}' exceeds safety ceiling ({} > {})",
                    size, MAX_SYSUPGRADE_METADATA_BYTES
                ));
            }
            continue;
        }
        if size > MAX_SYSUPGRADE_IMAGE_PAYLOAD_BYTES {
            return Err(format!(
                "OTA bundle image payload '{leaf}' exceeds safety ceiling ({} > {})",
                size, MAX_SYSUPGRADE_IMAGE_PAYLOAD_BYTES
            ));
        }
        let path = entry.path();
        observed.insert(
            leaf.clone(),
            ObservedPayload {
                path: format!("{payload_prefix}/{leaf}"),
                size,
                sha256: hash_file(&path, &format!("payload '{leaf}'"))?,
            },
        );
    }

    Ok(observed)
}

fn verify_sysupgrade_tar_bundle(
    tar_path: &Path,
    allow_unsigned: bool,
    pinned_release_key_path: Option<&Path>,
) -> Result<SysupgradeBundle, String> {
    let mut file = std::fs::File::open(tar_path)
        .map_err(|e| format!("OTA bundle: failed to open '{}': {}", tar_path.display(), e))?;
    let archive = read_sysupgrade_tar(&mut file)?;

    if archive.manifest.is_empty() {
        return Err("OTA bundle is missing MANIFEST.json".to_string());
    }

    if archive.bundle.signature_path.as_os_str().is_empty() {
        if !allow_unsigned {
            return Err(
                "OTA bundle is missing MANIFEST.sig - refusing unsigned upgrade. \
                 Set [ota] allow_unsigned_ota = true in /etc/dcentrald.toml only \
                 for controlled lab recovery."
                    .to_string(),
            );
        }
        // CE-183: the allow_unsigned lab override must NOT apply to a bundle
        // that declares release status — mirror the target sysupgrade:507 rule.
        if manifest_declares_release_status(&archive.manifest) {
            return Err(
                "OTA bundle declares release status but has no MANIFEST.sig — \
                        the allow_unsigned lab override does not apply to release-status \
                        packages (CE-183)"
                    .to_string(),
            );
        }
        if !archive.bundle.release_key_path.as_os_str().is_empty() {
            return Err(
                "OTA bundle: unsigned lab profile forbids archive member 'release_ed25519.pub'"
                    .to_string(),
            );
        }
        validate_unsigned_lab_artifact_contract(&archive.manifest, &archive.payload_prefix)?;
        // Unsigned lab bundle: the manifest is not authenticated, but the
        // explicit lab escape still requires the same closed payload contract.
        let contract = verify_manifest_payload_contract(
            &archive.manifest,
            &archive.payload_prefix,
            &archive.observed_payloads,
        )?;
        let mut bundle = archive.bundle;
        bundle.kernel_path = contract.kernel_path;
        bundle.rootfs_path = contract.rootfs_path;
        return Ok(bundle);
    }

    if archive.release_key.is_empty() {
        return Err("Signed OTA bundle is missing release_ed25519.pub".to_string());
    }

    verify_sysupgrade_signature_bytes(
        &archive.manifest,
        &archive.signature,
        &archive.release_key,
        pinned_release_key_path,
    )?;
    let verified_artifact = verified_artifact_contract(&archive.manifest, &archive.payload_prefix)?;
    let authenticated_board_target = Some(verified_artifact.board_target.clone());

    // Bind every supported payload byte-for-byte to the now-authenticated
    // manifest and reject unknown or unmanifested image members.
    let contract = verify_manifest_payload_contract(
        &archive.manifest,
        &archive.payload_prefix,
        &archive.observed_payloads,
    )?;

    let mut bundle = archive.bundle;
    bundle.authenticated_board_target = authenticated_board_target;
    bundle.verified_artifact = Some(verified_artifact);
    bundle.kernel_path = contract.kernel_path;
    bundle.rootfs_path = contract.rootfs_path;
    Ok(bundle)
}

#[derive(Debug, Default)]
struct TarSysupgradeArchive {
    bundle: SysupgradeBundle,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    release_key: Vec<u8>,
    payload_prefix: String,
    observed_payloads: std::collections::BTreeMap<String, ObservedPayload>,
}

fn read_sysupgrade_tar<R: Read + Seek>(reader: &mut R) -> Result<TarSysupgradeArchive, String> {
    const BLOCK: u64 = 512;

    let mut archive = TarSysupgradeArchive::default();
    let mut payload_prefix: Option<String> = None;
    let mut seen_payload_files = std::collections::BTreeSet::<String>::new();
    let mut payload_directory_count = 0usize;
    let mut entry_count = 0usize;
    let mut total_payload_bytes = 0u64;

    loop {
        let mut header = [0u8; BLOCK as usize];
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(
                    "OTA bundle tar is truncated or missing its two-block end marker".to_string(),
                )
            }
            Err(e) => return Err(format!("OTA bundle: failed reading tar header: {}", e)),
        }

        if header.iter().all(|&b| b == 0) {
            validate_tar_end_marker(reader)?;
            break;
        }

        validate_tar_header_checksum(&header)?;

        entry_count = entry_count
            .checked_add(1)
            .ok_or_else(|| "OTA bundle tar entry count overflow".to_string())?;
        if entry_count > MAX_SYSUPGRADE_TAR_ENTRIES {
            return Err(format!(
                "OTA bundle contains too many tar entries ({} > {})",
                entry_count, MAX_SYSUPGRADE_TAR_ENTRIES
            ));
        }

        let name = tar_header_path(&header)?;
        let size = parse_tar_octal(&header[124..136])?;
        let entry_kind = classify_tar_entry_kind(header[156], &name)?;
        let sysupgrade_entry = classify_sysupgrade_tar_entry(&name)?;

        match sysupgrade_entry {
            SysupgradeTarEntry::Directory { prefix } => {
                if entry_kind != TarEntryKind::Directory {
                    return Err(format!(
                        "OTA bundle tar entry '{}' must be a directory",
                        name
                    ));
                }
                if size != 0 {
                    return Err(format!(
                        "OTA bundle tar directory '{}' unexpectedly has payload bytes",
                        name
                    ));
                }
                remember_payload_prefix(&mut payload_prefix, prefix)?;
                payload_directory_count = payload_directory_count
                    .checked_add(1)
                    .ok_or_else(|| "OTA bundle payload directory count overflow".to_string())?;
                if payload_directory_count > 1 {
                    return Err(
                        "OTA bundle must contain exactly one canonical payload directory entry"
                            .to_string(),
                    );
                }
                skip_tar_padding(reader, size)?;
            }
            SysupgradeTarEntry::File { prefix, leaf } => {
                if entry_kind != TarEntryKind::Regular {
                    return Err(format!(
                        "OTA bundle tar entry '{}' must be a regular file",
                        name
                    ));
                }
                remember_payload_prefix(&mut payload_prefix, prefix)?;
                if !seen_payload_files.insert(leaf.to_string()) {
                    return Err(format!(
                        "OTA bundle contains duplicate sysupgrade payload '{}'",
                        leaf
                    ));
                }

                total_payload_bytes = total_payload_bytes
                    .checked_add(size)
                    .ok_or_else(|| "OTA bundle declared payload size overflow".to_string())?;
                if total_payload_bytes > MAX_SYSUPGRADE_TOTAL_PAYLOAD_BYTES {
                    return Err(format!(
                        "OTA bundle payload bytes exceed safety ceiling ({} > {})",
                        total_payload_bytes, MAX_SYSUPGRADE_TOTAL_PAYLOAD_BYTES
                    ));
                }

                match leaf {
                    "MANIFEST.json" => {
                        archive.bundle.manifest_path = PathBuf::from(&name);
                        archive.manifest =
                            read_small_tar_entry(reader, size, MAX_SYSUPGRADE_METADATA_BYTES)?;
                    }
                    "MANIFEST.sig" => {
                        archive.bundle.signature_path = PathBuf::from(&name);
                        archive.signature =
                            read_small_tar_entry(reader, size, MAX_SYSUPGRADE_METADATA_BYTES)?;
                    }
                    "release_ed25519.pub" => {
                        archive.bundle.release_key_path = PathBuf::from(&name);
                        archive.release_key =
                            read_small_tar_entry(reader, size, MAX_SYSUPGRADE_METADATA_BYTES)?;
                        archive.observed_payloads.insert(
                            leaf.to_string(),
                            ObservedPayload {
                                path: format!("{}/{}", prefix, leaf),
                                size,
                                sha256: sha256_bytes(&archive.release_key),
                            },
                        );
                    }
                    "SHA256SUMS" => {
                        if size > MAX_SYSUPGRADE_METADATA_BYTES {
                            return Err(format!(
                                "OTA bundle metadata '{}' exceeds safety ceiling ({} > {})",
                                name, size, MAX_SYSUPGRADE_METADATA_BYTES
                            ));
                        }
                        seek_current(reader, size, &format!("tar entry '{}'", name))?;
                    }
                    _ => {
                        if size > MAX_SYSUPGRADE_IMAGE_PAYLOAD_BYTES {
                            return Err(format!(
                                "OTA bundle image payload '{}' exceeds safety ceiling ({} > {})",
                                name, size, MAX_SYSUPGRADE_IMAGE_PAYLOAD_BYTES
                            ));
                        }
                        let sha256 =
                            hash_tar_payload(reader, size, &format!("payload '{}'", name))?;
                        archive.observed_payloads.insert(
                            leaf.to_string(),
                            ObservedPayload {
                                path: format!("{}/{}", prefix, leaf),
                                size,
                                sha256,
                            },
                        );
                    }
                }
                skip_tar_padding(reader, size)?;
            }
        }
    }

    if payload_prefix.is_none() || payload_directory_count != 1 {
        return Err(
            "OTA bundle must contain exactly one canonical payload directory entry".to_string(),
        );
    }
    archive.payload_prefix = payload_prefix.unwrap_or_default();

    Ok(archive)
}

/// Fuzz-only entry point for the sysupgrade tar reader.
///
/// This deliberately exposes only the parser verdict, not the private archive
/// internals. It is compiled for unit tests and for the `dcentrald-fuzz` crate
/// via the `fuzzing` feature; production builds do not export it.
#[cfg(any(test, feature = "fuzzing"))]
pub fn fuzz_read_sysupgrade_tar_bytes(input: &[u8]) -> Result<(), String> {
    let mut cursor = std::io::Cursor::new(input);
    read_sysupgrade_tar(&mut cursor).map(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TarEntryKind {
    Regular,
    Directory,
}

fn classify_tar_entry_kind(typeflag: u8, name: &str) -> Result<TarEntryKind, String> {
    match typeflag {
        0 | b'0' => Ok(TarEntryKind::Regular),
        b'5' => Ok(TarEntryKind::Directory),
        _ => Err(format!(
            "OTA bundle tar entry '{}' has unsupported typeflag 0x{:02x}; only regular files and directories are accepted",
            name, typeflag
        )),
    }
}

fn validate_tar_header_checksum(header: &[u8; 512]) -> Result<(), String> {
    let declared = parse_tar_octal(&header[148..156])?;
    let computed: u64 = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if (148..156).contains(&index) {
                u64::from(b' ')
            } else {
                u64::from(*byte)
            }
        })
        .sum();
    if declared != computed {
        return Err(format!(
            "OTA bundle tar header checksum mismatch (declared {declared}, computed {computed})"
        ));
    }
    Ok(())
}

fn validate_tar_end_marker<R: Read>(reader: &mut R) -> Result<(), String> {
    let mut second_zero_block = [0u8; 512];
    reader
        .read_exact(&mut second_zero_block)
        .map_err(|e| format!("OTA bundle tar has a truncated single-block end marker: {e}"))?;
    if second_zero_block.iter().any(|byte| *byte != 0) {
        return Err(
            "OTA bundle tar has non-zero data where the second end-marker block is required"
                .to_string(),
        );
    }

    let mut trailing = [0u8; 4096];
    loop {
        let count = reader
            .read(&mut trailing)
            .map_err(|e| format!("OTA bundle: failed reading tar trailing padding: {e}"))?;
        if count == 0 {
            return Ok(());
        }
        if trailing[..count].iter().any(|byte| *byte != 0) {
            return Err(
                "OTA bundle tar contains non-zero trailing data after its end marker".to_string(),
            );
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum SysupgradeTarEntry<'a> {
    Directory { prefix: &'a str },
    File { prefix: &'a str, leaf: &'a str },
}

fn classify_sysupgrade_tar_entry(path: &str) -> Result<SysupgradeTarEntry<'_>, String> {
    if path.is_empty() {
        return Err("OTA bundle tar entry has empty name".to_string());
    }
    if path.starts_with('/') || path.contains('\\') {
        return Err(format!(
            "OTA bundle tar entry '{}' is not a safe relative path",
            path
        ));
    }

    let directory_path = path.ends_with('/');
    let path = path.trim_end_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    for part in &parts {
        if part.is_empty() || *part == "." || *part == ".." {
            return Err(format!(
                "OTA bundle tar entry '{}' contains an unsafe path component",
                path
            ));
        }
    }

    let prefix = parts.first().copied().unwrap_or_default();
    if !is_safe_payload_prefix(prefix) {
        return Err(format!(
            "OTA bundle tar entry '{}' has an unsafe payload directory",
            path
        ));
    }

    if parts.len() == 1 {
        if !directory_path {
            return Err(format!(
                "OTA bundle tar directory entry '{}' must use the canonical trailing slash",
                path
            ));
        }
        return Ok(SysupgradeTarEntry::Directory { prefix });
    }

    if directory_path || parts.len() != 2 {
        return Err(format!(
            "OTA bundle tar entry '{}' must be a direct sysupgrade payload file",
            path
        ));
    }

    if !is_safe_payload_leaf(parts[1]) {
        return Err(format!(
            "OTA bundle tar entry '{}' has an unsafe payload leaf",
            path
        ));
    }

    Ok(SysupgradeTarEntry::File {
        prefix,
        leaf: parts[1],
    })
}

fn is_safe_payload_leaf(leaf: &str) -> bool {
    !leaf.is_empty()
        && leaf.len() <= 128
        && leaf
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_safe_payload_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix.len() <= 128
        && prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn remember_payload_prefix(
    payload_prefix: &mut Option<String>,
    prefix: &str,
) -> Result<(), String> {
    if payload_prefix
        .as_deref()
        .map(|seen| seen != prefix)
        .unwrap_or(false)
    {
        return Err("OTA bundle contains multiple sysupgrade-* payload directories".to_string());
    }
    payload_prefix.get_or_insert_with(|| prefix.to_string());
    Ok(())
}

fn seek_current<R: Seek>(reader: &mut R, bytes: u64, label: &str) -> Result<(), String> {
    let offset = i64::try_from(bytes)
        .map_err(|_| format!("OTA bundle: {} is too large to skip safely", label))?;
    reader
        .seek(SeekFrom::Current(offset))
        .map_err(|e| format!("OTA bundle: failed skipping {}: {}", label, e))?;
    Ok(())
}

/// Stream exactly `size` bytes from `reader` through a SHA-256 hasher (in 64 KiB
/// chunks so a multi-MB payload is never held in memory) and return the lowercase
/// hex digest. Advances the reader by `size` bytes, exactly like `seek_current`,
/// so the caller's subsequent `skip_tar_padding` still lands on the block boundary.
fn hash_tar_payload<R: Read>(reader: &mut R, size: u64, label: &str) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut remaining = size;
    let mut buf = [0u8; 64 * 1024];
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        reader
            .read_exact(&mut buf[..want])
            .map_err(|e| format!("OTA bundle: failed reading {}: {}", label, e))?;
        hasher.update(&buf[..want]);
        remaining -= want as u64;
    }
    Ok(to_hex(&hasher.finalize()))
}

/// SHA-256 (lowercase hex) of a file's contents, streamed in 64 KiB chunks so a
/// multi-MB payload is never held in memory. Used by the extracted-directory
/// bundle path to bind the kernel/root files to the signed manifest.
fn hash_file(path: &Path, label: &str) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| {
        format!(
            "OTA bundle: failed to open {} '{}': {}",
            label,
            path.display(),
            e
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("OTA bundle: failed reading {}: {}", label, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

fn sha256_bytes(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    to_hex(&Sha256::digest(data))
}

/// Bind every image-bearing archive member to the authenticated manifest.
///
/// The parser accepts only flat, safe paths and bounded regular files before it
/// has authenticated `MANIFEST.json`. This second pass is the semantic gate: a
/// member is accepted only when a supported payload kind declares its exact
/// path, size (structured schema), and SHA-256. Unknown manifest kinds and
/// unmanifested image members both fail closed.
fn verify_manifest_payload_contract(
    manifest_bytes: &[u8],
    payload_prefix: &str,
    observed_payloads: &std::collections::BTreeMap<String, ObservedPayload>,
) -> Result<VerifiedPayloadContract, String> {
    let manifest = parse_authority_manifest_json(manifest_bytes)?;
    let payloads = manifest
        .get("payloads")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            "OTA bundle: MANIFEST.json must contain an object-valued payloads registry".to_string()
        })?;
    if payloads.is_empty() {
        return Err("OTA bundle: MANIFEST.json payloads registry is empty".to_string());
    }
    let requires_verification_key = manifest
        .get("manifest_profile")
        .and_then(serde_json::Value::as_str)
        == Some(SYSUPGRADE_AUTHORITY_PROFILE);

    let mut declared_leaves = std::collections::BTreeSet::<String>::new();
    let mut kernel_path: Option<PathBuf> = None;
    let mut rootfs_path: Option<PathBuf> = None;
    let mut metadata_declared = false;
    let mut verification_key_declared = false;

    for (kind, declaration) in payloads {
        let spec = supported_payload_kind(kind)
            .ok_or_else(|| format!("OTA bundle: unsupported manifest payload kind '{kind}'"))?;

        let (declared_path, declared_size, declared_sha256) = {
            let object = declaration.as_object().ok_or_else(|| {
                format!("OTA bundle: payload kind '{kind}' must be a path/size/sha256 object")
            })?;
            let path = object
                .get("path")
                .and_then(serde_json::Value::as_str)
                .filter(|path| !path.is_empty())
                .ok_or_else(|| {
                    format!("OTA bundle: payload kind '{kind}' is missing a non-empty path")
                })?
                .to_string();
            let size = object
                .get("size")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    format!("OTA bundle: payload kind '{kind}' is missing an integer size")
                })?;
            let sha256 = object
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            (path, size, sha256)
        };

        if declared_path != declared_path.trim() {
            return Err(format!(
                "OTA bundle: payload kind '{kind}' path must not contain surrounding whitespace"
            ));
        }
        if declared_size == 0 {
            return Err(format!(
                "OTA bundle: payload kind '{kind}' size must be a positive integer"
            ));
        }

        if declared_sha256.len() != 64
            || !declared_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(format!(
                "OTA bundle: payload kind '{kind}' SHA-256 must be exact lowercase hexadecimal"
            ));
        }

        let expected_prefix = format!("{payload_prefix}/");
        let leaf = declared_path
            .strip_prefix(&expected_prefix)
            .filter(|leaf| !leaf.is_empty() && !leaf.contains('/'))
            .ok_or_else(|| {
                format!(
                    "OTA bundle: payload kind '{kind}' path '{}' is outside the authenticated payload directory",
                    declared_path
                )
            })?;
        if !spec.accepted_leaves.contains(&leaf) {
            return Err(format!(
                "OTA bundle: payload kind '{kind}' does not support archive leaf '{leaf}'"
            ));
        }
        if !declared_leaves.insert(leaf.to_string()) {
            return Err(format!(
                "OTA bundle: multiple manifest payload kinds resolve to archive leaf '{leaf}'"
            ));
        }
        match kind.as_str() {
            "metadata" => metadata_declared = true,
            "verification_key" => verification_key_declared = true,
            _ => {}
        }

        let observed = observed_payloads.get(leaf).ok_or_else(|| {
            format!(
                "OTA bundle: manifest payload kind '{kind}' declares missing archive member '{leaf}'"
            )
        })?;
        if observed.path != declared_path {
            return Err(format!(
                "OTA bundle: payload kind '{kind}' path mismatch (manifest '{}', archive '{}')",
                declared_path, observed.path
            ));
        }
        if declared_size != observed.size {
            return Err(format!(
                "OTA bundle: payload kind '{kind}' size mismatch (manifest {}, archive {})",
                declared_size, observed.size
            ));
        }
        if declared_sha256 != observed.sha256 {
            return Err(format!(
                "OTA bundle: payload kind '{kind}' sha256 mismatch (manifest declares {}, computed {}) - refusing bundle whose payloads do not match the signed manifest",
                declared_sha256, observed.sha256
            ));
        }

        match spec.role {
            PayloadRole::Kernel => {
                if kernel_path.replace(PathBuf::from(&observed.path)).is_some() {
                    return Err(
                        "OTA bundle: manifest declares multiple kernel payloads".to_string()
                    );
                }
            }
            PayloadRole::Rootfs => {
                if rootfs_path.replace(PathBuf::from(&observed.path)).is_some() {
                    return Err(
                        "OTA bundle: manifest declares multiple rootfs payloads".to_string()
                    );
                }
            }
            PayloadRole::Other => {}
        }
    }

    if requires_verification_key && !verification_key_declared {
        return Err(format!(
            "OTA bundle: manifest_profile '{SYSUPGRADE_AUTHORITY_PROFILE}' requires payload kind 'verification_key' binding release_ed25519.pub"
        ));
    }
    if !metadata_declared {
        return Err(
            "OTA bundle manifest is missing required payload kind 'metadata' binding METADATA"
                .to_string(),
        );
    }

    for leaf in observed_payloads.keys() {
        // The signed registry binds every accepted archive member. Trust-anchor
        // comparison establishes who may sign; it does not authenticate an
        // otherwise undeclared embedded key's path, size, or bytes.
        if !declared_leaves.contains(leaf) {
            return Err(format!("OTA bundle contains unmanifested payload '{leaf}'"));
        }
    }

    Ok(VerifiedPayloadContract {
        kernel_path: kernel_path.ok_or_else(|| {
            "OTA bundle manifest is missing a supported kernel payload".to_string()
        })?,
        rootfs_path: rootfs_path.ok_or_else(|| {
            "OTA bundle manifest is missing a supported rootfs payload".to_string()
        })?,
    })
}

fn read_small_tar_entry<R: Read>(
    reader: &mut R,
    size: u64,
    max_size: u64,
) -> Result<Vec<u8>, String> {
    if size > max_size {
        return Err(format!(
            "OTA bundle metadata entry is too large: {} bytes",
            size
        ));
    }
    let mut buf = vec![0u8; size as usize];
    reader
        .read_exact(&mut buf)
        .map_err(|e| format!("OTA bundle: failed reading tar metadata entry: {}", e))?;
    Ok(buf)
}

fn skip_tar_padding<R: Seek>(reader: &mut R, size: u64) -> Result<(), String> {
    let padding = (512 - (size % 512)) % 512;
    if padding > 0 {
        seek_current(reader, padding, "tar padding")?;
    }
    Ok(())
}

fn tar_header_path(header: &[u8; 512]) -> Result<String, String> {
    let name = tar_path_string(&header[0..100], "name")?;
    if name.is_empty() {
        return Err("OTA bundle tar entry has empty name".to_string());
    }
    let prefix = tar_path_string(&header[345..500], "prefix")?;
    let path = if prefix.is_empty() {
        name
    } else {
        format!("{}/{}", prefix, name)
    };
    Ok(path)
}

fn tar_path_string(bytes: &[u8], field: &str) -> Result<String, String> {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    if bytes[end..].iter().any(|byte| *byte != 0) {
        return Err(format!(
            "OTA bundle tar {field} contains non-zero bytes after its terminator"
        ));
    }
    std::str::from_utf8(&bytes[..end])
        .map(str::to_string)
        .map_err(|_| format!("OTA bundle tar {field} is not valid UTF-8"))
}

fn tar_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

fn parse_tar_octal(bytes: &[u8]) -> Result<u64, String> {
    let text = tar_string(bytes);
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text.trim(), 8)
        .map_err(|e| format!("OTA bundle tar entry has invalid size '{}': {}", text, e))
}

fn verify_sysupgrade_signature_bytes(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    embedded_key_bytes: &[u8],
    pinned_release_key_path: Option<&Path>,
) -> Result<(), String> {
    let embedded_key_bytes = strip_pem_if_present(embedded_key_bytes);
    let mut trust_anchor_found = false;

    if let Some(pinned_path) = pinned_release_key_path {
        if pinned_path.is_file() {
            let pinned_bytes = std::fs::read(pinned_path).map_err(|e| {
                format!(
                    "OTA bundle: failed to read pinned release key '{}': {}",
                    pinned_path.display(),
                    e
                )
            })?;
            let pinned_bytes = strip_pem_if_present(&pinned_bytes);
            trust_anchor_found = true;
            if pinned_bytes != embedded_key_bytes {
                return Err(
                    "OTA bundle: embedded release_ed25519.pub does not match pinned \
                     /etc/dcentos/release_ed25519.pub - rejecting bundle"
                        .to_string(),
                );
            }
        }
    }

    if let Some(compiled_hex) = compiled_public_key_hex() {
        let compiled_bytes = decode_hex(compiled_hex)?;
        trust_anchor_found = true;
        if compiled_bytes != embedded_key_bytes {
            return Err(
                "OTA bundle: embedded release_ed25519.pub does not match the OTA \
                 public key compiled into this firmware build - rejecting bundle"
                    .to_string(),
            );
        }
    }

    if !trust_anchor_found {
        return Err(
            "OTA bundle: no trusted OTA public key is available in firmware or \
             /etc/dcentos/release_ed25519.pub"
                .to_string(),
        );
    }

    // ADDITIVE two-level PKI (W8 GROUP C, 2026-06-02). The embedded
    // `release_ed25519.pub` (just matched against the trust anchor) is the
    // ROOT key. If — and ONLY if — the manifest carries an `ota_intermediate_cert`
    // object, route through the two-level chain verifier:
    //
    //   root (pinned, == embedded_key_bytes)
    //     -> verify intermediate cert (root-signed + validity window + not-revoked)
    //       -> verify payload signature with the intermediate key
    //
    // A manifest WITHOUT that field falls straight through to the legacy
    // single-key direct path below, byte-identical to pre-W8 behavior.
    match parse_intermediate_cert_from_manifest(manifest_bytes)? {
        Some(cert_envelope) => verify_two_level_chain(
            &embedded_key_bytes,
            manifest_bytes,
            signature_bytes,
            &cert_envelope,
        ),
        None => {
            // Legacy single-key direct path — unchanged.
            verify_raw(&embedded_key_bytes, manifest_bytes, signature_bytes)
        }
    }
}

/// Strip a single PEM `PUBLIC KEY` envelope to raw 32-byte ed25519 if needed.
/// If the input doesn't look like PEM, returns it unchanged.
///
/// Note: shell sysupgrade stores `release_ed25519.pub` as the openssl-style
/// PEM SubjectPublicKeyInfo. ed25519-dalek wants raw 32 bytes. PEM SPKI for
/// ed25519 is a fixed 12-byte ASN.1 prefix + 32-byte raw key = 44 bytes
/// base64-encoded. We detect the `-----BEGIN PUBLIC KEY-----` header and
/// extract the trailing 32 bytes after base64-decoding.
fn strip_pem_if_present(input: &[u8]) -> Vec<u8> {
    let text = match std::str::from_utf8(input) {
        Ok(t) => t,
        Err(_) => return input.to_vec(), // binary already
    };
    if !text.contains("-----BEGIN PUBLIC KEY-----") {
        // Could be raw 32 bytes (binary) that happens to be valid utf-8, but
        // ed25519 raw keys are 32 random bytes — unlikely to be valid utf-8 in
        // practice, so the conservative thing is to return as-is.
        return input.to_vec();
    }
    let mut b64 = String::new();
    for line in text.lines() {
        if line.starts_with("-----") {
            continue;
        }
        b64.push_str(line.trim());
    }
    let decoded = decode_base64(&b64).unwrap_or_default();
    if decoded.len() >= 32 {
        decoded[decoded.len() - 32..].to_vec()
    } else {
        input.to_vec()
    }
}

/// Minimal base64 decoder (standard alphabet, `=` padding). Avoids pulling in
/// the `base64` crate just for this one call site.
fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [0u8; 256];
    for (i, &c) in ALPHABET.iter().enumerate() {
        lookup[c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &c in input.as_bytes() {
        if c == b'=' {
            break;
        }
        let pos = ALPHABET.iter().position(|&x| x == c);
        let v = pos.ok_or_else(|| format!("invalid base64 char: 0x{:02X}", c))?;
        buf = (buf << 6) | (v as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// W29 (2026-05-13): at-rest ed25519 signature pin on the stock-Bitmain
// manifest. The manifest itself is compile-time-baked into the dcentrald
// binary (see `routes::restore_to_stock::STOCK_MANIFEST_BAKED`, closed in
// W11-prep A4''-CRITICAL-1), but baked-in alone doesn't protect against
// build-pipeline tampering or post-build binary patching. Defense in depth:
// ed25519-sign the manifest at release time, bake both the manifest AND
// the signature, verify at runtime against a compile-time-pinned public key.
//
// Pin uses a SEPARATE env var (`DCENT_MANIFEST_PUBLIC_KEY_HEX`) from the OTA
// pin so the manifest key can be rotated independently. Optional opaque key id
// in `DCENT_MANIFEST_KEY_ID`.
//
// Release process to generate keys + signature is documented in
// `routes::restore_to_stock` near `STOCK_MANIFEST_SIG_BAKED`.
// ---------------------------------------------------------------------------

/// Returns true when this build was compiled with a pinned manifest public
/// key. When false, manifest signature verification is skipped at runtime
/// (the OTA pattern's startup-warning + fail-open-on-absent-pin convention).
pub fn manifest_signature_required() -> bool {
    option_env!("DCENT_MANIFEST_PUBLIC_KEY_HEX").is_some()
}

/// Optional opaque key id pinned at build time alongside the manifest
/// public key.
pub fn compiled_manifest_key_id() -> Option<&'static str> {
    option_env!("DCENT_MANIFEST_KEY_ID")
}

/// Compile-time-pinned manifest public key (hex). None when no key was
/// pinned.
pub fn compiled_manifest_public_key_hex() -> Option<&'static str> {
    option_env!("DCENT_MANIFEST_PUBLIC_KEY_HEX")
}

/// Verify an Ed25519 signature over the raw manifest bytes using the
/// compile-time-pinned manifest public key.
///
/// Returns `Err` when:
/// - `manifest_signature_required()` is false (no key was pinned). Callers
///   should check this first — it's a hard error here so a caller that
///   forgets the gate doesn't silently accept any input.
/// - Pinned key fails to hex-decode or doesn't fit a 32-byte ed25519
///   verifying key.
/// - Signature bytes don't fit a 64-byte ed25519 signature.
/// - The signature does not verify against the pinned key over the
///   provided manifest bytes.
///
/// The caller (`routes::restore_to_stock::lookup_in_stock_manifest`) gates
/// the call on `manifest_signature_required()` and only invokes this when
/// the pin is present. A test-only helper
/// (`verify_manifest_signature_with_explicit_pubkey`) takes the pubkey as
/// a function parameter so unit tests don't need a build-time env var.
pub fn verify_manifest_signature(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<(), String> {
    let public_key_hex = compiled_manifest_public_key_hex().ok_or_else(|| {
        "No manifest public key compiled into this firmware build (DCENT_MANIFEST_PUBLIC_KEY_HEX unset)"
            .to_string()
    })?;
    let public_key_bytes = decode_hex(public_key_hex)?;
    verify_manifest_signature_with_explicit_pubkey(
        manifest_bytes,
        signature_bytes,
        &public_key_bytes,
    )
}

/// Test-only friendly helper: verify with an explicit pubkey passed as a
/// parameter so unit tests don't need to set
/// `DCENT_MANIFEST_PUBLIC_KEY_HEX` at build time. Production callers should
/// prefer `verify_manifest_signature` so the compile-time pin is enforced.
///
/// The runtime verification logic is identical to `verify_raw` — pubkey
/// decoded as 32 bytes, signature as 64 bytes, ed25519 verify over the
/// supplied message bytes.
pub fn verify_manifest_signature_with_explicit_pubkey(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    public_key_bytes: &[u8],
) -> Result<(), String> {
    let public_key_array: [u8; 32] = public_key_bytes
        .try_into()
        .map_err(|_| "manifest public key must decode to 32 bytes".to_string())?;
    let verifying_key = VerifyingKey::from_bytes(&public_key_array)
        .map_err(|e| format!("Invalid manifest public key: {}", e))?;
    let signature = Signature::try_from(signature_bytes)
        .map_err(|e| format!("Invalid manifest signature: {}", e))?;
    verifying_key
        .verify(manifest_bytes, &signature)
        .map_err(|e| format!("Manifest signature verification failed: {}", e))
}

/// Comparison helper from the BitAxe sibling — DCENT_OS doesn't enforce a
/// rollback floor today, but keeping the implementation here avoids drift if
/// we later wire it in.
pub fn version_is_newer(candidate: &str, current: &str) -> bool {
    fn parse(version: &str) -> Vec<u32> {
        version
            .trim_start_matches('v')
            .split(['.', '-'])
            .filter_map(|part| part.parse::<u32>().ok())
            .collect()
    }

    let candidate = parse(candidate);
    let current = parse(current);
    let len = candidate.len().max(current.len());

    for idx in 0..len {
        let a = *candidate.get(idx).unwrap_or(&0);
        let b = *current.get(idx).unwrap_or(&0);
        if a > b {
            return true;
        }
        if a < b {
            return false;
        }
    }

    false
}

// ===========================================================================
// Two-level OTA PKI: compile-pinned ROOT key signs rotatable INTERMEDIATE
// keys; an intermediate key (carried in the manifest with a validity window
// + a revocation list) signs the OTA payload. (W8 GROUP C, 2026-06-02.)
//
// WHY: today an OTA key can only be rotated by reflashing the firmware (the
// trust anchor is the compile-time `DCENT_OTA_PUBLIC_KEY_HEX` pin and/or the
// on-disk `/etc/dcentos/release_ed25519.pub`). VNish/stock ship a two-level
// chain so the day-to-day signing key can rotate WITHOUT a full reflash. This
// closes that parity gap ADDITIVELY:
//
//   root (pinned)  ->  intermediate (rotation key, manifest-carried)  ->  payload
//
// BACK-COMPAT / BRICK-SAFETY CONTRACT (load-bearing — do NOT weaken):
//   * A manifest WITHOUT an `ota_intermediate_cert` field verifies EXACTLY as
//     before (single-key direct path). `parse_intermediate_cert_from_manifest`
//     returns `Ok(None)` and the caller runs the legacy `verify_raw`. The
//     gate-off path is byte-identical to today.
//   * A malformed / expired / not-yet-valid / revoked / wrong-root cert chain
//     => REJECT the OTA (Err). Refusing a bad update never bricks a running
//     unit — it simply does not update.
//   * The ROOT key is the SAME trust anchor the legacy path already matched
//     against the embedded `release_ed25519.pub` (so deployments that pin a
//     root and ship a root-signed intermediate need no new key plumbing).
//   * A dedicated root pin env var `DCENT_OTA_ROOT_PUBLIC_KEY_HEX` is also
//     honored when present: if set, the cert's declared root key MUST equal it
//     (defense in depth — lets an operator pin the root distinctly from the
//     leaf release key). When unset, the embedded/anchored release key is the
//     root, preserving the existing single-pin deployment model.
// ===========================================================================

/// Optional compile-time pin for the OTA ROOT key (hex), distinct from the
/// leaf `DCENT_OTA_PUBLIC_KEY_HEX`. When present, a manifest-carried
/// intermediate cert's declared `root` key MUST equal this pin (in addition to
/// matching the trust-anchored embedded key). When absent, the embedded/
/// trust-anchored release key is treated as the root — preserving the existing
/// single-pin deployment model.
pub fn compiled_root_public_key_hex() -> Option<&'static str> {
    option_env!("DCENT_OTA_ROOT_PUBLIC_KEY_HEX")
}

/// Parsed intermediate certificate envelope carried inside `MANIFEST.json`.
///
/// Wire shape (all hex strings are lowercase, no `0x`):
/// ```json
/// {
///   "ota_intermediate_cert": {
///     "root_key_hex":        "<32-byte ed25519 root pubkey, hex>",
///     "intermediate_key_hex":"<32-byte ed25519 intermediate pubkey, hex>",
///     "not_before":          1700000000,
///     "not_after":           1800000000,
///     "serial":              "rot-2026-06",
///     "root_signature_hex":  "<64-byte ed25519 sig by ROOT over the canonical cert bytes>"
///   },
///   "ota_revoked_intermediates": ["<intermediate_key_hex>", "rot-2025-01", ...]
/// }
/// ```
/// `serial` and the revocation list are optional. A revoked intermediate is
/// matched by EITHER its `serial` OR its `intermediate_key_hex`.
#[derive(Debug, Clone)]
pub struct IntermediateCertEnvelope {
    pub root_key: Vec<u8>,
    pub intermediate_key: Vec<u8>,
    pub not_before: i64,
    pub not_after: i64,
    pub serial: Option<String>,
    pub root_signature: Vec<u8>,
    /// Revocation list pulled from the SAME manifest (defense-in-depth; the
    /// on-disk list is consulted in addition, see `revocation_list_paths`).
    pub manifest_revocations: Vec<String>,
}

/// Canonical bytes the ROOT key signs to authorize an intermediate cert. MUST
/// stay byte-stable across the signing tool and this verifier. Deliberately a
/// flat, newline-delimited `key=value` form (same family as
/// `canonical_message`) rather than re-serialized JSON, so signature validity
/// never depends on JSON field ordering / whitespace.
pub fn canonical_intermediate_cert_message(
    root_key_hex: &str,
    intermediate_key_hex: &str,
    not_before: i64,
    not_after: i64,
    serial: Option<&str>,
) -> String {
    format!(
        "schema=1\ntype=ota-intermediate-cert\nroot={}\nintermediate={}\nnot_before={}\nnot_after={}\nserial={}\n",
        root_key_hex.to_ascii_lowercase(),
        intermediate_key_hex.to_ascii_lowercase(),
        not_before,
        not_after,
        serial.unwrap_or(""),
    )
}

/// Parse the optional `ota_intermediate_cert` (+ `ota_revoked_intermediates`)
/// out of the raw manifest JSON.
///
/// * `Ok(None)`  — no `ota_intermediate_cert` field => legacy single-key path
///                 (byte-identical to today).
/// * `Ok(Some)`  — a well-formed cert envelope to route through the chain.
/// * `Err`       — the field is present but malformed (missing/short keys, bad
///                 hex, non-integer window). FAIL CLOSED: a present-but-broken
///                 cert must never silently fall back to the single-key path
///                 (that would let an attacker strip the chain by corrupting
///                 the cert). The manifest itself is already size-bounded by
///                 the tar reader (`MAX_METADATA_BYTES`).
pub fn parse_intermediate_cert_from_manifest(
    manifest_bytes: &[u8],
) -> Result<Option<IntermediateCertEnvelope>, String> {
    #[derive(serde::Deserialize)]
    struct RawCert {
        root_key_hex: String,
        intermediate_key_hex: String,
        not_before: i64,
        not_after: i64,
        #[serde(default)]
        serial: Option<String>,
        root_signature_hex: String,
    }

    // BACK-COMPAT: the legacy single-key path never required MANIFEST.json to
    // be JSON-parseable at all (it ed25519-verifies the raw bytes regardless).
    // If the whole manifest is not valid JSON, there cannot be an
    // `ota_intermediate_cert`, so return Ok(None) and let the caller run the
    // unchanged legacy direct path — we do NOT newly reject a bundle here.
    let root_value = match parse_unique_manifest_json(manifest_bytes) {
        Ok(value) => value,
        Err(ManifestJsonError::Malformed(_)) => return Ok(None),
        Err(ManifestJsonError::DuplicateObjectKey(key)) => {
            return Err(format!(
                "OTA two-level cert: MANIFEST.json contains duplicate object key '{key}'"
            ))
        }
    };

    // No `ota_intermediate_cert` key => legacy single-key path (Ok(None)).
    let cert_value = match root_value.get("ota_intermediate_cert") {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(v) => v,
    };

    // The key IS present: from here on, ANY problem FAILS CLOSED (Err), so a
    // chain can't be stripped by corrupting the cert into a non-deserializable
    // shape.
    let raw: RawCert = serde_json::from_value(cert_value.clone())
        .map_err(|e| format!("OTA two-level cert: malformed ota_intermediate_cert object: {e}"))?;

    let manifest_revocations_raw: Vec<String> = match root_value.get("ota_revoked_intermediates") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            format!("OTA two-level cert: malformed ota_revoked_intermediates list: {e}")
        })?,
    };

    let root_key = decode_hex(&raw.root_key_hex)
        .map_err(|e| format!("OTA two-level cert: bad root_key_hex: {e}"))?;
    if root_key.len() != 32 {
        return Err(format!(
            "OTA two-level cert: root key must be 32 bytes, got {}",
            root_key.len()
        ));
    }
    let intermediate_key = decode_hex(&raw.intermediate_key_hex)
        .map_err(|e| format!("OTA two-level cert: bad intermediate_key_hex: {e}"))?;
    if intermediate_key.len() != 32 {
        return Err(format!(
            "OTA two-level cert: intermediate key must be 32 bytes, got {}",
            intermediate_key.len()
        ));
    }
    let root_signature = decode_hex(&raw.root_signature_hex)
        .map_err(|e| format!("OTA two-level cert: bad root_signature_hex: {e}"))?;
    if root_signature.len() != 64 {
        return Err(format!(
            "OTA two-level cert: root signature must be 64 bytes, got {}",
            root_signature.len()
        ));
    }
    if raw.not_after < raw.not_before {
        return Err(format!(
            "OTA two-level cert: validity window is inverted (not_after {} < not_before {})",
            raw.not_after, raw.not_before
        ));
    }

    Ok(Some(IntermediateCertEnvelope {
        root_key,
        intermediate_key,
        not_before: raw.not_before,
        not_after: raw.not_after,
        serial: raw
            .serial
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        root_signature,
        manifest_revocations: manifest_revocations_raw
            .into_iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
    }))
}

/// On-disk revocation list paths the verifier consults IN ADDITION to the
/// manifest-carried list. Each file is a newline-delimited list of revoked
/// intermediate identifiers (serial OR lowercase intermediate-key hex); blank
/// lines and `#` comment lines are ignored. A missing file is not an error
/// (no revocations from that source). Kept as a function (not a const) so the
/// path stays in one place and is easy to test/override.
fn revocation_list_paths() -> &'static [&'static str] {
    &["/etc/dcentos/ota_revoked_intermediates"]
}

fn load_on_disk_revocations() -> Vec<String> {
    let mut out = Vec::new();
    for path in revocation_list_paths() {
        let p = Path::new(path);
        if !p.is_file() {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(p) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                out.push(line.to_ascii_lowercase());
            }
        }
    }
    out
}

/// Current wall-clock time in UNIX seconds, for the validity-window check.
/// Isolated so tests can reason about it; the chain verifier accepts an
/// explicit `now` to stay deterministic.
fn unix_now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Verify a full root -> intermediate -> payload chain.
///
/// `trusted_root_key`: the trust-anchored embedded `release_ed25519.pub` (raw
/// 32 bytes) — already proven to match the compile-time and/or on-disk pin by
/// the caller. The cert's declared `root_key` MUST equal this (and, if the
/// `DCENT_OTA_ROOT_PUBLIC_KEY_HEX` pin is set, MUST also equal that).
///
/// Order (each step fail-closed):
///   1. cert.root_key == trusted_root_key  (and == the optional root pin)
///   2. root signature over the canonical cert bytes verifies under root_key
///   3. now within [not_before, not_after]
///   4. intermediate not revoked (manifest list ∪ on-disk list; by serial OR key)
///   5. payload signature verifies under the intermediate key
pub fn verify_two_level_chain(
    trusted_root_key: &[u8],
    manifest_bytes: &[u8],
    payload_signature_bytes: &[u8],
    cert: &IntermediateCertEnvelope,
) -> Result<(), String> {
    verify_two_level_chain_at(
        trusted_root_key,
        manifest_bytes,
        payload_signature_bytes,
        cert,
        unix_now_seconds(),
        &load_on_disk_revocations(),
    )
}

/// Deterministic core of `verify_two_level_chain` — `now` and the on-disk
/// revocation list are injected so unit tests don't depend on wall-clock or
/// `/etc`. Production callers use `verify_two_level_chain`.
pub fn verify_two_level_chain_at(
    trusted_root_key: &[u8],
    manifest_bytes: &[u8],
    payload_signature_bytes: &[u8],
    cert: &IntermediateCertEnvelope,
    now_unix_seconds: i64,
    on_disk_revocations: &[String],
) -> Result<(), String> {
    // 1) The cert's declared root MUST be the trust-anchored root key. This is
    //    what prevents a "wrong-root" cert (signed by an attacker's own root)
    //    from being accepted.
    if cert.root_key.as_slice() != trusted_root_key {
        return Err(
            "OTA two-level cert: declared root key does not match the trusted/pinned \
             release root key — rejecting chain"
                .to_string(),
        );
    }
    // Optional distinct root pin (defense in depth).
    if let Some(root_pin_hex) = compiled_root_public_key_hex() {
        let root_pin = decode_hex(root_pin_hex).map_err(|e| {
            format!("OTA two-level cert: bad DCENT_OTA_ROOT_PUBLIC_KEY_HEX pin: {e}")
        })?;
        if root_pin != cert.root_key {
            return Err(
                "OTA two-level cert: declared root key does not match the compile-time \
                 DCENT_OTA_ROOT_PUBLIC_KEY_HEX pin — rejecting chain"
                    .to_string(),
            );
        }
    }

    // 2) Root must have signed the canonical cert bytes.
    let cert_msg = canonical_intermediate_cert_message(
        &to_hex(&cert.root_key),
        &to_hex(&cert.intermediate_key),
        cert.not_before,
        cert.not_after,
        cert.serial.as_deref(),
    );
    verify_raw(&cert.root_key, cert_msg.as_bytes(), &cert.root_signature).map_err(|e| {
        format!("OTA two-level cert: root signature over the intermediate cert is invalid: {e}")
    })?;

    // 3) Validity window.
    if now_unix_seconds < cert.not_before {
        return Err(format!(
            "OTA two-level cert: intermediate is not yet valid (now {} < not_before {})",
            now_unix_seconds, cert.not_before
        ));
    }
    if now_unix_seconds > cert.not_after {
        return Err(format!(
            "OTA two-level cert: intermediate has expired (now {} > not_after {})",
            now_unix_seconds, cert.not_after
        ));
    }

    // 4) Revocation: union of the manifest-carried list and the on-disk list.
    //    Match by serial OR by intermediate-key hex (both lowercased).
    let intermediate_key_hex = to_hex(&cert.intermediate_key);
    let serial_lower = cert.serial.as_deref().map(|s| s.to_ascii_lowercase());
    let is_revoked = cert
        .manifest_revocations
        .iter()
        .chain(on_disk_revocations.iter())
        .any(|entry| {
            entry.as_str() == intermediate_key_hex.as_str()
                || serial_lower
                    .as_deref()
                    .map(|s| s == entry.as_str())
                    .unwrap_or(false)
        });
    if is_revoked {
        return Err(format!(
            "OTA two-level cert: intermediate key (serial={:?}) is REVOKED — rejecting chain",
            cert.serial
        ));
    }

    // 5) Payload signature under the intermediate key.
    verify_raw(
        &cert.intermediate_key,
        manifest_bytes,
        payload_signature_bytes,
    )
    .map_err(|e| {
        format!(
            "OTA two-level cert: payload signature does not verify under the intermediate key: {e}"
        )
    })
}

/// Lowercase hex encoder for the canonical cert message. (We already have a
/// hex DECODER `decode_hex`; the verifier re-encodes the raw key bytes to
/// rebuild the exact signed message, so a tiny encoder avoids a crate dep.)
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SYSUPGRADE_METADATA: &[u8] = b"metadata";

    #[test]
    fn classify_sysupgrade_tar_entry_rejects_all_hostile_member_names() {
        // Security pin (priority 5: upgrade reliability). classify_sysupgrade_tar_entry
        // is the extraction ALLOWLIST for a signed sysupgrade bundle: a malicious tar
        // member with path-traversal (`..`), an absolute path, a backslash, a nested
        // directory, or an unsafe component MUST be rejected. Safe candidate leaves
        // are intentionally classified before authentication; the signed manifest
        // payload registry is the later semantic allowlist.

        // Positive: the only two accepted shapes.
        assert!(matches!(
            classify_sysupgrade_tar_entry("sysupgrade-am1-s9/"),
            Ok(SysupgradeTarEntry::Directory { .. })
        ));
        for leaf in [
            "kernel",
            "root",
            "METADATA",
            "SHA256SUMS",
            "MANIFEST.json",
            "MANIFEST.sig",
            "release_ed25519.pub",
            "fpga_bitstream.bit",
            "uImage",
            "rootfs.gz",
        ] {
            let p = format!("sysupgrade-am1-s9/{leaf}");
            assert!(
                matches!(
                    classify_sysupgrade_tar_entry(&p),
                    Ok(SysupgradeTarEntry::File { .. })
                ),
                "expected {p} to classify as a File"
            );
        }

        // Hostile: every one of these MUST be Err.
        for hostile in [
            "",
            ".",
            "..",
            "/",
            "/etc/passwd",
            "//etc",
            "..\\windows\\system32",
            "sysupgrade-x/../../etc/passwd",
            "sysupgrade-x/..",
            "../sysupgrade-x/kernel",
            "sysupgrade-x/sub/kernel",   // nested (parts != 2)
            "sysupgrade-x/kernel/",      // dir-shaped file
            "sysupgrade-x\\kernel",      // backslash
            "./sysupgrade-x/kernel",     // non-canonical relative alias
            "sysupgrade-x/payload name", // unsafe whitespace
            "sysupgrade-x/payload:$",    // unsafe punctuation
        ] {
            assert!(
                classify_sysupgrade_tar_entry(hostile).is_err(),
                "hostile member '{hostile}' was NOT rejected"
            );
        }

        // Fuzz: no random path may panic, and any escape-capable path (leading '/',
        // a backslash, or a '.'/'..' component) may NEVER classify as Ok.
        let mut lcg: u64 = 0x0F1E_2D3C_4B5A_6978;
        let mut next = || {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (lcg >> 33) as u32
        };
        let seg = |n: u32| match n % 7 {
            0 => "..",
            1 => ".",
            2 => "sysupgrade-x",
            3 => "kernel",
            4 => "",
            5 => "sub",
            _ => "root",
        };
        for _ in 0..8000u32 {
            let nparts = 1 + (next() % 4) as usize;
            let lead = if next() % 3 == 0 { "/" } else { "" };
            let mut p = String::from(lead);
            for i in 0..nparts {
                if i > 0 {
                    p.push(if next() % 5 == 0 { '\\' } else { '/' });
                }
                p.push_str(seg(next()));
            }
            let r = classify_sysupgrade_tar_entry(&p); // must not panic
            let norm = p.trim_start_matches("./");
            let escapes = norm.starts_with('/')
                || norm.contains('\\')
                || norm.split('/').any(|c| c == ".." || c == ".");
            if escapes {
                assert!(r.is_err(), "escape-capable path '{p}' classified Ok: {r:?}");
            }
        }
    }
    use ed25519_dalek::{Signer, SigningKey};
    use std::io::Cursor;

    fn make_key() -> SigningKey {
        // Deterministic key for unit tests — never used in production.
        let seed: [u8; 32] = [42u8; 32];
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn canonical_message_byte_identical_to_bitaxe() {
        let msg = canonical_message("am1-s9", "0.20.1", 18874368, "deadbeef");
        assert_eq!(
            msg,
            "schema=1\nboard_target=am1-s9\nversion=0.20.1\nsize=18874368\nsha256=deadbeef\n"
        );
    }

    /// WAVE 0 STABILIZE (2026-06-05) — Task 5: OTA honesty. When NO trust
    /// anchor exists (no compile-time pin AND no on-disk
    /// /etc/dcentos/release_ed25519.pub), the daemon must NOT claim a signature
    /// gate — it reports `inert_no_key`. With either anchor present it reports
    /// `enforced`. This is the pure derivation the REST builders consume; pin
    /// every cell of the truth table so a future edit can't silently re-claim a
    /// gate that would reject every update.
    #[test]
    fn ota_signature_state_is_inert_only_when_no_key_anywhere() {
        // No key anywhere → INERT, NOT a claimed gate.
        let inert = ota_signature_state_from(false, false);
        assert_eq!(inert, OtaSignatureState::InertNoKey);
        assert!(!inert.is_enforced(), "no-key state must NOT be enforced");
        assert_eq!(inert.as_str(), "inert_no_key");

        // On-disk key only (the shipped overlay key, no compile-time pin) →
        // ENFORCED.
        let on_disk_only = ota_signature_state_from(false, true);
        assert_eq!(on_disk_only, OtaSignatureState::Enforced);
        assert!(on_disk_only.is_enforced());
        assert_eq!(on_disk_only.as_str(), "enforced");

        // Compile-time pin only → ENFORCED.
        let compiled_only = ota_signature_state_from(true, false);
        assert_eq!(compiled_only, OtaSignatureState::Enforced);
        assert!(compiled_only.is_enforced());

        // Both anchors → ENFORCED.
        assert!(ota_signature_state_from(true, true).is_enforced());
    }

    /// `honest_key_id()` must NEVER surface a key id when no key is pinned —
    /// claiming a `keyId` while OTA is inert is exactly the dishonesty the
    /// audit flagged. In the host test build no `DCENT_OTA_PUBLIC_KEY_HEX` is
    /// set, so this must be `None`.
    #[test]
    fn honest_key_id_is_none_without_a_pinned_key() {
        // This test build has no compiled OTA pin → no key id is honest.
        assert!(
            compiled_public_key_hex().is_none(),
            "test precondition: this build must not pin an OTA key"
        );
        assert!(
            honest_key_id().is_none(),
            "honest_key_id must be None when no OTA public key is compiled in"
        );
        // And the live runtime state on the host (no /etc key either) is inert.
        assert_eq!(ota_signature_state(), OtaSignatureState::InertNoKey);
    }

    #[test]
    fn verify_raw_accepts_known_good_signature() {
        let signing_key = make_key();
        let public_key = signing_key.verifying_key();
        let message = b"hello dcentos sysupgrade";
        let signature = signing_key.sign(message);

        verify_raw(
            public_key.as_bytes(),
            message,
            signature.to_bytes().as_slice(),
        )
        .expect("known-good signature must verify");
    }

    #[test]
    fn verify_raw_rejects_tampered_message() {
        let signing_key = make_key();
        let public_key = signing_key.verifying_key();
        let message = b"hello dcentos sysupgrade";
        let signature = signing_key.sign(message);

        let mut tampered = message.to_vec();
        tampered[0] ^= 0x01;
        let err = verify_raw(
            public_key.as_bytes(),
            &tampered,
            signature.to_bytes().as_slice(),
        )
        .expect_err("tampered message must fail verification");
        assert!(err.contains("Ed25519 verification failed"), "err = {err}");
    }

    #[test]
    fn verify_raw_rejects_wrong_key() {
        let signing_key = make_key();
        let message = b"hello dcentos sysupgrade";
        let signature = signing_key.sign(message);

        let other_seed: [u8; 32] = [7u8; 32];
        let other_key = SigningKey::from_bytes(&other_seed);
        let err = verify_raw(
            other_key.verifying_key().as_bytes(),
            message,
            signature.to_bytes().as_slice(),
        )
        .expect_err("wrong key must fail verification");
        assert!(err.contains("Ed25519 verification failed"), "err = {err}");
    }

    #[test]
    fn version_is_newer_handles_dcentos_format() {
        assert!(version_is_newer("0.20.1", "0.20.0"));
        assert!(!version_is_newer("0.20.0", "0.20.1"));
        assert!(!version_is_newer("0.20.0", "0.20.0"));
        assert!(version_is_newer("v1.0.0", "0.99.99"));
    }

    #[test]
    fn strip_pem_extracts_raw_ed25519_key_bytes() {
        // Real PEM structure: -----BEGIN PUBLIC KEY-----, base64 SPKI, end.
        // For ed25519 the 12-byte ASN.1 prefix is fixed; we only care that the
        // last 32 bytes match the original raw key.
        let signing = make_key();
        let raw_pubkey = signing.verifying_key().to_bytes();
        // Build a fake SPKI: 12 prefix bytes + 32 key bytes (we don't need a
        // real ASN.1 prefix here — strip_pem_if_present just takes the trailing
        // 32 bytes).
        let mut spki = vec![0u8; 12];
        spki.extend_from_slice(&raw_pubkey);
        let b64 = base64_encode(&spki);
        let pem = format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            b64
        );
        let stripped = strip_pem_if_present(pem.as_bytes());
        assert_eq!(stripped, raw_pubkey.to_vec());
    }

    fn append_tar_entry(tar: &mut Vec<u8>, name: &str, typeflag: u8, payload: &[u8]) {
        let mut header = [0u8; 512];
        assert!(name.len() <= 100);
        header[0..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");

        let size = format!("{:011o}\0", payload.len());
        header[124..136].copy_from_slice(size.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = typeflag;
        let checksum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
        let checksum_field = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum_field.as_bytes());

        tar.extend_from_slice(&header);
        tar.extend_from_slice(payload);
        let padding = (512 - (payload.len() % 512)) % 512;
        tar.extend(std::iter::repeat_n(0u8, padding));
    }

    fn finish_tar(mut tar: Vec<u8>) -> Cursor<Vec<u8>> {
        tar.extend(std::iter::repeat_n(0u8, 1024));
        Cursor::new(tar)
    }

    fn valid_sysupgrade_tar() -> Cursor<Vec<u8>> {
        let mut tar = Vec::new();
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/", b'5', &[]);
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/kernel", b'0', b"kernel");
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/root", b'0', b"rootfs");
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/METADATA", b'0', b"meta");
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/SHA256SUMS", b'0', b"hashes");
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/MANIFEST.json", b'0', b"{}");
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/MANIFEST.sig", b'0', &[1u8; 64]);
        append_tar_entry(
            &mut tar,
            "sysupgrade-am1-s9/release_ed25519.pub",
            b'0',
            &[2u8; 32],
        );
        finish_tar(tar)
    }

    #[test]
    fn read_sysupgrade_tar_accepts_direct_payload_layout() {
        let mut tar = valid_sysupgrade_tar();
        let archive = read_sysupgrade_tar(&mut tar).expect("valid sysupgrade tar should parse");

        assert_eq!(
            archive.bundle.manifest_path,
            PathBuf::from("sysupgrade-am1-s9/MANIFEST.json")
        );
        assert_eq!(archive.payload_prefix, "sysupgrade-am1-s9");
        assert_eq!(
            archive.observed_payloads["kernel"].sha256,
            sha256_hex(b"kernel")
        );
        assert_eq!(
            archive.observed_payloads["root"].sha256,
            sha256_hex(b"rootfs")
        );
        assert_eq!(
            archive.observed_payloads["METADATA"].sha256,
            sha256_hex(b"meta")
        );
        assert_eq!(archive.manifest, b"{}");
        assert_eq!(archive.signature, vec![1u8; 64]);
        assert_eq!(archive.release_key, vec![2u8; 32]);
    }

    #[test]
    fn read_sysupgrade_tar_rejects_entries_outside_payload_dir() {
        let mut tar = Vec::new();
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/", b'5', &[]);
        append_tar_entry(&mut tar, "README.txt", b'0', b"not allowed");

        let err = read_sysupgrade_tar(&mut finish_tar(tar))
            .expect_err("outside payload entry must be rejected");
        assert!(err.contains("canonical trailing slash"), "err = {err}");
    }

    #[test]
    fn read_sysupgrade_tar_requires_one_exact_canonical_directory_entry() {
        let mut missing = Vec::new();
        append_tar_entry(&mut missing, "sysupgrade-am1-s9/kernel", b'0', b"kernel");

        let mut duplicate = Vec::new();
        append_tar_entry(&mut duplicate, "sysupgrade-am1-s9/", b'5', &[]);
        append_tar_entry(&mut duplicate, "sysupgrade-am1-s9/", b'5', &[]);

        let mut missing_slash = Vec::new();
        append_tar_entry(&mut missing_slash, "sysupgrade-am1-s9", b'5', &[]);

        for (label, tar) in [
            ("missing", missing),
            ("duplicate", duplicate),
            ("missing trailing slash", missing_slash),
        ] {
            let err = read_sysupgrade_tar(&mut finish_tar(tar))
                .expect_err("non-canonical payload directory shape must reject");
            assert!(
                err.contains("canonical payload directory")
                    || err.contains("canonical trailing slash"),
                "{label}: {err}"
            );
        }
    }

    #[test]
    fn read_sysupgrade_tar_rejects_duplicate_payload_files() {
        let mut tar = Vec::new();
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/", b'5', &[]);
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/kernel", b'0', b"one");
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/kernel", b'0', b"two");

        let err = read_sysupgrade_tar(&mut finish_tar(tar))
            .expect_err("duplicate payload file must be rejected");
        assert!(
            err.contains("duplicate sysupgrade payload 'kernel'"),
            "err = {err}"
        );
    }

    #[test]
    fn read_sysupgrade_tar_rejects_non_regular_payload_files() {
        let mut tar = Vec::new();
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/", b'5', &[]);
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/kernel", b'2', b"");

        let err = read_sysupgrade_tar(&mut finish_tar(tar))
            .expect_err("symlink payload entry must be rejected");
        assert!(err.contains("unsupported typeflag 0x32"), "err = {err}");
    }

    #[test]
    fn read_sysupgrade_tar_rejects_corrupt_header_checksum() {
        let mut bytes = valid_sysupgrade_tar().into_inner();
        bytes[10] ^= 0x01;
        let err = read_sysupgrade_tar(&mut Cursor::new(bytes))
            .expect_err("corrupt tar header checksum must reject");
        assert!(err.contains("header checksum mismatch"), "err = {err}");
    }

    #[test]
    fn read_sysupgrade_tar_requires_two_zero_end_blocks() {
        let mut bytes = valid_sysupgrade_tar().into_inner();
        bytes.truncate(bytes.len() - 512);
        let err = read_sysupgrade_tar(&mut Cursor::new(bytes))
            .expect_err("single zero end block must reject");
        assert!(err.contains("single-block end marker"), "err = {err}");
    }

    #[test]
    fn read_sysupgrade_tar_rejects_appended_archive_data() {
        let mut bytes = valid_sysupgrade_tar().into_inner();
        let appended = valid_sysupgrade_tar().into_inner();
        bytes.extend_from_slice(&appended);
        let err = read_sysupgrade_tar(&mut Cursor::new(bytes))
            .expect_err("appended second archive must reject");
        assert!(err.contains("non-zero trailing data"), "err = {err}");
    }

    #[test]
    fn read_sysupgrade_tar_accepts_zero_record_padding_after_end_marker() {
        let mut bytes = valid_sysupgrade_tar().into_inner();
        bytes.extend(std::iter::repeat_n(0u8, 10 * 512));
        read_sysupgrade_tar(&mut Cursor::new(bytes))
            .expect("zero record padding after the two-block end marker must accept");
    }

    #[test]
    fn read_sysupgrade_tar_enforces_entry_count_ceiling() {
        let mut tar = Vec::new();
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/", b'5', &[]);
        for index in 0..MAX_SYSUPGRADE_TAR_ENTRIES {
            append_tar_entry(
                &mut tar,
                &format!("sysupgrade-am1-s9/payload-{index}"),
                b'0',
                b"x",
            );
        }
        let err = read_sysupgrade_tar(&mut finish_tar(tar))
            .expect_err("entry count beyond safety ceiling must reject");
        assert!(err.contains("too many tar entries"), "err = {err}");
    }

    #[test]
    fn read_sysupgrade_tar_enforces_image_payload_size_ceiling_before_read() {
        let mut tar = Vec::new();
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/", b'5', &[]);
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/oversized", b'0', b"");

        let header = &mut tar[512..1024];
        let size_field = format!("{:011o}\0", MAX_SYSUPGRADE_IMAGE_PAYLOAD_BYTES + 1);
        header[124..136].copy_from_slice(size_field.as_bytes());
        header[148..156].fill(b' ');
        let checksum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
        let checksum_field = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum_field.as_bytes());

        let err = read_sysupgrade_tar(&mut finish_tar(tar))
            .expect_err("oversized image payload must reject before attempting to read it");
        assert!(
            err.contains("image payload") && err.contains("safety ceiling"),
            "err = {err}"
        );
    }

    #[test]
    fn read_sysupgrade_tar_never_panics_on_arbitrary_input() {
        use std::io::Cursor;
        // read_sysupgrade_tar parses an UNTRUSTED tar (the OTA upload) BEFORE the
        // Ed25519 signature is verified, so it must never panic / overflow / OOM on
        // adversarial bytes — only ever return Ok or a clean Err. The specific
        // hostile-member / traversal / symlink / duplicate cases are pinned above;
        // this fuzzes the raw byte parser like the pool / cgminer / serial-chain
        // never-panics fuzzes. Deterministic LCG — no RNG dependency.
        let mut lcg: u64 = 0x0BAD_C0DE_F00D_1234;
        let mut next = || {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (lcg >> 33) as u32
        };
        for _ in 0..3000 {
            let len = (next() % 2000) as usize;
            let choice = next() % 3;
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                buf.push(match choice {
                    0 => (next() & 0xFF) as u8,      // uniform random
                    1 => 0u8,                        // all-zero (empty tar blocks)
                    _ => 48u8 + (next() % 64) as u8, // ascii-ish (stresses octal size fields)
                });
            }
            // Must not panic; the Result value is irrelevant.
            let _ = read_sysupgrade_tar(&mut Cursor::new(buf));
        }
        // Structured edge cases: a lone zero block and an all-'7' (octal) block.
        let _ = read_sysupgrade_tar(&mut Cursor::new(vec![0u8; 512]));
        let _ = read_sysupgrade_tar(&mut Cursor::new(vec![b'7'; 600]));
    }

    #[test]
    fn ota_sysupgrade_tar_fuzz_corpus_replays_under_cargo_test() {
        const CORPUS: &[(&str, &[u8])] = &[(
            "zero-block.tar",
            include_bytes!("../../fuzz/corpus/ota_sysupgrade_tar/zero-block.tar"),
        )];

        for (name, bytes) in CORPUS {
            let _ = fuzz_read_sysupgrade_tar_bytes(bytes)
                .map_err(|err| assert!(!err.is_empty(), "{name}: empty parser error"));
        }
    }

    /// Tiny base64 encoder for the test above only — not exposed in the
    /// public API. Standard alphabet, no padding handling needed because we
    /// hand it 44-byte input which is divisible by 3.
    fn base64_encode(input: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            out.push(ALPHABET[(b0 >> 2) as usize] as char);
            out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[(b2 & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    #[test]
    fn signature_required_reflects_compile_time_pin() {
        // In CI/host tests we don't set DCENT_OTA_PUBLIC_KEY_HEX, so this must
        // be false. (If you set it locally, this test will skip its assertion.)
        if compiled_public_key_hex().is_none() {
            assert!(!signature_required());
        }
    }

    // -----------------------------------------------------------------------
    // End-to-end fail-closed contract for `verify_sysupgrade_bundle()` on the
    // production `.tar` path (Security productionization sweep CRITICAL 4,
    // 2026-05-21). The production browser-upload caller
    // (`rest.rs::system_upgrade`) invokes the verifier with
    // `allow_unsigned = false` and an on-disk pinned key path
    // (`/etc/dcentos/release_ed25519.pub`). These tests pin the matrix the
    // sweep asked about: missing sig → reject, missing/absent pinned key
    // (no trust anchor) → reject, embedded-key-mismatch → reject, mismatched
    // signature → reject, valid sig + trusted on-disk key → accept. They prove
    // the verifier does NOT fail open when no compile-time `DCENT_OTA_PUBLIC_KEY_HEX`
    // pin is present (host tests never set it): the on-disk pin is the runtime
    // trust anchor, and absent ANY trust anchor the verifier returns Err.
    //
    // The host test harness deliberately avoids the `tempfile` crate (matching
    // the rest of this crate's tests); a per-test scratch dir under the
    // runner's temp dir is used and cleaned up.
    // -----------------------------------------------------------------------

    fn ota_scratch_dir(label: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "dcentos-ota-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create ota scratch dir");
        dir
    }

    /// Build a sysupgrade `.tar` byte buffer with caller-controlled signature /
    /// embedded-pubkey contents so the fail-closed matrix can be exercised.
    /// `manifest` is the raw MANIFEST.json bytes that the signature (if any) is
    /// computed over.
    fn build_sysupgrade_tar_bytes(
        manifest: &[u8],
        signature: Option<&[u8]>,
        embedded_pubkey: Option<&[u8]>,
    ) -> Vec<u8> {
        build_sysupgrade_tar_bytes_with_extra(manifest, signature, embedded_pubkey, &[])
    }

    fn build_sysupgrade_tar_bytes_with_extra(
        manifest: &[u8],
        signature: Option<&[u8]>,
        embedded_pubkey: Option<&[u8]>,
        extra_payloads: &[(&str, &[u8])],
    ) -> Vec<u8> {
        let prefix = test_manifest_payload_prefix(manifest);
        let mut tar = Vec::new();
        append_tar_entry(&mut tar, &format!("{prefix}/"), b'5', &[]);
        append_tar_entry(&mut tar, &format!("{prefix}/kernel"), b'0', b"kernel");
        append_tar_entry(&mut tar, &format!("{prefix}/root"), b'0', b"rootfs");
        append_tar_entry(
            &mut tar,
            &format!("{prefix}/METADATA"),
            b'0',
            TEST_SYSUPGRADE_METADATA,
        );
        for (leaf, payload) in extra_payloads {
            append_tar_entry(&mut tar, &format!("{prefix}/{leaf}"), b'0', payload);
        }
        append_tar_entry(&mut tar, &format!("{prefix}/MANIFEST.json"), b'0', manifest);
        if let Some(sig) = signature {
            append_tar_entry(&mut tar, &format!("{prefix}/MANIFEST.sig"), b'0', sig);
        }
        if let Some(key) = embedded_pubkey {
            append_tar_entry(
                &mut tar,
                &format!("{prefix}/release_ed25519.pub"),
                b'0',
                key,
            );
        }
        // finish_tar returns a Cursor; we want the raw bytes to write to disk.
        finish_tar(tar).into_inner()
    }

    fn test_manifest_payload_prefix(manifest: &[u8]) -> String {
        let target = serde_json::from_slice::<serde_json::Value>(manifest)
            .ok()
            .and_then(|value| {
                value
                    .get("board_target")
                    .and_then(serde_json::Value::as_str)
                    .filter(|target| {
                        !target.is_empty()
                            && target.bytes().all(|byte| {
                                byte.is_ascii_lowercase()
                                    || byte.is_ascii_digit()
                                    || matches!(byte, b'-' | b'_')
                            })
                    })
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "am1-s9".to_string());
        format!("sysupgrade-{target}")
    }

    fn manifest_with_standard_payloads(base_manifest: &[u8]) -> Vec<u8> {
        manifest_with_standard_payloads_and_key(
            base_manifest,
            make_key().verifying_key().as_bytes(),
        )
    }

    fn manifest_with_unsigned_lab_payloads(base_manifest: &[u8]) -> Vec<u8> {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_with_standard_payloads(base_manifest))
                .expect("test manifest JSON");
        manifest["manifest_profile"] = serde_json::json!(SYSUPGRADE_UNSIGNED_LAB_PROFILE);
        manifest["status"] = serde_json::json!("lab_unsigned");
        manifest
            .get_mut("payloads")
            .and_then(serde_json::Value::as_object_mut)
            .expect("test payload registry")
            .remove("verification_key");
        serde_json::to_vec(&manifest).expect("serialize unsigned lab test manifest")
    }

    fn manifest_with_standard_payloads_and_key(
        base_manifest: &[u8],
        verification_key: &[u8],
    ) -> Vec<u8> {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(base_manifest).expect("test manifest JSON");
        let prefix = test_manifest_payload_prefix(base_manifest);
        let object = manifest.as_object_mut().expect("test manifest object");
        object
            .entry("schema".to_string())
            .or_insert(serde_json::json!(1));
        object
            .entry("manifest_profile".to_string())
            .or_insert(serde_json::json!(SYSUPGRADE_AUTHORITY_PROFILE));
        object
            .entry("product".to_string())
            .or_insert(serde_json::json!("DCENT_OS"));
        object
            .entry("package_type".to_string())
            .or_insert(serde_json::json!("sysupgrade"));
        object
            .entry("installable".to_string())
            .or_insert(serde_json::json!(true));
        object
            .entry("artifact_maturity".to_string())
            .or_insert(serde_json::json!("experimental"));
        object
            .entry("version".to_string())
            .or_insert(serde_json::json!("0.20.1"));
        object
            .entry("status".to_string())
            .or_insert(serde_json::json!("release"));
        if !object.contains_key("board") {
            if let Some(board_target) = object
                .get("board_target")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
            {
                object.insert("board".to_string(), serde_json::json!(board_target));
            }
        }
        object.insert(
            "payloads".to_string(),
            serde_json::json!({
                "kernel": {
                    "path": format!("{prefix}/kernel"),
                    "size": 6,
                    "sha256": sha256_hex(b"kernel"),
                },
                "rootfs": {
                    "path": format!("{prefix}/root"),
                    "size": 6,
                    "sha256": sha256_hex(b"rootfs"),
                },
                "metadata": {
                    "path": format!("{prefix}/METADATA"),
                    "size": TEST_SYSUPGRADE_METADATA.len(),
                    "sha256": sha256_hex(TEST_SYSUPGRADE_METADATA),
                },
                "verification_key": {
                    "path": format!("{prefix}/release_ed25519.pub"),
                    "size": verification_key.len(),
                    "sha256": sha256_hex(verification_key),
                }
            }),
        );
        serde_json::to_vec(&manifest).expect("serialize test manifest")
    }

    fn set_manifest_payload_hash(manifest: &[u8], kind: &str, sha256: &str) -> Vec<u8> {
        let mut value: serde_json::Value =
            serde_json::from_slice(manifest).expect("test manifest JSON");
        value["payloads"][kind]["sha256"] = serde_json::Value::String(sha256.to_string());
        serde_json::to_vec(&value).expect("serialize test manifest")
    }

    fn add_manifest_payload(manifest: &[u8], kind: &str, leaf: &str, payload: &[u8]) -> Vec<u8> {
        let mut value: serde_json::Value =
            serde_json::from_slice(manifest).expect("test manifest JSON");
        let prefix = test_manifest_payload_prefix(manifest);
        value["payloads"][kind] = serde_json::json!({
            "path": format!("{prefix}/{leaf}"),
            "size": payload.len(),
            "sha256": sha256_hex(payload),
        });
        serde_json::to_vec(&value).expect("serialize test manifest")
    }

    fn signed_manifest_rejection(manifest: &[u8], label: &str) -> String {
        let scratch = ota_scratch_dir(label);
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let signature = signing.sign(manifest).to_bytes();
        let tar_path = scratch.join("duplicate-key.tar");
        std::fs::write(
            &tar_path,
            build_sysupgrade_tar_bytes(manifest, Some(&signature), Some(&pubkey)),
        )
        .unwrap();
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();

        let error = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
            .expect_err("a signed manifest with duplicate JSON keys must fail closed");
        std::fs::remove_dir_all(&scratch).ok();
        error
    }

    #[test]
    fn verified_artifact_contract_requires_explicit_install_authority() {
        let valid = manifest_with_standard_payloads(
            br#"{"board_target":"am1-s9","board":"am1-s9","version":"0.20.1"}"#,
        );
        let parsed = verified_artifact_contract(&valid, "sysupgrade-am1-s9")
            .expect("complete typed sysupgrade manifest must parse");
        assert_eq!(parsed.board_target, "am1-s9");
        assert_eq!(parsed.artifact_kind, ArtifactKind::SysupgradeBundle);
        assert_eq!(parsed.artifact_maturity, ArtifactMaturity::Experimental);
        assert!(parsed.installable);

        for (field, replacement, expected) in [
            (
                "manifest_profile",
                serde_json::json!("legacy"),
                "manifest_profile",
            ),
            (
                "package_type",
                serde_json::json!("offline_analysis"),
                "not a persistent sysupgrade artifact",
            ),
            ("installable", serde_json::json!(false), "installable=false"),
            (
                "artifact_maturity",
                serde_json::json!("not_implemented"),
                "artifact_maturity",
            ),
            (
                "artifact_maturity",
                serde_json::json!("production"),
                "current 'experimental' authority policy",
            ),
        ] {
            let mut manifest: serde_json::Value = serde_json::from_slice(&valid).unwrap();
            manifest[field] = replacement;
            let bytes = serde_json::to_vec(&manifest).unwrap();
            let err = verified_artifact_contract(&bytes, "sysupgrade-am1-s9")
                .expect_err("non-authorizing manifest must reject");
            assert!(err.contains(expected), "{field}: {err}");
        }

        for missing in ["manifest_profile", "board"] {
            let mut manifest: serde_json::Value = serde_json::from_slice(&valid).unwrap();
            manifest.as_object_mut().unwrap().remove(missing);
            let bytes = serde_json::to_vec(&manifest).unwrap();
            let err = verified_artifact_contract(&bytes, "sysupgrade-am1-s9")
                .expect_err("required authority field must not be optional");
            assert!(err.contains(missing), "{missing}: {err}");
        }

        for unsupported_chain_field in ["ota_intermediate_cert", "ota_revoked_intermediates"] {
            let mut manifest: serde_json::Value = serde_json::from_slice(&valid).unwrap();
            manifest[unsupported_chain_field] =
                if unsupported_chain_field == "ota_intermediate_cert" {
                    serde_json::json!({})
                } else {
                    serde_json::json!([])
                };
            let bytes = serde_json::to_vec(&manifest).unwrap();
            let err = verified_artifact_contract(&bytes, "sysupgrade-am1-s9").expect_err(
                "authority-v1 must never derive mutation authority from wall-clock certificates",
            );
            assert!(
                err.contains("direct release-root signature")
                    && err.contains("trusted-time authority"),
                "{unsupported_chain_field}: {err}"
            );
        }
    }

    #[test]
    fn verified_artifact_contract_binds_target_fields_and_prefix() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_with_standard_payloads(
                br#"{"board_target":"am1-s9","board":"am1-s9","version":"0.20.1"}"#,
            ))
            .unwrap();
        manifest["board"] = serde_json::json!("am2-s19j");
        let conflict = serde_json::to_vec(&manifest).unwrap();
        let err = verified_artifact_contract(&conflict, "sysupgrade-am1-s9")
            .expect_err("conflicting board aliases must reject");
        assert!(err.contains("conflicts with board_target"), "{err}");

        manifest["board"] = serde_json::json!("am1-s9");
        let exact = serde_json::to_vec(&manifest).unwrap();
        let err = verified_artifact_contract(&exact, "sysupgrade-am2-s19j")
            .expect_err("payload prefix must be derived from signed target");
        assert!(
            err.contains("signed target prefix 'sysupgrade-am1-s9'"),
            "{err}"
        );
    }

    #[test]
    fn signed_manifest_rejects_duplicate_root_authority_keys_before_last_wins() {
        let valid = String::from_utf8(manifest_with_standard_payloads(
            br#"{"board_target":"am1-s9","board":"am1-s9","version":"0.20.1"}"#,
        ))
        .unwrap();
        let needle = r#""board_target":"am1-s9""#;
        assert!(
            valid.contains(needle),
            "fixture must contain exact target field"
        );

        for (label, first_value) in [("identical", "am1-s9"), ("conflicting", "am2-s19j")] {
            // The final occurrence is deliberately the valid S9 value. Normal
            // serde_json::Value parsing would silently keep it and authorize
            // the package, masking the preceding contradictory claim.
            let replacement = format!(r#""board_target":"{first_value}",{needle}"#);
            let duplicate = valid.replacen(needle, &replacement, 1);
            let error =
                signed_manifest_rejection(duplicate.as_bytes(), &format!("duplicate-root-{label}"));
            assert!(
                error.contains("duplicate object key 'board_target'"),
                "{label}: unexpected error: {error}"
            );
        }
    }

    #[test]
    fn signed_manifest_rejects_duplicate_nested_payload_fields_before_last_wins() {
        let valid = manifest_with_standard_payloads(
            br#"{"board_target":"am1-s9","board":"am1-s9","version":"0.20.1"}"#,
        );
        let parsed: serde_json::Value = serde_json::from_slice(&valid).unwrap();
        let kernel = serde_json::to_string(&parsed["payloads"]["kernel"]).unwrap();
        let size = r#""size":6"#;
        assert!(kernel.contains(size), "kernel fixture must declare size 6");

        for (label, first_value) in [("identical", 6), ("conflicting", 999)] {
            // This duplicate is three object levels deep. The last value again
            // matches the archive, so last-wins parsing would incorrectly pass
            // the signed payload binding check.
            let replacement = format!(r#""size":{first_value},{size}"#);
            let duplicate_kernel = kernel.replacen(size, &replacement, 1);
            let manifest =
                String::from_utf8(valid.clone())
                    .unwrap()
                    .replacen(&kernel, &duplicate_kernel, 1);
            let error = signed_manifest_rejection(
                manifest.as_bytes(),
                &format!("duplicate-payload-{label}"),
            );
            assert!(
                error.contains("duplicate object key 'size'"),
                "{label}: unexpected error: {error}"
            );
        }
    }

    #[test]
    fn public_update_policy_is_exact_and_excludes_lab_and_offline_targets() {
        assert_eq!(
            require_public_update_policy("am1-s9")
                .expect("S9 public beta policy")
                .board_target,
            "am1-s9"
        );
        let am2 = require_public_update_policy("am2-s19j").expect("AM2 S19j public update policy");
        assert_eq!(am2.board_target, "am2-s19j");
        assert!(
            !am2.public_beta_install && !am2.product_install_allowed(),
            "AM2 public update authority must not become vendor-source first-install authority"
        );
        for denied in ["am2-s19pro", "cv1835-s19jpro", "unknown"] {
            assert!(
                require_public_update_policy(denied).is_err(),
                "{denied} must not acquire browser OTA authority"
            );
        }
    }

    #[test]
    fn board_target_marker_parser_is_single_line_and_exact() {
        assert_eq!(parse_board_target_marker(b"am1-s9\n").unwrap(), "am1-s9");
        assert_eq!(
            parse_board_target_marker(b"am2-s19j\r\n").unwrap(),
            "am2-s19j"
        );
        for invalid in [
            b"".as_slice(),
            b" am1-s9\n".as_slice(),
            b"am1-s9 \n".as_slice(),
            b"am1-s9\ncv1835-s19jpro\n".as_slice(),
            b"AM1-S9\n".as_slice(),
        ] {
            assert!(parse_board_target_marker(invalid).is_err());
        }
    }

    #[test]
    fn bundle_valid_sig_and_trusted_on_disk_key_accepts() {
        let scratch = ota_scratch_dir("accept");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest =
            manifest_with_standard_payloads(br#"{"board_target":"am1-s9","version":"0.20.1"}"#);
        let sig = signing.sign(&manifest).to_bytes();

        let tar_bytes = build_sysupgrade_tar_bytes(&manifest, Some(&sig), Some(&pubkey));
        let tar_path = scratch.join("good.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();

        // On-disk pinned trust anchor matching the embedded key (the prod
        // /etc/dcentos/release_ed25519.pub role).
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();

        let res = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path));
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            res.is_ok(),
            "valid sig + trusted pinned key must accept: {res:?}"
        );
    }

    fn sha256_hex(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(data))
    }

    // `build_sysupgrade_tar_bytes` puts b"kernel"/b"rootfs" as the kernel/root
    // payloads. A manifest whose declared payloads.*.sha256 match those accepts;
    // a manifest whose declared rootfs hash does NOT match (the "valid sig +
    // swapped payload" attack) must be rejected by the payload-binding check.

    #[test]
    fn bundle_valid_sig_matching_payload_hashes_accepts() {
        let scratch = ota_scratch_dir("payload-ok");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest =
            manifest_with_standard_payloads(br#"{"board_target":"am1-s9","version":"0.20.1"}"#);
        let sig = signing.sign(&manifest).to_bytes();
        let tar_bytes = build_sysupgrade_tar_bytes(&manifest, Some(&sig), Some(&pubkey));
        let tar_path = scratch.join("ok.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();

        let res = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path));
        std::fs::remove_dir_all(&scratch).ok();
        let bundle = res
            .unwrap_or_else(|err| panic!("valid sig + matching payload hashes must accept: {err}"));
        assert_eq!(bundle.authenticated_board_target.as_deref(), Some("am1-s9"));
        bundle
            .require_authenticated_board_target("am1-s9")
            .expect("matching authenticated target must accept");
        let authorized = bundle
            .authorize_public_update("am1-s9")
            .expect("verified S9 artifact must bind to public-beta policy");
        assert_eq!(authorized.board_target(), "am1-s9");
        assert_eq!(authorized.version(), "0.20.1");
    }

    #[test]
    fn signed_authority_profile_requires_exact_verification_key_binding() {
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let base =
            manifest_with_standard_payloads(br#"{"board_target":"am1-s9","version":"0.20.1"}"#);

        for (label, mutate, expected) in [
            (
                "missing",
                "missing",
                "requires payload kind 'verification_key'",
            ),
            ("path", "path", "does not support archive leaf 'kernel'"),
            (
                "size",
                "size",
                "payload kind 'verification_key' size mismatch",
            ),
            (
                "sha256",
                "sha256",
                "payload kind 'verification_key' sha256 mismatch",
            ),
        ] {
            let scratch = ota_scratch_dir(&format!("verification-key-{label}"));
            let mut manifest: serde_json::Value = serde_json::from_slice(&base).unwrap();
            match mutate {
                "missing" => {
                    manifest["payloads"]
                        .as_object_mut()
                        .unwrap()
                        .remove("verification_key");
                }
                "path" => {
                    manifest["payloads"]["verification_key"]["path"] =
                        serde_json::json!("sysupgrade-am1-s9/kernel");
                }
                "size" => {
                    manifest["payloads"]["verification_key"]["size"] =
                        serde_json::json!(pubkey.len() - 1);
                }
                "sha256" => {
                    manifest["payloads"]["verification_key"]["sha256"] =
                        serde_json::json!("00".repeat(32));
                }
                _ => unreachable!(),
            }
            let manifest = serde_json::to_vec(&manifest).unwrap();
            let signature = signing.sign(&manifest).to_bytes();
            let tar_path = scratch.join("verification-key.tar");
            std::fs::write(
                &tar_path,
                build_sysupgrade_tar_bytes(&manifest, Some(&signature), Some(&pubkey)),
            )
            .unwrap();
            let pin_path = scratch.join("release_ed25519.pub");
            std::fs::write(&pin_path, pubkey).unwrap();

            let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
                .expect_err("an unbound embedded verification key must reject");
            std::fs::remove_dir_all(&scratch).ok();
            assert!(err.contains(expected), "{label}: unexpected error: {err}");
        }
    }

    #[test]
    fn signed_am2_artifact_rejects_when_bound_to_s9_release_lane() {
        let scratch = ota_scratch_dir("board-target-swap");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest =
            manifest_with_standard_payloads(br#"{"board_target":"am2-s19j","version":"0.20.1"}"#);
        let sig = signing.sign(&manifest).to_bytes();
        let tar_path = scratch.join("artifact-with-arbitrary-name.tar");
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(
            &tar_path,
            build_sysupgrade_tar_bytes(&manifest, Some(&sig), Some(&pubkey)),
        )
        .unwrap();
        std::fs::write(&pin_path, pubkey).unwrap();

        let bundle = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
            .expect("the AM2 artifact itself has a valid signature and payload contract");
        let err = bundle
            .require_authenticated_board_target("am1-s9")
            .expect_err("a signed AM2 artifact must not satisfy the S9 release lane");
        std::fs::remove_dir_all(&scratch).ok();

        assert_eq!(
            bundle.authenticated_board_target.as_deref(),
            Some("am2-s19j")
        );
        assert!(err.contains("signed manifest targets 'am2-s19j'"), "{err}");
        assert!(err.contains("expected 'am1-s9'"), "{err}");
    }

    #[test]
    fn signed_legacy_manifest_without_board_target_is_rejected() {
        let scratch = ota_scratch_dir("board-target-legacy");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest = manifest_with_standard_payloads(br#"{"version":"0.20.1"}"#);
        let sig = signing.sign(&manifest).to_bytes();
        let tar_path = scratch.join("legacy.tar");
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(
            &tar_path,
            build_sysupgrade_tar_bytes(&manifest, Some(&sig), Some(&pubkey)),
        )
        .unwrap();
        std::fs::write(&pin_path, pubkey).unwrap();

        let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
            .expect_err("a signed mutating artifact must declare exact board_target authority");
        std::fs::remove_dir_all(&scratch).ok();

        assert!(err.contains("must declare an exact board_target"), "{err}");
    }

    #[test]
    fn signed_manifest_rejects_present_but_invalid_board_target() {
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();

        for (label, base) in [
            ("null", br#"{"board_target":null}"#.as_slice()),
            ("empty", br#"{"board_target":""}"#.as_slice()),
            ("whitespace", br#"{"board_target":" am1-s9 "}"#.as_slice()),
        ] {
            let scratch = ota_scratch_dir(label);
            let manifest = manifest_with_standard_payloads(base);
            let sig = signing.sign(&manifest).to_bytes();
            let tar_path = scratch.join("invalid-target.tar");
            let pin_path = scratch.join("release_ed25519.pub");
            std::fs::write(
                &tar_path,
                build_sysupgrade_tar_bytes(&manifest, Some(&sig), Some(&pubkey)),
            )
            .unwrap();
            std::fs::write(&pin_path, pubkey).unwrap();

            let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
                .expect_err("present malformed board_target must not degrade to legacy absence");
            std::fs::remove_dir_all(&scratch).ok();
            assert!(err.contains("board_target"), "{label}: {err}");
        }
    }

    #[test]
    fn am2_signed_manifested_bitstream_accepts_through_public_verifier() {
        let scratch = ota_scratch_dir("am2-bitstream-ok");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let bitstream = b"am2-fpga-bitstream";
        let manifest = manifest_with_standard_payloads(br#"{"board_target":"am2-s19j"}"#);
        let manifest =
            add_manifest_payload(&manifest, "bitstream", "fpga_bitstream.bit", bitstream);
        let sig = signing.sign(&manifest).to_bytes();
        let tar_bytes = build_sysupgrade_tar_bytes_with_extra(
            &manifest,
            Some(&sig),
            Some(&pubkey),
            &[("fpga_bitstream.bit", bitstream)],
        );
        let tar_path = scratch.join("am2.tar");
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&tar_path, tar_bytes).unwrap();
        std::fs::write(&pin_path, pubkey).unwrap();

        let bundle = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
            .expect("signed and manifested AM2 bitstream must verify");
        std::fs::remove_dir_all(&scratch).ok();
        assert_eq!(
            bundle.kernel_path,
            PathBuf::from("sysupgrade-am2-s19j/kernel")
        );
        assert_eq!(
            bundle.rootfs_path,
            PathBuf::from("sysupgrade-am2-s19j/root")
        );
    }

    #[test]
    fn am2_bitstream_rejects_when_unmanifested_or_hash_mismatched() {
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let bitstream = b"am2-fpga-bitstream";

        for (label, manifest, archived_bitstream, expected) in [
            (
                "unmanifested",
                manifest_with_standard_payloads(br#"{"board_target":"am2-s19j"}"#),
                &bitstream[..],
                "unmanifested payload",
            ),
            (
                "mismatched",
                add_manifest_payload(
                    &manifest_with_standard_payloads(br#"{"board_target":"am2-s19j"}"#),
                    "bitstream",
                    "fpga_bitstream.bit",
                    bitstream,
                ),
                &b"tampered-bitstream"[..],
                "sha256 mismatch",
            ),
        ] {
            let scratch = ota_scratch_dir(label);
            let sig = signing.sign(&manifest).to_bytes();
            let tar = build_sysupgrade_tar_bytes_with_extra(
                &manifest,
                Some(&sig),
                Some(&pubkey),
                &[("fpga_bitstream.bit", archived_bitstream)],
            );
            let tar_path = scratch.join("am2.tar");
            let pin_path = scratch.join("release_ed25519.pub");
            std::fs::write(&tar_path, tar).unwrap();
            std::fs::write(&pin_path, pubkey).unwrap();
            let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
                .expect_err("invalid AM2 bitstream contract must reject");
            std::fs::remove_dir_all(&scratch).ok();
            assert!(err.contains(expected), "{label}: unexpected error: {err}");
        }
    }

    #[test]
    fn signed_manifest_rejects_unsupported_payload_kind() {
        let scratch = ota_scratch_dir("unsupported-kind");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest = manifest_with_standard_payloads(br#"{"board_target":"am2-s19j"}"#);
        let manifest = add_manifest_payload(&manifest, "future_blob", "future.bin", b"future");
        let sig = signing.sign(&manifest).to_bytes();
        let tar = build_sysupgrade_tar_bytes_with_extra(
            &manifest,
            Some(&sig),
            Some(&pubkey),
            &[("future.bin", b"future")],
        );
        let tar_path = scratch.join("unsupported.tar");
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&tar_path, tar).unwrap();
        std::fs::write(&pin_path, pubkey).unwrap();
        let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
            .expect_err("unsupported manifest payload kind must reject");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            err.contains("unsupported manifest payload kind"),
            "err = {err}"
        );
    }

    #[test]
    fn signed_payload_contract_requires_metadata_and_exact_path_digest_text() {
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let canonical = manifest_with_standard_payloads_and_key(
            br#"{"board_target":"am1-s9","status":"release"}"#,
            &pubkey,
        );
        let canonical: serde_json::Value = serde_json::from_slice(&canonical).unwrap();

        let mut missing_metadata = canonical.clone();
        missing_metadata["payloads"]
            .as_object_mut()
            .unwrap()
            .remove("metadata");

        let mut padded_path = canonical.clone();
        padded_path["payloads"]["kernel"]["path"] = serde_json::json!(" sysupgrade-am1-s9/kernel");

        let mut uppercase_digest = canonical.clone();
        let uppercase = uppercase_digest["payloads"]["kernel"]["sha256"]
            .as_str()
            .unwrap()
            .to_ascii_uppercase();
        uppercase_digest["payloads"]["kernel"]["sha256"] = serde_json::json!(uppercase);

        let mut padded_digest = canonical;
        let padded = format!(
            "{} ",
            padded_digest["payloads"]["kernel"]["sha256"]
                .as_str()
                .unwrap()
        );
        padded_digest["payloads"]["kernel"]["sha256"] = serde_json::json!(padded);

        for (label, manifest, expected) in [
            (
                "missing-metadata",
                missing_metadata,
                "missing required payload kind 'metadata'",
            ),
            (
                "padded-path",
                padded_path,
                "path must not contain surrounding whitespace",
            ),
            (
                "uppercase-digest",
                uppercase_digest,
                "SHA-256 must be exact lowercase hexadecimal",
            ),
            (
                "padded-digest",
                padded_digest,
                "SHA-256 must be exact lowercase hexadecimal",
            ),
        ] {
            let scratch = ota_scratch_dir(label);
            let manifest = serde_json::to_vec(&manifest).unwrap();
            let signature = signing.sign(&manifest).to_bytes();
            let tar_path = scratch.join("invalid-payload-contract.tar");
            std::fs::write(
                &tar_path,
                build_sysupgrade_tar_bytes(&manifest, Some(&signature), Some(&pubkey)),
            )
            .unwrap();
            let pin_path = scratch.join("release_ed25519.pub");
            std::fs::write(&pin_path, pubkey).unwrap();

            let error = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
                .expect_err("non-canonical signed payload declaration must fail closed");
            std::fs::remove_dir_all(&scratch).ok();
            assert!(error.contains(expected), "{label}: {error}");
        }
    }

    #[test]
    fn legacy_cv1835_install_shaped_payload_contract_is_rejected() {
        let prefix = "dcentos-cv1835-s19jpro-sysupgrade";
        let kernel = b"cv-uimage";
        let rootfs = b"cv-rootfs-gzip";
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema": 1,
            "board_target": "cv1835-s19jpro",
            "payloads": {
                "kernel": {
                    "path": format!("{prefix}/uImage"),
                    "size": kernel.len(),
                    "sha256": sha256_hex(kernel),
                },
                "rootfs": {
                    "path": format!("{prefix}/rootfs.gz"),
                    "size": rootfs.len(),
                    "sha256": sha256_hex(rootfs),
                }
            }
        }))
        .unwrap();
        let observed = std::collections::BTreeMap::from([
            (
                "uImage".to_string(),
                ObservedPayload {
                    path: format!("{prefix}/uImage"),
                    size: kernel.len() as u64,
                    sha256: sha256_hex(kernel),
                },
            ),
            (
                "rootfs.gz".to_string(),
                ObservedPayload {
                    path: format!("{prefix}/rootfs.gz"),
                    size: rootfs.len() as u64,
                    sha256: sha256_hex(rootfs),
                },
            ),
        ]);

        let err = verify_manifest_payload_contract(&manifest, prefix, &observed)
            .expect_err("historical CV install-shaped leaves must not regain write authority");
        assert!(
            err.contains("does not support archive leaf 'uImage'"),
            "{err}"
        );
    }

    #[test]
    fn bundle_valid_sig_but_swapped_payload_rejects() {
        // The signed manifest declares a rootfs hash that does NOT match the
        // actual `root` payload bytes in the tar. The Ed25519 signature over the
        // manifest is valid, but the payload binding must reject the bundle
        // (closes "valid MANIFEST.sig + swapped payloads passes").
        let scratch = ota_scratch_dir("payload-bad");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest =
            manifest_with_standard_payloads(br#"{"board_target":"am1-s9","version":"0.20.1"}"#);
        let manifest = set_manifest_payload_hash(
            &manifest,
            "rootfs",
            &sha256_hex(b"a-different-rootfs-image"),
        );
        let sig = signing.sign(&manifest).to_bytes();
        let tar_bytes = build_sysupgrade_tar_bytes(&manifest, Some(&sig), Some(&pubkey));
        let tar_path = scratch.join("bad.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();

        let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
            .expect_err("valid sig but swapped payload must be rejected");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            err.contains("sha256 mismatch"),
            "expected payload-hash mismatch rejection, got: {err}"
        );
    }

    #[test]
    fn bundle_valid_sig_without_payload_registry_rejects() {
        let scratch = ota_scratch_dir("payload-none");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest = br#"{
            "schema":1,
            "manifest_profile":"dcentos.sysupgrade-authority/v1",
            "product":"DCENT_OS",
            "package_type":"sysupgrade",
            "installable":true,
            "artifact_maturity":"experimental",
            "board":"am1-s9",
            "board_target":"am1-s9",
            "version":"0.20.1",
            "status":"release"
        }"#;
        let sig = signing.sign(manifest).to_bytes();
        let tar_bytes = build_sysupgrade_tar_bytes(manifest, Some(&sig), Some(&pubkey));
        let tar_path = scratch.join("none.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();

        let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
            .expect_err("signed manifest without payload registry must reject");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            err.contains("payloads registry"),
            "expected missing payload registry rejection, got: {err}"
        );
    }

    #[test]
    fn extracted_bundle_valid_sig_but_swapped_payload_rejects() {
        // Parity with the tar path on the extracted-directory bundle: a valid
        // signature over a manifest whose declared rootfs hash does NOT match the
        // actual `root` file must be rejected by the payload binding.
        let scratch = ota_scratch_dir("extracted-bad");
        let payload_dir = scratch.join("sysupgrade-am1-s9");
        std::fs::create_dir_all(&payload_dir).unwrap();
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest =
            manifest_with_standard_payloads(br#"{"board_target":"am1-s9","version":"0.20.1"}"#);
        let manifest = set_manifest_payload_hash(
            &manifest,
            "rootfs",
            &sha256_hex(b"a-different-rootfs-image"),
        );
        let sig = signing.sign(&manifest).to_bytes();
        std::fs::write(payload_dir.join("kernel"), b"kernel").unwrap();
        std::fs::write(payload_dir.join("root"), b"rootfs").unwrap();
        std::fs::write(payload_dir.join("METADATA"), TEST_SYSUPGRADE_METADATA).unwrap();
        std::fs::write(payload_dir.join("MANIFEST.json"), &manifest).unwrap();
        std::fs::write(payload_dir.join("MANIFEST.sig"), sig).unwrap();
        std::fs::write(payload_dir.join("release_ed25519.pub"), pubkey).unwrap();
        let pin_path = payload_dir.join("release_ed25519.pub");

        let err = verify_sysupgrade_bundle(&scratch, false, Some(&pin_path))
            .expect_err("extracted bundle: valid sig but swapped payload must be rejected");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            err.contains("sha256 mismatch"),
            "expected payload-hash mismatch rejection, got: {err}"
        );
    }

    #[test]
    fn extracted_bundle_manifested_bitstream_accepts_and_unmanifested_rejects() {
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let bitstream = b"am2-fpga-bitstream";

        for (label, declare_bitstream, should_accept) in [
            ("extracted-am2-ok", true, true),
            ("extracted-am2-extra", false, false),
        ] {
            let scratch = ota_scratch_dir(label);
            let payload_dir = scratch.join("sysupgrade-am2-s19j");
            std::fs::create_dir_all(&payload_dir).unwrap();
            let manifest = manifest_with_standard_payloads(br#"{"board_target":"am2-s19j"}"#);
            let manifest = if declare_bitstream {
                add_manifest_payload(&manifest, "bitstream", "fpga_bitstream.bit", bitstream)
            } else {
                manifest
            };
            let sig = signing.sign(&manifest).to_bytes();
            std::fs::write(payload_dir.join("kernel"), b"kernel").unwrap();
            std::fs::write(payload_dir.join("root"), b"rootfs").unwrap();
            std::fs::write(payload_dir.join("METADATA"), TEST_SYSUPGRADE_METADATA).unwrap();
            std::fs::write(payload_dir.join("fpga_bitstream.bit"), bitstream).unwrap();
            std::fs::write(payload_dir.join("MANIFEST.json"), &manifest).unwrap();
            std::fs::write(payload_dir.join("MANIFEST.sig"), sig).unwrap();
            std::fs::write(payload_dir.join("release_ed25519.pub"), pubkey).unwrap();
            let pin_path = payload_dir.join("release_ed25519.pub");

            let result = verify_sysupgrade_bundle(&scratch, false, Some(&pin_path));
            if should_accept {
                let bundle = result.expect("manifested extracted AM2 bitstream must accept");
                assert_eq!(
                    bundle.authenticated_board_target.as_deref(),
                    Some("am2-s19j"),
                    "extracted verifier must expose signed manifest identity"
                );
            } else {
                let err = result.expect_err("unmanifested extracted AM2 bitstream must reject");
                assert!(err.contains("unmanifested payload"), "err = {err}");
            }
            std::fs::remove_dir_all(&scratch).ok();
        }
    }

    #[test]
    fn extracted_bundle_rejects_outside_file_and_second_payload_directory() {
        for (label, add_second_dir) in [("outside-file", false), ("second-dir", true)] {
            let scratch = ota_scratch_dir(label);
            let payload_dir = scratch.join("sysupgrade-am1-s9");
            std::fs::create_dir_all(&payload_dir).unwrap();
            std::fs::write(payload_dir.join("MANIFEST.json"), b"{}").unwrap();
            if add_second_dir {
                let second = scratch.join("sysupgrade-am2-s19j");
                std::fs::create_dir_all(&second).unwrap();
                std::fs::write(second.join("MANIFEST.json"), b"{}").unwrap();
            } else {
                std::fs::write(scratch.join("outside.txt"), b"unexpected").unwrap();
            }
            let err = verify_sysupgrade_bundle(&scratch, false, None)
                .expect_err("unexpected extracted-root entries must reject");
            std::fs::remove_dir_all(&scratch).ok();
            assert!(
                err.contains("unexpected") || err.contains("multiple"),
                "{label}: err = {err}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn extracted_bundle_rejects_symlink_payload() {
        use std::os::unix::fs::symlink;

        let scratch = ota_scratch_dir("extracted-symlink");
        let payload_dir = scratch.join("sysupgrade-am1-s9");
        std::fs::create_dir_all(&payload_dir).unwrap();
        let manifest = manifest_with_unsigned_lab_payloads(
            br#"{"board_target":"am1-s9","status":"lab_unsigned"}"#,
        );
        std::fs::write(payload_dir.join("MANIFEST.json"), &manifest).unwrap();
        std::fs::write(payload_dir.join("kernel"), b"kernel").unwrap();
        symlink(payload_dir.join("kernel"), payload_dir.join("root")).unwrap();
        let err = verify_sysupgrade_bundle(&scratch, true, None)
            .expect_err("symlink payload must reject");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(err.contains("direct regular file"), "err = {err}");
    }

    #[test]
    fn bundle_missing_signature_rejects_when_not_allow_unsigned() {
        let scratch = ota_scratch_dir("nosig");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest = br#"{"board_target":"am1-s9"}"#;

        // No MANIFEST.sig at all — the prod path passes allow_unsigned = false.
        let tar_bytes = build_sysupgrade_tar_bytes(manifest, None, Some(&pubkey));
        let tar_path = scratch.join("nosig.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();

        let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
            .expect_err("missing MANIFEST.sig must be rejected when allow_unsigned=false");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(err.contains("missing MANIFEST.sig"), "err = {err}");
    }

    // CE-183: the [ota] allow_unsigned lab override must NOT accept an unsigned
    // bundle that declares release status — mirrors the target sysupgrade:507
    // release-status-requires-signature rule.
    #[test]
    fn bundle_unsigned_release_status_rejected_even_with_allow_unsigned() {
        // tar path
        let scratch = ota_scratch_dir("ce183-tar");
        let manifest = br#"{"board_target":"am1-s9","status":"release"}"#;
        let tar_bytes = build_sysupgrade_tar_bytes(manifest, None, None);
        let tar_path = scratch.join("unsigned-release.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let err = verify_sysupgrade_bundle(&tar_path, true, None)
            .expect_err("unsigned release-status tar must be rejected even with allow_unsigned");
        assert!(
            err.contains("release status") && err.contains("CE-183"),
            "tar err = {err}"
        );
        std::fs::remove_file(&tar_path).unwrap();

        // extracted path
        let payload_dir = scratch.join("sysupgrade-am1-s9");
        std::fs::create_dir_all(&payload_dir).unwrap();
        std::fs::write(payload_dir.join("kernel"), b"kernel").unwrap();
        std::fs::write(payload_dir.join("root"), b"rootfs").unwrap();
        std::fs::write(payload_dir.join("MANIFEST.json"), manifest).unwrap();
        let err2 = verify_sysupgrade_bundle(&scratch, true, None).expect_err(
            "unsigned release-status extracted bundle must be rejected even with allow_unsigned",
        );
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            err2.contains("release status") && err2.contains("CE-183"),
            "extracted err = {err2}"
        );
    }

    #[test]
    fn bundle_unsigned_missing_status_treated_as_release_rejected() {
        // A manifest with no "status" field is treated as release (fail-closed),
        // so the allow_unsigned override must still reject it.
        let scratch = ota_scratch_dir("ce183-nostatus");
        let manifest = br#"{"board_target":"am1-s9"}"#;
        let tar_bytes = build_sysupgrade_tar_bytes(manifest, None, None);
        let tar_path = scratch.join("unsigned-nostatus.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let err = verify_sysupgrade_bundle(&tar_path, true, None)
            .expect_err("unsigned manifest with missing status must be treated as release");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(err.contains("CE-183"), "err = {err}");
    }

    #[test]
    fn bundle_unsigned_lab_status_still_accepted_with_allow_unsigned() {
        // A genuine unsigned-lab profile works only as an unauthenticated lab
        // inspection result. It cannot cross the public write boundary.
        let scratch = ota_scratch_dir("ce183-lab");
        let manifest = manifest_with_unsigned_lab_payloads(
            br#"{"board_target":"am1-s9","status":"lab_unsigned"}"#,
        );
        let tar_bytes = build_sysupgrade_tar_bytes(&manifest, None, None);
        let tar_path = scratch.join("unsigned-lab.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let res = verify_sysupgrade_bundle(&tar_path, true, None);
        let bundle = res.unwrap_or_else(|err| {
            panic!("unsigned lab-status bundle must still accept with allow_unsigned: {err}")
        });
        assert_eq!(
            bundle.authenticated_board_target, None,
            "an unsigned lab manifest must not mint authenticated target identity"
        );
        assert_eq!(bundle.verified_artifact, None);
        let error = bundle
            .authorize_public_update("am1-s9")
            .expect_err("unsigned lab validation must not mint public write authority");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(error.contains("no authenticated board_target"), "{error}");
    }

    #[test]
    fn extracted_unsigned_lab_profile_accepts_without_minting_authority() {
        let scratch = ota_scratch_dir("unsigned-lab-extracted");
        let payload_dir = scratch.join("sysupgrade-am1-s9");
        std::fs::create_dir_all(&payload_dir).unwrap();
        let manifest = manifest_with_unsigned_lab_payloads(
            br#"{"board_target":"am1-s9","status":"lab_unsigned"}"#,
        );
        std::fs::write(payload_dir.join("MANIFEST.json"), manifest).unwrap();
        std::fs::write(payload_dir.join("kernel"), b"kernel").unwrap();
        std::fs::write(payload_dir.join("root"), b"rootfs").unwrap();
        std::fs::write(payload_dir.join("METADATA"), TEST_SYSUPGRADE_METADATA).unwrap();

        let bundle = verify_sysupgrade_bundle(&scratch, true, None)
            .expect("canonical extracted unsigned-lab profile must validate");
        assert_eq!(bundle.authenticated_board_target, None);
        assert_eq!(bundle.verified_artifact, None);
        std::fs::remove_dir_all(&scratch).ok();
    }

    #[test]
    fn unsigned_lab_profile_rejects_authority_material_and_wrong_identity() {
        let canonical = manifest_with_unsigned_lab_payloads(
            br#"{"board_target":"am1-s9","status":"lab_unsigned"}"#,
        );
        let mut cases = Vec::new();

        let mut authority_profile: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
        authority_profile["manifest_profile"] = serde_json::json!(SYSUPGRADE_AUTHORITY_PROFILE);
        cases.push((
            "authority profile",
            serde_json::to_vec(&authority_profile).unwrap(),
        ));

        let mut wrong_status: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
        wrong_status["status"] = serde_json::json!("lab_signed");
        cases.push(("wrong status", serde_json::to_vec(&wrong_status).unwrap()));

        let mut certificate: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
        certificate["ota_intermediate_cert"] = serde_json::json!({});
        cases.push((
            "certificate field",
            serde_json::to_vec(&certificate).unwrap(),
        ));

        let mut key_declaration: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
        key_declaration["payloads"]["verification_key"] = serde_json::json!({
            "path": "sysupgrade-am1-s9/release_ed25519.pub",
            "size": 32,
            "sha256": "00".repeat(32),
        });
        cases.push((
            "key declaration",
            serde_json::to_vec(&key_declaration).unwrap(),
        ));

        for (label, manifest) in cases {
            let scratch = ota_scratch_dir(label);
            let tar_path = scratch.join("invalid-unsigned-lab.tar");
            std::fs::write(&tar_path, build_sysupgrade_tar_bytes(&manifest, None, None)).unwrap();
            let result = verify_sysupgrade_bundle(&tar_path, true, None);
            std::fs::remove_dir_all(&scratch).ok();
            assert!(result.is_err(), "{label} must fail closed");
        }

        let scratch = ota_scratch_dir("unsigned-lab-physical-key");
        let tar_path = scratch.join("invalid-unsigned-lab-key.tar");
        std::fs::write(
            &tar_path,
            build_sysupgrade_tar_bytes(&canonical, None, Some(&[7_u8; 32])),
        )
        .unwrap();
        let error = verify_sysupgrade_bundle(&tar_path, true, None)
            .expect_err("unsigned lab bundle must not carry a release key");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(error.contains("release_ed25519.pub"), "{error}");
    }

    #[test]
    fn signed_authority_profile_requires_canonical_non_lab_unsigned_status() {
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let canonical = manifest_with_standard_payloads_and_key(
            br#"{"board_target":"am1-s9","status":"release"}"#,
            &pubkey,
        );
        let invalid_statuses = [
            ("missing", None),
            ("non-string", Some(serde_json::json!(true))),
            ("empty", Some(serde_json::json!(""))),
            ("blank", Some(serde_json::json!(" "))),
            ("leading-space", Some(serde_json::json!(" release"))),
            ("trailing-space", Some(serde_json::json!("release "))),
            ("lab-unsigned", Some(serde_json::json!("lab_unsigned"))),
        ];

        for (label, invalid_status) in invalid_statuses {
            let scratch = ota_scratch_dir(&format!("signed-authority-status-{label}"));
            let mut manifest: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
            match invalid_status {
                Some(value) => manifest["status"] = value,
                None => {
                    manifest
                        .as_object_mut()
                        .expect("manifest object")
                        .remove("status");
                }
            }
            let manifest = serde_json::to_vec(&manifest).unwrap();
            let signature = signing.sign(&manifest).to_bytes();
            let tar_path = scratch.join("invalid-authority-status.tar");
            std::fs::write(
                &tar_path,
                build_sysupgrade_tar_bytes(&manifest, Some(&signature), Some(&pubkey)),
            )
            .unwrap();
            let pin_path = scratch.join("release_ed25519.pub");
            std::fs::write(&pin_path, pubkey).unwrap();

            let error = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
                .expect_err("authority-v1 status must fail closed");
            std::fs::remove_dir_all(&scratch).ok();
            assert!(error.contains("status"), "{label}: {error}");
        }
    }

    #[test]
    fn bundle_signed_but_no_trust_anchor_rejects() {
        // THE core CRITICAL-4 pin: a signed bundle with NO available trust
        // anchor (no on-disk pinned key file, and host tests never set a
        // compile-time DCENT_OTA_PUBLIC_KEY_HEX) must FAIL CLOSED, not accept.
        if compiled_public_key_hex().is_some() {
            // A local build pinned a compile-time key — the trust anchor would
            // exist and this scenario is unreachable. Skip the assertion.
            return;
        }
        let scratch = ota_scratch_dir("noanchor");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest = br#"{"board_target":"am1-s9"}"#;
        let sig = signing.sign(manifest).to_bytes();

        let tar_bytes = build_sysupgrade_tar_bytes(manifest, Some(&sig), Some(&pubkey));
        let tar_path = scratch.join("noanchor.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();

        // pinned_release_key_path points at a path that does NOT exist on disk,
        // so `pinned_path.is_file()` is false and no trust anchor is found.
        let missing_pin = scratch.join("does-not-exist.pub");
        let err = verify_sysupgrade_bundle(&tar_path, false, Some(&missing_pin))
            .expect_err("signed bundle with no trust anchor must fail closed");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            err.contains("no trusted OTA public key"),
            "expected no-trust-anchor rejection, err = {err}"
        );
    }

    #[test]
    fn bundle_embedded_key_mismatch_with_pin_rejects() {
        let scratch = ota_scratch_dir("keymismatch");
        let signing = make_key();
        let embedded_pubkey = signing.verifying_key().to_bytes();
        let manifest = br#"{"board_target":"am1-s9"}"#;
        let sig = signing.sign(manifest).to_bytes();

        let tar_bytes = build_sysupgrade_tar_bytes(manifest, Some(&sig), Some(&embedded_pubkey));
        let tar_path = scratch.join("keymismatch.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();

        // Pinned trust anchor is a DIFFERENT key than the one embedded in the
        // bundle — must reject before any signature check.
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, other.verifying_key().to_bytes()).unwrap();

        let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
            .expect_err("embedded key not matching the pin must be rejected");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            err.contains("does not match pinned"),
            "expected pin-mismatch rejection, err = {err}"
        );
    }

    #[test]
    fn bundle_tampered_signature_rejects() {
        let scratch = ota_scratch_dir("badsig");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest = br#"{"board_target":"am1-s9","version":"0.20.1"}"#;
        // Sign DIFFERENT bytes than the manifest the bundle ships — the
        // signature will not verify against the shipped manifest.
        let sig = signing.sign(b"a different message entirely").to_bytes();

        let tar_bytes = build_sysupgrade_tar_bytes(manifest, Some(&sig), Some(&pubkey));
        let tar_path = scratch.join("badsig.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();

        let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
            .expect_err("signature over the wrong bytes must fail verification");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            err.contains("Ed25519 verification failed"),
            "expected signature verification failure, err = {err}"
        );
    }

    // ── W24-OTA-1: downgrade protection on the OTA write path ──────────────
    //
    // The write path (`rest.rs::post_system_upgrade`) now reads the candidate
    // version via `read_manifest_version_from_bundle` and runs `assess_rollback`
    // before scheduling the flash. These tests pin that bundle-version read +
    // the downgrade decision the write path makes (host-testable, no HAL).

    #[test]
    fn manifest_version_read_from_tar_bundle() {
        let scratch = ota_scratch_dir("verread");
        let manifest = br#"{"board_target":"am1-s9","version":"0.20.1"}"#;
        let tar_bytes = build_sysupgrade_tar_bytes(manifest, None, None);
        let tar_path = scratch.join("v.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();

        let version =
            read_manifest_version_from_bundle(&tar_path).expect("manifest read should succeed");
        std::fs::remove_dir_all(&scratch).ok();
        assert_eq!(version.as_deref(), Some("0.20.1"));
    }

    #[test]
    fn manifest_without_version_field_reads_none() {
        let scratch = ota_scratch_dir("noverfield");
        let manifest = br#"{"board_target":"am1-s9"}"#;
        let tar_bytes = build_sysupgrade_tar_bytes(manifest, None, None);
        let tar_path = scratch.join("nover.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();

        let version =
            read_manifest_version_from_bundle(&tar_path).expect("manifest read should succeed");
        std::fs::remove_dir_all(&scratch).ok();
        assert_eq!(version, None);
    }

    #[test]
    fn write_path_refuses_signed_but_older_downgrade() {
        use dcentrald_api_types::ota_rollback_protection::{assess_rollback, RollbackVerdict};

        // Candidate version pulled out of a real bundle, exactly as the write
        // path does it.
        let scratch = ota_scratch_dir("downgrade");
        let manifest = br#"{"board_target":"am1-s9","version":"0.19.0"}"#;
        let tar_bytes = build_sysupgrade_tar_bytes(manifest, None, None);
        let tar_path = scratch.join("old.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();

        let candidate = read_manifest_version_from_bundle(&tar_path)
            .expect("manifest read should succeed")
            .expect("version present");
        std::fs::remove_dir_all(&scratch).ok();

        // Running firmware is newer than the candidate → write path must deny.
        let current = "0.20.1";
        let verdict = assess_rollback(&candidate, current, false);
        assert!(
            !verdict.is_allowed(),
            "signed-but-older package must be denied on the write path: {verdict:?}"
        );
        assert!(matches!(verdict, RollbackVerdict::DenyOlderVersion { .. }));
    }

    #[test]
    fn write_path_allows_forward_and_reinstall() {
        use dcentrald_api_types::ota_rollback_protection::assess_rollback;
        // A newer or equal candidate proceeds (the write path only rejects on
        // deny verdicts).
        assert!(assess_rollback("0.21.0", "0.20.1", false).is_allowed());
        assert!(assess_rollback("0.20.1", "0.20.1", false).is_allowed());
    }

    // ── W8 GROUP C: two-level PKI (root -> intermediate -> payload) ─────────
    //
    // Additive chain verification. Tests pin the back-compat + brick-safe
    // contract: legacy single-key manifest still verifies byte-identically; a
    // valid root->intermediate->payload chain verifies; expired / not-yet-valid
    // / revoked / wrong-root intermediate is rejected; tampered payload is
    // rejected; a present-but-malformed cert FAILS CLOSED (never silently
    // downgrades to the single-key path).

    fn root_key() -> SigningKey {
        SigningKey::from_bytes(&[11u8; 32])
    }
    fn intermediate_key() -> SigningKey {
        SigningKey::from_bytes(&[22u8; 32])
    }

    /// Build a MANIFEST.json (as raw bytes) embedding a root-signed
    /// intermediate cert + an optional revocation list. The returned bytes are
    /// exactly what gets signed by the intermediate (the payload signature is
    /// over these manifest bytes).
    fn build_manifest_with_cert(
        root: &SigningKey,
        intermediate: &SigningKey,
        not_before: i64,
        not_after: i64,
        serial: Option<&str>,
        revocations: &[&str],
        good_root_sig: bool,
    ) -> Vec<u8> {
        let root_hex = to_hex(root.verifying_key().as_bytes());
        let inter_hex = to_hex(intermediate.verifying_key().as_bytes());
        let cert_msg = canonical_intermediate_cert_message(
            &root_hex, &inter_hex, not_before, not_after, serial,
        );
        let root_sig = if good_root_sig {
            root.sign(cert_msg.as_bytes())
        } else {
            // Sign different bytes so the root signature won't verify.
            root.sign(b"forged cert authorization")
        };
        let root_sig_hex = to_hex(root_sig.to_bytes().as_slice());

        let serial_json = match serial {
            Some(s) => format!(r#","serial":"{s}""#),
            None => String::new(),
        };
        let rev_json = if revocations.is_empty() {
            String::new()
        } else {
            let joined = revocations
                .iter()
                .map(|r| format!("\"{r}\""))
                .collect::<Vec<_>>()
                .join(",");
            format!(r#","ota_revoked_intermediates":[{joined}]"#)
        };

        let manifest = format!(
            r#"{{"board_target":"am1-s9","version":"0.21.0","ota_intermediate_cert":{{"root_key_hex":"{root_hex}","intermediate_key_hex":"{inter_hex}","not_before":{not_before},"not_after":{not_after}{serial_json},"root_signature_hex":"{root_sig_hex}"}}{rev_json}}}"#
        );
        manifest_with_standard_payloads_and_key(
            manifest.as_bytes(),
            root.verifying_key().as_bytes(),
        )
    }

    #[test]
    fn legacy_single_key_manifest_has_no_cert_and_uses_direct_path() {
        // A manifest with no ota_intermediate_cert => Ok(None) => the caller
        // runs the legacy verify_raw path (byte-identical to pre-W8).
        let legacy_manifest = br#"{"board_target":"am1-s9","version":"0.20.1"}"#;
        let cert = parse_intermediate_cert_from_manifest(legacy_manifest)
            .expect("legacy manifest must parse without error");
        assert!(
            cert.is_none(),
            "legacy manifest must yield no cert envelope"
        );

        // And the full single-key bundle path still accepts (regression guard
        // that the additive branch did not disturb the direct path).
        let scratch = ota_scratch_dir("legacy-direct");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest = manifest_with_standard_payloads(legacy_manifest);
        let sig = signing.sign(&manifest).to_bytes();
        let tar_bytes = build_sysupgrade_tar_bytes(&manifest, Some(&sig), Some(&pubkey));
        let tar_path = scratch.join("legacy.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();
        let res = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path));
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            res.is_ok(),
            "legacy single-key bundle must still verify: {res:?}"
        );
    }

    #[test]
    fn valid_root_intermediate_payload_chain_verifies() {
        let root = root_key();
        let inter = intermediate_key();
        let now = 1_700_000_000i64;
        let manifest = build_manifest_with_cert(
            &root,
            &inter,
            now - 1000,
            now + 1000,
            Some("rot-2026-06"),
            &[],
            true,
        );
        // Intermediate signs the payload (= the manifest bytes).
        let payload_sig = inter.sign(&manifest).to_bytes();

        let cert = parse_intermediate_cert_from_manifest(&manifest)
            .expect("cert must parse")
            .expect("cert must be present");

        verify_two_level_chain_at(
            root.verifying_key().as_bytes(),
            &manifest,
            &payload_sig,
            &cert,
            now,
            &[],
        )
        .expect("a valid root->intermediate->payload chain must verify");
    }

    #[test]
    fn full_authority_v1_bundle_with_two_level_chain_is_non_authorizing() {
        // The low-level chain is cryptographically valid, but authority-v1 is
        // direct-root-only because Zynq update admission has no authenticated
        // clock for certificate validity. End-to-end verification must reject
        // before producing a VerifiedArtifact/AuthorizedSysupgrade.
        let scratch = ota_scratch_dir("twolevel-bundle");
        let root = root_key();
        let inter = intermediate_key();
        let now = unix_now_seconds();
        let manifest = build_manifest_with_cert(
            &root,
            &inter,
            now - 1000,
            now + 100_000,
            Some("rot-x"),
            &[],
            true,
        );
        let payload_sig = inter.sign(&manifest).to_bytes();

        let tar_bytes = build_sysupgrade_tar_bytes(
            &manifest,
            Some(&payload_sig),
            Some(root.verifying_key().as_bytes()),
        );
        let tar_path = scratch.join("twolevel.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        // On-disk trust anchor == the ROOT key (embedded release_ed25519.pub).
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, root.verifying_key().to_bytes()).unwrap();

        let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path)).expect_err(
            "authority-v1 must reject even a cryptographically valid intermediate chain",
        );
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            err.contains("direct release-root signature") && err.contains("trusted-time authority"),
            "unexpected authority-v1 chain rejection: {err}"
        );
    }

    #[test]
    fn expired_intermediate_is_rejected() {
        let root = root_key();
        let inter = intermediate_key();
        let now = 1_700_000_000i64;
        // Window ends before `now`.
        let manifest = build_manifest_with_cert(
            &root,
            &inter,
            now - 2000,
            now - 1000,
            Some("old"),
            &[],
            true,
        );
        let payload_sig = inter.sign(&manifest).to_bytes();
        let cert = parse_intermediate_cert_from_manifest(&manifest)
            .unwrap()
            .unwrap();
        let err = verify_two_level_chain_at(
            root.verifying_key().as_bytes(),
            &manifest,
            &payload_sig,
            &cert,
            now,
            &[],
        )
        .expect_err("expired intermediate must be rejected");
        assert!(err.contains("expired"), "err = {err}");
    }

    #[test]
    fn not_yet_valid_intermediate_is_rejected() {
        let root = root_key();
        let inter = intermediate_key();
        let now = 1_700_000_000i64;
        let manifest = build_manifest_with_cert(
            &root,
            &inter,
            now + 1000,
            now + 2000,
            Some("future"),
            &[],
            true,
        );
        let payload_sig = inter.sign(&manifest).to_bytes();
        let cert = parse_intermediate_cert_from_manifest(&manifest)
            .unwrap()
            .unwrap();
        let err = verify_two_level_chain_at(
            root.verifying_key().as_bytes(),
            &manifest,
            &payload_sig,
            &cert,
            now,
            &[],
        )
        .expect_err("not-yet-valid intermediate must be rejected");
        assert!(err.contains("not yet valid"), "err = {err}");
    }

    #[test]
    fn revoked_intermediate_is_rejected_by_serial() {
        let root = root_key();
        let inter = intermediate_key();
        let now = 1_700_000_000i64;
        // Manifest revokes its own serial.
        let manifest = build_manifest_with_cert(
            &root,
            &inter,
            now - 1000,
            now + 1000,
            Some("rot-bad"),
            &["rot-bad"],
            true,
        );
        let payload_sig = inter.sign(&manifest).to_bytes();
        let cert = parse_intermediate_cert_from_manifest(&manifest)
            .unwrap()
            .unwrap();
        let err = verify_two_level_chain_at(
            root.verifying_key().as_bytes(),
            &manifest,
            &payload_sig,
            &cert,
            now,
            &[],
        )
        .expect_err("revoked-by-serial intermediate must be rejected");
        assert!(err.contains("REVOKED"), "err = {err}");
    }

    #[test]
    fn revoked_intermediate_is_rejected_by_on_disk_key_hex() {
        let root = root_key();
        let inter = intermediate_key();
        let now = 1_700_000_000i64;
        let manifest =
            build_manifest_with_cert(&root, &inter, now - 1000, now + 1000, None, &[], true);
        let payload_sig = inter.sign(&manifest).to_bytes();
        let cert = parse_intermediate_cert_from_manifest(&manifest)
            .unwrap()
            .unwrap();
        // On-disk revocation list names the intermediate key hex.
        let on_disk = vec![to_hex(inter.verifying_key().as_bytes())];
        let err = verify_two_level_chain_at(
            root.verifying_key().as_bytes(),
            &manifest,
            &payload_sig,
            &cert,
            now,
            &on_disk,
        )
        .expect_err("revoked-by-on-disk-key-hex intermediate must be rejected");
        assert!(err.contains("REVOKED"), "err = {err}");
    }

    #[test]
    fn wrong_root_intermediate_is_rejected() {
        let root = root_key();
        let inter = intermediate_key();
        let now = 1_700_000_000i64;
        let manifest = build_manifest_with_cert(
            &root,
            &inter,
            now - 1000,
            now + 1000,
            Some("rot"),
            &[],
            true,
        );
        let payload_sig = inter.sign(&manifest).to_bytes();
        let cert = parse_intermediate_cert_from_manifest(&manifest)
            .unwrap()
            .unwrap();
        // Trust-anchored root is a DIFFERENT key than the cert's declared root.
        let other_root = SigningKey::from_bytes(&[99u8; 32]);
        let err = verify_two_level_chain_at(
            other_root.verifying_key().as_bytes(),
            &manifest,
            &payload_sig,
            &cert,
            now,
            &[],
        )
        .expect_err("cert whose declared root != trusted root must be rejected");
        assert!(err.contains("does not match the trusted"), "err = {err}");
    }

    #[test]
    fn forged_root_signature_is_rejected() {
        // The cert claims the correct (trusted) root, but the root signature
        // over the cert is invalid — must be rejected at the cert step.
        let root = root_key();
        let inter = intermediate_key();
        let now = 1_700_000_000i64;
        let manifest = build_manifest_with_cert(
            &root,
            &inter,
            now - 1000,
            now + 1000,
            Some("rot"),
            &[],
            /*good_root_sig=*/ false,
        );
        let payload_sig = inter.sign(&manifest).to_bytes();
        let cert = parse_intermediate_cert_from_manifest(&manifest)
            .unwrap()
            .unwrap();
        let err = verify_two_level_chain_at(
            root.verifying_key().as_bytes(),
            &manifest,
            &payload_sig,
            &cert,
            now,
            &[],
        )
        .expect_err("forged root signature over the cert must be rejected");
        assert!(
            err.contains("root signature over the intermediate cert is invalid"),
            "err = {err}"
        );
    }

    #[test]
    fn tampered_payload_under_intermediate_is_rejected() {
        let root = root_key();
        let inter = intermediate_key();
        let now = 1_700_000_000i64;
        let manifest = build_manifest_with_cert(
            &root,
            &inter,
            now - 1000,
            now + 1000,
            Some("rot"),
            &[],
            true,
        );
        // Intermediate signs DIFFERENT bytes than the manifest that ships.
        let payload_sig = inter.sign(b"a different payload entirely").to_bytes();
        let cert = parse_intermediate_cert_from_manifest(&manifest)
            .unwrap()
            .unwrap();
        let err = verify_two_level_chain_at(
            root.verifying_key().as_bytes(),
            &manifest,
            &payload_sig,
            &cert,
            now,
            &[],
        )
        .expect_err("tampered payload must fail the intermediate payload check");
        assert!(
            err.contains("payload signature does not verify under the intermediate key"),
            "err = {err}"
        );
    }

    #[test]
    fn malformed_cert_fails_closed_not_silent_single_key() {
        // A present-but-malformed ota_intermediate_cert (short root key) must
        // return Err from the parser — it must NOT yield Ok(None) and silently
        // fall back to the single-key path (that would let an attacker strip
        // the chain by corrupting the cert).
        let manifest = br#"{"board_target":"am1-s9","ota_intermediate_cert":{"root_key_hex":"abcd","intermediate_key_hex":"00","not_before":1,"not_after":2,"root_signature_hex":"00"}}"#;
        let err = parse_intermediate_cert_from_manifest(manifest)
            .expect_err("malformed cert must fail closed");
        assert!(err.contains("root key must be 32 bytes"), "err = {err}");
    }

    #[test]
    fn inverted_validity_window_is_rejected_at_parse() {
        let manifest = format!(
            r#"{{"ota_intermediate_cert":{{"root_key_hex":"{}","intermediate_key_hex":"{}","not_before":2000,"not_after":1000,"root_signature_hex":"{}"}}}}"#,
            to_hex(&[1u8; 32]),
            to_hex(&[2u8; 32]),
            to_hex(&[3u8; 64]),
        );
        let err = parse_intermediate_cert_from_manifest(manifest.as_bytes())
            .expect_err("inverted validity window must fail closed");
        assert!(err.contains("inverted"), "err = {err}");
    }

    #[test]
    fn canonical_cert_message_is_byte_stable() {
        // Pin the exact canonical cert bytes so the signing tool and verifier
        // never drift (a drift would silently invalidate every cert).
        let msg = canonical_intermediate_cert_message("aa", "bb", 100, 200, Some("rot-1"));
        assert_eq!(
            msg,
            "schema=1\ntype=ota-intermediate-cert\nroot=aa\nintermediate=bb\nnot_before=100\nnot_after=200\nserial=rot-1\n"
        );
        // No serial => empty serial field, still stable.
        let msg_no_serial = canonical_intermediate_cert_message("aa", "bb", 100, 200, None);
        assert_eq!(
            msg_no_serial,
            "schema=1\ntype=ota-intermediate-cert\nroot=aa\nintermediate=bb\nnot_before=100\nnot_after=200\nserial=\n"
        );
    }
}
