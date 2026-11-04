#!/usr/bin/env bash
# Comprueba que la versión actual sincroniza con una versión anterior
# (por defecto la del primer commit, 0.1.0) en ambos sentidos y en ambos
# papeles (crear grupo / unirse). Uso: scripts/prueba-compatibilidad.sh [ref-git]
set -euo pipefail
cd "$(dirname "$0")/.."
REF="${1:-$(git rev-list --max-parents=0 HEAD)}"
T="$(mktemp -d)"
OLD_TARGET="$PWD/target/compat-$REF"   # caché de la compilación antigua
trap 'kill $(jobs -p) 2>/dev/null || true; git worktree remove --force "$T/old" >/dev/null 2>&1 || true; rm -rf "$T"' EXIT

git worktree add --detach "$T/old" "$REF" >/dev/null 2>&1
echo "Compilando versión antigua ($REF)…"
(cd "$T/old" && CARGO_TARGET_DIR="$OLD_TARGET" cargo build -q -p sfs-cli)
cargo build -q -p sfs-cli
OLD="$OLD_TARGET/debug/showfilesync-cli"
NEW="$PWD/target/debug/showfilesync-cli"

run_pair() { # $1 binario que crea, $2 binario que se une, $3 etiqueta
  local d="$T/$3"; mkdir -p "$d"/{da,db,fa,fb}
  echo "de A" > "$d/fa/A.pptx"
  "$1" --data "$d/da" --folder "$d/fa" --nombre "A-$3" --sin-descubrimiento --puerto 47911 crear > "$d/a.log" 2>&1 &
  for _ in $(seq 40); do grep -q "Código" "$d/a.log" && break; sleep 0.25; done
  local code; code=$(sed -n 's/.*(15 min): //p' "$d/a.log" | tr -d ' ')
  local help; help=$("$2" --help 2>&1 || true)   # --help sale con código 2
  if grep -q -- "--anfitrion" <<<"$help"; then
    "$2" --data "$d/db" --folder "$d/fb" --nombre "B-$3" --sin-descubrimiento --anfitrion 127.0.0.1:47911 unir "$code" > "$d/b.log" 2>&1 &
  else
    # La versión antigua solo sabe unirse con descubrimiento: se usa el real
    # (mDNS + baliza) en este equipo. A también debe anunciarse.
    kill %1 2>/dev/null; wait 2>/dev/null || true; rm -rf "$d"; mkdir -p "$d"/{da,db,fa,fb}; echo "de A" > "$d/fa/A.pptx"
    "$1" --data "$d/da" --folder "$d/fa" --nombre "A-$3" --puerto 47911 crear > "$d/a.log" 2>&1 &
    for _ in $(seq 40); do grep -q "Código" "$d/a.log" && break; sleep 0.25; done
    code=$(sed -n 's/.*(15 min): //p' "$d/a.log" | tr -d ' ')
    "$2" --data "$d/db" --folder "$d/fb" --nombre "B-$3" unir "$code" > "$d/b.log" 2>&1 &
  fi
  for _ in $(seq 60); do [ -f "$d/fb/A.pptx" ] && break; sleep 0.5; done
  [ -f "$d/fb/A.pptx" ] || { echo "FALLO $3: B no recibió de A"; cat "$d/a.log" "$d/b.log"; exit 1; }
  echo "de B" > "$d/fb/B.pdf"
  for _ in $(seq 60); do [ -f "$d/fa/B.pdf" ] && break; sleep 0.5; done
  [ -f "$d/fa/B.pdf" ] || { echo "FALLO $3: A no recibió de B"; cat "$d/a.log" "$d/b.log"; exit 1; }
  echo "  ✓ $3: sincroniza en ambos sentidos"
  kill $(jobs -p) 2>/dev/null || true; wait 2>/dev/null || true
}

echo "Antigua crea el grupo, nueva se une:"; run_pair "$OLD" "$NEW" antigua-crea
echo "Nueva crea el grupo, antigua se une:"; run_pair "$NEW" "$OLD" nueva-crea
echo "Compatibilidad verificada."
