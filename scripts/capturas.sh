#!/usr/bin/env bash
# Regenera las capturas del README con la interfaz real en modo demostración
# (datos de ejemplo, nada personal), en los tres idiomas:
#   docs/img/*.png (castellano), docs/img/ca/*.png y docs/img/en/*.png
# Necesita Google Chrome. Uso: scripts/capturas.sh [es|ca|en ...]
set -euo pipefail
cd "$(dirname "$0")/.."
CH="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
U="file://$PWD/app/ui/index.html"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
cargo build -q -p sfs-core --example pagina_movil
PM="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/examples/pagina_movil"

LANGS="${*:-es ca en}"
for L in $LANGS; do
  if [ "$L" = es ]; then D=docs/img; else D="docs/img/$L"; fi
  mkdir -p "$D"
  shot() { "$CH" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=2 --window-size="$2" --virtual-time-budget=3000 --screenshot="$PWD/$D/$1.png" "$3" >/dev/null 2>&1; }
  shot principal     640,1190 "$U?lang=$L&demo=main"
  shot nueva-carpeta 640,760  "$U?lang=$L&demo=main&open=new"
  shot unirse        640,780  "$U?lang=$L&demo=main&open=join"
  shot recibir       640,600  "$U?lang=$L&demo=main&open=receive"
  shot ajustes       640,760  "$U?lang=$L&demo=main&open=settings"
  # Página que ve el móvil, con envíos de ejemplo.
  case $L in
    es) DEV="Portátil de Papá"; SH="Pareja" ;;
    ca) DEV="Portàtil del pare"; SH="Parella" ;;
    *)  DEV="Dad's laptop"; SH="Couple" ;;
  esac
  "$PM" "$L" "$DEV" "$SH" > "$T/base.html"
  python3 - "$L" "$T/base.html" "$T/movil.html" <<'PY'
import sys
L, src, dst = sys.argv[1:]
t = {"es": ("Enviado ✓", "Vídeo cumpleaños.MOV", "Recibo luz octubre.pdf", "En espera"),
     "ca": ("Enviat ✓", "Vídeo aniversari.MOV", "Rebut llum octubre.pdf", "En espera"),
     "en": ("Sent ✓", "Birthday video.MOV", "Electricity bill October.pdf", "Waiting")}[L]
items = (f'<li class="ok"><div class="row"><span class="name">IMG_3301.HEIC</span><span class="st">{t[0]}</span></div><div class="bar"><i style="width:100%"></i></div></li>'
         f'<li><div class="row"><span class="name">{t[1]}</span><span class="st">62 % · 48.3 MB</span></div><div class="bar"><i style="width:62%"></i></div></li>'
         f'<li><div class="row"><span class="name">{t[2]}</span><span class="st">{t[3]}</span></div><div class="bar"><i></i></div></li>')
s = open(src).read()
assert '<ul id="list"></ul>' in s
open(dst, "w").write(s.replace('<ul id="list"></ul>', '<ul id="list">' + items + '</ul>'))
PY
  shot movil 500,620 "file://$T/movil.html"
  ls "$D"
done
