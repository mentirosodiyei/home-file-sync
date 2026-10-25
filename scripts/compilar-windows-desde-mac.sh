#!/usr/bin/env bash
# Genera el instalador de Windows (NSIS) desde un Mac, mediante compilación cruzada.
# Requisitos (una vez): brew install nsis llvm lld && cargo install cargo-xwin --locked
#                       rustup target add x86_64-pc-windows-msvc
# Nota: makensis de Homebrew falla (std::bad_alloc) sin una configuración regional UTF-8.
set -euo pipefail
cd "$(dirname "$0")/../app/src-tauri"
export LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8
export PATH="$(brew --prefix llvm)/bin:$(brew --prefix lld)/bin:$PATH"
npx --yes @tauri-apps/cli@2 build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis
ls -la ../../target/x86_64-pc-windows-msvc/release/bundle/nsis/*.exe
