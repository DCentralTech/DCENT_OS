# AMLCtrl Hardware Boundary — What DCENT_OS Does Not Do

| Field        | Value                                                              |
|--------------|--------------------------------------------------------------------|
| Document     | AMLCTRL_BOUNDARY.md                                                |
| Version      | 1.0                                                                |
| Status       | **PUBLIC**                                                         |
| Effective    | 2026-05-10                                                         |
| Authority    | D-Central Technologies Inc. (Laval, QC, Canada)                    |
| Owner        | Jonathan Bertrand, CEO — `jonathan@d-central.tech`                 |
| Applies to   | DCENT_OS firmware on Bitmain Antminer AMLCtrl-class control boards |

---

## 1. Purpose

This document is the canonical, public statement of the cryptographic
boundary between DCENT_OS (D-Central Technologies' open-source
Bitcoin-miner firmware) and Bitmain's proprietary, encrypted boot
chain on AMLCtrl-class Antminer control boards.

It exists to give a single, unambiguous answer to the questions:

1. **Does DCENT_OS decrypt Bitmain firmware on AMLCtrl hardware?** No.
2. **Does DCENT_OS extract, replicate, or distribute Bitmain's signing
   keys?** No.
3. **What does DCENT_OS actually do on an AMLCtrl-class miner?** It
   either runs alongside the unmodified stock firmware, or — for
   salvage/repair scenarios — it runs on a physically replaced
   control board that the operator already owns.

If you are a regulator, legal counsel, security researcher, journalist,
or operator evaluating DCENT_OS for compliance reasons, this document
is the authoritative reference. It is intentionally durable: it
remains accurate independent of future Bitmain key rotations or
hypothetical key disclosures.

---

## 2. What is "AMLCtrl"?

"AMLCtrl" is D-Central's internal designator for Bitmain's
Amlogic-based Antminer control-board variant, built around the
**Amlogic S905-class SoC** with a hardware-backed AES-256-CBC
encrypted boot chain.

AMLCtrl is the control board shipped on, at minimum, the following
Antminer models:

- Antminer **S19j Pro** (Amlogic variant — distinct from the older
  Zynq-based S19j Pro)
- Antminer **S19 Pro+ Hyd**
- Antminer **S19 XP**
- Antminer **S21**
- Antminer **S21 Pro**
- Antminer **S21 XP** (where shipped with AMLCtrl carrier)

To distinguish AMLCtrl from the other Bitmain control-board families
DCENT_OS supports:

| Family    | SoC                       | Boot chain                            | DCENT_OS install path                      |
|-----------|---------------------------|---------------------------------------|--------------------------------------------|
| **AMLCtrl** | Amlogic S905-class      | RSA-wrapped AES-256-CBC, all stages   | **Stock-untouched OR control-board-replacement (am3-bb)** |
| BBCtrl    | TI AM335x (BeagleBone-class) | Unencrypted U-Boot + Linux         | Live in-place rootfs replacement (rootfs-window) |
| CVCtrl    | Cvitek CV1835             | Unencrypted U-Boot + Linux            | Live in-place rootfs replacement (rootfs-window) |
| Zynq      | Xilinx Zynq-7007/7010     | Unencrypted U-Boot + Linux            | Live in-place rootfs replacement (rootfs-window) |

The cryptographic boundary in this document is specific to **AMLCtrl**.
DCENT_OS's behaviour on BBCtrl, CVCtrl, and Zynq boards is described
in the project's per-platform installation documentation; those paths
involve no decryption of Bitmain firmware either, but the mechanics
differ because their boot chains are not encrypted to begin with.

---

## §2.1 — VNish-installed AMLCtrl units

The cryptographic boundary established in §3-§5 applies to the **stock
Bitmain AMLCtrl boot chain** as shipped by Bitmain in the unmodified
S19j Pro Amlogic / S21 / S19k Pro firmware images.

AML units already running 3rd-party VNish firmware have **already
replaced the Bitmain boot chain** themselves: the Bitmain-signed
BL2 → FIP → U-Boot → kernel chain was substituted by VNish's own boot
package (unencrypted, but RSA-signed).

DCENT_OS does NOT perform this replacement and does NOT bundle any
Bitmain key material. Inheriting an already-VNish-modified unit (e.g.
an operator buys a used miner already running VNish, then installs
DCENT_OS over it) is operator-driven and outside the cryptographic
boundary §3 protects — DCENT_OS does not weaken or strengthen the AML
encryption envelope; the operator's prior firmware choice already did.

**This carve-out does NOT permit DCENT_OS to ship any tooling that
performs the stock-AMLCtrl boot-chain bypass.** DCENT_OS install on
stock-AMLCtrl AML hardware remains control-board replacement to an
`am3-bb`-class carrier (§6). VNish-AML units have their own
install-cost path, but DCENT_OS provides no automated tooling to
identify or flash VNish-AML units differently from stock-AMLCtrl until
a live VNish AML bench unit and repeated successful round-trips are
proven.

Two supporting hardware findings inform the VNish-AML carve-out:

- **PWR_EN polarity.** VNish enables the PSU by driving `gpio437`
  electrically HIGH, with no `active_low` override. Both stock-Bitmain
  bmminer and VNish cgminer use the same active-HIGH semantics on this
  GPIO.
- **BraiinsOS does not support stock-Bitmain AML.** The community
  alternative operators sometimes propose ("flash BraiinsOS over the
  stock Bitmain image") is upstream-unsupported on AMLCtrl hardware for
  the same reason DCENT_OS cannot do it: BraiinsOS itself has no
  tooling to bypass the encrypted Bitmain boot chain. Control-board
  replacement to an `am3-bb`-class carrier stays the canonical install
  path for any AMLCtrl unit not already running VNish.

DCENT_OS includes two inert HAL-side support modules for AML carriers:

- A userspace firmware-state detector that probes filesystem markers
  to classify a carrier as stock-Bitmain, VNish, or unknown. A future
  rootfs-window install route would gate on this result before
  permitting any write on an AML carrier.
- A data-only description of the cold-boot phase sequence (GPIO map,
  phase trace, PWM and I²C topology). It is gated off and wires into no
  orchestrator; every constant is marked inferred until a live VNish
  AML bench unit verifies it end-to-end.

Neither module changes the cryptographic boundary in §3-§5. The
boundary stays the same on every AML carrier regardless of firmware
state: DCENT_OS performs zero decryption of Bitmain firmware on
AMLCtrl hardware. The only thing that changes when an AML carrier is
already running VNish is whether the rootfs-window install path (the
carve-out above) becomes reachable as a salvage alternative to the
control-board-replacement default.

---

## 3. Cryptographic Boundary

DCENT_OS performs **zero decryption** of Bitmain firmware on AMLCtrl
hardware. Specifically, on every supported AMLCtrl model and on every
DCENT_OS release:

### What DCENT_OS does NOT do on AMLCtrl

- Does **not** extract, recover, glitch, side-channel, fault-inject,
  or otherwise attempt to obtain the AES-256 content-encryption key
  used by Bitmain to encrypt the kernel, ramdisk, and DTB stages of
  the AMLCtrl boot image.
- Does **not** extract, recover, or attempt to obtain the **RSA-2048
  private key** that wraps the AES content-encryption key.
- Does **not** decrypt `boot.img`, kernel, ramdisk, DTB, or any other
  Bitmain-signed/encrypted boot artifact.
- Does **not** modify, re-sign, strip, replace, or bypass Bitmain's
  signature chain on any boot artifact stored in eMMC, NAND, or SPI
  flash.
- Does **not** redistribute Bitmain firmware images, decrypted
  payloads, RSA private keys, AES keys, or signing material.
- Does **not** ship any tool, utility, script, or documentation that
  performs or assists any of the above.

### What DCENT_OS does on AMLCtrl

- For **working units with intact AMLCtrl firmware**: DCENT_OS does
  **nothing** to the on-board firmware. The unit continues to run
  Bitmain's unmodified stock firmware on the original control board.
  D-Central's tooling (dcent-toolbox, fleet management, dashboards)
  may interact with the unit over the network using documented
  protocols (SSH, CGMiner API, web UI), exactly as any other
  third-party fleet-management tool would.
- For **salvage/repair units with a broken or non-recoverable AMLCtrl
  control board**: the operator may physically replace the AMLCtrl
  control board with a `am3-bb`-class BeagleBone Black carrier
  running DCENT_OS. The replacement uses hardware the operator has
  already legally purchased; it does not depend on, modify, or even
  read the original AMLCtrl board's encrypted contents.

There is no third path. DCENT_OS does not run on the original AMLCtrl
control board and does not attempt to.

---

## 4. Why DCENT_OS Does Not Decrypt AMLCtrl Firmware

This is a deliberate engineering and policy choice, grounded in the
following technical findings:

1. **The AES-256 content key is RSA-2048-wrapped per the Amlogic S905
   secure-boot specification.** Recovery requires the corresponding
   RSA private key.
2. **Bitmain holds the RSA-2048 private key.** D-Central does not
   have access to it, has not requested access to it, and has no
   intention of obtaining it.
3. **The S905 OTP fuse region is not publicly extractable.** As of
   this document's effective date, no public glitch attack, fault
   injection, or side-channel result against the Amlogic S905 secure
   boot ROM has been disclosed that would yield the per-die key
   material.
4. **The same RSA-wrapped AES key blob is reused across the entire
   Bitmain Amlogic fleet** (S19 Pro+, S19 XP, S21 series, AML S19j
   Pro). This is a property of Bitmain's manufacturing decisions, not
   a vulnerability DCENT_OS exploits.
5. **Even if the RSA private key were ever publicly disclosed**
   (for example, through an unrelated supply-chain disclosure outside
   D-Central's control), DCENT_OS-installed units would still not be
   decrypting anything: they either run on stock-untouched AMLCtrl
   hardware or on a separate, replaced control board. This document
   would remain accurate.

DCENT_OS's open-source posture is strengthened, not weakened, by
declining to operate inside the AMLCtrl encrypted boot envelope. Our
firmware is auditable end-to-end precisely because it does not
contain, depend on, or interact with proprietary decryption material.

---

## 5. Why "Rootfs-Window" Does Not Apply to AMLCtrl

> **CONFIRMED PERMANENT.** An earlier engineering review confirmed
> that BraiinsOS itself does NOT support stock-Bitmain AMLCtrl —
> the upstream alternative-firmware ecosystem also has no tooling
> to bypass the encrypted Bitmain boot chain. Control-board
> replacement to an `am3-bb`-class carrier remains the canonical
> install path for any AMLCtrl unit not already running VNish. The
> rootfs-window path's inapplicability described below is durable
> across community firmware ecosystems, not just DCENT_OS-specific.

DCENT_OS supports a **live in-place rootfs replacement** install path
on CVCtrl, BBCtrl, and Zynq control boards. This works by writing a
DCENT_OS rootfs into a known-safe region of flash *while the existing
Bitmain Linux kernel is running*, then chaining into the new rootfs at
the next reboot. The boot chain is preserved; only the userspace
rootfs changes.

This path **does not exist for AMLCtrl**, and cannot be added, because:

- AMLCtrl encrypts **every** boot stage: SPL, U-Boot, kernel,
  ramdisk, DTB, and rootfs are all wrapped by Bitmain's signature
  and/or encryption chain.
- There is no unencrypted "window" between Bitmain's secure-boot ROM
  decrypting the boot.img and Linux taking over.
- Any DCENT_OS rootfs written into AMLCtrl flash would not be loaded
  by the Bitmain boot chain, because it would not carry a valid
  Bitmain signature — and DCENT_OS, by policy and design, does not
  produce Bitmain signatures.

This is not a "DCENT_OS limitation" — it is a property of the AMLCtrl
hardware platform. It is the reason the **control-board-replacement**
path exists for salvage scenarios.

---

## 6. Control-Board-Replacement Path (Salvage Tier Only)

For AMLCtrl-class units whose original control board is non-functional
(failed eMMC, dead SoC, corrupted boot chain, water damage, etc.) and
whose hashboards are still good, DCENT_OS supports a physical
control-board-replacement procedure at a high level:

- **Hardware**: BeagleBone Black (TI AM335x) plus a custom adapter
  PCB that bridges the S19j Pro / S21-class hashboard headers to the
  BeagleBone expansion headers, plus appropriate ribbon cables and
  power harness.
- **Firmware**: DCENT_OS `am3-bb-s19jpro` (or equivalent for other
  AML hashboard families) flashed to the BeagleBone eMMC.
- **Procedure**: physically remove the original AMLCtrl control
  board from the miner chassis; install the BeagleBone + adapter PCB
  in its place; reconnect the existing hashboards and PSU; boot
  DCENT_OS.
- **Indicative cost**: $120–$140 USD bill-of-materials per unit,
  approximately 45 minutes of skilled labour.
- **Recovered value per unit**: typically $700–$900 USD vs. roughly
  $1,200–$1,500 retail for a working AMLCtrl-equipped unit.

This path is **explicitly salvage-tier only**. It is not a general
fleet-migration recommendation, it is not a default install path, and
DCENT_OS does not advertise or encourage operators to convert working
AMLCtrl units. Working units remain on stock Bitmain firmware.

The control-board-replacement path uses hardware the operator has
already purchased and owns. It does not read, decrypt, copy, or
otherwise interact with the encrypted contents of the original
AMLCtrl board's flash. The original board is physically removed and
either retained by the operator, returned for warranty (where
applicable), or discarded according to local e-waste regulations.

---

## 7. Legal & Intellectual-Property Posture

D-Central Technologies' position on this boundary is straightforward:

- **DCENT_OS is open source.** Firmware is GPL-3.0; tools and helper scripts are
  covered by the repository's GPL-3.0 license unless a file states otherwise;
  hardware designs are CERN-OHL-S-2.0.
  Source code, build scripts, and design files are publicly
  available.
- **D-Central does not redistribute Bitmain firmware.** No Bitmain
  binaries, decrypted payloads, or signing material are present in
  the DCENT_OS source tree, release artifacts, or installer
  packages.
- **D-Central does not bypass Bitmain's signature chain.** On
  AMLCtrl, DCENT_OS does not run inside the Bitmain boot envelope
  at all. On other platforms (BBCtrl, CVCtrl, Zynq) the boot chains
  are not signed in a manner that DCENT_OS bypasses.
- **The control-board-replacement path uses hardware the operator
  owns.** Bitmain has already sold the hashboards, the PSU, the
  chassis, and (when applicable) the original control board to the
  operator. Replacing one component (the control board) with a
  different, third-party-supplied component is a long-established
  practice for industrial equipment ownership and right-to-repair.
- **This document establishes the cryptographic limits of D-Central's
  work on AMLCtrl hardware.** It is intended as a durable public
  reference for compliance, due-diligence, and right-to-repair
  conversations.

D-Central welcomes good-faith inquiries from regulators, counsel, and
security researchers about this boundary. Contact information is in
§10.

---

## 8. Future Evolution

This document is intentionally written to remain accurate through
foreseeable changes:

- **If Bitmain rotates the RSA-2048 private key** that wraps the
  AMLCtrl AES content keys, DCENT_OS is unaffected on every supported
  platform. We do not depend on Bitmain's signing chain anywhere.
- **If the Bitmain RSA-2048 private key is ever publicly disclosed**
  (through any channel outside D-Central's control), this document
  remains accurate: DCENT_OS-installed units do not perform any
  decryption, and DCENT_OS will not begin doing so in response to
  such a disclosure. Any policy change would be announced explicitly
  in a revised version of this document.
- **If new AMLCtrl-class hardware ships from Bitmain**, DCENT_OS's
  default behaviour remains the same: stock-untouched on working
  units, control-board-replacement (`am3-bb`) for salvage, no
  decryption.
- **If a public, reproducible attack against the Amlogic S905 secure
  boot ROM is published**, DCENT_OS will not adopt it. The boundary
  in §3 is a policy boundary as well as a technical one.

Substantive changes to this document will be reflected by bumping the
version field in the header and by an entry in the project changelog.

---

## 9. References

- Amlogic S905 datasheet and secure-boot specification (publicly
  available from Amlogic / SoC vendor documentation).
- Bitmain Antminer model documentation (publicly available from
  Bitmain product pages).

---

## 10. Contact

For compliance, legal, security-research, or right-to-repair inquiries
related to this boundary:

- **Company**: D-Central Technologies Inc.
- **Headquarters**: Laval, Québec, Canada
- **Web**: https://d-central.tech/
- **CEO**: Jonathan Bertrand — `jonathan@d-central.tech`
- **Security / compliance**: `security@d-central.tech`
- **Legal**: `legal@d-central.tech`

---

*This document is published under the same open licensing posture as
the rest of the DCENT_OS project. It may be reproduced, cited, and
linked freely. Substantive paraphrases should reference the version
number in the header above.*
