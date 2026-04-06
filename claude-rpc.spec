# -*- mode: python ; coding: utf-8 -*-

a = Analysis(
    ['main.py'],
    pathex=[],
    binaries=[],
    datas=[
        # logo assets
        ('logo/tray-icon.png',       'logo'),
        ('logo/discord.png',         'logo'),
        ('logo/anthropic-rpc.ico',   'logo'),
        # esbuild bundle (replaces index.js + node_modules entirely)
        ('build/bundle.js',          'runtime'),
        # node.exe (~67 MB)
        ('build/runtime/node.exe',   'runtime'),
    ],
    hiddenimports=[],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    noarchive=False,
    optimize=0,
)

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.datas,
    [],
    name='claude-rpc',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    version='version_info.txt',
    icon=['logo\\anthropic-rpc.ico'],
)
