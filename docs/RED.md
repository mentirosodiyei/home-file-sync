# Home File Sync en la red de casa

Home File Sync no necesita configuración de red, pero sí que los equipos
**puedan verse y conectarse directamente** en la red local.

## Qué usa

| Uso | Protocolo | Puerto |
|---|---|---|
| Descubrimiento (Bonjour/mDNS) | UDP multicast | 5353 |
| Descubrimiento de respaldo (baliza) | UDP broadcast | 47821 |
| Recepción desde el móvil (navegador) | TCP (HTTP) | 47830 |
| Sincronización (cifrada) | TCP | 47820 (y siguientes libres, uno por carpeta compartida) |

No usa internet ni ningún servidor externo.

## Mensajes que puede mostrar la aplicación

**«No se detecta ningún otro equipo del grupo»**
- Comprueba que los demás equipos están encendidos, con Home File Sync abierto, y en la **misma red**: misma wifi, mismo switch o misma VLAN.
- Muchas wifi de hoteles y recintos aíslan a los clientes (*client isolation*, *AP isolation*): cada equipo solo ve el router. Usa una red de producción propia, un switch o un router portátil sin aislamiento.
- Las VLAN distintas o el multicast bloqueado también impiden el descubrimiento.

**«Se detectan otros equipos pero no se puede conectar con ellos»**
- Hay un cortafuegos bloqueando la conexión TCP.
  - En **Windows**, el instalador crea las reglas. Si se usa la versión portátil, acepta el aviso del Firewall marcando también **Redes públicas**.
  - En **macOS**, revisa *Ajustes del Sistema → Red → Firewall* y *Privacidad y seguridad → Red local*: Home File Sync debe estar permitido.
- Algunas redes permiten el descubrimiento pero bloquean las conexiones entre clientes. Hay que usar otra red.

**«macOS está bloqueando las conexiones de Home File Sync hacia la red local»**
- macOS exige el permiso **Red local** para que una app *inicie* conexiones hacia otros equipos de la red. Aceptar conexiones entrantes no lo necesita. Por eso, sin el permiso, otros equipos pueden unirse al Mac pero el Mac no puede unirse a ellos.
- El síntoma es el error del sistema `No route to host (os error 65)` hacia una dirección de la propia subred, aunque `ping` o `nc` funcionen desde Terminal (Terminal tiene su propio permiso).
- Solución: *Ajustes del Sistema → Privacidad y seguridad → Red local* y activa Home File Sync. Si ya estaba activado, desactívalo, vuelve a activarlo y reinicia la app.
- macOS asocia el permiso a la **firma** de la app. Las versiones hasta la 0.1.0 solo llevaban la firma automática del enlazador, cuyo identificador cambiaba en cada compilación, así que el permiso podía no aplicarse aunque apareciera activado. Desde la 0.1.1 la app va firmada como paquete con el identificador estable `org.homefilesync.app` (ver `scripts/compilar-macos.sh`).

**«Este equipo no tiene conexión a una red local»**
- No hay ninguna interfaz con IPv4. Conecta el cable o la wifi.

## Fuera de casa: VPN (WireGuard)

El descubrimiento automático no atraviesa una VPN, porque los túneles no transmiten multicast ni broadcast. Aun así se puede seguir sincronizando fuera de casa:

1. Configura WireGuard para que el equipo que sale de casa pueda llegar a la red de casa. En su configuración, `AllowedIPs` debe incluir la subred de casa, por ejemplo `192.168.1.0/24`. El servidor de WireGuard puede estar en el router o en un equipo de casa.
2. En *Ajustes → Fuera de casa (VPN)*, activa **Permitir conexiones por VPN** en ese portátil y, si WireGuard funciona en un equipo de casa, también en él.
3. Si el portátil ya se había conectado en casa, recuerda las direcciones de los demás y reconecta solo al activar la VPN. Si no, usa **Conexión manual** con la dirección de casa del equipo y el puerto de la carpeta, que aparecen en Ajustes.

Con el ajuste desactivado, que es lo predeterminado, el equipo rechaza las conexiones que le llegan por un túnel y no comunica sus direcciones de VPN.

## Si el descubrimiento no funciona

Home File Sync no depende solo del descubrimiento:
- Recuerda por dónde ha alcanzado a cada equipo del grupo, así que reconecta tras un reinicio aunque la red bloquee mDNS y broadcast.
- Los equipos se comunican entre sí las direcciones de los demás miembros: basta con que un equipo vea a otro para que todos acaben conectados.

## Conexión manual (recuperación)

Es una herramienta de recuperación para el primer contacto cuando el descubrimiento no funciona (multicast o broadcast bloqueados, VLAN distintas). En «Unirse a un grupo» abre **Conexión manual**:
- Escribe la IP o el nombre del equipo que muestra el código. Acepta IPv4, IPv6 o nombre, con `:puerto` opcional.
- El puerto por defecto es 47820.
- El código de 8 cifras sigue siendo obligatorio: la autenticación y el cifrado son los mismos que en la conexión automática.
- La dirección solo se guarda si marcas «Recordar esta dirección». Se puede olvidar en *Ajustes → General*.
- Cada unión manual queda anotada en el registro de diagnóstico con el motivo por el que no funcionó la automática. Si necesitas usarla, exporta el registro y compártelo: sirve para mejorar la detección automática.

## Registro de diagnóstico

*Ajustes (icono ⚙) → Avanzado → Diagnóstico* muestra:
- los equipos descubiertos, con qué direcciones y por qué vía (mDNS o baliza, con la IP de origen del paquete);
- cada intento de conexión, con la dirección, el puerto, la interfaz local, la fase (TCP, cifrado, autenticación, emparejamiento) y el error original del sistema;
- desconexiones y operaciones de sincronización.

Se puede filtrar por nivel o texto, copiar y exportar a un archivo para enviarlo. El registro detallado (DEBUG) se activa ahí mismo.

El registro nunca guarda códigos de unión, claves ni el contenido de los archivos. Los archivos están en `<datos de la app>/logs`:
- máximo 5 archivos de 2 MB;
- rotación automática.

## Recomendaciones para producción

- Red cableada o wifi dedicada sin aislamiento de clientes.
- Para vídeos grandes, mejor cable gigabit que wifi.
- Home File Sync no promete sincronización instantánea. Prioriza integridad y estabilidad:
  - un archivo nuevo tarda unos segundos en considerarse estable antes de enviarse;
  - los archivos grandes se reparten entre equipos para no saturar la red.
