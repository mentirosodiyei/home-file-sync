# Show File Sync en redes de eventos

Show File Sync no necesita configuración de red, pero sí que los equipos
**puedan verse y conectarse directamente** en la red local.

## Qué usa

| Uso | Protocolo | Puerto |
|---|---|---|
| Descubrimiento (Bonjour/mDNS) | UDP multicast | 5353 |
| Descubrimiento de respaldo (baliza) | UDP broadcast | 47801 |
| Sincronización (cifrada) | TCP | 47800, o uno libre si está ocupado |

No usa internet ni ningún servidor externo.

## Mensajes que puede mostrar la aplicación

**«No se detecta ningún otro equipo del grupo»**
- Comprueba que los demás equipos están encendidos, con Show File Sync abierto, y en la **misma red**: misma wifi, mismo switch o misma VLAN.
- Muchas wifi de hoteles y recintos aíslan a los clientes (*client isolation*, *AP isolation*): cada equipo solo ve el router. Usa una red de producción propia, un switch o un router portátil sin aislamiento.
- Las VLAN distintas o el multicast bloqueado también impiden el descubrimiento.

**«Se detectan otros equipos pero no se puede conectar con ellos»**
- Hay un cortafuegos bloqueando la conexión TCP.
  - En **Windows**, el instalador crea las reglas. Si se usa la versión portátil, acepta el aviso del Firewall marcando también **Redes públicas**.
  - En **macOS**, revisa *Ajustes del Sistema → Red → Firewall* y *Privacidad y seguridad → Red local*: Show File Sync debe estar permitido.
- Algunas redes permiten el descubrimiento pero bloquean las conexiones entre clientes. Hay que usar otra red.

**«Este equipo no tiene conexión a una red local»**
- No hay ninguna interfaz con IPv4. Conecta el cable o la wifi.

## Recomendaciones para producción

- Red cableada o wifi dedicada sin aislamiento de clientes.
- Para vídeos grandes, mejor cable gigabit que wifi.
- Show File Sync no promete sincronización instantánea. Prioriza integridad y estabilidad:
  - un archivo nuevo tarda unos segundos en considerarse estable antes de enviarse;
  - los archivos grandes se reparten entre equipos para no saturar la red.
