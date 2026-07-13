# DCENT_OS operator shell configuration

# Colored prompt (red for root)
if [ "$(id -u)" -eq 0 ]; then
    PS1='\[\033[1;31m\]dcentos\[\033[0m\]:\[\033[1;34m\]\w\[\033[0m\]# '
else
    PS1='\[\033[1;32m\]\u@dcentos\[\033[0m\]:\[\033[1;34m\]\w\[\033[0m\]$ '
fi

# Aliases
alias ll='ls -la'
alias la='ls -A'
alias l='ls -CF'
alias dmesg='dmesg --color=always'

# Read-only daemon snapshot aliases. Standalone hardware access is excluded
# from runtime images so dcentrald remains the sole I2C/UART/UIO owner.
alias miner-status='curl -fsS http://127.0.0.1:8080/api/status'
alias device-info='curl -fsS http://127.0.0.1:8080/api/system/info'
alias pic-info='curl -fsS http://127.0.0.1:8080/api/hardware/pic_info'
alias psu-info='curl -fsS http://127.0.0.1:8080/api/diagnostics/troubleshoot/psu'
alias mtd-info='cat /proc/mtd'
alias mod-info='lsmod'

# History
HISTSIZE=1000
HISTFILESIZE=2000
HISTCONTROL=ignoredups:ignorespace

# Help function
help-tools() {
    echo ""
    echo "=== DCENT_OS Operator Tools ==="
    echo ""
    echo "Daemon snapshots:"
    echo "  miner-status   - Live miner and per-chain telemetry"
    echo "  device-info    - Platform and ASIC identity"
    echo "  pic-info       - PIC catalog and daemon-owned observations"
    echo "  psu-info       - PSU/power snapshot without a live bus probe"
    echo ""
    echo "System:"
    echo "  mtd-info       - Show NAND partition table"
    echo "  mod-info       - Show loaded kernel modules"
    echo ""
    echo "Raw research tools require a future exclusive-owner repair image."
    echo "They are intentionally absent from the normal runtime rootfs."
}
