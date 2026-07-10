@echo off
set "ZIG_DIR=C:\zig-0.13.0"
set "ZIG_LIB_DIR=%ZIG_DIR%\lib"
"%ZIG_DIR%\zig.exe" ar %*
exit /b %ERRORLEVEL%
