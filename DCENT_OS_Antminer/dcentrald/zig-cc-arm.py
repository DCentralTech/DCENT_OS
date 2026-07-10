"""Zig CC wrapper that strips --target flag added by cc-rs and uses arm-linux-musleabihf."""
import os
import pathlib
import subprocess
import sys

ZIG_DIR = pathlib.Path(r"C:\zig-0.13.0")
ZIG = ZIG_DIR / "zig.exe"

# Set ZIG_LIB_DIR explicitly. Without this, zig 0.13 sometimes fails
# executable-path resolution when invoked via Python subprocess and
# emits "unable to find zig installation directory: FileNotFound".
env = os.environ.copy()
env["ZIG_LIB_DIR"] = str(ZIG_DIR / "lib")

args = []
for arg in sys.argv[1:]:
    if arg.startswith("--target="):
        continue  # Strip cc-rs target, zig uses its own
    if arg.startswith("-march="):
        continue  # Strip -march, zig handles arch via -target
    args.append(arg)

cmd = [str(ZIG), "cc", "-target", "arm-linux-musleabihf"] + args
sys.exit(subprocess.call(cmd, env=env))
