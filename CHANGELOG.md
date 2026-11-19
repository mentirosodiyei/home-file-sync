# Historial de versiones

## Sin publicar
- **IA en varios equipos.** Una IA (Claude Code, Claude Desktop, Codex, Cursor, Gemini CLI, LM Studio…) puede pedir a otro equipo de la carpeta que ejecute una orden —las pruebas, la compilación— y recibir la salida y el código de salida.
  - Servidor MCP incluido en la app: `home-file-sync --mcp`. Herramientas `list_machines`, `run_command`, `get_run`, `cancel_run` y `sync_now`. Habla con la app abierta por un canal local con token.
  - Decide el equipo que ejecuta: desactivado por defecto. Se activa por carpeta, con una lista de órdenes que se ejecutan sin preguntar; las demás se aprueban (una vez o siempre) o se rechazan en su pantalla, que se abre sola al llegar una petición.
  - Sin intérprete de órdenes, dentro de la carpeta y con el `PATH` de la shell del usuario (Homebrew, cargo, nvm…). Detener o cancelar termina también los procesos hijos.
  - Antes de ejecutar espera a tener los cambios del que lo pide; al terminar, lo generado vuelve antes de dar el resultado.
  - Viaja por la conexión cifrada de la carpeta; los equipos con versiones anteriores lo ignoran.
- **Carpetas de proyecto de código:** respetan los `.gitignore` (de cualquier subcarpeta) y `.homefilesyncignore`, y no sincronizan `.git`, `.hg`, `.svn` ni `.jj`. Lo que pasa a excluirse deja de sincronizarse sin borrarse en ningún equipo.

## 0.4.0
- **Nube o NAS de relevo**, para cuando los equipos no coinciden conectados. Cinco formas de conectar, desde la propia app:
  - carpeta de la app del servicio (Google Drive, Dropbox, OneDrive, Proton Drive, iCloud, pCloud…);
  - Google Drive iniciando sesión (permiso mínimo; misma cuenta en todos los equipos);
  - Dropbox iniciando sesión (vale con cuentas distintas compartiendo la carpeta desde Dropbox);
  - NAS por SMB con dirección, usuario y contraseña, conectado por el propio sistema;
  - servidor WebDAV (Nextcloud, ownCloud, kDrive…).
  - Contraseñas y sesiones en el llavero del sistema.
  - Si todos los equipos están conectados no se usa. Si falta alguno, lo que le falta se deja allí y lo recoge al volver, también los borrados.
  - Todo va cifrado con la clave del grupo, incluidos los nombres: el servicio no ve qué hay.
  - Cuando lo tienen todos los equipos que usan el relevo, se borra del servicio. Opción común: moverlo a `old/` en el servicio, para vaciarlo a mano.
  - Los equipos en copia de seguridad reciben por el relevo, pero lo suyo no se sube.
- **Recibir desde fuera de casa con el QR:** opción *Permitir enviar desde fuera de casa* en la ventana de recepción. Abre un enlace `https://` temporal con Cloudflare (sin cuenta) mientras la ventana está abierta.
  - `cloudflared` se descarga la primera vez de la publicación oficial y solo se usa si su SHA-256 coincide con el fijado.
- La página del móvil sube por partes y continúa donde se quedó si se corta la conexión.

## 0.3.1
- **En tres idiomas: català, castellano e inglés.** La app usa el idioma del sistema (catalán, castellano, o inglés para cualquier otro idioma).
  - Traducidos la ventana, el menú de la bandeja o de la barra de menús, los avisos y errores, la página que se abre en el móvil (según el idioma del teléfono) y el aviso de macOS sobre la red local.
  - El instalador de Windows también está en catalán, castellano e inglés.
  - El registro de diagnóstico se mantiene en castellano.
- README en los tres idiomas, con capturas de cada uno.

## 0.3.0
- **Fuera de casa por VPN (WireGuard):** nuevo ajuste «Permitir conexiones por VPN».
  - Desactivado por defecto: el equipo solo atiende a la red local.
  - Activado: acepta conexiones que llegan por un túnel y comunica sus direcciones de VPN a los demás. Si la VPN da acceso a la red de casa, el equipo reconecta solo fuera de casa.
  - Ajustes muestra las direcciones del equipo y el puerto de cada carpeta, por si hace falta una conexión manual.
  - Los túneles se detectan en Windows por el tipo de adaptador y en macOS porque son punto a punto.
- Se quita «Recibir aquí» de cada carpeta. La recepción desde el móvil sigue en el botón superior, eligiendo el destino.
- Autoría en Ajustes.

## 0.2.0 — Home File Sync
La app pasa a ser de uso doméstico y se llama **Home File Sync**. No es compatible con Show File Sync: usa otro servicio de red, otros puertos y otra carpeta interna (`.homefilesync`).
- **Varias carpetas compartidas por equipo, cada una con sus propios miembros y su código** (por ejemplo, una para la pareja y otra para el padre y el hijo).
- **Modo por carpeta y equipo:**
  - *sincronizada*;
  - *copia de seguridad*: solo recibe y conserva lo que se borra en el original.
- **Nombres locales** para los demás equipos: solo los ve quien los pone.
- **Recibir desde el móvil o cualquier navegador** escaneando un código QR, sin instalar nada.
- Las carpetas que esperan equipos aparecen solas en «Unirme con código».

## 0.1.4
- **Carpeta `old` visible:** cuando un archivo cambia, la versión nueva queda en su sitio y la anterior pasa a `old/` (misma subcarpeta, fecha en el nombre).
  - `old` se sincroniza, así que todos los equipos tienen las versiones anteriores, también el que hizo el cambio.
  - Las copias de conflicto también van a `old/`.

## 0.1.3
- **Las carpetas funcionan como una sola**: lo que se añade, edita, renombra o borra en un equipo se replica en todos.
  - Lo borrado en otro equipo no se destruye: se mueve a `.showfilesync/papelera` y se conserva 14 días.
  - Si un equipo edita mientras otro borra el mismo archivo, gana la edición.
  - Renombrar o mover no vuelve a transferir el archivo.
- **Windows:** lo guardado en PowerPoint, Word o Excel se replica sin tener que cerrar el documento.
- **Seguridad ante discos desconectados:** si la carpeta queda vacía y sin su carpeta interna, no se borra nada en los demás equipos y se vuelve a llenar.
- Corregido: al arrancar, a veces se descargaba un archivo que ya estaba en disco.

Importante: actualiza todos los equipos. Las versiones anteriores no envían ni aplican borrados.

## 0.1.2
- **Reconexión sin descubrimiento:** cada equipo recuerda por dónde ha alcanzado a los demás y reconecta solo tras un reinicio, aunque la red bloquee mDNS y broadcast.
- **Los equipos se comunican las direcciones de los demás miembros:** basta con ver a uno para acabar conectado con todos. Es compatible con las versiones anteriores.
- Cada unión manual deja en el registro el motivo por el que no funcionó la automática.

## 0.1.1
- **Corregido el error «No route to host (os error 65)» al unir un Mac a un grupo de Windows.** La app de macOS va firmada como paquete con un identificador estable, para que el permiso «Red local» funcione.
- La app elige mejor la dirección por la que conectar cuando un equipo tiene varias (Ethernet, wifi, VPN, adaptadores virtuales), y admite IPv6.
- Un código incorrecto ya no gasta varios intentos.
- **Registro de diagnóstico** en *Ajustes → Avanzado → Diagnóstico*: se puede filtrar, copiar y exportar.
- **Conexión manual** (IP o nombre) como herramienta de recuperación.

## 0.1.0
- Primera versión funcional:
  - descubrimiento automático;
  - emparejamiento con código;
  - sincronización cifrada;
  - conflictos que conservan ambas copias;
  - reanudación de transferencias;
  - versiones anteriores guardadas.
