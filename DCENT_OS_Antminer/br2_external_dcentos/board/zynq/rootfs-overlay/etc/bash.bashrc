# DCENTos Hacker Shell - bash configuration

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
alias tools='ls /root/tools/*.py'
alias dmesg='dmesg --color=always'

# Quick hardware access aliases
alias fpga='devmem 0x43C00000 32'
alias i2c-scan='for b in 0 1 2 3 4 5 6 7; do echo "=== Bus $b ==="; i2cdetect -y $b 2>/dev/null; done'
alias uart-cfg='stty -F /dev/ttyPS1 115200 cs8 -cstopb -parenb raw'
alias mtd-info='cat /proc/mtd'
alias mod-info='lsmod'

# Tool shortcuts
alias scan-regs='python3 /root/tools/register_scanner.py'
alias enum-chain='python3 /root/tools/asic_enumerator.py'
alias probe-psu='python3 /root/tools/psu_probe.py'
alias find-temp='python3 /root/tools/temp_finder.py'
alias verify='python3 /root/tools/assumption_verifier.py'
alias test-all='/root/tools/test_suite.sh'

# History
HISTSIZE=1000
HISTFILESIZE=2000
HISTCONTROL=ignoredups:ignorespace

# Help function
help-tools() {
    echo ""
    echo "=== DCENTos Hacker Shell Tools ==="
    echo ""
    echo "Hardware Probing:"
    echo "  scan-regs      - Scan all ASIC registers (BM1387)"
    echo "  enum-chain     - Enumerate ASIC chain (discover chips)"
    echo "  probe-psu      - Probe PSU via I2C/PMBus"
    echo "  find-temp      - Discover temperature registers"
    echo "  fpga           - Read FPGA base register"
    echo "  i2c-scan       - Scan all I2C buses"
    echo ""
    echo "Testing:"
    echo "  verify         - Run assumption verifier"
    echo "  test-all       - Run full test suite"
    echo ""
    echo "System:"
    echo "  uart-cfg       - Configure UART for hash board"
    echo "  mtd-info       - Show NAND partition table"
    echo "  mod-info       - Show loaded kernel modules"
    echo ""
    echo "Interactive:"
    echo "  dcent-shell    - Launch interactive research toolkit"
    echo ""
}
