# Arquitectura de Show File Sync

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
│     ├─ Planificador de descargas     ││ 47800    │                          │
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

## 3. Descubrimiento

- **mDNS/DNS-SD**: servicio `_showfilesync._tcp`, implementado en Rust puro con `mdns-sd`. Los registros TXT incluyen el id del equipo, su nombre, el id y nombre del grupo, si está mostrando un código y la versión del protocolo.
- **Baliza UDP**: cada 3 s se envía un JSON con lo mismo por broadcast al puerto 47801, tanto a `255.255.255.255` como a la dirección de broadcast de cada interfaz. Sirve de respaldo cuando mDNS falla, por ejemplo con conflictos con otros respondedores mDNS en Windows o con switches que filtran multicast.
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
| La remota es un borrado posterior a la versión local | Eliminar: el archivo se mueve a `.showfilesync/papelera/<fecha>/` (14 días). |
| La remota es un borrado y aquí no existe | Registrar el borrado, para no recibir el archivo de un equipo con una versión antigua. |
| La remota es un borrado concurrente con una edición local | Nada: gana la edición, que después llega al equipo que borró. |
| No existe aquí | Descargar. |
| Mismo hash | Fusionar vectores. No se transfiere nada. |
| Remota posterior | Descargar. La versión local se mueve a la carpeta visible `old/` (misma subruta, fecha de esa versión en el nombre), que se sincroniza: así todos los equipos, también el que editó, tienen la versión anterior. Dentro de `old/` no se generan más versiones (la anterior va a la copia oculta `.showfilesync/versiones`). |
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
  - la carpeta interna `.showfilesync`;
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
  - Las descargas van a `.showfilesync/tmp/<hash>.sfspart`, en el mismo volumen que el destino.
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
