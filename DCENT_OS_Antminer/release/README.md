# DCENT_OS Release Signing Keys

This directory holds the public-key material that pins firmware integrity for
production builds. The matching **private keys never live here** — they live
in an HSM, password-protected disk, or air-gapped vault, and are only
mounted on the build host long enough to sign a release artifact.

There are two independent ed25519 keypairs in the DCENT_OS supply chain:

| Keypair | Purpose | Pin variable | Generator |
|---|---|---|---|
| **Release signing key** | Signs the sysupgrade `MANIFEST.json` produced by `scripts/package_sysupgrade.sh`. Verified at install time on the miner. | `DCENT_RELEASE_SIGNING_KEY` (private path) + `DCENT_RELEASE_PUBKEY_FILE` (public PEM) | `scripts/generate_release_keypair.sh` |
| **Manifest pubkey pin** (this dir) | Compile-time-baked into `dcentrald` via `option_env!`. Verifies the at-rest stock-Bitmain manifest signature (`STOCK_MANIFEST_SIG_BAKED`) so the manifest can't be tampered with after the binary is built. | `DCENT_MANIFEST_PUBLIC_KEY_HEX` (raw 32-byte hex, 64 chars) + optional `DCENT_MANIFEST_KEY_ID` | this README's `openssl` recipe |

A 2026-05-07 change added a **CI gate** in `scripts/build_in_docker.sh` and the
`make release` target that **fails closed** when these are not set. Dev
builds opt out via `DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1` (or `make dev` /
`--lab-unsigned`).

---

## 1. Generate the manifest-pin keypair (operator step)

This is a one-time setup. Run it on a trusted, offline machine.

```bash
# Private key (PEM, raw ed25519). NEVER commit. NEVER copy to a build VM
# unless the VM is short-lived and audited.
openssl genpkey -algorithm ed25519 -out release_ed25519.priv.pem

# Matching public key (PEM, optional helper output).
openssl pkey -in release_ed25519.priv.pem -pubout -out release_ed25519.pub.pem

# Extract raw 32-byte ed25519 pubkey as 64 hex chars. This is the value
# the Rust crate's `option_env!("DCENT_MANIFEST_PUBLIC_KEY_HEX")` expects.
# DER SPKI for ed25519 is exactly 44 bytes (12-byte AlgorithmIdentifier +
# 32-byte pubkey). The 64-hex tail is the raw pubkey.
openssl pkey -in release_ed25519.priv.pem -pubout -outform DER \
    | xxd -p -c 64 \
    | tail -c 65 | head -c 64 \
    > release_ed25519.pub.hex

cat release_ed25519.pub.hex   # 64 hex chars; commit ONLY this if needed
```

A working example pubkey-hex file shape is included as
`release_ed25519.pub.hex.example` (NOT a real pin — its corresponding
private key was never generated).

## 2. Permission + storage rules

```bash
chmod 600 release_ed25519.priv.pem release_ed25519.priv.*.pem
```

- Private key (`release_ed25519.priv*`) is **gitignored** at the repo root
  (`.gitignore` excludes the private files but keeps the public hex
  trackable).
- Move the private key off the build host into your HSM/Vault as soon
  as the keypair is generated. Bring it back only at sign-time.

### Beta public-release key custody (2026-06-15)

The beta-public XIL gate uses a release keypair generated outside the
repository and held in your secret manager / HSM. Reference it by path or
environment variable only — never check the private key in:

```text
$DCENT_RELEASE_KEY_DIR/dcent_beta_release_ed25519.pem
$DCENT_RELEASE_KEY_DIR/dcent_beta_release_ed25519.pub
```

Do not regenerate it for this beta. The public key decodes to raw Ed25519
hex:

```text
26985575eae77d56c490ceeb9054af012eab5ae59119cd20eaa70dd7e722df83
```

Release builds reference the private key only through environment variables
or a mounted read-only path:

```bash
export DCENT_RELEASE_SIGNING_KEY=/secure/path/dcent_beta_release_ed25519.pem
export DCENT_RELEASE_PUBKEY_FILE=/secure/path/dcent_beta_release_ed25519.pub
export DCENT_MANIFEST_PUBLIC_KEY_HEX=26985575eae77d56c490ceeb9054af012eab5ae59119cd20eaa70dd7e722df83
export DCENT_MANIFEST_KEY_ID=dcent-beta-2026-06
export DCENT_RELEASE_IMAGE=1
export DCENT_REQUIRE_RELEASE_KEY=1
```

The `.pem` private key must never be copied into the repo, artifact tarball,
Docker image layer, CI log, or support bundle. To rotate, generate a new
release keypair, update the toolbox production pin, rebuild `dcentrald` with
the new `DCENT_MANIFEST_PUBLIC_KEY_HEX`, sign fresh artifacts, and retire the
old key only after the replacement release has been verified.

## 3. Sign the stock-Bitmain manifest (release-time)

```bash
# Sign the stock-Bitmain manifest.
# Output is written to .../stock-bitmain-manifest.json.sig (raw 64-byte
# ed25519 signature, exactly the shape `verify_manifest_signature` expects).
bash DCENT_OS_Antminer/scripts/sign_stock_manifest.sh \
    /secure/path/release_ed25519.priv.pem
```

## 4. Build with the pin baked in

```bash
export DCENT_MANIFEST_PUBLIC_KEY_HEX="$(cat release_ed25519.pub.hex)"
# Optional opaque key id (logged on startup for forensic key-rotation tracking).
export DCENT_MANIFEST_KEY_ID="dcent-release-2026-05"

# Cargo build embeds the hex via option_env!()
cargo build --release --target armv7-unknown-linux-musleabihf

# Production package (fail-closed if the pin is unset).
make release RELEASE_TARGET=s9
```

Every release/signing target (`s9`, `am2-s19jpro`, `am3-s19kpro`, `am3-s21`,
`am3-bb`, `am3-bb-s19jpro`) goes through this gate. That list is packaging
coverage, not a blanket production-readiness claim; BB, Amlogic, and other
non-Xilinx lanes still follow their documented lab/evidence gates. BB SD-card
tarballs and raw SD-image variants are signed with the release key as well. Use
`--lab-unsigned` only for controlled lab artifacts; it is not acceptable for a
release package.

## 5. Validate the pin actually shipped

`scripts/validate_production_readiness.ps1` runs `strings <binary> | grep
<hex>` and fails when the pin isn't visibly embedded:

```powershell
pwsh -File DCENT_OS_Antminer/scripts/validate_production_readiness.ps1 `
    -ManifestPubkeyHex $env:DCENT_MANIFEST_PUBLIC_KEY_HEX `
    -DcentraldBinaryPath dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentrald
```

A green line in the output:

```
PASS: manifest pubkey pin - DCENT_MANIFEST_PUBLIC_KEY_HEX is embedded in dcentrald
```

confirms the binary is OK to ship. A FAIL means the operator forgot to
export the env var when running `cargo build`; that binary is dev/lab-only
and must not be published as a release image. The release targets fail closed
before shipment when this pin is absent.

## 6. Key rotation

1. Generate a new keypair (`release_ed25519.priv.NEW.pem`).
2. Update `DCENT_MANIFEST_KEY_ID` with the new opaque id.
3. Sign the manifest with the new private key.
4. Rebuild dcentrald with the new pubkey hex pinned.
5. Ship a new firmware release. Old binaries continue to verify against
   the old pin — there is no in-binary key-list, the pin is single-key
   by design.
6. After enough fleet uptake, retire the old key from the HSM.

## 7. What happens without the pin

| State | Effect |
|---|---|
| `DCENT_MANIFEST_PUBLIC_KEY_HEX` set at `cargo build` time | `manifest_signature_required` returns true; `STOCK_MANIFEST_SIG_BAKED` is verified at runtime; tampered manifests are rejected. |
| `DCENT_MANIFEST_PUBLIC_KEY_HEX` unset at `cargo build` time | `manifest_signature_required` returns false; manifest is accepted as-baked (no signature gate). Acceptable for dev/lab; **NOT** acceptable for release. |
| `make release` without the env var exported | Build fails at the CI signing gate before anything ships. |
| `make dev` / `--lab-unsigned` / `DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1` | The CI signing gate is bypassed; produces a `lab_unsigned`-tagged package. |

## 8. Files in this directory

- `README.md` (this file) — the procedure above.
- `release_ed25519.pub.hex.example` — example pubkey-hex shape, **NOT** a real key.
- `release_ed25519.priv*.pem` — gitignored. Real private keys never live here in tree.
- `release_ed25519.pub.hex` — the operator's chosen committed pubkey (optional). Tracking it in-tree makes auditing the build pin trivial; keeping it out keeps the pin operator-private. Either is fine — the `.gitignore` only excludes private files.
