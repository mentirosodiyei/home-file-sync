#!/usr/bin/env bash
# Genera el instalador de Windows (NSIS) desde un Mac, mediante compilación cruzada.
# Requisitos (una vez): brew install nsis llvm lld && cargo install cargo-xwin --locked
#                       rustup target add x86_64-pc-windows-msvc
# Nota: makensis de Homebrew falla (std::bad_alloc) sin una configuración regional UTF-8.
set -euo pipefail
cd "$(dirname "$0")/../app/src-tauri"
export LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8
# Sin rutas personales en los ejecutables (usuario, carpetas): el compilador
# las sustituye por prefijos genéricos. cargo-xwin no admite espacios en esas
# opciones, así que se compila a través de un enlace sin espacios.
ROOT="$(cd ../.. && pwd)"
LINK="${TMPDIR:-/tmp}/hfs-src-$$"
ln -s "$ROOT" "$LINK"
trap 'rm -f "$LINK"' EXIT
cd "$LINK/app/src-tauri"
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$LINK=/src --remap-path-prefix=$HOME/.cargo=/cargo --remap-path-prefix=$HOME/.rustup=/rustup --remap-path-prefix=$HOME=/home"
npx --yes @tauri-apps/cli@2 build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis
ls -la "$ROOT"/target/x86_64-pc-windows-msvc/release/bundle/nsis/*.exe
