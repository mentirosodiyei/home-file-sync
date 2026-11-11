#!/usr/bin/env bash
# Prueba de extremo a extremo con descubrimiento REAL (mDNS + baliza) en este
# equipo: dos instancias sin interfaz se encuentran, se emparejan con el código
# y sincronizan en ambos sentidos. Uso: scripts/prueba-descubrimiento.sh
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q -p sfs-cli
BIN="$PWD/target/debug/homefilesync-cli"
T="$(mktemp -d)"
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$T"' EXIT
mkdir -p "$T"/{da,db,fa,fb}
echo "hola desde A" > "$T/fa/Presentación A.pptx"

"$BIN" --data "$T/da" --folder "$T/fa" --nombre "Equipo A" crear > "$T/a.log" 2>&1 &
for _ in $(seq 20); do grep -q "Código" "$T/a.log" && break; sleep 0.5; done
CODE=$(sed -n 's/.*(15 min): //p' "$T/a.log")
echo "Código: $CODE"
"$BIN" --data "$T/db" --folder "$T/fb" --nombre "Equipo B" unir "$CODE" > "$T/b.log" 2>&1 &

ok=0
for _ in $(seq 40); do [ -f "$T/fb/Presentación A.pptx" ] && ok=1 && break; sleep 0.5; done
[ $ok = 1 ] || { echo "FALLO: B no recibió el archivo"; cat "$T/a.log" "$T/b.log"; exit 1; }
echo "✓ A → B"
echo "hola desde B" > "$T/fb/Nota B.txt"
ok=0
for _ in $(seq 40); do [ -f "$T/fa/Nota B.txt" ] && ok=1 && break; sleep 0.5; done
[ $ok = 1 ] || { echo "FALLO: A no recibió el archivo"; cat "$T/a.log" "$T/b.log"; exit 1; }
echo "✓ B → A"
cmp "$T/fa/Presentación A.pptx" "$T/fb/Presentación A.pptx" && cmp "$T/fa/Nota B.txt" "$T/fb/Nota B.txt"
echo "✓ Contenidos idénticos. Prueba superada."
