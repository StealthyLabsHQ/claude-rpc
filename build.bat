@echo off
echo Building Claude RPC...

REM --- 1. Python tray exe -> dist\ ---
pip install pyinstaller==6.14.1 pyinstaller-versionfile==2.1.1 -r requirements.txt --quiet 2>nul
create-version-file version_info.yaml --outfile version_info.txt
pyinstaller --onefile --windowed ^
  --icon=logo\anthropic-rpc.ico ^
  --name=claude-rpc ^
  --distpath=dist ^
  --version-file=version_info.txt ^
  main.py

REM --- 2. Copy logo folder next to exe ---
echo.
echo Copying logo...
if not exist dist\logo mkdir dist\logo
copy /Y logo\tray-icon.png     dist\logo\tray-icon.png     >nul
copy /Y logo\discord.png       dist\logo\discord.png       >nul
copy /Y logo\anthropic-rpc.ico dist\logo\anthropic-rpc.ico >nul

REM --- 3. Prepare dist\runtime\ ---
echo Preparing runtime folder...
if not exist dist\runtime mkdir dist\runtime

REM --- 4. Copy JS source files ---
echo Copying JS files...
copy /Y index.js       dist\runtime\index.js       >nul
copy /Y tray.js        dist\runtime\tray.js        >nul
copy /Y secure-env.js  dist\runtime\secure-env.js  >nul
copy /Y package.json   dist\runtime\package.json   >nul
if exist .env copy /Y .env dist\runtime\.env >nul

REM --- 5. Install production-only dependencies ---
echo Installing production dependencies...
pushd dist\runtime
if exist node_modules rmdir /s /q node_modules
npm install --omit=dev --ignore-scripts --no-fund --no-audit >nul 2>&1
popd

REM --- 6. Copy node.exe ---
echo Copying node.exe...
if exist node.exe (
  copy /Y node.exe dist\runtime\node.exe >nul
) else if exist "C:\Program Files\nodejs\node.exe" (
  copy /Y "C:\Program Files\nodejs\node.exe" dist\runtime\node.exe >nul
) else (
  for /f "delims=" %%i in ('where node 2^>nul') do (
    copy /Y "%%i" dist\runtime\node.exe >nul
    goto :node_done
  )
  echo [warn] node.exe not found.
)
:node_done

echo.
echo ============================================
echo Build complete:
echo   dist\claude-rpc.exe
echo   dist\runtime\node.exe
echo   dist\runtime\index.js
echo   dist\runtime\tray.js
echo   dist\runtime\node_modules\ (production only)
echo   dist\runtime\.env
echo   dist\logo\
echo ============================================
pause
