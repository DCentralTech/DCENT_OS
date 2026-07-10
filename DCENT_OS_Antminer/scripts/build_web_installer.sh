#!/bin/bash
#
# build_web_installer.sh — Build web-uploadable DCENTos packages for locked S9
# D-Central Technologies, 2026
#
# Creates tar.gz packages that can be uploaded through the Bitmain web firmware
# upgrade page to install DCENTos on locked S9 miners (no SSH, no SD card needed).
#
# PREREQUISITE: The miner must have the signature bypass applied first.
#   Option A: Upload VNish's signature.tar.gz (for pre-July 2019 firmware)
#   Option B: Use dcentos-signature-bypass.tar.gz (our own, built by this script)
#   Option C: Use SD card boot (works on any firmware)
#
# This script builds three packages:
#
#   1. dcentos-signature-bypass.tar.gz
#      Replaces the stock upgrade.cgi with one that has NO signature verification.
#      Upload first, get "Incorrect firmware!!!" error (normal), then upload #2 or #3.
#
#   2. dcentos-ssh-enabler.tar.gz
#      Enables SSH (Dropbear) on the miner. After upload, SSH in with root:admin
#      and use the supported install/sysupgrade workflow for full DCENTos installation.
#
#   3. dcentos-web-flash.tar.gz (FUTURE — requires built firmware)
#      Full DCENTos installation via web upload. Flashes boot chain + rootfs to NAND.
#
# Usage:
#   ./build_web_installer.sh                 # Build all packages
#   ./build_web_installer.sh --output-dir /path/to/output
#
# Installation flow:
#   1. Upload dcentos-signature-bypass.tar.gz → error (normal, CGI replaced)
#   2. Upload dcentos-ssh-enabler.tar.gz → SSH enabled on port 22
#   3. SSH root@<ip> (password: admin) → full access
#   4. Run the supported sysupgrade/install workflow to install DCENTos
#

set -e

# =============================================================================
# CE-126: QUARANTINE — legacy web-installer signature-bypass generator
# =============================================================================
# This script emits a signature-free upgrade.cgi (replacing Bitmain's
# RSA-verified handler), an SSH enabler, and a CGMiner API enabler. It is a
# legacy RESEARCH / unlock tool that bypasses the canonical firmware-generation
# unlock matrix (preflight/sig_bypass_matrix.py + locked_install/zynq_unlock_state.py)
# and the fail-closed release SSH posture (S50dropbear locked-release-image).
# It is fail-closed by default: it REFUSES to run unless the operator explicitly
# opts in with DCENT_ALLOW_LEGACY_WEB_INSTALLER=1.
if [ "${DCENT_ALLOW_LEGACY_WEB_INSTALLER:-0}" != "1" ]; then
    echo "ERROR: build_web_installer.sh is a quarantined unsafe research tool." >&2
    echo "  It emits a signature-free upgrade.cgi + SSH/API enablers that bypass" >&2
    echo "  the canonical unlock matrix and the fail-closed release SSH posture." >&2
    echo "  Refusing to run by default. If you understand the risk and are on a" >&2
    echo "  research bench, re-run with DCENT_ALLOW_LEGACY_WEB_INSTALLER=1." >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="${1:-$SCRIPT_DIR/../buildroot/output/images/web_installer}"

RED='\033[1;31m'
GREEN='\033[1;32m'
YELLOW='\033[1;33m'
CYAN='\033[1;36m'
BOLD='\033[1m'
NC='\033[0m'

info()   { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()   { echo -e "${YELLOW}[WARN]${NC} $*"; }
header() { echo -e "\n${CYAN}${BOLD}=== $* ===${NC}"; }

mkdir -p "$OUTPUT_DIR"
STAGING=$(mktemp -d)
trap "rm -rf $STAGING" EXIT

# =============================================================================
# Package 1: Signature Bypass (CGI Replacement)
# =============================================================================

header "Package 1: Signature Bypass"

info "Building dcentos-signature-bypass.tar.gz..."

mkdir -p "$STAGING/sig_bypass/cgi-bin"

# Create our own signature-free upgrade.cgi
# Based on VNish's approach but with DCENTos branding
cat > "$STAGING/sig_bypass/cgi-bin/upgrade.cgi" << 'UPGRADE_CGI'
#!/bin/sh -e
# DCENTos upgrade.cgi — signature-free firmware upgrade handler
# Replaces Bitmain's RSA-verified upgrade.cgi
# D-Central Technologies, 2026

file=/tmp/$$

trap atexit 0

atexit() {
	rm -rf $file
	umount $file.boot 2>/dev/null || true
	rmdir $file.boot 2>/dev/null || true
	sync
	if [ ! $ok ]; then
	    print "<h1>System upgrade failed</h1>"
	fi
}

CR=`printf '\r'`

exec 2>/tmp/upgrade_result

IFS="$CR"
read -r delim_line
IFS=""

while read -r line; do
    test x"$line" = x"" && break
    test x"$line" = x"$CR" && break
done

mkdir $file
cd $file
tar zxf -

if [ ! -f ubi_info ]; then
    echo "Incorrect firmware!!!" >> /tmp/upgrade_result
else
    if [ ! -d /mnt/config ];then
        mkdir /mnt/config
    fi

    ubiattach /dev/ubi_ctrl -m 2 2>/dev/null || true
    mount -t ubifs ubi1:rootfs /mnt/config 2>/dev/null || true

    if [ ! -d /mnt/config/home/usr_config ];then
        mkdir -p /mnt/config/home/usr_config
    fi
    cp -r /config/* /mnt/config/home/usr_config/ 2>/dev/null || true
    umount /mnt/config 2>/dev/null || true
    ubidetach -d 1 /dev/ubi_ctrl 2>/dev/null || true

    if [ -f runme.sh ]; then
        sh runme.sh
    else
        echo "Incorrect firmware!!!!" >> /tmp/upgrade_result
    fi
fi

ant_result=`cat /tmp/upgrade_result`

printf "Content-type: text/html\r\n\r\n"

cat <<-EOH
<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Strict//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="en" lang="en">
<head>
<meta http-equiv="Content-Type" content="text/html; charset=utf-8" />
<meta http-equiv="cache-control" content="no-cache" />
<link rel="stylesheet" type="text/css" media="screen" href="/css/cascade.css" />
<script type="text/javascript" src="/js/jquery-1.10.2.js"></script>
<script>
function f_submit_reboot() {
	setTimeout(function(){
		window.location.href="/index.html";
	}, 120000);
	jQuery.ajax({
		url: '/cgi-bin/reboot.cgi',
		type: 'GET',
		timeout: 30000,
		cache: false,
		data: {},
		success: function(data) {},
		error: function() {}
	});
}
function f_submit_goback() {
	window.location.href="/upgrade.html";
}
</script>
<title>DCENTos Installer</title>
</head>
EOH

if [ "${ant_result}" == "" ]; then
	echo "<body class=\"lang_en\" onload=\"f_submit_reboot();\">"
else
	echo "<body class=\"lang_en\">"
fi

cat <<-EOB
<div id="maincontainer">
<div id="maincontent">
EOB

if [ "${ant_result}" == "" ]; then
	echo "<h2>System Upgrade Succeeded</h2>"
	echo "<fieldset class=\"cbi-section\">"
	echo "<img src=\"/resources/icons/loading.gif\" alt=\"Loading\" style=\"vertical-align:middle\" />"
	echo "<span>Rebooting System ...<br />&nbsp;<br />(please wait for 120 seconds)</span>"
	echo "</fieldset>"
else
	echo "<h2>System Upgrade Status</h2>"
	echo "<fieldset class=\"cbi-section\">"
	echo "<p>"
	cat /tmp/upgrade_result
	echo "</p>"
	echo "<input class=\"cbi-button\" type=\"button\" onclick=\"f_submit_goback();\" value=\"Go Back\" />"
	echo "</fieldset>"
fi

cat <<EOT
</div></div>
</body></html>
EOT

ok=1
UPGRADE_CGI

# Create upgrade_clear.cgi (same but calls reset_conf.cgi instead of reboot.cgi)
cp "$STAGING/sig_bypass/cgi-bin/upgrade.cgi" "$STAGING/sig_bypass/cgi-bin/upgrade_clear.cgi"
sed -i "s|/cgi-bin/reboot.cgi|/cgi-bin/reset_conf.cgi|" "$STAGING/sig_bypass/cgi-bin/upgrade_clear.cgi"

chmod +x "$STAGING/sig_bypass/cgi-bin/upgrade.cgi"
chmod +x "$STAGING/sig_bypass/cgi-bin/upgrade_clear.cgi"

(cd "$STAGING/sig_bypass" && tar czf "$OUTPUT_DIR/dcentos-signature-bypass.tar.gz" cgi-bin/)

SIG_SIZE=$(stat -c%s "$OUTPUT_DIR/dcentos-signature-bypass.tar.gz" 2>/dev/null || stat -f%z "$OUTPUT_DIR/dcentos-signature-bypass.tar.gz")
info "Created: dcentos-signature-bypass.tar.gz ($SIG_SIZE bytes)"

# =============================================================================
# Package 2: SSH Enabler
# =============================================================================

header "Package 2: SSH Enabler"

info "Building dcentos-ssh-enabler.tar.gz..."

mkdir -p "$STAGING/ssh_enabler"
echo "dcentos-ssh-enabler" > "$STAGING/ssh_enabler/ubi_info"

cat > "$STAGING/ssh_enabler/runme.sh" << 'SSH_ENABLER'
#!/bin/sh
# DCENTos SSH Enabler — enables SSH on locked Bitmain S9
# D-Central Technologies, 2026
#
# After uploading this through the web interface:
#   SSH: root@<miner_ip> (password: admin)

{
    echo "DCENTos SSH Enabler v1.0"
    echo ""

    # Enable Dropbear SSH
    if [ -f /etc/default/dropbear ]; then
        sed -i 's/NO_START=1/NO_START=0/' /etc/default/dropbear
        echo "Dropbear auto-start enabled"
    fi

    # Generate host keys if missing
    mkdir -p /etc/dropbear
    if [ ! -f /etc/dropbear/dropbear_rsa_host_key ]; then
        dropbearkey -t rsa -f /etc/dropbear/dropbear_rsa_host_key 2>/dev/null
        echo "RSA host key generated"
    fi

    # Start SSH daemon
    if ! pidof dropbear >/dev/null 2>&1; then
        /usr/sbin/dropbear -p 22 2>/dev/null
        sleep 1
    fi

    if pidof dropbear >/dev/null 2>&1; then
        echo ""
        echo "SUCCESS: SSH is now enabled on port 22"
        echo "Connect with: ssh root@$(hostname -i 2>/dev/null || echo '<miner_ip>')"
        echo "Password: admin (default Bitmain)"
        echo ""
        echo "To install DCENTos:"
        echo "  1. SSH in: ssh root@<ip>"
        echo "  2. Run DCENTos installer script"
    else
        echo "WARNING: Could not start SSH daemon"
        echo "Dropbear binary may be missing or corrupted"
    fi
} > /tmp/upgrade_result 2>&1
SSH_ENABLER

chmod +x "$STAGING/ssh_enabler/runme.sh"

(cd "$STAGING/ssh_enabler" && tar czf "$OUTPUT_DIR/dcentos-ssh-enabler.tar.gz" ubi_info runme.sh)

SSH_SIZE=$(stat -c%s "$OUTPUT_DIR/dcentos-ssh-enabler.tar.gz" 2>/dev/null || stat -f%z "$OUTPUT_DIR/dcentos-ssh-enabler.tar.gz")
info "Created: dcentos-ssh-enabler.tar.gz ($SSH_SIZE bytes)"

# =============================================================================
# Package 3: CGMiner API Enabler (bonus)
# =============================================================================

header "Package 3: CGMiner API Enabler"

info "Building dcentos-api-enabler.tar.gz..."

mkdir -p "$STAGING/api_enabler"
echo "dcentos-api-enabler" > "$STAGING/api_enabler/ubi_info"

cat > "$STAGING/api_enabler/runme.sh" << 'API_ENABLER'
#!/bin/sh
# DCENTos API Enabler — enables CGMiner API on port 4028
# D-Central Technologies, 2026
{
    echo "DCENTos API Enabler v1.0"

    # Find and modify bmminer/cgminer config to enable API access
    for conf in /config/bmminer.conf /config/cgminer.conf; do
        if [ -f "$conf" ]; then
            # Add or modify api-listen to true
            if grep -q "api-listen" "$conf"; then
                sed -i 's/"api-listen".*:.*false/"api-listen" : true/' "$conf"
            fi
            # CE-126: scope api-allow to LAN ranges, never all-interfaces.
            # A world-writable CGMiner control API on all interfaces is a
            # remote-control foot-gun; restrict to RFC1918 LAN by default.
            if grep -q "api-allow" "$conf"; then
                sed -i 's/"api-allow".*:.*"[^"]*"/"api-allow" : "W:192.168.0.0\/16,W:203.0.113.0\/8,W:172.16.0.0\/12"/' "$conf"
            fi
            echo "Updated $conf"
        fi
    done

    echo "API will be available after bmminer restart"
    echo "Port 4028 should be accessible for pyasic/hass-miner"
} > /tmp/upgrade_result 2>&1
API_ENABLER

chmod +x "$STAGING/api_enabler/runme.sh"

(cd "$STAGING/api_enabler" && tar czf "$OUTPUT_DIR/dcentos-api-enabler.tar.gz" ubi_info runme.sh)

API_SIZE=$(stat -c%s "$OUTPUT_DIR/dcentos-api-enabler.tar.gz" 2>/dev/null || stat -f%z "$OUTPUT_DIR/dcentos-api-enabler.tar.gz")
info "Created: dcentos-api-enabler.tar.gz ($API_SIZE bytes)"

# =============================================================================
# Summary
# =============================================================================

header "Build Complete"

echo ""
echo -e "${BOLD}Output directory:${NC} $OUTPUT_DIR/"
echo ""
echo -e "${BOLD}Packages:${NC}"
for f in "$OUTPUT_DIR"/*.tar.gz; do
    FSIZE=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f")
    printf "  ${GREEN}%-40s${NC} %d bytes\n" "$(basename $f)" "$FSIZE"
done

echo ""
echo -e "${BOLD}Installation flow for locked S9:${NC}"
echo ""
echo "  Step 1: Apply signature bypass (if not already done)"
echo "    Upload dcentos-signature-bypass.tar.gz via web upgrade page"
echo "    Error message is NORMAL — the CGI has been replaced"
echo ""
echo "  Step 2: Enable SSH"
echo "    Upload dcentos-ssh-enabler.tar.gz via web upgrade page"
echo "    Wait for success message"
echo ""
echo "  Step 3: Connect and install"
echo "    ssh root@<miner_ip>  (password: admin)"
echo "    Run DCENTos installation script"
echo ""
echo -e "${YELLOW}NOTE:${NC} Step 1 only works on pre-July 2019 firmware."
echo "For July 2019+ firmware, use SD card boot instead."
echo ""
