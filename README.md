# Home File Sync

**Castellano** · [Català](README.ca.md) · [English](README.en.md)

**Carpetas compartidas entre los ordenadores de casa, en tu propia red.**
Windows y macOS, sin nube, sin cuentas y sin configurar direcciones IP. Y para enviar fotos desde el móvil basta con escanear un código QR.

<p align="center"><img src="docs/img/principal.png" width="560" alt="Pantalla principal de Home File Sync con tres carpetas compartidas"></p>

## Qué hace

- 📁 **Carpetas compartidas que funcionan como una sola.** Lo que añades, editas, renombras o borras en un equipo aparece igual en los demás.
- 👨‍👩‍👧 **Tantas carpetas como quieras, cada una con sus miembros.** Por ejemplo, una para la pareja, otra entre padre e hijo y una copia de seguridad en el ordenador del salón. Nadie ve las carpetas en las que no está.
- 💾 **Modo copia de seguridad.** Un equipo puede limitarse a recibir: sus cambios no se envían y lo que se borre en el original se conserva en la copia.
- 🕘 **Versiones anteriores.** Cuando un archivo cambia, la versión anterior se guarda en la carpeta `old`, con la fecha en el nombre.
- 📱 **Recibir desde el móvil sin instalar nada.** La app muestra un QR; el móvil lo escanea, elige las fotos o documentos y llegan a la carpeta que elijas.
- 🏷️ **Nombres a tu gusto.** Puedes llamar a cada equipo como quieras («Ana», «PC del salón»…). Ese nombre solo lo ves tú.
- 🔒 **Privado y cifrado.** Los archivos van directamente de un equipo a otro por tu red, cifrados; no pasan por ningún servidor ni por internet.
- ☁️ **Aunque los equipos no coincidan encendidos.** Con una carpeta de Google Drive, Dropbox, OneDrive o tu NAS, lo que le falta a un equipo apagado se guarda allí cifrado y lo recoge al volver. Cuando todos lo tienen, se borra.
- 🤖 **IA en varios equipos.** Claude, Codex, Cursor, Gemini o cualquier IA compatible con MCP puede pedir a otro equipo de la carpeta que ejecute las pruebas o la compilación —por ejemplo, en el PC con Windows mientras trabaja en el Mac— y recibir el resultado. Tú decides en cada equipo qué órdenes acepta.
- 🌍 **También fuera de casa**, a través de tu propia VPN (por ejemplo, WireGuard), y se pueden recibir archivos de alguien que no está en casa con un enlace temporal.
- 🗣️ **En català, castellano e inglés**, según el idioma del sistema.

## Cómo se usa

### 1. Crear una carpeta compartida

Pulsa **Nueva carpeta compartida**, ponle un nombre y elige la carpeta de tu equipo. Elige también cómo participa este equipo:
- **Sincronizada:** lo que cambie en cualquier equipo se replica en todos.
- **Copia de seguridad:** este equipo solo recibe.

Al crearla aparece un **código de 8 cifras**, válido 15 minutos.

<p align="center"><img src="docs/img/nueva-carpeta.png" width="480" alt="Crear una carpeta compartida"></p>

### 2. Unirse desde otro equipo

En el otro ordenador pulsa **Unirme con código**. La carpeta aparece sola, porque los equipos se descubren en la red. Escribe el código y elige dónde guardarla. Para añadir más equipos más adelante, pulsa **Invitar equipo** en la carpeta y se genera un código nuevo.

<p align="center"><img src="docs/img/unirse.png" width="480" alt="Unirse a una carpeta compartida con el código"></p>

### 3. Recibir desde el móvil (o cualquier navegador)

Pulsa **📱 Recibir del móvil** y elige dónde guardar lo que llegue. Escanea el QR con la cámara del teléfono, o abre la dirección en cualquier navegador conectado a la misma wifi:

<p align="center">
  <img src="docs/img/recibir.png" width="440" alt="Código QR para recibir archivos">
  &nbsp;
  <img src="docs/img/movil.png" width="300" alt="Página que se abre en el móvil para enviar archivos">
</p>

- El enlace caduca a los 15 minutos y deja de funcionar al cerrar la ventana.
- Nunca sobrescribe un archivo: si ya existe uno con ese nombre, guarda «(2)».
- La página del móvil sale en el idioma del teléfono.
- **Desde fuera de casa:** activa *Permitir enviar desde fuera de casa* y el QR pasa a ser un enlace `https://` temporal (a través de Cloudflare, sin cuenta) que funciona desde cualquier sitio mientras la ventana está abierta. La primera vez la app descarga `cloudflared` de Cloudflare y comprueba su huella antes de usarlo.

### 4. Nube o NAS, para cuando los equipos no coinciden

En una carpeta compartida pulsa **☁︎ Nube o NAS** y elige cómo conectar. Hazlo en cada equipo que quieras que lo use:

| Opción | Para qué |
|---|---|
| **Carpeta de la app del servicio** | Google Drive, Dropbox, OneDrive, Proton Drive, iCloud, pCloud… con su app instalada. Con cuentas distintas, comparte la carpeta desde el propio servicio. |
| **Google Drive (iniciar sesión)** | Sin instalar nada. Todos los equipos con la misma cuenta de Google; la app solo ve su propia carpeta. |
| **Dropbox (iniciar sesión)** | Sin instalar nada. Para varias cuentas, comparte la carpeta desde Dropbox y cada uno inicia sesión con la suya. |
| **NAS de casa (SMB)** | Dirección, carpeta compartida, usuario y contraseña. La app conecta sola (y reconecta). |
| **Servidor WebDAV** | Nextcloud, ownCloud, Infomaniak kDrive, pCloud, NAS… con usuario y contraseña. |

Las contraseñas y sesiones se guardan en el llavero del sistema, no en la app.

- Si todos los equipos están conectados, no se usa: los archivos van directamente por la red.
- Si alguno no lo está, lo que le falta se guarda allí **cifrado** (el servicio no puede ver ni los nombres) y lo descarga cuando vuelve.
- Cuando lo tienen todos los equipos que usan esa carpeta, se **borra** del servicio. Si lo prefieres, se mueve a una carpeta `old` del servicio, que tendrás que vaciar a mano.

<p align="center"><img src="docs/img/relevo.png" width="480" alt="Elegir una carpeta de la nube o del NAS como relevo"></p>

### 5. IA en varios equipos (Claude, Codex, Cursor…)

Si programas con una IA en un equipo y necesitas probar en otro (por ejemplo, escribir en el Mac y compilar en Windows), comparte la carpeta del proyecto y pulsa **🤖 IA** en ella:

- **Es un proyecto de código.** Respeta los `.gitignore` y no sincroniza `.git`: cada equipo conserva su repositorio y sus carpetas de compilación (`target/`, `node_modules/`…). Actívalo en todos los equipos de la carpeta. Lo que pasa a excluirse no se borra en ningún sitio.
- **Permitir que las IA de los otros equipos ejecuten órdenes aquí.** Está desactivado por defecto. Puedes poner órdenes que se ejecutan sin preguntar (`cargo test`, `npm test`…; la app sugiere las del proyecto). Las demás aparecen en la carpeta para **permitirlas una vez, siempre o rechazarlas**. Las órdenes se ejecutan sin intérprete de órdenes (sin tuberías ni `&&`), con tu usuario y dentro de la carpeta.
- **Conectar una IA de este equipo.** Añade Home File Sync como servidor MCP en tu aplicación de IA. La pantalla muestra la orden exacta; por ejemplo, en Claude Code:

  ```bash
  claude mcp add --scope user home-file-sync -- "/Applications/Home File Sync.app/Contents/MacOS/home-file-sync" --mcp
  ```

  En Windows, la ruta es la de `home-file-sync.exe` en la carpeta de instalación. Para Codex: `codex mcp add home-file-sync -- <ruta> --mcp`. Claude Desktop, Cursor, Gemini CLI, LM Studio y las demás usan el bloque JSON que muestra la app.

La IA tiene entonces las herramientas `list_machines`, `run_command`, `get_run`, `cancel_run` y `sync_now`. Antes de ejecutar, el otro equipo espera a tener todos tus cambios. Al terminar, lo que haya generado la orden vuelve antes de que la IA reciba el resultado. Home File Sync tiene que estar abierto en los dos equipos.

### 6. Ajustes

Desde **Ajustes** se puede cambiar el nombre de este equipo y permitir las conexiones por VPN, para usar la app fuera de casa. En **Avanzado → Diagnóstico** hay un registro detallado de conexiones que se puede filtrar, copiar y exportar.

<p align="center"><img src="docs/img/ajustes.png" width="480" alt="Ajustes: nombre del equipo y conexiones por VPN"></p>

Al cerrar la ventana, la app sigue funcionando en la **bandeja del sistema** (Windows) o en la **barra de menús** (macOS). Para detenerla, elige *Salir* en ese menú.

## Instalación

Descarga la última versión en [Releases](../../releases).

| Sistema | Archivo |
|---|---|
| macOS 11 Big Sur o posterior (Apple Silicon e Intel) | `HomeFileSync-x.y.z-macOS.dmg` |
| Windows 10 / 11 (64 bits) | `HomeFileSync-x.y.z-Windows.exe` |

**macOS**
- La app no está firmada con un certificado de desarrollador de Apple. La primera vez macOS avisará de que no puede comprobarla: ve a *Ajustes del Sistema → Privacidad y seguridad* y pulsa **Abrir igualmente**.
- Cuando macOS pregunte si Home File Sync puede acceder a la **red local**, pulsa **Permitir**. Sin ese permiso no podrá conectar con los demás equipos.

**Windows**
- Windows SmartScreen puede avisar porque el instalador no está firmado: pulsa **Más información → Ejecutar de todas formas**.
- El instalador añade las reglas del Firewall de Windows necesarias, también para redes marcadas como «Públicas».

## Cómo cuida tus archivos

- **No se pierde nada.**
  - Cuando un archivo cambia, la versión anterior pasa a `old/` en todos los equipos.
  - Si dos equipos cambian el mismo archivo a la vez, se queda la versión más reciente y la otra va a `old/` como copia de conflicto.
  - Lo que se borra en un equipo se elimina en los demás, pero en ellos no se destruye: se guarda 14 días en una papelera oculta (`.homefilesync/papelera`).
- **Gana la edición.** Si un equipo edita un archivo mientras otro lo borra, el archivo editado vuelve a todos.
- **Nada a medias.**
  - No se copian archivos que todavía se están escribiendo.
  - Cada archivo se verifica con su huella digital (BLAKE3) antes de colocarlo.
  - Si se corta la red a mitad de un archivo grande, la copia continúa donde se quedó.
- **Discos desconectados.** Si una carpeta desaparece o queda vacía porque se desconectó un disco, no se borra nada en los demás equipos.
- **Seguridad.**
  - Solo los equipos que conocen el código pueden unirse. El código no viaja por la red (se usa SPAKE2) y se bloquea tras 5 intentos fallidos.
  - Las conexiones van cifradas (protocolo Noise).
  - Nadie puede leer ni escribir fuera de la carpeta compartida, y los archivos recibidos nunca se ejecutan.

## Red

- Todos los equipos deben estar en la **misma red local**. Funciona sin internet.
- Algunas wifi de hoteles o de invitados aíslan a los equipos entre sí; la app lo indica en pantalla.
- Si una carpeta no aparece al unirse, en «Unirme con código» hay una **Conexión manual** para escribir la dirección del otro equipo.
- **Fuera de casa:** con una VPN que llegue a la red de casa (WireGuard u otra), activa *Ajustes → Permitir conexiones por VPN* y el portátil seguirá sincronizando. Sin VPN, la **nube o NAS** hace llegar los cambios cuando el otro equipo vuelve a casa.
- Puertos: 47820 (TCP, la primera carpeta; cada una de las demás usa un puerto libre), 47821 (UDP, descubrimiento), 5353 (mDNS) y 47830 (TCP, recepción desde el móvil).

La recepción desde el móvil usa HTTP sin cifrar dentro de la red de casa, porque los navegadores no aceptan sin avisos un certificado propio. No la uses en wifis públicas. El enlace para enviar desde fuera de casa sí va por HTTPS, pero los archivos pasan por los servidores de Cloudflare.

Más detalles en [docs/RED.md](docs/RED.md) y [docs/ARQUITECTURA.md](docs/ARQUITECTURA.md).

## Compilar desde el código

Requisitos: [Rust](https://rustup.rs) estable y Node.js, que solo hace falta para la herramienta de empaquetado de Tauri.

```bash
cargo test --workspace
```

```bash
cargo run -p home-file-sync
```

Instaladores (macOS universal y, desde un Mac, también el de Windows):

```bash
scripts/compilar-macos.sh --publico
```

```bash
scripts/compilar-windows-desde-mac.sh
```

```bash
scripts/publicar-instaladores.sh
```

`scripts/capturas.sh` regenera las capturas de estos README, en los tres idiomas, con datos de ejemplo.

### Estructura

```
crates/sfs-core   motor: descubrimiento, emparejamiento, sincronización, hub, recepción web, órdenes de las IA (MCP), traducciones
crates/sfs-cli    versión sin interfaz (diagnóstico y pruebas)
app/src-tauri     aplicación de escritorio (Tauri 2)
app/ui            interfaz (HTML/CSS/JS sin dependencias)
docs/             arquitectura, red y capturas
```

## Licencia

MIT o Apache-2.0, a tu elección. Historial de cambios en [CHANGELOG.md](CHANGELOG.md).
