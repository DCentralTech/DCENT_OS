#!/bin/sh
# Compatibility entry point retained to fail closed while CV1835 is evidence-only.

printf '%s\n' 'ERROR: CV1835 artifact build is NOT IMPLEMENTED.' >&2
printf '%s\n' 'The target has no firmware, sysupgrade, or supported analysis-artifact lane.' >&2
printf '%s\n' 'No flag or environment override can authorize this entry point.' >&2
exit 78
