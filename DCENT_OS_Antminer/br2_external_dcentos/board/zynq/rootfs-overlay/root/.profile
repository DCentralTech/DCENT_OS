# DCENTos root profile
[ -f ~/.bashrc ] && . ~/.bashrc

# Show system info on login
DCENTOS_VER=$(cat /etc/dcentos-version 2>/dev/null || echo "dev")
echo ""
echo "=== DCENTos v${DCENTOS_VER} ==="
uname -a
echo "Uptime: $(uptime -p 2>/dev/null || uptime)"
echo "Memory: $(free -h 2>/dev/null | grep Mem | awk '{print $3 "/" $2}' || echo 'N/A')"
echo ""

# Check hardware access (UIO-based, no kernel modules needed)
UIO_COUNT=$(ls -d /sys/class/uio/uio* 2>/dev/null | wc -l)
if [ "$UIO_COUNT" -gt 0 ]; then
    echo "[OK] FPGA access: $UIO_COUNT UIO devices"
else
    echo "[!!] No UIO devices found — FPGA bitstream may not be loaded"
fi

echo "Hardware status: use miner-status (daemon-owned snapshot)"
echo ""
