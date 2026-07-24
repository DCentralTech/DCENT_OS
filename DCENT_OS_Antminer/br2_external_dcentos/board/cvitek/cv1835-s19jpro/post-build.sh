#!/bin/sh
# Buildroot containment hook for the evidence-only CV1835 board scaffold.
# DCENT_BUILD_POLICY=not-implemented-refusal

printf '%s\n' 'ERROR: CV1835 Buildroot post-build is NOT IMPLEMENTED.' >&2
printf '%s\n' 'No CV1835 rootfs or product artifact is admitted.' >&2
exit 78
