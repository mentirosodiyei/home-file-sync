# Home File Sync

**Carpetas compartidas entre los ordenadores de casa (Windows y macOS), en tu propia red.**
Sin nube, sin cuentas, sin direcciones IP.

## Cómo se usa

- **Crear una carpeta compartida:** pulsa **Nueva carpeta compartida**, ponle un nombre, elige la carpeta de tu equipo y si la quieres *sincronizada* o como *copia de seguridad*. Aparece un código de 8 cifras.
- **Unirse desde otro equipo:** pulsa **Unirme con código**. La carpeta aparece en la lista porque se descubre en la red. Escribe el código y elige dónde guardarla.
- **Tantas carpetas como quieras, cada una con sus miembros.** En una familia de cuatro, la pareja puede tener una carpeta compartida, el padre otra con su hijo, y el ordenador del salón una copia de seguridad de las fotos. Cada carpeta tiene su propio código y nadie ve las carpetas de los demás.
- **Dos modos por carpeta, elegidos en cada equipo:**
  - **Sincronizada:** lo que se añade, edita, renombra o borra en un equipo se replica en todos. Es como una sola carpeta.
  - **Copia de seguridad:** el equipo solo recibe. Sus cambios no se envían, y lo que se borra en los demás se conserva en la copia.
- **Nombres de los equipos a tu gusto:** pulsa sobre el nombre de cualquier equipo para llamarlo como quieras («Ana», «PC del salón»…). Ese nombre solo lo ves tú; no cambia el de nadie más.
- **Recibir desde el móvil, sin instalar nada:** pulsa **📱 Recibir del móvil**. Escanea el código QR con la cámara del teléfono, o abre la dirección en cualquier navegador de la misma wifi, y elige las fotos o archivos.
  - Se guardan en la carpeta compartida que elijas, o en *Descargas/Recibidos Home File Sync*.
  - El enlace caduca a los 15 minutos y deja de valer al cerrarlo.

- **Fuera de casa por VPN:** con WireGuard (u otra VPN que llegue a la red de casa) y el ajuste *Permitir conexiones por VPN*, el portátil sigue sincronizando cuando sales de casa. Ver [docs/RED.md](docs/RED.md).

Al cerrar la ventana, Home File Sync sigue funcionando en la **bandeja del sistema** (Windows) o en la **barra de menús** (macOS). Para detenerlo, elige *Salir* en ese menú.

## Qué garantiza

- **No se pierden archivos.**
  - Cuando un archivo cambia, la versión nueva queda en su sitio y la anterior pasa a la carpeta **`old`**, dentro de la carpeta sincronizada, con la fecha en el nombre: `old/Día 1/Ponencia (2026-11-04 15.20).pptx`. La carpeta `old` se sincroniza como las demás, así que todos los equipos tienen las versiones anteriores, también el que editó.
  - Si dos equipos cambian el mismo archivo a la vez, se queda la versión más reciente y la otra va a `old/` como `Nombre (conflicto - Equipo - abc123).pptx`.
  - Lo que hay en `old` no se borra solo: puedes vaciarla cuando quieras, y eso también se replica.
- **Las carpetas funcionan como una sola.** Lo que se añade, edita, renombra o borra en un equipo se replica en todos.
  - Un archivo borrado no se destruye en los demás equipos: se mueve a su carpeta oculta `.homefilesync/papelera` y se conserva 14 días, por si se borró por error.
  - Si un equipo edita un archivo mientras otro lo borra, gana la edición.
  - Si un disco se desconecta, o la carpeta se sustituye por otra vacía, no se borra nada en los demás: la carpeta se vuelve a llenar.
- **Nada a medias.**
  - No se copian archivos que todavía se están escribiendo.
  - Las descargas van a un temporal y solo se colocan, con un renombrado atómico, cuando el hash BLAKE3 coincide.
- **Reanudación.** Si se corta la red a mitad de un vídeo de 4 GB, la descarga continúa donde se quedó al volver.
- **Sin transferencias inútiles.** Los archivos idénticos no se vuelven a enviar, y con muchos equipos la carga se reparte entre ellos.
- **Seguridad.**
  - Solo los equipos emparejados con el código pueden intercambiar archivos, y todo viaja cifrado.
  - Nadie puede leer ni escribir fuera de la carpeta elegida.
  - Los archivos recibidos nunca se ejecutan.

Detalles en [docs/ARQUITECTURA.md](docs/ARQUITECTURA.md).

## Requisitos de red

Todos los equipos deben estar en la **misma red local**. Funciona sin internet, incluso en una red completamente aislada.

En **macOS** hay que permitir el acceso a la **Red local** cuando el sistema lo pida (*Ajustes del Sistema → Privacidad y seguridad → Red local*). Sin ese permiso, el Mac no puede unirse a grupos de otros equipos.

Si el grupo no aparece automáticamente, usa **Conexión manual** en «Unirse a un grupo», escribiendo la IP del otro equipo. Para cualquier problema, *Ajustes (⚙) → Avanzado → Diagnóstico* muestra el registro detallado de conexiones.

Algunas redes wifi de hoteles y recintos tienen **aislamiento de clientes**, que impide que los equipos se vean entre sí. Home File Sync lo indica en pantalla; en ese caso usa una red de producción propia o un switch. Más detalles en [docs/RED.md](docs/RED.md).

## Instalación

| Sistema | Archivo |
|---|---|
| macOS 11 Big Sur o posterior (Intel y Apple Silicon) | `Home File Sync_x.y.z_universal.dmg` |
| Windows 10 / 11 (64 bits) | `Home File Sync_x.y.z_x64-setup.exe` |

- **macOS**:
  - La primera vez, macOS pide permiso para acceder a la **red local**: hay que aceptarlo.
  - Mientras la app no esté firmada con un certificado de Apple, ábrela con *clic derecho → Abrir*.
- **Windows**:
  - El instalador añade automáticamente las reglas del Firewall de Windows, incluso para redes «Públicas», por si la red de casa está marcada así.
  - Necesita WebView2, que viene incluido en Windows 10/11 actualizados; el instalador lo descarga si falta.

## Compilar desde el código

Requisitos: [Rust](https://rustup.rs) estable y Node.js (solo para la herramienta de empaquetado de Tauri).

```bash
cargo test --workspace
```

```bash
cargo run -p home-file-sync
```

```bash
scripts/compilar-macos.sh
```

Este script compila la versión universal y la firma con la identidad de Apple del llavero, si la hay. La firma es necesaria para que el permiso «Red local» de macOS funcione de forma fiable.

En Windows, `npx @tauri-apps/cli@2 build` genera el instalador NSIS. La CI (`.github/workflows/build.yml`) compila y prueba en Windows y macOS en cada cambio.

### Sin interfaz gráfica

`homefilesync-cli` ejecuta el mismo motor sin ventana. Sirve para diagnóstico o para probar varias instancias en un mismo ordenador:

```bash
cargo run -p sfs-cli -- --data /tmp/a --folder ~/Escenario crear
```

```bash
cargo run -p sfs-cli -- --data /tmp/b --folder ~/Escenario2 unir 48210937
```

## Estructura

```
crates/sfs-core   motor (biblioteca, sin interfaz) + pruebas de integración
crates/sfs-cli    binario sin interfaz
app/src-tauri     aplicación de escritorio (Tauri 2)
app/ui            interfaz (HTML/CSS/JS sin dependencias)
docs/             arquitectura y guía de red
```

## Licencia

MIT o Apache-2.0, a tu elección.
