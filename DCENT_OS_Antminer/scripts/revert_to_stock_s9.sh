#!/bin/sh
#
# S9 stock restore containment boundary.
#
# The former implementation selected a target from unsupported boot-state
# assumptions. It is intentionally replaced, not merely hidden behind a branch:
# every invocation fails before inspecting input or invoking any external tool.
# A replacement restore engine must first prove the complete S9 image layout,
# selector transaction, write verification, and recovery contract.

set -eu

printf '%s\n' \
    'ERROR: S9 stock restore is disabled: the previous target-selection contract was invalidated by local hardware evidence.' \
    'No storage, boot state, network, or system lifecycle action was attempted.' >&2
exit 1
