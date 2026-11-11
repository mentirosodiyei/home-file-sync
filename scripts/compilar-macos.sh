#!/usr/bin/env bash
# Compila Home File Sync para macOS (universal: Apple Silicon + Intel) y lo firma.
#
# La firma importa: macOS asocia el permiso «Red local» a la firma del programa.
# Con la firma automática del enlazador (la que queda sin este paso) el
# identificador cambia en cada compilación y el permiso puede no aplicarse:
# las conexiones salientes fallan con «No route to host (os error 65)».
#
# Identidad usada, por orden:
#   1. $SFS_SIGN_IDENTITY si está definida
#   2. "Developer ID Application" del llavero (distribución)
#   3. "Apple Development" del llavero (equipos propios)
#   4. "-" firma ad hoc del paquete completo (identificador estable, menos fiable)
set -euo pipefail
cd "$(dirname "$0")/../app/src-tauri"

ID="${SFS_SIGN_IDENTITY:-}"
if [ -z "$ID" ]; then
  ID=$(security find-identity -v -p codesigning 2>/dev/null | grep -m1 '"Developer ID Application' | awk '{print $2}' || true)
fi
if [ -z "$ID" ]; then
  ID=$(security find-identity -v -p codesigning 2>/dev/null | grep -m1 '"Apple Development' | awk '{print $2}' || true)
fi
ID="${ID:--}"
echo "Identidad de firma: $( [ "$ID" = "-" ] && echo "ad hoc" || security find-identity -v -p codesigning | grep "$ID" | sed 's/.*"\(.*\)"/\1/' )"

rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
APPLE_SIGNING_IDENTITY="$ID" npx --yes @tauri-apps/cli@2 build --target universal-apple-darwin --bundles app,dmg

APP="../../target/universal-apple-darwin/release/bundle/macos/Home File Sync.app"
echo
echo "Comprobación de la firma:"
codesign --verify --deep --strict "$APP" && echo "  firma válida"
codesign -dv "$APP" 2>&1 | grep -E "^Identifier|^TeamIdentifier|^Info.plist|flags" | sed 's/^/  /'
otool -arch all -l "$APP/Contents/MacOS/home-file-sync" | grep -A2 LC_UUID | grep uuid | sed 's/^ */  UUID /'
