#!/bin/sh
#
# revert_to_stock_cv1835.sh - CV1835 stock-revert containment.
#
# Maturity: NOT IMPLEMENTED. This historical entry point is retained only so
# automation and operator runbooks fail closed. It deliberately uses only
# POSIX shell builtins and performs no argument, filesystem, storage,
# environment, network, process-control, or reboot work. There is no dry-run,
# authorization-variable, proof-count, or interactive bypass.
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
# This bootloader relocates a built-in default environment from raw image
# offset 0x4eae8 and has no persistent MMC environment read/save backend.
# Therefore the former bootcount and factory_kernel recovery proposal cannot
# work and must not be used.
#
# The observed selector is p2 at LBA 40960 (0xa000), length 2048 sectors:
# all-zero content selects minerfs and nonzero content selects upgrade-ramfs.
# Selector observation is not a proven stock-restore or marker-write protocol.
# Policy for these fingerprints is BuiltInVolatile/mutation-denied.

printf '%s\n' 'CV1835 stock revert refused: NOT IMPLEMENTED for FIP sha256=874efb83b18a5cfbf76f1a9b514438813ced1aa279a115678c3cd9c50a66fd2e / derived U-Boot sha256=be3bb20a30a52d49315442454aedc542394d1c5571f83de531d09685a052f466.' >&2
printf '%s\n' 'Held bootloader fingerprint uses BuiltInVolatile/mutation-denied; no persistent MMC environment or bootcount recovery backend exists.' >&2
printf '%s\n' 'Observed selector is p2 at LBA 40960 (0xa000), 2048 sectors: zero=minerfs, nonzero=upgrade-ramfs; no safe stock-restore or marker-write transaction is implemented.' >&2
exit 78
