#!/usr/bin/env bash
# Regenera las capturas del README (docs/img/*.png) con la interfaz real en
# modo demostración (datos de ejemplo, nada personal). Necesita Google Chrome.
# Uso: scripts/capturas.sh
set -euo pipefail
cd "$(dirname "$0")/.."
CH="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
U="file://$PWD/app/ui/index.html"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir -p docs/img
shot() { "$CH" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=2 --window-size="$2" --virtual-time-budget=3000 --screenshot="$PWD/docs/img/$1.png" "$3" >/dev/null 2>&1; }
shot principal     640,1190 "$U?demo=main"
shot nueva-carpeta 640,760  "$U?demo=main&open=new"
shot unirse        640,780  "$U?demo=main&open=join"
shot recibir       640,600  "$U?demo=main&open=receive"
shot ajustes       640,760  "$U?demo=main&open=settings"
# Página que ve el móvil, con envíos de ejemplo.
python3 - "$T/movil.html" <<'PY'
import sys
s = open("crates/sfs-core/src/inbox_page.html").read().replace("{{DEVICE}}", "Portátil de Papá").replace("{{DEST}}", "la carpeta compartida «Pareja»")
items = ('<li class="ok"><div class="row"><span class="name">IMG_3301.HEIC</span><span class="st">Enviado ✓</span></div><div class="bar"><i style="width:100%"></i></div></li>'
         '<li><div class="row"><span class="name">Vídeo cumpleaños.MOV</span><span class="st">62 % · 48.3 MB</span></div><div class="bar"><i style="width:62%"></i></div></li>'
         '<li><div class="row"><span class="name">Recibo luz octubre.pdf</span><span class="st">En espera</span></div><div class="bar"><i></i></div></li>')
open(sys.argv[1], "w").write(s.replace('<ul id="list"></ul>', '<ul id="list">' + items + '</ul>'))
PY
shot movil 500,620 "file://$T/movil.html"
ls -la docs/img
