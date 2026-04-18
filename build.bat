@echo off
echo Building Claude Rich Presence (All-in-One)...

REM --- 1. Stage runtime in build\staging\runtime\ ---
echo Preparing build\staging\runtime...
if not exist build\staging mkdir build\staging
if exist build\staging\runtime rmdir /s /q build\staging\runtime
if exist build\staging\logo rmdir /s /q build\staging\logo
mkdir build\staging\runtime

REM Copy JS source files
copy /Y index.js       build\staging\runtime\index.js       >nul
copy /Y tray.js        build\staging\runtime\tray.js        >nul
copy /Y secure-env.js  build\staging\runtime\secure-env.js  >nul
copy /Y package.json   build\staging\runtime\package.json   >nul
mkdir build\staging\logo >nul
copy /Y logo\*         build\staging\logo\ >nul

REM Install production-only dependencies
echo Installing production dependencies...
pushd build\staging\runtime
if exist node_modules rmdir /s /q node_modules
call npm install --omit=dev --ignore-scripts --no-fund --no-audit >nul 2>&1
popd

REM Copy node.exe into staging folder
echo Copying node.exe...
if exist node.exe (
  copy /Y node.exe build\staging\runtime\node.exe >nul
) else if exist "C:\Program Files\nodejs\node.exe" (
  copy /Y "C:\Program Files\nodejs\node.exe" build\staging\runtime\node.exe >nul
) else (
  for /f "delims=" %%i in ('where node 2^>nul') do (
    copy /Y "%%i" build\staging\runtime\node.exe >nul
    goto :node_done
  )
  echo [warn] node.exe not found - build may fail.
)
:node_done

REM --- 2. Build version info ---
echo Generating version info...
call pip install "pyinstaller>=6.15.0" "pyinstaller-versionfile>=2.1.1" -r requirements.txt --quiet
call create-version-file version_info.yaml --outfile version_info.txt

REM --- 3. PyInstaller - single EXE with everything embedded ---
echo Running PyInstaller...
call pyinstaller --clean --distpath=dist claude-rpc.spec

echo.
echo ============================================
echo Build complete:
echo   dist\claude-rpc.exe  (all-in-one)
echo
echo Note: ~80-90 MB expected (node.exe embedded)
echo Users only need claude-rpc.exe - no extra folders!
echo ============================================
pause
