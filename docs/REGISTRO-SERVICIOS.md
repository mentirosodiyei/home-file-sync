# Registrar Home File Sync en Google y Dropbox

El inicio de sesión con Google Drive y Dropbox necesita que la app esté
registrada en cada servicio. Se hace una sola vez, desde una cuenta del
proyecto (no una personal: su correo aparece en las pantallas de inicio de
sesión). Sin este registro la app funciona igual; esas dos opciones salen
como «no disponibles en esta versión».

Lo que se obtiene son identificadores **públicos** de la app (van dentro del
programa). No dan acceso a ninguna cuenta: cada usuario inicia sesión con la
suya y acepta los permisos.

## 0. Cuenta del proyecto

Crea un correo solo para Home File Sync (por ejemplo, en Proton o Gmail) y
úsalo para todo lo de abajo. Ese correo será el de contacto que ve la gente.

## 1. Google Drive

1. Entra en <https://console.cloud.google.com> con la cuenta del proyecto y crea un proyecto llamado **Home File Sync**.
2. **APIs y servicios → Biblioteca**: busca **Google Drive API** y pulsa **Habilitar**.
3. **Google Auth Platform** (pantalla de consentimiento):
   - *Información de la marca*: nombre **Home File Sync**, correo de asistencia = el del proyecto, página principal `https://github.com/mentirosodiyei/home-file-sync`, política de privacidad `https://github.com/mentirosodiyei/home-file-sync/blob/main/docs/PRIVACIDAD.md`, correo de contacto del desarrollador = el del proyecto.
   - *Público*: **Externo**.
   - *Acceso a datos → Añadir permisos*: `.../auth/drive.file`, `openid` y `.../auth/userinfo.email`. Los tres son permisos **no sensibles**: no hace falta la auditoría de seguridad de Google.
   - *Público → Estado de publicación*: pulsa **Publicar la app** (pasar a *En producción*). **Importante:** en modo *Prueba* solo pueden entrar los usuarios de prueba y la sesión caduca a los 7 días.
4. **Clientes → Crear cliente**: tipo **App de escritorio**, nombre `Home File Sync`. Copia el **ID de cliente** y el **secreto del cliente**.
   - En una app de escritorio ese «secreto» no es secreto (Google lo indica así); la seguridad la dan PKCE y la redirección a `127.0.0.1`.
5. Opcional: **verificación de la marca**, para que Google muestre el nombre y el logotipo. Pide un dominio propio verificado; sin ella el inicio de sesión funciona igual.

Limitación del permiso mínimo (`drive.file`): la app solo ve lo que crea ella, así que **todos los equipos de una carpeta deben iniciar sesión con la misma cuenta de Google**. Con cuentas distintas: app de Google Drive y una carpeta compartida desde Drive (opción «Carpeta de la app del servicio»).

## 2. Dropbox

1. Entra en <https://www.dropbox.com/developers/apps> con la cuenta del proyecto y pulsa **Create app**.
2. **Scoped access** → **Full Dropbox** (necesario para usar carpetas compartidas entre cuentas) → nombre **Home File Sync** (si está cogido, uno parecido; no se puede cambiar después de aprobarla).
3. Pestaña **Settings**:
   - *Redirect URIs*: añade exactamente `http://127.0.0.1:47835/`
   - *Allow public clients (Implicit Grant & PKCE)*: **Allow**.
   - Copia la **App key** (el *App secret* no se usa).
4. Pestaña **Permissions**: marca `account_info.read`, `files.metadata.read`, `files.content.read` y `files.content.write`, y pulsa **Submit**.
5. Hasta 50 usuarios funciona tal cual. Al acercarse a 50, pulsa **Apply for production** en la consola (revisión gratuita de Dropbox); si no se aprueba en dos semanas desde el usuario 50, no podrán entrar más.

## 3. Ponerlos en la compilación

En la carpeta del proyecto, copia `.env.oauth.example` como `.env.oauth` (no se sube al repositorio) y rellénalo:

```
HFS_GOOGLE_CLIENT_ID=123456789-xxxx.apps.googleusercontent.com
HFS_GOOGLE_CLIENT_SECRET=GOCSPX-xxxx
HFS_DROPBOX_APP_KEY=xxxxxxxxxxxxxxx
```

`scripts/compilar-macos.sh` y `scripts/compilar-windows-desde-mac.sh` lo leen solos. Para las compilaciones de GitHub Actions, añade los tres valores en *Settings → Secrets and variables → Actions* del repositorio, con esos mismos nombres.

## 4. Probarlo de verdad

Con una cuenta de prueba, inicia sesión desde la app y copia el token de renovación del Llavero (servicio «Home File Sync», cuenta `relay/<id>/gdrive` o `relay/<id>/dropbox`). Después:

```bash
HFS_TEST_GOOGLE_REFRESH=… HFS_TEST_DROPBOX_REFRESH=… cargo test -p sfs-core --test stores -- --ignored
```

Hace las mismas comprobaciones que con una carpeta y con WebDAV: escribir, sustituir, subir un archivo grande por partes, listar, mover, borrar.
