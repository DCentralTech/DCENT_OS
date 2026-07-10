@echo off
cd /d "%~dp0"
set CC_armv7_unknown_linux_musleabihf=%CD%\zig-cc-arm.bat
set AR_armv7_unknown_linux_musleabihf=%CD%\zig-ar-arm.bat
echo Starting cargo build...
cargo build --release --target armv7-unknown-linux-musleabihf > build_output.txt 2>&1
set BUILD_RC=%ERRORLEVEL%
echo BUILD_EXIT_CODE=%BUILD_RC% >> build_output.txt
if not "%BUILD_RC%"=="0" (
    echo Build failed with exit code %BUILD_RC%. See build_output.txt.
    exit /b %BUILD_RC%
)
echo Done.
exit /b 0
