#!/bin/sh
# Historical target-side stock-revert alias; unconditional CV1835 refusal.

printf '%s\n' 'CV1835 uninstall refused: persistent stock restore is NOT IMPLEMENTED for the admitted bootloader fingerprints.' >&2
printf '%s\n' 'The environment is BuiltInVolatile/mutation-denied and no safe p2 marker-write transaction exists.' >&2
exit 78
