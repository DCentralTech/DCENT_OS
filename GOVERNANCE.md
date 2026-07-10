# DCENT_OS Governance

DCENT_OS is free and open-source software, released under the
[GNU General Public License v3.0](LICENSE). This document explains, plainly, who steers the
project and how decisions get made — so there are no surprises.

## Who runs the project

**DCENT_OS's direction is determined solely by D-Central Technologies Inc.**

We design the architecture, set the roadmap, decide what ships, and decide which contributions are
merged upstream. **We reserve all rights to accept or decline any contribution, for any reason,
without obligation to explain.** Maintainership and the canonical repository at
[`github.com/DCentralTech/DCENT_OS`](https://github.com/DCentralTech/DCENT_OS) are D-Central's.

This is a **benevolent-maintainer** model, not a committee. We move fast, we hold a strong opinion
about what DCENT_OS should be (open, fee-free, home-first, safe), and we are not running a vote.

## Why it works this way — and why that's fine

The GPL already guarantees you everything that matters:

- The **freedom to run** DCENT_OS for any purpose.
- The **freedom to study** how it works — every line of source is here.
- The **freedom to modify** it to do what *you* want.
- The **freedom to redistribute** your modified version under the same license.

In other words: **if you don't like a decision we make, you are completely free to fork DCENT_OS
and take it in your own direction.** That is not a threat — it is the entire point of open source,
and it is a feature we actively endorse. 🙂 We'd rather you fork than be unhappy.

What D-Central controls is the *upstream* project that carries the DCENT_OS name and D-Central's
reputation. What you control is your own copy, forever.

## Why D-Central, specifically

DCENT_OS runs on real hardware, in real homes, controlling real voltage and real heat. A careless
change can **brick a miner or start a fire.** D-Central has done thousands of repairs and built this
firmware against a fleet of physical units with reverse-engineered, hardware-specific knowledge
that mostly doesn't exist in public. A single maintainer with deep hardware context and a hard
safety bias is the responsible way to steward firmware like this.

## What we optimize for (the project's values)

In rough priority order:

1. **Safety** — never brick a unit, never start a fire, cut hash before noise.
2. **Honesty** — the dashboard and APIs never claim something that isn't true.
3. **Openness** — GPL-3.0, no license server, no phone-home, no hidden fee route.
4. **Home-miner experience** — quiet, efficient, space-heater-first.
5. **Decentralization** — no vendor lock-in at any layer of the stack.

Contributions that advance these values, with evidence, are the most likely to merge.

## How contributions are handled

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the practical workflow. In short: issues and pull
requests are welcome, hardware-touching changes get a review pass against our safety rules, and
we'll tell you if something is a fit. If we decline a change, that's not a judgment of you — it's
a judgment of fit for *this* codebase. Fork freely.

## Forking

You are explicitly welcome to fork DCENT_OS under the GPL-3.0. We ask only two courtesies (these
are requests, not license terms): give D-Central credit for the original work, and **rename your
fork** so users don't confuse it with the official DCENT_OS that D-Central supports. The DCENT_OS,
DCENT, and D-Central names and logos are D-Central's trademarks; the *code* is GPL, the *brand* is
not.

---

*Questions about governance or partnership: **jonathan@d-central.tech** (Jonathan Bertrand, CEO).*
