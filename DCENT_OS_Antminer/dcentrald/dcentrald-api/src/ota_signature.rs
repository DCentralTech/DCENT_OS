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
//!    `sysupgrade-*/MANIFEST.json` + `MANIFEST.sig` + `release_ed25519.pub`,
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

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

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

/// Outcome of inspecting a staged sysupgrade `.tar`.
#[derive(Debug, Clone)]
pub struct SysupgradeBundle {
    pub manifest_path: PathBuf,
    pub signature_path: PathBuf,
    pub release_key_path: PathBuf,
    pub kernel_path: PathBuf,
    pub rootfs_path: PathBuf,
}

impl Default for SysupgradeBundle {
    fn default() -> Self {
        Self {
            manifest_path: PathBuf::new(),
            signature_path: PathBuf::new(),
            release_key_path: PathBuf::new(),
            kernel_path: PathBuf::new(),
            rootfs_path: PathBuf::new(),
        }
    }
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
    let parsed: ManifestVersion = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("OTA rollback check: malformed MANIFEST.json: {}", e))?;
    Ok(parsed
        .version
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty()))
}

/// CE-183: manifest-declared package status. Missing/unparseable status is
/// treated as release (fail-closed), matching the target sysupgrade script.
fn manifest_declares_release_status(manifest_bytes: &[u8]) -> bool {
    let status = serde_json::from_slice::<serde_json::Value>(manifest_bytes)
        .ok()
        .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "release".to_string());
    matches!(status.trim(), "release" | "production" | "stable")
}

fn verify_sysupgrade_extracted_bundle(
    extracted_root: &Path,
    allow_unsigned: bool,
    pinned_release_key_path: Option<&Path>,
) -> Result<SysupgradeBundle, String> {
    // Find `sysupgrade-*/` payload subdir (matches the shell script's logic).
    let entries = std::fs::read_dir(extracted_root).map_err(|e| {
        format!(
            "OTA bundle: failed to read extracted root '{}': {}",
            extracted_root.display(),
            e
        )
    })?;
    let mut payload_dir: Option<PathBuf> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("sysupgrade-") {
                    payload_dir = Some(path);
                    break;
                }
            }
        }
    }
    let payload_dir = payload_dir
        .ok_or_else(|| "OTA bundle is missing sysupgrade-* payload directory".to_string())?;

    let manifest_path = payload_dir.join("MANIFEST.json");
    let signature_path = payload_dir.join("MANIFEST.sig");
    let release_key_path = payload_dir.join("release_ed25519.pub");
    let kernel_path = payload_dir.join("kernel");
    let rootfs_path = payload_dir.join("root");

    if !manifest_path.is_file() {
        return Err("OTA bundle is missing MANIFEST.json".to_string());
    }
    if !kernel_path.is_file() || !rootfs_path.is_file() {
        return Err("OTA bundle must contain both kernel and root payloads".to_string());
    }

    let signature_present = signature_path.is_file();
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
        let manifest_bytes = std::fs::read(&manifest_path)
            .map_err(|e| format!("OTA bundle: failed to read MANIFEST.json: {}", e))?;
        if manifest_declares_release_status(&manifest_bytes) {
            return Err("OTA bundle declares release status but has no MANIFEST.sig — \
                        the allow_unsigned lab override does not apply to release-status \
                        packages (CE-183)"
                .to_string());
        }
        // Unsigned but explicitly allowed — no signature work to do.
        return Ok(SysupgradeBundle {
            manifest_path,
            signature_path,
            release_key_path,
            kernel_path,
            rootfs_path,
        });
    }

    if !release_key_path.is_file() {
        return Err("Signed OTA bundle is missing release_ed25519.pub".to_string());
    }

    // Read the embedded release pubkey (raw 32 bytes for ed25519-dalek).
    let embedded_key_bytes = std::fs::read(&release_key_path).map_err(|e| {
        format!(
            "OTA bundle: failed to read embedded release_ed25519.pub: {}",
            e
        )
    })?;
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
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| format!("OTA bundle: failed to read MANIFEST.json: {}", e))?;
    let signature_bytes = std::fs::read(&signature_path)
        .map_err(|e| format!("OTA bundle: failed to read MANIFEST.sig: {}", e))?;

    verify_sysupgrade_signature_bytes(
        &manifest_bytes,
        &signature_bytes,
        &embedded_key_bytes,
        pinned_release_key_path,
    )?;

    // Parity with the tar path: bind the kernel/root files to the verified
    // manifest so a valid signature over MANIFEST.json plus swapped payload
    // files is rejected.
    let kernel_sha = hash_file(&kernel_path, "kernel payload")?;
    let rootfs_sha = hash_file(&rootfs_path, "root payload")?;
    enforce_declared_payload_hashes(&manifest_bytes, &kernel_sha, &rootfs_sha)?;

    Ok(SysupgradeBundle {
        manifest_path,
        signature_path,
        release_key_path,
        kernel_path,
        rootfs_path,
    })
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
    if !archive.kernel_present || !archive.rootfs_present {
        return Err("OTA bundle must contain both kernel and root payloads".to_string());
    }

    if archive.signature.is_empty() {
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
            return Err("OTA bundle declares release status but has no MANIFEST.sig — \
                        the allow_unsigned lab override does not apply to release-status \
                        packages (CE-183)"
                .to_string());
        }
        // Unsigned lab bundle: the manifest is not authenticated, but if it
        // declares payload hashes still confirm the payloads match (accidental
        // swap / on-wire corruption detection).
        enforce_declared_payload_hashes(
            &archive.manifest,
            &archive.kernel_sha256,
            &archive.rootfs_sha256,
        )?;
        return Ok(archive.bundle);
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

    // Bind the payload bytes to the now-verified manifest: a valid signature over
    // MANIFEST.json only proves the manifest is authentic. Re-hash the kernel/root
    // payloads and confirm they match the signed `payloads.*.sha256`, so a bundle
    // with a valid signature but swapped payloads is rejected here rather than
    // relying solely on the shell sysupgrade's later re-check.
    enforce_declared_payload_hashes(
        &archive.manifest,
        &archive.kernel_sha256,
        &archive.rootfs_sha256,
    )?;

    Ok(archive.bundle)
}

#[derive(Debug, Default)]
struct TarSysupgradeArchive {
    bundle: SysupgradeBundle,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    release_key: Vec<u8>,
    kernel_present: bool,
    rootfs_present: bool,
    /// sha256 (lowercase hex) of the kernel payload bytes as they appear in the
    /// tar, computed while streaming past the entry. Compared against the signed
    /// manifest's `payloads.kernel.sha256` so a valid-signature bundle with
    /// swapped payloads is rejected.
    kernel_sha256: String,
    /// sha256 (lowercase hex) of the root payload bytes as they appear in the tar.
    rootfs_sha256: String,
}

fn read_sysupgrade_tar<R: Read + Seek>(reader: &mut R) -> Result<TarSysupgradeArchive, String> {
    const BLOCK: u64 = 512;
    const MAX_METADATA_BYTES: u64 = 1024 * 1024;

    let mut archive = TarSysupgradeArchive::default();
    let mut payload_prefix: Option<String> = None;
    let mut seen_payload_files = std::collections::BTreeSet::<String>::new();

    loop {
        let mut header = [0u8; BLOCK as usize];
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("OTA bundle: failed reading tar header: {}", e)),
        }

        if header.iter().all(|&b| b == 0) {
            break;
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

                match leaf {
                    "MANIFEST.json" => {
                        archive.bundle.manifest_path = PathBuf::from(&name);
                        archive.manifest = read_small_tar_entry(reader, size, MAX_METADATA_BYTES)?;
                    }
                    "MANIFEST.sig" => {
                        archive.bundle.signature_path = PathBuf::from(&name);
                        archive.signature = read_small_tar_entry(reader, size, MAX_METADATA_BYTES)?;
                    }
                    "release_ed25519.pub" => {
                        archive.bundle.release_key_path = PathBuf::from(&name);
                        archive.release_key =
                            read_small_tar_entry(reader, size, MAX_METADATA_BYTES)?;
                    }
                    "kernel" => {
                        archive.bundle.kernel_path = PathBuf::from(&name);
                        archive.kernel_present = true;
                        archive.kernel_sha256 = hash_tar_payload(reader, size, "kernel payload")?;
                    }
                    "root" => {
                        archive.bundle.rootfs_path = PathBuf::from(&name);
                        archive.rootfs_present = true;
                        archive.rootfs_sha256 = hash_tar_payload(reader, size, "root payload")?;
                    }
                    "METADATA" | "SHA256SUMS" => {
                        seek_current(reader, size, &format!("tar entry '{}'", name))?;
                    }
                    _ => unreachable!("classify_sysupgrade_tar_entry rejects unknown leaves"),
                }
                skip_tar_padding(reader, size)?;
            }
        }
    }

    if payload_prefix.is_none() {
        return Err("OTA bundle is missing sysupgrade-* payload directory".to_string());
    }

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

#[derive(Debug, Clone, Copy)]
enum SysupgradeTarEntry<'a> {
    Directory { prefix: &'a str },
    File { prefix: &'a str, leaf: &'a str },
}

fn classify_sysupgrade_tar_entry(path: &str) -> Result<SysupgradeTarEntry<'_>, String> {
    let path = path.trim_start_matches("./");
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
    if !is_sysupgrade_prefix(prefix) {
        return Err(format!(
            "OTA bundle tar entry '{}' is outside the sysupgrade payload directory",
            path
        ));
    }

    if parts.len() == 1 {
        return Ok(SysupgradeTarEntry::Directory { prefix });
    }

    if directory_path || parts.len() != 2 {
        return Err(format!(
            "OTA bundle tar entry '{}' must be a direct sysupgrade payload file",
            path
        ));
    }

    let leaf = parts[1];
    if !is_allowed_sysupgrade_leaf(leaf) {
        return Err(format!(
            "OTA bundle tar entry '{}' is not an allowed sysupgrade payload",
            path
        ));
    }

    Ok(SysupgradeTarEntry::File { prefix, leaf })
}

fn is_sysupgrade_prefix(prefix: &str) -> bool {
    prefix
        .strip_prefix("sysupgrade-")
        .map(|suffix| !suffix.is_empty())
        .unwrap_or(false)
}

fn is_allowed_sysupgrade_leaf(leaf: &str) -> bool {
    matches!(
        leaf,
        "kernel"
            | "root"
            | "METADATA"
            | "SHA256SUMS"
            | "MANIFEST.json"
            | "MANIFEST.sig"
            | "release_ed25519.pub"
    )
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

/// Extract `payloads.kernel.sha256` and `payloads.rootfs.sha256` (lowercased) from
/// a MANIFEST.json byte buffer. Missing fields return `None` — a signed manifest
/// cannot omit them without invalidating its signature, and legacy minimal
/// manifests that never declared them simply aren't payload-hash-enforced.
fn parse_manifest_payload_hashes(manifest_bytes: &[u8]) -> (Option<String>, Option<String>) {
    // Tolerant by design: a manifest that isn't valid JSON (or omits `payloads`)
    // simply declares no hashes, so nothing is enforced. The signature already
    // covers the raw manifest bytes, so this only ADDS a check when the signed
    // manifest declares a payload hash — it never newly-rejects a bundle that
    // previously verified.
    let root: serde_json::Value = match serde_json::from_slice(manifest_bytes) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let payloads = root.get("payloads");
    let pick = |key: &str| -> Option<String> {
        payloads
            .and_then(|p| p.get(key))
            .and_then(|entry| entry.get("sha256"))
            .and_then(|s| s.as_str())
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
    };
    (pick("kernel"), pick("rootfs"))
}

/// Bind the actual kernel/root payload bytes to the (already signature-verified)
/// manifest: when the manifest declares `payloads.{kernel,rootfs}.sha256`, the
/// computed digest MUST match. This is the check that closes the
/// "valid MANIFEST.sig + swapped payloads" gap — the manifest signature covers
/// the declared hashes, so re-hashing the payloads and comparing extends the
/// signature's authenticity to the payload bytes themselves. Enforced only when a
/// hash is declared (a signed manifest cannot drop the `payloads` block without
/// breaking its signature, so every real signed bundle is covered).
fn enforce_declared_payload_hashes(
    manifest_bytes: &[u8],
    kernel_sha256: &str,
    rootfs_sha256: &str,
) -> Result<(), String> {
    let (want_kernel, want_rootfs) = parse_manifest_payload_hashes(manifest_bytes);
    check_declared_hash("kernel", want_kernel.as_deref(), kernel_sha256)?;
    check_declared_hash("root", want_rootfs.as_deref(), rootfs_sha256)?;
    Ok(())
}

fn check_declared_hash(label: &str, want: Option<&str>, got: &str) -> Result<(), String> {
    if let Some(want) = want {
        if want != got.to_ascii_lowercase() {
            return Err(format!(
                "OTA bundle: {} payload sha256 mismatch (manifest declares {}, computed {}) \
                 - refusing bundle whose payloads do not match the signed manifest",
                label, want, got
            ));
        }
    }
    Ok(())
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
    let name = tar_string(&header[0..100]);
    if name.is_empty() {
        return Err("OTA bundle tar entry has empty name".to_string());
    }
    let prefix = tar_string(&header[345..500]);
    let path = if prefix.is_empty() {
        name
    } else {
        format!("{}/{}", prefix, name)
    };
    Ok(path.trim_start_matches("./").to_string())
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
    let root_value: serde_json::Value = match serde_json::from_slice(manifest_bytes) {
        Ok(v) => v,
        Err(_) => return Ok(None),
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

    #[test]
    fn classify_sysupgrade_tar_entry_rejects_all_hostile_member_names() {
        // Security pin (priority 5: upgrade reliability). classify_sysupgrade_tar_entry
        // is the extraction ALLOWLIST for a signed sysupgrade bundle: a malicious tar
        // member with path-traversal (`..`), an absolute path, a backslash, a nested
        // directory, or a non-allowlisted leaf MUST be rejected so a bundle can never
        // write outside the payload dir or drop an unexpected file. This load-bearing
        // validator shipped with no regression test — add a property + fuzz pin.

        // Positive: the only two accepted shapes.
        assert!(matches!(
            classify_sysupgrade_tar_entry("sysupgrade-am1-s9"),
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
        ] {
            let p = format!("sysupgrade-am1-s9/{leaf}");
            assert!(
                matches!(
                    classify_sysupgrade_tar_entry(&p),
                    Ok(SysupgradeTarEntry::File { .. })
                ),
                "expected {p} to classify as a File"
            );
            assert!(matches!(
                classify_sysupgrade_tar_entry(&format!("./{p}")),
                Ok(SysupgradeTarEntry::File { .. })
            ));
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
            "sysupgrade-x/sub/kernel", // nested (parts != 2)
            "sysupgrade-x/evil",       // disallowed leaf
            "sysupgrade-x/kernel/",    // dir-shaped file
            "notprefix/kernel",        // wrong prefix
            "sysupgrade-",             // empty suffix
            "sysupgrade-x\\kernel",    // backslash
            "kernel",                  // no prefix
            "sysupgrade-x/KERNEL",     // case-sensitive leaf
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

        tar.extend_from_slice(&header);
        tar.extend_from_slice(payload);
        let padding = (512 - (payload.len() % 512)) % 512;
        tar.extend(std::iter::repeat_n(0u8, padding));
    }

    fn finish_tar(mut tar: Vec<u8>) -> Cursor<Vec<u8>> {
        tar.extend(std::iter::repeat_n(0u8, 512));
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
        assert_eq!(
            archive.bundle.kernel_path,
            PathBuf::from("sysupgrade-am1-s9/kernel")
        );
        assert_eq!(
            archive.bundle.rootfs_path,
            PathBuf::from("sysupgrade-am1-s9/root")
        );
        assert!(archive.kernel_present);
        assert!(archive.rootfs_present);
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
        assert!(
            err.contains("outside the sysupgrade payload directory"),
            "err = {err}"
        );
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
        let mut tar = Vec::new();
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/", b'5', &[]);
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/kernel", b'0', b"kernel");
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/root", b'0', b"rootfs");
        append_tar_entry(&mut tar, "sysupgrade-am1-s9/MANIFEST.json", b'0', manifest);
        if let Some(sig) = signature {
            append_tar_entry(&mut tar, "sysupgrade-am1-s9/MANIFEST.sig", b'0', sig);
        }
        if let Some(key) = embedded_pubkey {
            append_tar_entry(&mut tar, "sysupgrade-am1-s9/release_ed25519.pub", b'0', key);
        }
        // finish_tar returns a Cursor; we want the raw bytes to write to disk.
        finish_tar(tar).into_inner()
    }

    #[test]
    fn bundle_valid_sig_and_trusted_on_disk_key_accepts() {
        let scratch = ota_scratch_dir("accept");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest = br#"{"board_target":"am1-s9","version":"0.20.1"}"#;
        let sig = signing.sign(manifest).to_bytes();

        let tar_bytes = build_sysupgrade_tar_bytes(manifest, Some(&sig), Some(&pubkey));
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
        let manifest = format!(
            r#"{{"board_target":"am1-s9","version":"0.20.1","payloads":{{"kernel":{{"sha256":"{}"}},"rootfs":{{"sha256":"{}"}}}}}}"#,
            sha256_hex(b"kernel"),
            sha256_hex(b"rootfs")
        );
        let sig = signing.sign(manifest.as_bytes()).to_bytes();
        let tar_bytes = build_sysupgrade_tar_bytes(manifest.as_bytes(), Some(&sig), Some(&pubkey));
        let tar_path = scratch.join("ok.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();

        let res = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path));
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            res.is_ok(),
            "valid sig + matching payload hashes must accept: {res:?}"
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
        let manifest = format!(
            r#"{{"board_target":"am1-s9","version":"0.20.1","payloads":{{"kernel":{{"sha256":"{}"}},"rootfs":{{"sha256":"{}"}}}}}}"#,
            sha256_hex(b"kernel"),
            sha256_hex(b"a-different-rootfs-image") // tar actually contains b"rootfs"
        );
        let sig = signing.sign(manifest.as_bytes()).to_bytes();
        let tar_bytes = build_sysupgrade_tar_bytes(manifest.as_bytes(), Some(&sig), Some(&pubkey));
        let tar_path = scratch.join("bad.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();

        let err = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path))
            .expect_err("valid sig but swapped payload must be rejected");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            err.contains("payload sha256 mismatch"),
            "expected payload-hash mismatch rejection, got: {err}"
        );
    }

    #[test]
    fn bundle_valid_sig_no_declared_payload_hashes_still_accepts() {
        // Backward compatibility: a signed manifest that declares no payload
        // hashes (legacy minimal manifest) is still accepted — enforcement is
        // additive and only fires when a hash is declared.
        let scratch = ota_scratch_dir("payload-none");
        let signing = make_key();
        let pubkey = signing.verifying_key().to_bytes();
        let manifest = br#"{"board_target":"am1-s9","version":"0.20.1"}"#;
        let sig = signing.sign(manifest).to_bytes();
        let tar_bytes = build_sysupgrade_tar_bytes(manifest, Some(&sig), Some(&pubkey));
        let tar_path = scratch.join("none.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();

        let res = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path));
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            res.is_ok(),
            "signed bundle with no declared payload hashes must still accept: {res:?}"
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
        let manifest = format!(
            r#"{{"board_target":"am1-s9","version":"0.20.1","payloads":{{"kernel":{{"sha256":"{}"}},"rootfs":{{"sha256":"{}"}}}}}}"#,
            sha256_hex(b"kernel"),
            sha256_hex(b"a-different-rootfs-image") // actual root file is b"rootfs"
        );
        let sig = signing.sign(manifest.as_bytes()).to_bytes();
        std::fs::write(payload_dir.join("kernel"), b"kernel").unwrap();
        std::fs::write(payload_dir.join("root"), b"rootfs").unwrap();
        std::fs::write(payload_dir.join("MANIFEST.json"), manifest.as_bytes()).unwrap();
        std::fs::write(payload_dir.join("MANIFEST.sig"), sig).unwrap();
        std::fs::write(payload_dir.join("release_ed25519.pub"), pubkey).unwrap();
        let pin_path = scratch.join("release_ed25519.pub");
        std::fs::write(&pin_path, pubkey).unwrap();

        let err = verify_sysupgrade_bundle(&scratch, false, Some(&pin_path))
            .expect_err("extracted bundle: valid sig but swapped payload must be rejected");
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            err.contains("payload sha256 mismatch"),
            "expected payload-hash mismatch rejection, got: {err}"
        );
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
        // A genuine non-release lab bundle still works under allow_unsigned.
        let scratch = ota_scratch_dir("ce183-lab");
        let manifest = br#"{"board_target":"am1-s9","status":"lab_unsigned"}"#;
        let tar_bytes = build_sysupgrade_tar_bytes(manifest, None, None);
        let tar_path = scratch.join("unsigned-lab.tar");
        std::fs::write(&tar_path, &tar_bytes).unwrap();
        let res = verify_sysupgrade_bundle(&tar_path, true, None);
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            res.is_ok(),
            "unsigned lab-status bundle must still accept with allow_unsigned: {res:?}"
        );
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

        format!(
            r#"{{"board_target":"am1-s9","version":"0.21.0","ota_intermediate_cert":{{"root_key_hex":"{root_hex}","intermediate_key_hex":"{inter_hex}","not_before":{not_before},"not_after":{not_after}{serial_json},"root_signature_hex":"{root_sig_hex}"}}{rev_json}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn legacy_single_key_manifest_has_no_cert_and_uses_direct_path() {
        // A manifest with no ota_intermediate_cert => Ok(None) => the caller
        // runs the legacy verify_raw path (byte-identical to pre-W8).
        let manifest = br#"{"board_target":"am1-s9","version":"0.20.1"}"#;
        let cert = parse_intermediate_cert_from_manifest(manifest)
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
        let sig = signing.sign(manifest).to_bytes();
        let tar_bytes = build_sysupgrade_tar_bytes(manifest, Some(&sig), Some(&pubkey));
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
    fn full_bundle_with_two_level_chain_verifies() {
        // End-to-end through verify_sysupgrade_bundle: the embedded
        // release_ed25519.pub IS the root key; the on-disk pin matches it; the
        // manifest carries a root-signed cert; the MANIFEST.sig is the
        // intermediate's signature over the manifest.
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

        let res = verify_sysupgrade_bundle(&tar_path, false, Some(&pin_path));
        std::fs::remove_dir_all(&scratch).ok();
        assert!(
            res.is_ok(),
            "valid two-level chain bundle must verify end-to-end: {res:?}"
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
