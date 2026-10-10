@echo off
rem Installs FilmCraft (with OpenFX plug-ins) for this user. See install.ps1.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
echo.
pause
