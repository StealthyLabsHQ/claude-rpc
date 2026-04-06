# -*- mode: python ; coding: utf-8 -*-

from PyInstaller.building.datastruct import Tree

# node_modules tree — Tree() handles recursive dirs correctly
node_tree = Tree(
    'build/runtime/node_modules',
    prefix='runtime/node_modules',
    excludes=['.bin', '__pycache__'],
)

a = Analysis(
    ['main.py'],
    pathex=[],
    binaries=[],
    datas=[
        # logo assets
        ('logo/tray-icon.png',       'logo'),
        ('logo/discord.png',         'logo'),
        ('logo/anthropic-rpc.ico',   'logo'),
        # JS runtime files
        ('build/runtime/index.js',      'runtime'),
        ('build/runtime/tray.js',       'runtime'),
        ('build/runtime/secure-env.js', 'runtime'),
        ('build/runtime/package.json',  'runtime'),
        # node.exe (~67 MB)
        ('build/runtime/node.exe',      'runtime'),
    ],
    hiddenimports=[],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    noarchive=False,
    optimize=0,
)

# Append node_modules tree to analysis datas (TOC format, compatible)
a.datas += node_tree

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
