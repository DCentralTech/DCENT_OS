#!/bin/sh
#
# safe_sysupgrade_cv_emmc.sh - CV1835 persistent-update containment.
#
# Maturity: NOT IMPLEMENTED.  This installed name is retained so old callers
# fail closed instead of falling through to a different platform's updater.
# It deliberately uses only POSIX shell builtins and performs no argument,
# filesystem, storage, environment, network, process-control, or reboot work.
# There is no override and no dry-run acceptance path.
#
# Exact held anchor:
#   FIP SHA256:
#     874efb83b18a5cfbf76f1a9b514438813ced1aa279a115678c3cd9c50a66fd2e
#   derived U-Boot/BL33 SHA256 (fiptool/analysis extraction from the FIP;
#   no independently persisted BL33 artifact):
#     be3bb20a30a52d49315442454aedc542394d1c5571f83de531d09685a052f466
#   version:
#     U-Boot 2017.07 (Feb 15 2023 - 16:04:02 +0800) cvitek_cv1835
#
# Binary inspection proves that this bootloader relocates its built-in default
# environment from raw image offset 0x4eae8.  It has no MMC environment read or
# save backend; its only saveenv reference belongs to USB-programmer command
# filtering.  Neither eMMC boot hardware partition is an environment store.
# Four other held CV1835 builds agree with this backend classification.
#
# The observed boot selector is the p2 marker at LBA 40960 (0xa000), length
# 2048 sectors: all-zero content selects minerfs and nonzero content selects
# upgrade-ramfs.  That evidence does not define a safe update transaction,
# recovery protocol, or marker-write API.  DCENT_OS therefore describes these
# fingerprints as BuiltInVolatile/mutation-denied and does not implement eMMC
# update or persistent boot-environment mutation for them.

printf '%s\n' 'CV1835 persistent update refused: NOT IMPLEMENTED for FIP sha256=874efb83b18a5cfbf76f1a9b514438813ced1aa279a115678c3cd9c50a66fd2e / derived U-Boot sha256=be3bb20a30a52d49315442454aedc542394d1c5571f83de531d09685a052f466 (U-Boot 2017.07, 2023-02-15 16:04:02 +0800, cvitek_cv1835).' >&2
printf '%s\n' 'Held bootloader fingerprint uses BuiltInVolatile/mutation-denied; no persistent MMC environment backend exists.' >&2
printf '%s\n' 'Observed selector is p2 at LBA 40960 (0xa000), 2048 sectors: zero=minerfs, nonzero=upgrade-ramfs; no safe marker update transaction is implemented.' >&2
exit 78
