#!/usr/bin/env bash
# Copia los instaladores recién compilados a Instaladores/ con nombres claros
# y mueve las versiones anteriores a Instaladores/old/.
# Uso (después de compilar-macos.sh y compilar-windows-desde-mac.sh):
#   scripts/publicar-instaladores.sh
set -euo pipefail
cd "$(dirname "$0")/.."
V=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
DMG="target/universal-apple-darwin/release/bundle/dmg/Show File Sync_${V}_universal.dmg"
EXE="target/x86_64-pc-windows-msvc/release/bundle/nsis/Show File Sync_${V}_x64-setup.exe"
mkdir -p Instaladores/old
# Lo que no sea de esta versión pasa a old/.
find Instaladores -maxdepth 1 -type f \( -name "*.dmg" -o -name "*.exe" \) ! -name "*${V}*" -exec mv {} Instaladores/old/ \;
[ -f "$DMG" ] && cp "$DMG" "Instaladores/Show File Sync ${V} - macOS (Apple Silicon e Intel).dmg"
[ -f "$EXE" ] && cp "$EXE" "Instaladores/Show File Sync ${V} - Windows.exe"
ls -la Instaladores Instaladores/old | grep -v DS_Store
