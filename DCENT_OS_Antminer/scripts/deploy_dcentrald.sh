#!/bin/bash
# DCENTos — Legacy quick deploy wrapper
#
# Kept for compatibility. The platform-aware logic now lives in dev_deploy.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "deploy_dcentrald.sh is deprecated. Forwarding to dev_deploy.sh..."
exec "$SCRIPT_DIR/dev_deploy.sh" "$@"
