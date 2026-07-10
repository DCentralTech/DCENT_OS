"""Zig CC wrapper that strips --target flag added by cc-rs and uses aarch64-linux-musl."""
import pathlib
import subprocess
import sys

ZIG = pathlib.Path(r"C:\bt\zig-windows-x86_64-0.13.0-clean\zig-windows-x86_64-0.13.0\zig.exe")

args = []
for arg in sys.argv[1:]:
    if arg.startswith("--target="):
        continue  # Strip cc-rs target, zig uses its own
    if arg.startswith("-march="):
        continue  # Strip -march, zig handles arch via -target
    args.append(arg)

cmd = [str(ZIG), "cc", "-target", "aarch64-linux-musl"] + args
sys.exit(subprocess.call(cmd))
