# DCENT_OS — Frequently Asked Questions

## Is DCENT_OS really free, with no developer fee?

Yes. DCENT_OS has **no mandatory fee and no license server**. There is an *optional* donation
(default ~2%, adjustable from 0% to 5%, and fully disableable) that is always visible on the
dashboard when active. D-Central makes its living repairing and selling mining hardware — not by
skimming your hashrate. Zero donation is a completely valid setting.

## How is DCENT_OS different from Braiins OS+, LuxOS, and VNish?

DCENT_OS is **fully open source (GPL-3.0)**, has **no mandatory fee**, avoids smart-PSU lock-in on
supported lanes, **auto-detects supported hash boards** via ChipID, and is designed **home-first as
a space heater**. See [`COMPARISON.md`](COMPARISON.md) for the full table.

## Which Antminer models actually work?

Antminer **S9, S19j Pro (Zynq and BeagleBone), and S21** have bench mining proof on real hardware.
Only S9 and S19j Pro Zynq are in the current Public Beta install lane. **S19 Pro** is an
Experimental feature with cold-boot and nonce evidence; its accepted-share and persistent-install
promotion gates remain open. S17, S19, the Amlogic S19j Pro, and S19k Pro are in active bring-up.
See [`PLATFORMS.md`](PLATFORMS.md).

## Why is it written in Rust?

Your miner is critical infrastructure sitting in your home, controlling real voltage and heat. A
memory-safety bug in firmware can brick hardware or worse. Rust eliminates that entire class of bug
by construction, which is why `dcentrald` is written in it.

## What is the "universal hash board" feature?

The Zynq-era Antminers share an 18-pin connector and UART protocol. DCENT_OS detects the chip type
automatically and loads the right driver. In validated/lab configurations this lets an inexpensive
S9 control board drive supported later-generation hash boards. Treat mixed-generation rigs as
per-route lab work until their exact control board, hash board, power, and recovery path is listed
in [`PLATFORMS.md`](PLATFORMS.md).

## Can I mine to any pool?

Yes. DCENT_OS has **zero lock-in** — point it at any Stratum V1/V2 pool. The default and recommended
pool is **DCENT_Pool**, D-Central's [Solo/Guild pool](#what-is-dcent_pool).

## What is DCENT_Pool?

DCENT_Pool is D-Central's own **Solo/Guild pool** — a trustless, MMORPG-style take on solo mining.
Mine **solo** and keep a whole block reward when you find one, or join a **guild** to share the
block reward with other miners trustlessly, with no custodian holding your coins.

## Will DCENT_OS make my miner quieter?

Yes — that's a core design goal. Fans boot at a low PWM and are PID-controlled, and the safety
policy **cuts hash power before raising fan noise**. For home and night use it's much quieter than
stock firmware, which pins fans at 100% from boot.

## Is it faster than Braiins / VNish?

DCENT_OS prioritizes **efficiency and quiet home operation over maximum overclock**. On a typical
home unit you'll see solid efficiency gains over stock firmware; if your only goal is the highest
possible hashrate on a heavily-cooled warehouse machine, a performance-tuned closed firmware may
push further. DCENT_OS is built for the home, where cutting noise and running efficiently matters
more than the last few percent of hashrate.

## Can I run an old miner as a space heater?

That's the whole point. DCENT_OS shows **BTU/h** in every dashboard mode, has a Space Heater mode
with thermostat-style control, and can drive hashrate from *room* temperature when paired with an
external sensor (e.g. the [DCENT Expansion Pack](https://github.com/DCentralTech/DCENT_ExpansionPack)).

## Does it phone home or need a cloud account?

No. The dashboard is served locally off the miner, with no cloud account, no telemetry phone-home,
and no remote-management backdoor. All UI assets ship with the firmware (no Google Fonts, no CDN).

## How do I install it?

See [`INSTALL/`](INSTALL/) for per-platform guides. In short, you build a flashable image and install
it (the DCENT Toolbox auto-detects the stock firmware). A fresh install boots **management-only**
(no hash power) until you explicitly enable mining.

## Can I contribute? Who decides what gets merged?

Yes — it's GPL-3.0, fork and build freely. D-Central sets the project's direction and decides what
merges upstream (see [`GOVERNANCE.md`](../GOVERNANCE.md)). If our direction isn't yours, the GPL
guarantees your right to fork — and we genuinely encourage it.

## Is there a developer/community to talk to?

D-Central is at [d-central.tech](https://d-central.tech/). Bug reports and bring-up findings go
through GitHub Issues; security issues go to **security@d-central.tech** (see
[`SECURITY.md`](../SECURITY.md)).
