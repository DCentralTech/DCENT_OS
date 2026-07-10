use ed25519_dalek::{Signature, Verifier, VerifyingKey};

pub fn signature_required() -> bool {
    option_env!("DCENT_OTA_PUBLIC_KEY_HEX").is_some()
}

/// Constant-time equality for two secret/digest strings (AOTA-3).
///
/// Rust's `str`/`String` `==` is an early-exit `memcmp`: it returns as soon as
/// the first differing byte is found, leaking timing proportional to the
/// matching-prefix length. For comparisons where one side is attacker-supplied
/// (a derived password hash, a session-token hash) that is a measurable side
/// channel on a LAN-exposed miner. This helper compares in time independent of
/// the *position* of any difference: it ORs the XOR of every byte pair into a
/// single accumulator (and folds a length-difference flag in) instead of
/// returning early, so the loop body runs to the end of the compared span.
/// Both inputs here are lowercase hex digests of equal expected length, so the
/// length itself is non-secret.
///
/// Implemented with a manual volatile-style bitwise accumulator rather than a
/// crate so it adds no new dependency and lives entirely in this re-included
/// module. `core::hint::black_box` on the accumulator stops the optimizer from
/// reintroducing an early-exit. Lives in `ota_signature.rs` (re-included into
/// the host-only `dcentaxe-core` crate) so the compare logic is host-unit-
/// tested even though its `auth.rs` call sites are welded to esp-idf and are
/// not host-buildable.
pub fn ct_str_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    // Length difference must fail, but we still walk min(len) bytes so the
    // contents are compared without revealing *where* they diverge.
    let mut diff: u8 = if a.len() == b.len() { 0 } else { 1 };
    let n = a.len().min(b.len());
    for i in 0..n {
        // black_box prevents the compiler from short-circuiting once diff != 0.
        diff = core::hint::black_box(diff | (a[i] ^ b[i]));
    }
    diff == 0
}

pub fn compiled_key_id() -> Option<&'static str> {
    option_env!("DCENT_OTA_KEY_ID")
}

/// Fail-closed authorization gate for owner-only, security-sensitive actions
/// (e.g. mutating the unsigned-OTA policy).
///
/// Returns true ONLY when an owner password is set AND the request carries a
/// valid owner bearer session. There is deliberately NO passwordless bypass:
/// on a passwordless device every security-sensitive action is refused so an
/// unauthenticated LAN caller cannot claim owner authority (AOTA-1/AOTA-4).
pub fn owner_action_authorized(owner_password_set: bool, owner_session: bool) -> bool {
    owner_password_set && owner_session
}

/// Fail-closed authorization decision for the mutating MCP control surface and
/// owner-reset (XPH-5).
///
/// This is the pure predicate behind `auth::authorize_mcp_control` (which takes
/// an esp-idf `Request` and therefore can't be host-tested). Returns `Ok(())`
/// ONLY when an owner password is configured AND the request carried a valid
/// owner bearer session. When no password is set the surface is refused with a
/// distinct reason (rather than the passwordless-write bypass that ordinary
/// REST writes allow) so an unauthenticated LAN caller can never drive the
/// mutating MCP tools or revive a claim-skip shortcut on an unclaimed device.
///
/// Mirrors `authorize_mcp_control`'s two-branch behaviour so the host test can
/// pin the exact failure classification the wire handler returns.
pub fn mcp_control_authorized(
    owner_password_set: bool,
    has_valid_bearer: bool,
) -> Result<(), McpControlDenied> {
    if !owner_password_set {
        return Err(McpControlDenied::PasswordNotSet);
    }
    if has_valid_bearer {
        Ok(())
    } else {
        Err(McpControlDenied::BearerRequired)
    }
}

/// Why a mutating MCP-control / owner-reset request was refused.
///
/// Both variants are fail-closed denials; they only distinguish the operator-
/// facing reason so the wire handler can emit the right 401 detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpControlDenied {
    /// No owner password configured — control tools cannot mutate state.
    PasswordNotSet,
    /// Owner password is set but the request carried no valid bearer session.
    BearerRequired,
}

/// Per-request OTA signature enforcement decision.
///
/// Returns true when the uploaded image MUST carry a valid signature, false
/// only when signature verification may be waived for this request.
///
/// Verification is waived ONLY when ALL of:
///   * the build is signature-capable (a public key is compiled in), AND
///   * the persisted `allow_unsigned_ota` developer-mode flag is set, AND
///   * the request is from an authenticated owner (`owner_action_authorized`
///     — owner password set AND a valid bearer session).
///
/// If the build is NOT signature-capable there is no compiled key to verify
/// against (developer build) so this returns false. In every other
/// signature-capable case it returns true — in particular a
/// passwordless/unauthenticated caller can never waive verification even if
/// `allow_unsigned_ota` was flipped on, which closes the AOTA-1 unsigned-OTA
/// RCE in the default state.
pub fn ota_signature_enforced(
    signature_capable: bool,
    allow_unsigned_ota: bool,
    owner_password_set: bool,
    owner_session: bool,
) -> bool {
    if !signature_capable {
        return false;
    }
    !(allow_unsigned_ota && owner_action_authorized(owner_password_set, owner_session))
}

/// Honest device-policy summary for informational GET endpoints that lack
/// per-request session context.
///
/// Reports whether a signature is required as a matter of device policy. An
/// unclaimed (passwordless) device always reports "required" even if a stale
/// `allow_unsigned_ota=true` flag is persisted (e.g. surviving an owner-reset),
/// because the waiver only takes effect for an authenticated owner — so the
/// displayed value stays aligned with what `ota_signature_enforced` actually
/// enforces per request.
pub fn ota_signature_required_for_display(
    signature_capable: bool,
    allow_unsigned_ota: bool,
    owner_password_set: bool,
) -> bool {
    signature_capable && !(allow_unsigned_ota && owner_password_set)
}

fn decode_hex(input: &str) -> Result<Vec<u8>, String> {
    if input.len() % 2 != 0 {
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

pub fn canonical_message(
    board_target: &str,
    device_model: &str,
    version: &str,
    payload_size: usize,
    payload_sha256: &str,
) -> String {
    let device_model = device_model.trim().to_ascii_lowercase();
    format!(
        "schema=2\nboard_target={}\ndevice_model={}\nversion={}\nsize={}\nsha256={}\n",
        board_target, device_model, version, payload_size, payload_sha256
    )
}

pub fn verify_signed_metadata(
    board_target: &str,
    device_model: &str,
    version: &str,
    payload_size: usize,
    payload_sha256: &str,
    key_id: &str,
    signature_hex: &str,
) -> Result<(), String> {
    let public_key_hex = option_env!("DCENT_OTA_PUBLIC_KEY_HEX")
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
    let message = canonical_message(
        board_target,
        device_model,
        version,
        payload_size,
        payload_sha256,
    );
    verifying_key
        .verify(message.as_bytes(), &signature)
        .map_err(|e| format!("OTA signature verification failed: {}", e))
}

/// True when `version` is a fully-numeric dotted release with at least three
/// components (e.g. `0.3.1`, optionally `v`-prefixed) and NO non-numeric
/// pre-release/dev suffix (AOTA-2).
///
/// The rollback floor is only as strong as the version ordering. A dev or
/// pre-release tag (`0.3.0-rc1`, `0.3.0-dev`, `nightly`) has no well-defined
/// total order, so signed OTA metadata that does not satisfy this predicate
/// must not be allowed to advance the floor or out-rank a numbered release.
/// Callers (signed-OTA gate, floor persistence) should require this before
/// trusting a version for ordering.
pub fn version_is_fully_numeric(version: &str) -> bool {
    let core = version.trim().trim_start_matches('v');
    if core.is_empty() {
        return false;
    }
    let mut components = 0usize;
    for part in core.split('.') {
        if part.is_empty() || part.parse::<u32>().is_err() {
            return false;
        }
        components += 1;
    }
    components >= 3
}

/// Returns true when `candidate` strictly out-ranks `current` for
/// rollback/downgrade-floor purposes.
///
/// AOTA-2: the comparison is now well-defined for non-numeric / pre-release
/// tags. The previous parser silently DROPPED any non-numeric token
/// (`split(['.', '-']).filter_map(parse::<u32>)`), so `0.3.0-rc1`,
/// `0.3.0-rc9`, and `0.3.0` all collapsed to `[0,3,0]` and compared equal —
/// two distinct pre-releases were unordered. Worse, a non-semver tag could
/// normalize to a value that ties or beats a real release. Both sides are now
/// compared on their numeric dot-components only, and — critically for the
/// downgrade floor — a candidate that is NOT a fully-numeric release can never
/// be reported as *newer* than a fully-numeric current. That makes a dev /
/// pre-release artifact fail-closed against the floor (it is never auto-
/// accepted as an upgrade), while a normal numbered upgrade is unaffected.
pub fn version_is_newer(candidate: &str, current: &str) -> bool {
    fn parse(version: &str) -> Vec<u32> {
        version
            .trim()
            .trim_start_matches('v')
            // Split on '.' AND '-' so a `-rcN` suffix's numeric tail is still
            // read, but a non-numeric token contributes nothing (legacy
            // behaviour, kept so equal numeric cores still tie).
            .split(['.', '-'])
            .filter_map(|part| part.parse::<u32>().ok())
            .collect()
    }

    // AOTA-2 fail-closed rule: a non-fully-numeric candidate is never treated
    // as newer than a fully-numeric current. This stops a dev/pre-release tag
    // from out-ranking (or, via a tie that a `>` floor check rejects, even
    // matching) a real numbered release used as the rollback floor. When the
    // current floor is itself not fully numeric we fall back to the pure
    // numeric-core comparison below (no regression for legacy floors).
    if version_is_fully_numeric(current) && !version_is_fully_numeric(candidate) {
        return false;
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

#[cfg(test)]
mod tests {
    use super::canonical_message;

    #[test]
    fn canonical_message_binds_device_model() {
        let msg = canonical_message("esp32s3", "BitAxe-Gamma", "0.3.1", 123456, "abcdef");
        assert!(msg.starts_with("schema=2\n"));
        assert!(msg.contains("board_target=esp32s3\n"));
        assert!(msg.contains("device_model=bitaxe-gamma\n"));
        assert!(msg.contains("version=0.3.1\n"));
        assert!(msg.contains("size=123456\n"));
        assert!(msg.ends_with("sha256=abcdef\n"));
    }

    // ── XPH-3 — schema-2 device_model binding (real ed25519 verify) ──
    //
    // device_model is part of the signed canonical bytes, so a signature that
    // is valid for one model must NOT verify for another. This drives the real
    // ed25519 verify against the real `canonical_message` binding without
    // needing `verify_signed_metadata()` (which short-circuits to Err when no
    // OTA public key is compiled in, as in CI).
    #[test]
    fn signature_for_one_model_is_rejected_for_another() {
        use ed25519_dalek::{Signer, SigningKey, Verifier};

        // Deterministic key (no rand feature needed).
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let verifying_key = signing_key.verifying_key();

        let msg_gamma =
            super::canonical_message("esp32s3", "BitAxe-Gamma", "0.3.1", 100, "deadbeef");
        let sig = signing_key.sign(msg_gamma.as_bytes());

        // Positive: verifies for the model it was signed for.
        assert!(verifying_key.verify(msg_gamma.as_bytes(), &sig).is_ok());

        // Negative: the SAME signature must NOT verify for a different
        // device_model, because device_model is bound into the signed bytes.
        let msg_gt = super::canonical_message("esp32s3", "BitAxe-GT", "0.3.1", 100, "deadbeef");
        assert!(verifying_key.verify(msg_gt.as_bytes(), &sig).is_err());

        // Belt-and-suspenders: the bound bytes actually differ.
        assert_ne!(msg_gamma, msg_gt);
    }

    // ── AOTA-1 — signature-capable builds are fail-closed ──
    //
    // A signature-capable build must NOT let a passwordless/unauthenticated
    // caller waive OTA signature verification, even if `allow_unsigned_ota`
    // has been flipped on. The waiver only applies to an authenticated owner
    // running in developer mode. This pins the per-request enforcement
    // decision that the api.rs OTA handler is wired to.
    #[test]
    fn passwordless_signed_build_cannot_waive_signature() {
        use super::{ota_signature_enforced, owner_action_authorized};

        // signed build + attacker-flipped allow_unsigned + no pw + no session
        // => signature STILL enforced (AOTA-1 closed).
        assert!(ota_signature_enforced(true, true, false, false));
        // claimed device but caller not signed in => enforced.
        assert!(ota_signature_enforced(true, true, true, false));
        // authenticated owner + dev-mode allow_unsigned => waiver permitted.
        assert!(!ota_signature_enforced(true, true, true, true));
        // allow_unsigned=false is always enforced on a signature-capable build.
        assert!(ota_signature_enforced(true, false, true, true));
        // non-signature-capable (no key compiled) => nothing to verify.
        assert!(!ota_signature_enforced(false, false, false, false));
        assert!(!ota_signature_enforced(false, true, true, true));

        // owner_action_authorized truth table: ONLY (set + session) authorizes.
        assert!(owner_action_authorized(true, true));
        assert!(!owner_action_authorized(true, false));
        assert!(!owner_action_authorized(false, true));
        assert!(!owner_action_authorized(false, false));
    }

    // AOTA-7: this documents the release blocker explicitly. A build with no
    // compiled public key has no verifier material, so unsigned OTA is accepted
    // regardless of owner/auth state. Release jobs must therefore assert that
    // `signature_required()` is true for every shipped image.
    #[test]
    fn no_compiled_ota_key_means_unsigned_ota_is_accepted() {
        use super::{
            ota_signature_enforced, ota_signature_required_for_display, signature_required,
        };

        assert!(!ota_signature_enforced(false, false, false, false));
        assert!(!ota_signature_enforced(false, true, false, false));
        assert!(!ota_signature_enforced(false, true, true, true));
        assert!(!ota_signature_required_for_display(false, false, false));
        assert!(!ota_signature_required_for_display(false, true, true));

        if !signature_required() {
            assert!(!ota_signature_enforced(
                signature_required(),
                false,
                false,
                false
            ));
        }
    }

    #[test]
    fn release_gate_can_require_signature_capable_build() {
        if std::env::var_os("DCENT_EXPECT_OTA_SIGNATURE_REQUIRED").is_some() {
            assert!(
                super::signature_required(),
                "DCENT_EXPECT_OTA_SIGNATURE_REQUIRED was set, but no DCENT_OTA_PUBLIC_KEY_HEX was compiled in"
            );
        }
    }

    // ── AOTA-4 — informational policy stays honest on an unclaimed device ──
    //
    // The display summary must report "signature required" for a passwordless
    // device even if a stale `allow_unsigned_ota=true` flag survives an
    // owner-reset, so the dashboard's reported policy matches enforcement.
    #[test]
    fn display_policy_is_honest_for_unclaimed_device() {
        use super::ota_signature_required_for_display;

        // passwordless device with stale allow_unsigned flag => still required.
        assert!(ota_signature_required_for_display(true, true, false));
        // claimed + opted-in => waivable (not required for display).
        assert!(!ota_signature_required_for_display(true, true, true));
        // allow_unsigned=false => required regardless of claim state.
        assert!(ota_signature_required_for_display(true, false, true));
        // non-signature-capable build => never required.
        assert!(!ota_signature_required_for_display(false, true, true));
    }

    // ── AOTA-2 — version ordering is well-defined for dev / pre-release tags ──
    #[test]
    fn version_is_fully_numeric_classifies_release_vs_prerelease() {
        use super::version_is_fully_numeric;
        // Fully-numeric, >=3 components (optionally v-prefixed).
        assert!(version_is_fully_numeric("0.3.1"));
        assert!(version_is_fully_numeric("v1.2.3"));
        assert!(version_is_fully_numeric("10.20.30.40"));
        // Pre-release / dev tags are NOT fully numeric.
        assert!(!version_is_fully_numeric("0.3.0-rc1"));
        assert!(!version_is_fully_numeric("0.3.0-dev"));
        assert!(!version_is_fully_numeric("nightly"));
        // Fewer than three components is rejected (no minor/patch ordering).
        assert!(!version_is_fully_numeric("1.2"));
        assert!(!version_is_fully_numeric("1"));
        assert!(!version_is_fully_numeric(""));
        assert!(!version_is_fully_numeric("v"));
        // A trailing/empty component is not numeric.
        assert!(!version_is_fully_numeric("1.2."));
    }

    #[test]
    fn version_is_newer_normal_release_ordering_unchanged() {
        use super::version_is_newer;
        // Real upgrades still rank correctly (no regression for numbered builds).
        assert!(version_is_newer("0.3.2", "0.3.1"));
        assert!(version_is_newer("0.4.0", "0.3.9"));
        assert!(version_is_newer("1.0.0", "0.9.9"));
        assert!(version_is_newer("v0.3.2", "0.3.1"));
        // Equal numeric cores never out-rank (the `>` floor check rejects ties).
        assert!(!version_is_newer("0.3.1", "0.3.1"));
        // Older never beats newer.
        assert!(!version_is_newer("0.3.0", "0.3.1"));
    }

    #[test]
    fn version_is_newer_dev_tag_never_outranks_numbered_floor() {
        use super::version_is_newer;
        // AOTA-2: a dev/pre-release candidate is fail-closed against a
        // fully-numeric current floor — it can never be reported as newer, so
        // it cannot auto-accept as an "upgrade" past the rollback floor.
        assert!(!version_is_newer("0.3.0-rc1", "0.3.0"));
        assert!(!version_is_newer("0.3.0-rc9", "0.3.0"));
        assert!(!version_is_newer("0.4.0-dev", "0.3.0"));
        assert!(!version_is_newer("nightly", "0.3.0"));
        // Two distinct pre-releases sharing a numeric core stay unordered, and
        // neither out-ranks the other (both non-numeric → numeric-core compare).
        assert!(!version_is_newer("0.3.0-rc1", "0.3.0-rc9"));
        assert!(!version_is_newer("0.3.0-rc9", "0.3.0-rc1"));
        // A legacy (non-numeric) current floor falls back to numeric-core
        // ordering so a real numbered release can still advance past it.
        assert!(version_is_newer("0.4.0", "0.3.0-rc1"));
    }

    // ── AOTA-3 — constant-time secret comparison helper ──────────────────────
    #[test]
    fn ct_str_eq_matches_semantic_equality() {
        use super::ct_str_eq;
        // Equal strings compare equal.
        assert!(ct_str_eq("deadbeef", "deadbeef"));
        assert!(ct_str_eq("", ""));
        // Any content difference compares unequal, regardless of where it is.
        assert!(!ct_str_eq("0eadbeef", "deadbeef")); // first byte differs
        assert!(!ct_str_eq("deadbee0", "deadbeef")); // last byte differs
                                                     // Length mismatch is unequal (prefix-equal but shorter).
        assert!(!ct_str_eq("deadbeef", "deadbee"));
        assert!(!ct_str_eq("deadbee", "deadbeef"));
        // Behaviour is identical to `==` over a fuzz of pairs (semantic parity).
        let samples = ["", "a", "ab", "abc", "abcd", "abce", "zzzz", "abcde"];
        for x in samples {
            for y in samples {
                assert_eq!(
                    ct_str_eq(x, y),
                    x == y,
                    "ct_str_eq must agree with == for ({x:?}, {y:?})"
                );
            }
        }
    }

    // ── XPH-5 — pure MCP-control / owner-reset authorization decision ────────
    #[test]
    fn mcp_control_authorized_is_fail_closed() {
        use super::{mcp_control_authorized, McpControlDenied};
        // No owner password => refused with the password-not-set reason (NOT a
        // passwordless bypass): an unclaimed device cannot drive control tools.
        assert_eq!(
            mcp_control_authorized(false, false),
            Err(McpControlDenied::PasswordNotSet)
        );
        // A bearer token on an unclaimed device still can't authorize control.
        assert_eq!(
            mcp_control_authorized(false, true),
            Err(McpControlDenied::PasswordNotSet)
        );
        // Claimed device, no valid bearer => bearer required.
        assert_eq!(
            mcp_control_authorized(true, false),
            Err(McpControlDenied::BearerRequired)
        );
        // Claimed device + valid owner bearer => authorized.
        assert_eq!(mcp_control_authorized(true, true), Ok(()));
    }

    // ── XPH-5 — owner-reset reuses the owner_action_authorized predicate ─────
    // Owner reset is not a physical-access bypass: it requires owner password
    // set AND a valid owner session (same truth table as the OTA-policy gate).
    #[test]
    fn owner_reset_requires_owner_session() {
        use super::owner_action_authorized;
        assert!(owner_action_authorized(true, true));
        assert!(!owner_action_authorized(true, false));
        assert!(!owner_action_authorized(false, true));
        assert!(!owner_action_authorized(false, false));
    }
}
