@echo off
setlocal enabledelayedexpansion
title Claude Rich Presence

:: Check if Python is installed
where python >nul 2>&1
if %errorlevel% neq 0 (
    echo [ERROR] Python is not installed.
    echo Download it from https://www.python.org/downloads/
    pause
    exit /b 1
)

:: Check if Discord is running
tasklist /FI "IMAGENAME eq Discord.exe" 2>nul | find /I "Discord.exe" >nul
if %errorlevel% neq 0 (
    echo [ERROR] Discord is not running.
    echo Please open Discord before launching Anthropic Rich Presence.
    pause
    exit /b 1
)

:: Check if dependencies are installed
cd /d "%~dp0"
python -c "import pypresence" >nul 2>&1
if %errorlevel% neq 0 (
    echo [SETUP] Installing dependencies...
    pip install -r requirements.txt --quiet
    echo.
)

:: Check if .env exists, if not create it
if not exist "%~dp0.env" (
    echo ============================================
    echo   First time setup - Discord Application ID
    echo ============================================
    echo.
    echo 1. Go to https://discord.com/developers/applications
    echo 2. Create a new application ^(e.g. "Claude AI"^)
    echo 3. Copy the Application ID
    echo.
    set /p CLIENT_ID="Paste your Discord Application ID here: "
    echo DISCORD_CLIENT_ID=!CLIENT_ID!> "%~dp0.env"
    echo CLAUDE_DIR_PATH=~/.claude>> "%~dp0.env"
    echo CLAUDE_MODEL=claude-opus-4-6>> "%~dp0.env"
    echo.
    echo [OK] Configuration saved to .env
    echo.
)

:: Launch
echo Starting Claude Rich Presence...
python "%~dp0main.py"
pause
