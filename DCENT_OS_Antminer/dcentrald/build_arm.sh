#!/bin/bash
export CC_armv7_unknown_linux_musleabihf=C:/temp/dcent-cc/zig-cc-arm.bat
export AR_armv7_unknown_linux_musleabihf=C:/temp/dcent-cc/zig-ar-arm.bat
cargo build --release --target armv7-unknown-linux-musleabihf
