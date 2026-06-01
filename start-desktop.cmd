@echo off
setlocal

cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\start-desktop-windows.ps1"
set "EXIT_CODE=%ERRORLEVEL%"

echo.
echo CodeSeeX Next desktop exited with code %EXIT_CODE%.
pause
exit /b %EXIT_CODE%
