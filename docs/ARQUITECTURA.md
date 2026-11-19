# Arquitectura de Home File Sync

Este documento explica las decisiones de diseño y cómo funciona cada parte.
El orden de prioridades que guía todo es:

1. Integridad de los archivos
2. Seguridad
3. Fiabilidad entre Windows y macOS
4. Facilidad de uso
5. Rendimiento
6. Estética

## 1. Elección de tecnología

| Opción | A favor | En contra |
|---|---|---|
| **Rust + Tauri 2** (elegida) | Binario nativo pequeño. Usa la vista web del sistema (WebView2 en Windows, WKWebView en macOS), así que no incluye Chromium como Electron. Bandeja del sistema y diálogos nativos de serie. Rust aporta seguridad de memoria y concurrencia robusta. Hay crates en Rust puro para mDNS, Noise y SPAKE2, sin dependencias de sistema (no hace falta instalar Bonjour en Windows). | Compila más lento que Go. |
| Go + interfaz nativa (Wails/Fyne) | Compilación cruzada muy sencilla. | Wails v2 no tiene bandeja del sistema (v3 seguía en desarrollo). Fyne no tiene aspecto nativo. Go no estaba disponible en el entorno de desarrollo. |
| Syncthing integrado como motor | Motor muy probado. | Evitar que los borrados se propaguen depende de `ignoreDelete`, una opción que su propia documentación desaconseja y que se planteó eliminar en la 2.0. Si se desactiva o se borra su base de datos, se propagan todos los borrados acumulados. Habría que gestionar un proceso aparte de unos 25–30 MB con varios puertos, y depender de una API que cambió entre versiones mayores. |
| Electron | Interfaz web cómoda. | Unos 150 MB y mucho consumo de memoria. Se pidió evitarlo. |
| Swift + C#/WinUI | Interfaz 100 % nativa. | Dos aplicaciones distintas que mantener. |

El motor es una biblioteca independiente (`crates/sfs-core`) sin código de
interfaz. La aplicación (`app/`) y el binario sin interfaz (`crates/sfs-cli`)
son dos capas finas sobre ella. Así todo el comportamiento importante se puede
probar automáticamente sin abrir ventanas.

Del diseño de Syncthing se reutilizan ideas, no código: vectores de versión,
temporales con renombrado atómico, copias de conflicto deterministas y
transferencia por bloques con verificación por hash.

## 2. Visión general

```
┌────────────── Equipo A ──────────────┐          ┌──────── Equipo B ────────┐
│  Interfaz (HTML/JS)  ◄── get_state ──┐│          │                          │
│                                      ││          │                          │
│  Engine ── Scanner (hilo) ── Carpeta ││  Noise   │  Engine ── Carpeta       │
│     │  └─ Índice (JSON)              │├─────────►│     │                    │
│     ├─ Descubrimiento (mDNS+baliza)  ││  TCP     │     ├─ ...               │
│     ├─ Planificador de descargas     ││ 47820    │                          │
│     └─ Conexiones (una por equipo)   ││          │                          │
└──────────────────────────────────────┘          └──────────────────────────┘
```

| Módulo | Responsabilidad |
|---|---|
| `pathsafe` | Rutas del protocolo: validación, NFC, claves sin mayúsculas, nombres ignorados y reglas de nombres de Windows. |
| `vv` | Vectores de versión. |
| `index` | Índice local (versión de cada archivo + lo que hay en disco), persistido en JSON de forma atómica. |
| `scanner` | Detecta cambios en disco, espera a que un archivo deje de escribirse y calcula su hash (BLAKE3). |
| `decide` | Reglas puras de sincronización y de conflicto. |
| `transport` | Canal cifrado Noise sobre TCP. |
| `auth` | Identidad, prueba de grupo y emparejamiento SPAKE2. |
| `discovery` | mDNS/DNS-SD y baliza UDP. |
| `peer` | Conexiones: autenticación, emparejamiento y sesión. |
| `transfer` | Planificador, descargas por bloques, subidas y colocación atómica. |
| `netpath` | Orden de direcciones candidatas, conexión TCP escalonada, fases y errores comprensibles. Único punto de conexión saliente, para el modo automático y el manual. |
| `diag` | Registro de diagnóstico persistente: capa de `tracing`, memoria acotada y archivo rotativo. |
| `fsops` | Temporales, `rename` atómico, versiones anteriores y limpieza. |
| `engine` | Orquesta todo y genera el estado para la interfaz. |

## 2 bis. Varias carpetas compartidas (hub)

- Cada carpeta compartida es un grupo independiente, con su clave, sus miembros y su código, y la gestiona un `Engine` propio con su puerto TCP (47820 y siguientes libres).
- El `Hub` (`hub.rs`) coordina todo:
  - mantiene una **identidad única** del equipo (`hub.json`), compartida por todos sus motores;
  - ejecuta **un solo descubrimiento**, que anuncia una instancia mDNS (y una baliza) por carpeta, y reparte lo que ve entre los motores;
  - guarda los **alias** locales de otros equipos;
  - lista las invitaciones abiertas;
  - gestiona la recepción por navegador.
- Datos en disco: `hub.json` y `shares/<id>/` con los ajustes y el índice de cada carpeta.
- **Modo copia de seguridad** (por carpeta y equipo):
  - el motor anuncia un índice vacío, así que nadie le pide archivos ni recibe sus cambios;
  - ignora los borrados remotos;
  - sí aplica altas y ediciones, y la versión anterior va a `old/`.
- **Recepción por navegador** (`inbox.rs`):
  - servidor HTTP mínimo en el puerto 47830;
  - una sola sesión con un token aleatorio de 128 bits que caduca a los 15 minutos;
  - página web propia para elegir archivos;
  - subida con `Content-Length` a un temporal y colocación atómica sin sobrescribir;
  - nombres limpiados: sin rutas, ni caracteres no válidos, ni nombres reservados.

  Es HTTP sin cifrar dentro de la red local, porque los navegadores no aceptan sin avisos un certificado propio; está pensado para la red de casa.

## 3. Descubrimiento

- **mDNS/DNS-SD**: servicio `_homefilesync._tcp`, implementado en Rust puro con `mdns-sd`. Los registros TXT incluyen el id del equipo, su nombre, el id y nombre del grupo, si está mostrando un código y la versión del protocolo.
- **Baliza UDP**: cada 3 s se envía un JSON con lo mismo por broadcast al puerto 47821, tanto a `255.255.255.255` como a la dirección de broadcast de cada interfaz. Sirve de respaldo cuando mDNS falla, por ejemplo con conflictos con otros respondedores mDNS en Windows o con switches que filtran multicast.
- Lo anunciado **no es secreto ni sirve para autenticar**: solo indica a quién conectar.
- Si dos equipos del grupo se ven, conecta primero el de id menor. El otro conecta también si a los 6 s no hay conexión, por si un cortafuegos solo deja pasar en un sentido. Si llegan a abrirse dos conexiones, ambos lados conservan la iniciada por el id menor.
- **Elección de dirección** (`netpath`). Un equipo puede anunciar varias direcciones: Ethernet, wifi, VPN, adaptadores virtuales de Hyper-V/VirtualBox, IPv6.
  - Las candidatas se ordenan así:
    1. misma subred que una interfaz local;
    2. otras redes privadas;
    3. enlace local (169.254.x.x) al final.
  - A igualdad, se prefiere la dirección de origen real de la baliza, que tiene camino de vuelta probado, frente a la anunciada por mDNS.
  - Se conecta de forma escalonada: cada 350 ms se lanza la siguiente candidata, cada intento espera como máximo 4 s y gana la primera que responde.
  - Si TCP conecta pero falla el cifrado (otro servicio en ese puerto), se prueba la siguiente.
  - Un **rechazo** del otro equipo (código incorrecto, otro grupo) es definitivo: no se reintenta en otras direcciones para no gastar intentos del código.
- **Direcciones aprendidas.** Cada equipo recuerda por qué direcciones ha alcanzado a cada miembro del grupo y las guarda en `settings.json` (`peer_addrs`). Así reconecta tras un reinicio aunque el descubrimiento no funcione en la red.
  - Proceden de conexiones salientes que funcionaron, del origen de las conexiones entrantes y de lo que comunica cada equipo.
  - Las direcciones escritas a mano en la conexión manual no se guardan salvo que el usuario lo pida.
- **Miembros comunicados por el grupo.** Al conectar, cada equipo envía su puerto y sus direcciones, y después las de los demás miembros a los que está conectado. Un equipo que solo ve a uno de los miembros conecta directamente con los demás, sin conexión manual.
  - Esa información solo aporta *dónde intentarlo*: toda conexión exige la autenticación del grupo, así que una dirección errónea no da acceso a nada.
- **Compatibilidad hacia atrás.** Estos mensajes viajan como `Block` con el id reservado `EXT_CHANNEL` (`u32::MAX`). Las versiones 0.1.0 y 0.1.1 descartan en silencio los bloques que no reconocen, así que siguen funcionando con equipos nuevos; `scripts/prueba-compatibilidad.sh` lo verifica.
- **La conexión manual es solo de recuperación.** Cada unión manual que funciona deja en el registro por qué no lo consiguió la automática: equipo no descubierto, descubierto con otras direcciones, o sin código abierto. Así cada uso real señala qué mejorar.
- IPv4 e IPv6: se escucha en ambas y se aceptan direcciones IPv6 no de enlace local. Las de enlace local necesitan identificador de zona y se descartan.
- La aplicación avisa claramente en cuatro casos:
  - **macOS deniega el permiso «Red local»:** se detecta como `EHOSTUNREACH` hacia una dirección de la propia subred.
  - **Hay equipos detectados pero no se puede conectar con ellos:** cortafuegos o aislamiento de clientes.
  - **No se detecta ningún equipo del grupo a los 20 s:** la red no deja que los equipos se vean.
  - **No hay red local.**

## 4. Identidad, grupo y emparejamiento

- Cada equipo genera un par de claves X25519 la primera vez. Su **id** es `BLAKE3(clave pública)`, del que se toman 128 bits. Nadie puede suplantarlo sin la clave privada.
- Un **grupo** es una clave secreta aleatoria de 256 bits que comparten sus equipos.
- **Autenticación al conectar.** Tras el handshake Noise, cada lado envía un `MAC = BLAKE3-keyed(clave_grupo, "client"|"server" ‖ handshake_hash)`. El `handshake_hash` es único para cada sesión Noise, así que la prueba no se puede reutilizar ni reenviar con un intermediario. Un equipo sin la clave no recibe ni envía nada.
- **Emparejamiento.**
  1. En un equipo del grupo, «Añadir equipo» genera un código de 8 cifras válido 15 minutos.
  2. El equipo nuevo y ese equipo ejecutan **SPAKE2** (Ed25519) con el código como contraseña. El código nunca viaja por la red y no se puede atacar sin conexión: cada conexión permite probar un solo código.
  3. Ambos confirman la clave obtenida con un MAC atado al `handshake_hash`.
  4. Solo después el equipo del grupo envía la clave del grupo por el canal cifrado.
  5. Tras 5 intentos fallidos el código se bloquea. La probabilidad de acertar a ciegas es de 5 entre 10⁸.
- Los ajustes (clave privada, clave de grupo) se guardan en `settings.json` dentro del directorio de datos de la aplicación, con permisos `0600` en macOS y en el perfil del usuario en Windows.
- «Salir del grupo» borra la clave del grupo en ese equipo. Para **expulsar** un equipo hay que crear un grupo nuevo y emparejar de nuevo a los demás; es una limitación conocida de la v1.

## 5. Cifrado

`Noise_XX_25519_ChaChaPoly_BLAKE2s` (biblioteca `snow`): cifrado autenticado
con secreto hacia adelante. Los mensajes de aplicación de hasta 4 MiB se trocean
en mensajes Noise de 64 KiB como máximo.

## 6. Índice y versiones

Cada archivo tiene una entrada con estos campos: ruta (NFC, `/`), tamaño,
`mtime`, hash BLAKE3, **vector de versión**, marca de borrado y el nombre del
último equipo que lo modificó.

- Un cambio local incrementa el contador del propio equipo.
- Al comparar una versión remota con la local:

| Situación | Acción |
|---|---|
| La remota es un borrado posterior a la versión local | Eliminar: el archivo se mueve a `.homefilesync/papelera/<fecha>/` (14 días). |
| La remota es un borrado y aquí no existe | Registrar el borrado, para no recibir el archivo de un equipo con una versión antigua. |
| La remota es un borrado concurrente con una edición local | Nada: gana la edición, que después llega al equipo que borró. |
| No existe aquí | Descargar. |
| Mismo hash | Fusionar vectores. No se transfiere nada. |
| Remota posterior | Descargar. La versión local se mueve a la carpeta visible `old/` (misma subruta, fecha de esa versión en el nombre), que se sincroniza: así todos los equipos, también el que editó, tienen la versión anterior. Dentro de `old/` no se generan más versiones (la anterior va a la copia oculta `.homefilesync/versiones`). |
| Local posterior | Nada: el otro equipo la pedirá. |
| Independientes (concurrentes) | **Conflicto**. |
| Borrada aquí y la remota es la versión anterior al borrado | Nada. |
| Borrada aquí y la remota es posterior o una edición concurrente | Descargar. |

- **Conflicto.**
  - Gana la versión con `mtime` mayor; si empatan, el hash mayor. Es determinista, así que todos los equipos eligen la misma.
  - El equipo que tiene la versión perdedora la mueve a `old/Nombre (conflicto - <equipo> - <hash6>).ext` y después coloca la ganadora.
  - La copia de conflicto se sincroniza como un archivo más. Su nombre también es determinista, así que no se multiplican copias.
  - **Nunca se sobrescribe una versión en silencio.**
- Las claves de archivo no distinguen mayúsculas y están en NFC. APFS y NTFS no distinguen mayúsculas por defecto, y macOS puede entregar nombres en NFD.
- **Nombres no válidos en Windows** (`:`, `?`, `CON`…):
  - El equipo Windows no los descarga y lo muestra como error del archivo.
  - No se renombran, para no crear versiones divergentes.

## 7. Detección de cambios y archivos en escritura

- Vigilancia del sistema de archivos (`notify`: FSEvents en macOS y ReadDirectoryChangesW en Windows), más un escaneo completo cada 60 s por si se pierde algún evento.
- Un archivo nuevo o modificado solo se indexa cuando:
  - su tamaño y fecha no cambian entre dos observaciones separadas al menos 2,5 s;
  - en **Windows**, se puede abrir sin que otro proceso lo tenga abierto para escritura (copias del Explorador, guardados a medias). Excepción: los documentos abiertos en PowerPoint, Word o Excel (detectados por su archivo de bloqueo `~$…`) se leen compartidos. Office guarda escribiendo un temporal y renombrándolo, así que lo guardado se replica sin esperar a cerrar la presentación;
  - en **macOS**, no tiene la fecha especial que el Finder pone durante una copia;
  - no cambia mientras se calcula su hash.
- Se ignoran:
  - archivos de sistema y temporales: `.DS_Store`, `Thumbs.db`, `~$*` de Office, `.~lock.*`, `._*`, `*.tmp`, `*.part`, `*.crdownload`…;
  - la carpeta interna `.homefilesync`;
  - los enlaces simbólicos.
- Un borrado local es una versión más: se incrementa el contador del equipo y se anuncia a los demás.
- Si la carpeta deja de estar disponible (disco desconectado), **no** se interpreta como borrado.
- Si desaparece la carpeta interna (disco desmontado con el punto de montaje vacío, carpeta sustituida), la carpeta se trata como nueva: no se anuncia ningún borrado y se vuelve a recibir todo.
- Renombrar o mover es un borrado más un archivo nuevo. Si el contenido acaba de ir a la papelera, se recupera de ahí con un `rename`, sin transferirlo.
- Si en disco hay un cambio que el índice aún no refleja (archivo copiándose o recién editado), no se descarga nada para esa ruta hasta que el escáner lo procese.

## 8. Transferencias

- **Pull**: cada equipo calcula qué le falta y lo pide. Nunca se envía nada que el receptor no necesite.
- **Fuentes múltiples**: un archivo puede pedirse a cualquier equipo que tenga *exactamente* ese hash. Se elige primero el equipo con menos subidas en curso.
- **Reparto de carga**: cada equipo atiende como mucho 4 descargas a la vez y contesta «ocupado» a las demás. Con 20 equipos, los primeros en recibir un archivo pasan a servirlo a los siguientes (distribución en árbol), en lugar de saturar al equipo de origen.
- **Por bloques**:
  - Bloques de 512 KiB con 8 peticiones en vuelo para llenar el enlace.
  - Las peticiones se atienden en orden y el receptor escribe secuencialmente.
  - Los mensajes de control (índice, estado, ping) tienen prioridad sobre los de datos.
- **Temporales y reanudación**:
  - Las descargas van a `.homefilesync/tmp/<hash>.sfspart`, en el mismo volumen que el destino.
  - Si se cae la conexión, el temporal se conserva y la descarga continúa desde ese punto, con el mismo equipo o con otro.
- **Integridad**: al terminar se verifica el BLAKE3 completo. Si no coincide, se descarta y se descarga de nuevo.
- **Colocación** (bajo el bloqueo del índice):
  1. Se comprueba que el archivo local no ha cambiado desde que se decidió la descarga. Si cambió, se reevalúa; nunca se pisa una edición reciente.
  2. Se mueve la versión anterior a `old/` (visible y sincronizada).
  3. Se renombra el temporal a su destino (`rename`, atómico en APFS, HFS+ y NTFS).
  4. Se restaura la fecha original y se dejan permisos `0644`: nunca ejecutable.
- **Archivo ya presente con otro nombre**: si el contenido ya existe localmente, se copia en local sin usar la red.
- **Archivo abierto en Windows** (p. ej. en PowerPoint): no se puede sustituir. Se muestra «se actualizará cuando se cierre» y se reintenta con espera creciente.
- **Espacio en disco**: se comprueba antes de cada descarga.
- **Keep-alive**: ping cada 10 s; una conexión sin tráfico durante 35 s se da por perdida (cable desconectado) y se reconecta automáticamente.

## 8 bis. Relevo por un servicio externo (nube o NAS)

Módulo `relay`. Sirve para que los cambios lleguen a un equipo que no está conectado cuando se producen: se dejan en un servicio externo y ese equipo los recoge al volver.

**Dónde** (módulo `store`, interfaz `Store`: listar, leer, escribir, subir, mover, borrar). El relevo no sabe cuál usa:

| Destino | Cómo |
|---|---|
| Carpeta (`Folder`) | Una carpeta del equipo: la de la app de Google Drive, Dropbox, OneDrive, Proton Drive, iCloud… o un disco. En cada equipo puede estar en una ruta distinta. |
| NAS (`Smb`) | IP, carpeta compartida, usuario y contraseña. Se conecta con la función del sistema (NetFS en macOS, `WNetAddConnection2W` en Windows; la contraseña nunca va por línea de comandos) y después es una carpeta más. Si se desconecta, se vuelve a conectar en la siguiente vuelta. |
| WebDAV (`WebDav`) | PROPFIND/GET/PUT/MOVE/DELETE/MKCOL con autenticación básica. Se exige `https://` salvo en direcciones de la red local. Las escrituras van a un temporal y se mueven. |
| Google Drive (`GoogleDrive`) | OAuth con PKCE y permiso `drive.file` (no sensible): la app solo ve lo que crea, en «Mi unidad/Home File Sync». Todos los equipos con la misma cuenta. Subida reanudable por partes de 8 MiB. |
| Dropbox (`Dropbox`) | OAuth con PKCE y acceso completo, para poder usar una carpeta compartida entre cuentas. Subida por sesión en partes de 32 MiB a partir de 140 MiB. |

- Las contraseñas y los tokens de renovación se guardan en el almacén del sistema (`secrets`: Llavero de macOS, Administrador de credenciales de Windows), nunca en `settings.json` ni en el registro.
- Inicio de sesión: la app abre el navegador; el servicio redirige a `http://127.0.0.1:47835/`, donde la app comprueba el `state`, recoge el código y lo cambia por el token. Los identificadores de la app en Google y Dropbox se fijan al compilar (ver `docs/REGISTRO-SERVICIOS.md`); sin ellos esas opciones no aparecen.
- TLS del sistema (Security.framework en macOS, SChannel en Windows).

```
<servicio>/Home File Sync/
  LEEME - README.txt
  hfs-<id opaco>/          una por carpeta compartida
    config.hfs             opciones comunes (qué hacer con lo entregado)
    idx/<equipo>.hfs       índice publicado por cada equipo
    blobs/<id>.hfs         contenido, uno por hash
    old/<fecha>/           lo ya entregado, si se eligió conservarlo
```

- **Cifrado.** Todo va cifrado con una clave derivada de la del grupo (`BLAKE3 derive_key`): XChaCha20-Poly1305 en bloques de 1 MiB (construcción STREAM, con contador y marca de último bloque en el nonce, así que se detecta un archivo truncado o reordenado). Los nombres de archivo son opacos (`BLAKE3` con clave): el servicio no ve nombres, rutas, hashes ni ids de grupo. Un archivo de otro grupo o alterado no se descifra.
- **Sin conflictos en el servicio.** Cada equipo escribe solo su propio índice; los blobs son inmutables y se nombran por su contenido (si dos equipos suben el mismo, da igual). Todo se escribe en un temporal y se renombra.
- **Una fuente más.** El planificador de descargas trata los índices del relevo como los de un equipo conectado (salvo los de equipos en copia de seguridad, que solo reciben) y añade el relevo como fuente cuando el blob está allí. Por eso se aplican las mismas reglas de versiones, borrados, conflictos y `old/`, y el contenido se verifica con su hash BLAKE3 antes de colocarlo. Los equipos conectados tienen preferencia; el relevo se usa si ninguno lo sirve.
- **Cuándo se sube.** Cada 20 s (o tras un cambio) cada equipo calcula, para cada versión vigente, qué equipos del relevo no la tienen según su índice publicado o el que envía por la red si está conectado. Solo sube lo que le falta a algún equipo **desconectado** y está en su disco. Antes de subir comprueba que el archivo no ha cambiado y que lo cifrado tiene el hash indexado.
- **Destinatarios.** Solo cuentan los equipos que también usan el relevo (han publicado su índice en los últimos 30 días): un equipo que no lo usa no puede leerlo, así que no impide la limpieza. Cada equipo republica su índice al menos una vez al día.
- **Limpieza.** Un blob se elimina (o se mueve a `old/<fecha>/`, según `config.hfs`, común a todos) cuando todos los destinatarios tienen esa versión o una posterior. Un blob que no corresponde a ningún índice conocido se espera 24 h, por si su índice aún no ha llegado desde la nube. Cualquier equipo puede limpiar; si dos lo hacen a la vez no pasa nada.
- **Nube a medias.** Si un blob o un índice todavía no ha terminado de bajar de la nube, no se descifra y se reintenta más tarde.
- Un equipo que publica en el relevo tiene la clave del grupo, así que se añade a la lista de miembros.

## 8 ter. Enlace público para recibir desde fuera de casa

Módulo `tunnel`. En la ventana de recepción, *Permitir enviar desde fuera de casa* abre un «Quick Tunnel» de Cloudflare (`cloudflared tunnel --url http://127.0.0.1:47830`): no necesita cuenta ni abrir puertos, y da una dirección `https://….trycloudflare.com` que se muestra en el QR.

- `cloudflared` no va en el instalador. Se descarga la primera vez de la publicación oficial en GitHub (versión fijada) y solo se usa si su SHA-256 coincide con el fijado en el código: el del archivo descargado y, en macOS, también el del ejecutable que contiene (el que publica Cloudflare). Se guarda en la carpeta de datos de la app (`tools/`).
- Se ejecuta con una configuración vacía, para que una configuración propia de cloudflared en el equipo no interfiera.
- El enlace solo existe mientras la ventana de recepción está abierta y como mucho hasta que caduca la sesión (15 minutos). Sin el token de la sesión no se acepta nada.
- La página sube los archivos por partes de 16 MB (`?id=&offset=&total=`), porque Cloudflare limita el tamaño de cada petición. Si se corta la conexión, la siguiente parte continúa desde lo recibido; una parte desordenada recibe `409` con la posición correcta. Las partes se guardan en temporales ligados al token y se borran al cerrar la sesión.
- Entre el móvil y Cloudflare va por HTTPS; Cloudflare puede ver el contenido, como cualquier servicio en la nube, y la interfaz lo indica.

## 8 quater. Órdenes de las IA entre equipos

Objetivo: que una IA que programa en un equipo pueda compilar o probar en otro
(Mac ↔ Windows) sin copiar nada a mano. Código en `crates/sfs-core/src/agent/`.

```
App de IA ──stdio MCP──► home-file-sync --mcp ──TCP 127.0.0.1 + token──► app (Hub)
                                                                         │ extensión ExecReq
                                                       conexión Noise de la carpeta compartida
                                                                         ▼
                                                    equipo que ejecuta: permiso → turno →
                                                    espera de sincronización → proceso → salida
```

- **Servidor MCP** (`agent/mcp.rs`): JSON-RPC 2.0 por stdio, una línea por
  mensaje. Es el propio binario de la app con `--mcp`, sin ventana ni registro
  en la salida estándar. Cada llamada a una herramienta abre una conexión con
  la app, así que la app puede abrirse después que la IA.
- **Canal local** (`agent/control.rs`): la app escucha en `127.0.0.1` y deja en
  su directorio de datos `agent.json` (permisos de usuario) con el puerto y un
  token de 32 bytes. Sin el token no se atiende.
- **Entre equipos**: extensiones nuevas al final de `Ext` (`Agent`, `ExecReq`,
  `ExecEvent`, `ExecCancel`), que los equipos anteriores no decodifican y
  descartan. Solo las envían miembros autenticados del grupo de la carpeta. Los
  eventos se mandan por la conexión vigente del equipo; si se pierde del todo,
  la orden se detiene y el que la pidió la marca como «sin conexión».
- **Permiso**: lo decide solo el equipo que ejecuta, con sus ajustes locales
  (`Settings.agent`), que nunca se reciben de otro equipo. Las órdenes de la
  lista se comparan por prefijo de argumentos (`cargo test` permite
  `cargo test --release`, no `cargo run`). Las demás esperan aprobación
  5 minutos. Como mucho 2 a la vez por carpeta.
- **Sin shell** (`agent/exec.rs`): programa y argumentos separados. El programa
  es un nombre que se busca en el `PATH` (en macOS, el de la shell de inicio de
  sesión del usuario, porque las apps de escritorio arrancan con uno mínimo) o
  una ruta relativa que debe quedar dentro de la carpeta. En macOS/Linux el
  proceso tiene su propio grupo y detenerlo envía `SIGTERM` y después
  `SIGKILL` al grupo. En Windows se usa `taskkill /T` y `CREATE_NO_WINDOW`.
- **Archivos al día**: el que pide fuerza un escaneo y espera a que termine
  (también a que se estabilicen los archivos recién guardados), y después
  envía la petición por la misma conexión que los cambios del índice, en orden.
  El que ejecuta espera hasta que, para cada entrada del índice del otro, la
  decisión sea «nada que hacer» (`transfer::behind`), como mucho 3 minutos. Al
  terminar, el que ejecuta escanea lo generado antes de avisar. El que pidió
  espera a recibirlo (hasta 60 s) antes de dar el resultado.
- **Salida**: se envía en trozos cada 200 ms (máx. 8 MiB por ejecución). El que
  pide guarda el último MiB; la IA recibe el final, sin códigos de color.

### Proyectos de código (`ignore_rules.rs`)

Sincronizar `.git` entre dos equipos que trabajan a la vez corrompe el
repositorio, y las carpetas de compilación dependen del sistema. Con la opción
de proyecto de código se aplican los `.gitignore` de cada subcarpeta (con el
crate `ignore`, la misma semántica que git) y `.homefilesyncignore`. Además,
nunca se sincronizan `.git`, `.hg`, `.svn` ni `.jj`. Las reglas valen en los
dos sentidos: el escáner no indexa lo excluido y el planificador no descarga
lo excluido aunque otro equipo lo anuncie. Una entrada que pasa a excluirse se
retira del índice **sin** marca de borrado, así que nunca se borra en otro
equipo.

## 9. Seguridad de rutas

Toda ruta recibida por la red pasa por `validate_wire_path`. Se rechazan:
- rutas vacías o de más de 4 KiB;
- rutas absolutas o con unidad (`C:`);
- componentes `..`, `.` o vacíos;
- barras invertidas y caracteres de control;
- la carpeta interna y los nombres ignorados.

Además:
- Un equipo solo sirve archivos que están en su índice con el hash pedido: nunca una ruta arbitraria.
- Antes de escribir se comprueba que ningún componente de la ruta es un enlace simbólico.

## 10. Rendimiento y límites

- Interfaz desacoplada: consulta un resumen cada 700 ms y nunca espera al disco ni a la red.
- Hash y E/S de disco en hilos de bloqueo; red asíncrona (tokio).
- 3 descargas y 4 subidas simultáneas por equipo.
- Índice completo al conectar (en páginas de 1000 entradas) y después solo cambios incrementales.
- **Límites conocidos (v1)**:
  - Índice en JSON: adecuado hasta decenas de miles de archivos.
  - Solo IPv4.
  - Las carpetas vacías no se sincronizan.
  - Al conectar se envía el índice completo, no un delta.

## 11. Etapas

1. ✅ Núcleo: rutas, versiones, índice, escáner, decisiones. Pruebas unitarias.
2. ✅ Red: Noise, autenticación de grupo, emparejamiento SPAKE2, descubrimiento.
3. ✅ Transferencias: bloques, reanudación, verificación, colocación atómica, conflictos. Pruebas de integración multi-equipo.
4. ✅ Aplicación Tauri: interfaz, bandeja o barra de menús, instancia única.
5. ✅ Empaquetado: `.app`/`.dmg` universal y NSIS con reglas de cortafuegos (CI).
6. Pendiente (v1.x):
   - firma y notarización;
   - icono de bandeja que cambie con el estado;
   - índice en SQLite para carpetas enormes;
   - expulsar un equipo concreto del grupo.
