@echo off
set "ZIG_DIR=C:\bt\zig-windows-x86_64-0.13.0-clean\zig-windows-x86_64-0.13.0"
"%ZIG_DIR%\zig.exe" ar %*
exit /b %ERRORLEVEL%
