# DCENTos root profile
[ -f ~/.bashrc ] && . ~/.bashrc

# Add tools to PATH
export PATH="/root/tools:$PATH"

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

if command -v devmem > /dev/null 2>&1; then
    FPGA_VER=$(devmem 0x43C00000 32 2>/dev/null)
    if [ -n "$FPGA_VER" ]; then
        echo "[OK] FPGA version: $FPGA_VER"
    fi
else
    echo "[!!] devmem not available — install devmem2"
fi
echo ""
