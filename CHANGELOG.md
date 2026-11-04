# Historial de versiones

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
