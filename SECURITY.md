# Security Policy

DCENT_OS runs on hardware that controls real voltage, heat, and money. We take security seriously
and appreciate responsible disclosure.

## Reporting a vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Email **security@d-central.tech** with:

- A description of the issue and its impact.
- Steps to reproduce (and a proof-of-concept if you have one).
- The affected version / commit and platform (e.g. S9 Zynq, S21 Amlogic).
- How you'd like to be credited (or if you prefer to stay anonymous).

If you need to encrypt, request our PGP key in your first email and we'll provide it.

## What we especially want to hear about

- Authentication or authorization bypass (dashboard, REST/WebSocket API, MCP control surface).
- Credential or private-key leakage (logs, API responses, support bundles, OTA artifacts).
- OTA / sysupgrade signature-verification flaws or downgrade attacks.
- Unsafe recovery, install, or flash paths that could brick a unit or skip a safety gate.
- Anything that lets a network attacker change voltage, disable thermal safety, or drive fans/PSU.
- Wallet/worker-address or other operator-privacy leaks on any surface.

## What to expect

- **Acknowledgement:** within 3 business days.
- **Triage & assessment:** we'll confirm the issue and assess severity, and keep you updated.
- **Fix & disclosure:** we aim to ship a fix promptly and will coordinate a disclosure timeline
  with you. We're happy to credit you in the release notes.

## Scope

In scope: the DCENT_OS firmware in this repository (`dcentrald`, the Buildroot tree, the dashboard,
the build/flash tooling) and its update path.

Out of scope: issues that require already-root local access by design (DCENT_OS gives the operator
full control of their own device — that's a feature), third-party pools, and the underlying
BraiinsOS-derived boot components on platforms where DCENT_OS reuses them.

## Safe-harbor

We will not pursue or support legal action against researchers who act in good faith, follow this
policy, avoid privacy violations and service disruption, and give us reasonable time to remediate
before public disclosure.

---

*Security contact: **security@d-central.tech** · D-Central Technologies Inc., Laval, Québec.*
