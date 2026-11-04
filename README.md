# Show File Sync

**Sincroniza una carpeta entre varios ordenadores Windows y macOS de la misma red local.**
Sin servidores, sin cuentas, sin direcciones IP y sin carpetas compartidas.

Está pensado para técnicos audiovisuales en eventos, congresos y espectáculos.
Dejas una presentación en la carpeta de un portátil y aparece en todos los demás.

```
Instalar → Abrir → Elegir carpeta → Crear grupo o escribir el código → Listo
```

## Cómo se usa

1. Instala Show File Sync en todos los ordenadores y ábrelo.
2. En cada uno, **elige la carpeta** que quieres sincronizar. Cada equipo usa su propia carpeta local.
3. En el primer equipo pulsa **Crear grupo nuevo**. Aparece un código de 8 cifras, por ejemplo `4821 0937`.
4. En los demás, escribe ese código en **Unirse a un grupo**.
5. Arrastra un PowerPoint, Keynote, PDF o vídeo a cualquiera de las carpetas. Aparecerá en las demás.

Para añadir más adelante otro portátil, pulsa **Añadir equipo** en cualquier equipo del grupo y escribe el código nuevo. El portátil recibe todo lo que le falte.

Al cerrar la ventana, Show File Sync sigue funcionando en la **bandeja del sistema** (Windows) o en la **barra de menús** (macOS). Para detenerlo, elige *Salir* en ese menú.

## Qué garantiza

- **No se pierden archivos.**
  - Si dos equipos tienen versiones distintas del mismo archivo, se conservan ambas: la otra queda como `Nombre (conflicto - Equipo - abc123).pptx`.
  - Cuando llega una versión nueva, la anterior se guarda 14 días en `.showfilesync/versiones`.
- **Las carpetas funcionan como una sola.** Lo que se añade, edita, renombra o borra en un equipo se replica en todos.
  - Un archivo borrado no se destruye en los demás equipos: se mueve a su carpeta oculta `.showfilesync/papelera` y se conserva 14 días, por si se borró por error.
  - Si un equipo edita un archivo mientras otro lo borra, gana la edición.
  - Si un disco se desconecta, o la carpeta se sustituye por otra vacía, no se borra nada en los demás: la carpeta se vuelve a llenar.
- **Nada a medias.**
  - No se copian archivos que todavía se están escribiendo.
  - Las descargas van a un temporal y solo se colocan, con un renombrado atómico, cuando el hash BLAKE3 coincide.
- **Reanudación.** Si se desconecta un cable a mitad de un vídeo de 4 GB, la descarga continúa donde se quedó al volver la red.
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

Algunas redes wifi de hoteles y recintos tienen **aislamiento de clientes**, que impide que los equipos se vean entre sí. Show File Sync lo indica en pantalla; en ese caso usa una red de producción propia o un switch. Más detalles en [docs/RED.md](docs/RED.md).

## Instalación

| Sistema | Archivo |
|---|---|
| macOS 11 Big Sur o posterior (Intel y Apple Silicon) | `Show File Sync_x.y.z_universal.dmg` |
| Windows 10 / 11 (64 bits) | `Show File Sync_x.y.z_x64-setup.exe` |

- **macOS**:
  - La primera vez, macOS pide permiso para acceder a la **red local**: hay que aceptarlo.
  - Mientras la app no esté firmada con un certificado de Apple, ábrela con *clic derecho → Abrir*.
- **Windows**:
  - El instalador añade automáticamente las reglas del Firewall de Windows, incluso para redes «Públicas», que es como suelen clasificarse las redes de eventos.
  - Necesita WebView2, que viene incluido en Windows 10/11 actualizados; el instalador lo descarga si falta.

## Compilar desde el código

Requisitos: [Rust](https://rustup.rs) estable y Node.js (solo para la herramienta de empaquetado de Tauri).

```bash
cargo test --workspace
```

```bash
cargo run -p show-file-sync
```

```bash
scripts/compilar-macos.sh
```

Este script compila la versión universal y la firma con la identidad de Apple del llavero, si la hay. La firma es necesaria para que el permiso «Red local» de macOS funcione de forma fiable.

En Windows, `npx @tauri-apps/cli@2 build` genera el instalador NSIS. La CI (`.github/workflows/build.yml`) compila y prueba en Windows y macOS en cada cambio.

### Sin interfaz gráfica

`showfilesync-cli` ejecuta el mismo motor sin ventana. Sirve para diagnóstico o para probar varias instancias en un mismo ordenador:

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
