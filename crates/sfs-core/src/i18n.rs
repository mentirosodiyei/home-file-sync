//! Idiomas de la interfaz: català, castellano e inglés.
//!
//! El idioma se elige según el del sistema: catalán si empieza por `ca`,
//! castellano si empieza por `es` e inglés en cualquier otro caso.
//!
//! Los textos se escriben en el código en castellano con la macro [`tr!`]
//! (`tr!("Recibido {}", ruta)`); el castellano hace de clave y aquí están sus
//! traducciones. Una prueba comprueba que todo texto marcado tiene traducción
//! al catalán y al inglés con los mismos huecos `{}`.
//!
//! Los mensajes del registro de diagnóstico no se traducen: son técnicos y así
//! se leen igual en cualquier informe de error.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Es,
    Ca,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Es => "es",
            Lang::Ca => "ca",
            Lang::En => "en",
        }
    }

    /// `ca*` → catalán, `es*` → castellano, el resto → inglés.
    pub fn from_tag(tag: &str) -> Lang {
        let t = tag.trim().to_ascii_lowercase();
        if t.starts_with("ca") {
            Lang::Ca
        } else if t.starts_with("es") {
            Lang::Es
        } else {
            Lang::En
        }
    }

    /// Primer idioma admitido de una cabecera `Accept-Language` (para la página
    /// que se abre en el móvil). Si no hay ninguno de los tres, inglés.
    pub fn from_accept_language(header: &str) -> Lang {
        for part in header.split(',') {
            let tag = part.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            if tag.starts_with("ca") {
                return Lang::Ca;
            }
            if tag.starts_with("es") {
                return Lang::Es;
            }
            if tag.starts_with("en") {
                return Lang::En;
            }
        }
        Lang::En
    }
}

// Por defecto castellano (las pruebas y la línea de órdenes); la app fija el
// del sistema al arrancar.
static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set_lang(l: Lang) {
    LANG.store(
        match l {
            Lang::Es => 0,
            Lang::Ca => 1,
            Lang::En => 2,
        },
        Ordering::Relaxed,
    );
}

pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::Ca,
        2 => Lang::En,
        _ => Lang::Es,
    }
}

/// Idioma del sistema operativo.
pub fn system_lang() -> Lang {
    sys_locale::get_locales().next().map(|l| Lang::from_tag(&l)).unwrap_or(Lang::En)
}

/// Traduce `es` (texto en castellano) al idioma actual.
pub fn t(es: &str) -> String {
    t_in(lang(), es)
}

/// Traduce a un idioma concreto.
pub fn t_in(l: Lang, es: &str) -> String {
    if l == Lang::Es {
        return es.to_string();
    }
    match catalog().get(es) {
        Some((ca, en)) => (if l == Lang::Ca { *ca } else { *en }).to_string(),
        None => es.to_string(),
    }
}

/// Sustituye cada `{}` de `template` por el siguiente argumento.
pub fn fill(template: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut it = args.iter();
    let mut rest = template;
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        out.push_str(it.next().map(String::as_str).unwrap_or(""));
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

/// Texto traducido con datos: `tr!("Recibido {}", ruta)`.
#[macro_export]
macro_rules! tr {
    ($s:literal) => {
        $crate::i18n::t($s)
    };
    ($s:literal, $($a:expr),+ $(,)?) => {
        $crate::i18n::fill(&$crate::i18n::t($s), &[$(::std::string::ToString::to_string(&$a)),+])
    };
}

/// Como [`tr!`] pero en un idioma concreto (página del móvil).
#[macro_export]
macro_rules! tr_in {
    ($l:expr, $s:literal) => {
        $crate::i18n::t_in($l, $s)
    };
    ($l:expr, $s:literal, $($a:expr),+ $(,)?) => {
        $crate::i18n::fill(&$crate::i18n::t_in($l, $s), &[$(::std::string::ToString::to_string(&$a)),+])
    };
}

fn catalog() -> &'static HashMap<&'static str, (&'static str, &'static str)> {
    static C: OnceLock<HashMap<&'static str, (&'static str, &'static str)>> = OnceLock::new();
    C.get_or_init(|| ENTRIES.iter().map(|(es, ca, en)| (*es, (*ca, *en))).collect())
}

/// (castellano, català, English)
pub const ENTRIES: &[(&str, &str, &str)] = &[
    // --- Carpetas y grupos ---
    ("No se puede preparar la carpeta: {}", "No es pot preparar la carpeta: {}", "The folder can't be prepared: {}"),
    ("Carpeta compartida creada", "Carpeta compartida creada", "Shared folder created"),
    ("Este equipo no pertenece a ningún grupo.", "Aquest equip no pertany a cap grup.", "This computer doesn't belong to any group."),
    ("El código debe tener 8 cifras.", "El codi ha de tenir 8 xifres.", "The code must have 8 digits."),
    ("Hay varios grupos aceptando equipos. Elige a cuál unirte.", "Hi ha diversos grups acceptant equips. Tria a quin t'hi vols unir.", "Several groups are accepting computers. Choose which one to join."),
    (
        "No se encuentra ningún equipo mostrando un código. Pulsa «Invitar equipo» en el otro equipo y comprueba que estáis en la misma red. Si la red no permite el descubrimiento, usa «Conexión manual».",
        "No es troba cap equip mostrant un codi. Prem «Convida un equip» a l'altre equip i comprova que sou a la mateixa xarxa. Si la xarxa no permet la detecció, fes servir «Connexió manual».",
        "No computer showing a code was found. Click “Invite computer” on the other computer and check that you're on the same network. If the network doesn't allow discovery, use “Manual connection”.",
    ),
    ("No se encuentra el equipo «{}» en la red. Prueba con su dirección IP.", "No es troba l'equip «{}» a la xarxa. Prova-ho amb la seva adreça IP.", "The computer “{}” can't be found on the network. Try its IP address."),
    ("Unido a «{}»", "Unit a «{}»", "Joined “{}”"),
    ("Has salido del grupo", "Has sortit del grup", "You left the group"),
    (
        "La carpeta sincronizada ha cambiado por completo (¿disco desconectado?). No se borrará nada en los demás equipos.",
        "La carpeta sincronitzada ha canviat del tot (disc desconnectat?). No s'esborrarà res als altres equips.",
        "The synced folder changed completely (disconnected drive?). Nothing will be deleted on the other computers.",
    ),
    ("La carpeta sincronizada no está disponible (¿disco desconectado?).", "La carpeta sincronitzada no està disponible (disc desconnectat?).", "The synced folder isn't available (disconnected drive?)."),
    ("La carpeta sincronizada no está disponible", "La carpeta sincronitzada no està disponible", "The synced folder isn't available"),
    ("Nuevo cambio local: {}", "Canvi local nou: {}", "New local change: {}"),
    ("{} cambios locales detectados", "{} canvis locals detectats", "{} local changes detected"),
    (
        "{} archivo(s) borrado(s) en este equipo: se eliminan también en los demás (copia en su papelera interna)",
        "{} fitxer(s) esborrat(s) en aquest equip: també s'eliminen als altres (amb còpia a la seva paperera interna)",
        "{} file(s) deleted on this computer: they're also removed on the others (a copy is kept in their internal trash)",
    ),
    // --- Estado de los equipos ---
    ("Detectado pero sin conexión: {}", "Detectat però sense connexió: {}", "Found but not connected: {}"),
    ("Conectando…", "Connectant…", "Connecting…"),
    ("Desconectado", "Desconnectat", "Offline"),
    ("Sincronizado", "Sincronitzat", "Up to date"),
    ("Comparando archivos…", "Comparant fitxers…", "Comparing files…"),
    ("Recibiendo archivos · faltan {} ({})", "Rebent fitxers · en falten {} ({})", "Receiving files · {} left ({})"),
    ("{} archivo(s) con error", "{} fitxer(s) amb error", "{} file(s) with errors"),
    ("{} conflicto(s) resuelto(s) conservando ambas copias", "{} conflicte(s) resolt(s) conservant totes dues còpies", "{} conflict(s) solved by keeping both copies"),
    ("Conectado con {}", "Connectat amb {}", "Connected to {}"),
    ("Desconectado de {}", "Desconnectat de {}", "Disconnected from {}"),
    ("{} se ha unido al grupo", "{} s'ha unit al grup", "{} joined the group"),
    ("Intento de emparejamiento fallido desde «{}»", "Intent d'aparellament fallit des de «{}»", "Failed pairing attempt from “{}”"),
    // --- Avisos de red ---
    ("Este equipo no tiene conexión a una red local.", "Aquest equip no té connexió a cap xarxa local.", "This computer isn't connected to a local network."),
    (
        "Se detectan otros equipos pero no se puede conectar con ellos. Probablemente un cortafuegos o la red bloquea las conexiones directas entre equipos (aislamiento de clientes). Permite Home File Sync en el cortafuegos o usa una red sin aislamiento.",
        "Es detecten altres equips però no s'hi pot connectar. Probablement un tallafoc o la xarxa bloqueja les connexions directes entre equips (aïllament de clients). Permet Home File Sync al tallafoc o fes servir una xarxa sense aïllament.",
        "Other computers are visible but can't be reached. A firewall or the network is probably blocking direct connections between computers (client isolation). Allow Home File Sync in the firewall or use a network without isolation.",
    ),
    (
        "No se detecta ningún otro equipo del grupo. Comprueba que están encendidos, con Home File Sync abierto y en la misma red. Algunas redes wifi aíslan a los clientes y no permiten que los equipos se vean.",
        "No es detecta cap altre equip del grup. Comprova que estan engegats, amb Home File Sync obert i a la mateixa xarxa. Algunes xarxes wifi aïllen els clients i no deixen que els equips es vegin.",
        "No other computer of the group is visible. Check that they're on, with Home File Sync open and on the same network. Some Wi-Fi networks isolate clients and don't let computers see each other.",
    ),
    (
        "macOS está bloqueando las conexiones de Home File Sync hacia la red local. Abre Ajustes del Sistema → Privacidad y seguridad → Red local y activa Home File Sync. Si ya estaba activado, desactívalo y vuelve a activarlo, y reinicia la aplicación.",
        "macOS està bloquejant les connexions de Home File Sync cap a la xarxa local. Obre Configuració del Sistema → Privacitat i seguretat → Xarxa local i activa Home File Sync. Si ja estava activat, desactiva'l i torna'l a activar, i reinicia l'aplicació.",
        "macOS is blocking Home File Sync's connections to the local network. Open System Settings → Privacy & Security → Local Network and turn on Home File Sync. If it was already on, turn it off and on again, and restart the app.",
    ),
    // --- Conexión (explicaciones) ---
    ("No se conoce ninguna dirección del equipo.", "No es coneix cap adreça de l'equip.", "No address is known for that computer."),
    ("No se encuentra ese nombre de equipo en la red. Prueba con su dirección IP.", "No es troba aquest nom d'equip a la xarxa. Prova-ho amb la seva adreça IP.", "That computer name can't be found on the network. Try its IP address."),
    ("El equipo rechaza la conexión. Comprueba que Home File Sync está abierto en él y que el puerto es correcto.", "L'equip rebutja la connexió. Comprova que Home File Sync hi és obert i que el port és correcte.", "The computer refuses the connection. Check that Home File Sync is open on it and that the port is right."),
    ("El equipo no responde. Probablemente un cortafuegos bloquea la conexión o la red aísla a los equipos entre sí.", "L'equip no respon. Probablement un tallafoc bloqueja la connexió o la xarxa aïlla els equips entre ells.", "The computer doesn't respond. A firewall is probably blocking the connection, or the network isolates computers from each other."),
    ("No hay ruta de red hasta el equipo: está en otra red o subred que este equipo no alcanza.", "No hi ha ruta de xarxa fins a l'equip: és en una altra xarxa o subxarxa que aquest equip no abasta.", "There's no network route to that computer: it's on another network or subnet this computer can't reach."),
    ("Se conectó con el equipo pero la comunicación se interrumpió. Vuelve a intentarlo.", "S'ha connectat amb l'equip però la comunicació s'ha interromput. Torna-ho a provar.", "The connection was made but then interrupted. Try again."),
    ("No se puede conectar con el equipo.", "No es pot connectar amb l'equip.", "Can't connect to the computer."),
    ("manual", "manual", "manual"),
    ("baliza UDP", "balisa UDP", "UDP beacon"),
    ("conexión anterior", "connexió anterior", "previous connection"),
    ("mDNS", "mDNS", "mDNS"),
    ("comunicada por el grupo", "comunicada pel grup", "reported by the group"),
    ("resolución del nombre", "resolució del nom", "name resolution"),
    ("conexión TCP", "connexió TCP", "TCP connection"),
    ("cifrado (Noise)", "xifratge (Noise)", "encryption (Noise)"),
    ("autenticación de grupo", "autenticació de grup", "group authentication"),
    ("emparejamiento", "aparellament", "pairing"),
    (", interfaz {}", ", interfície {}", ", interface {}"),
    (" → conectado en {} ms", " → connectat en {} ms", " → connected in {} ms"),
    (" → fallo en {}: {}", " → error a {}: {}", " → failed at {}: {}"),
    (" [error del sistema {}]", " [error del sistema {}]", " [system error {}]"),
    ("sin respuesta (tiempo de espera agotado)", "sense resposta (temps d'espera esgotat)", "no response (timed out)"),
    ("conexión rechazada", "connexió rebutjada", "connection refused"),
    ("tiempo de espera agotado", "temps d'espera esgotat", "timed out"),
    ("es este mismo equipo", "és aquest mateix equip", "it's this same computer"),
    ("sin respuesta al cifrado (¿es Home File Sync?)", "sense resposta al xifratge (és Home File Sync?)", "no response to encryption (is it Home File Sync?)"),
    ("el otro equipo no ha demostrado pertenecer al grupo", "l'altre equip no ha demostrat que pertany al grup", "the other computer didn't prove it belongs to the group"),
    ("El otro equipo no ha demostrado pertenecer al grupo.", "L'altre equip no ha demostrat que pertany al grup.", "The other computer didn't prove it belongs to the group."),
    ("respuesta inesperada", "resposta inesperada", "unexpected response"),
    ("Código incorrecto.", "Codi incorrecte.", "Wrong code."),
    // --- Direcciones escritas a mano ---
    ("Escribe la dirección IP o el nombre del equipo.", "Escriu l'adreça IP o el nom de l'equip.", "Type the IP address or the computer name."),
    ("Escribe solo la dirección IP o el nombre del equipo, sin «http://» ni rutas.", "Escriu només l'adreça IP o el nom de l'equip, sense «http://» ni camins.", "Type only the IP address or computer name, without “http://” or paths."),
    ("Dirección IPv6 mal escrita: falta «]».", "Adreça IPv6 mal escrita: hi falta «]».", "Badly written IPv6 address: “]” is missing."),
    ("Dirección IPv6 no válida.", "Adreça IPv6 no vàlida.", "Invalid IPv6 address."),
    ("Dirección IPv6 mal escrita.", "Adreça IPv6 mal escrita.", "Badly written IPv6 address."),
    ("Dirección IPv4 no válida.", "Adreça IPv4 no vàlida.", "Invalid IPv4 address."),
    ("Nombre de equipo no válido.", "Nom d'equip no vàlid.", "Invalid computer name."),
    ("El puerto debe ser un número entre 1 y 65535.", "El port ha de ser un nombre entre 1 i 65535.", "The port must be a number between 1 and 65535."),
    // --- Rechazos del otro equipo ---
    ("versión de Home File Sync incompatible", "versió de Home File Sync incompatible", "incompatible Home File Sync version"),
    ("el equipo pertenece a otro grupo", "l'equip pertany a un altre grup", "the computer belongs to another group"),
    ("credenciales de grupo no válidas", "credencials de grup no vàlides", "invalid group credentials"),
    ("el equipo no está aceptando nuevos equipos ahora", "l'equip no accepta equips nous ara mateix", "the computer isn't accepting new computers right now"),
    ("código incorrecto", "codi incorrecte", "wrong code"),
    ("demasiados intentos fallidos; genera un código nuevo", "massa intents fallits; genera un codi nou", "too many failed attempts; generate a new code"),
    // --- Transferencias ---
    ("Recibido {}", "Rebut {}", "Received {}"),
    (
        "Conflicto en «{}»: se ha recibido la versión más reciente y la de este equipo se conserva como «{}»",
        "Conflicte a «{}»: s'ha rebut la versió més recent i la d'aquest equip es conserva com a «{}»",
        "Conflict in “{}”: the newest version was received and this computer's version is kept as “{}”",
    ),
    ("Eliminado {} (borrado en «{}»; copia en la papelera interna)", "Eliminat {} (esborrat a «{}»; còpia a la paperera interna)", "Removed {} (deleted on “{}”; a copy is in the internal trash)"),
    ("esperando a un equipo libre", "esperant un equip lliure", "waiting for a free computer"),
    ("cambió en este equipo; se vuelve a evaluar", "ha canviat en aquest equip; es torna a avaluar", "changed on this computer; checking again"),
    ("no hay espacio suficiente en el disco", "no hi ha prou espai al disc", "not enough disk space"),
    ("los datos recibidos no superan la verificación; se descargará de nuevo", "les dades rebudes no superen la verificació; es tornaran a baixar", "the received data failed verification; it will be downloaded again"),
    ("conexión perdida", "connexió perduda", "connection lost"),
    ("bloque inesperado", "bloc inesperat", "unexpected block"),
    ("error de escritura: {}", "error d'escriptura: {}", "write error: {}"),
    ("el archivo está abierto en otra aplicación; se actualizará cuando se cierre", "el fitxer és obert en una altra aplicació; s'actualitzarà quan es tanqui", "the file is open in another app; it will be updated when it's closed"),
    ("no se puede colocar el archivo: {}", "no es pot desar el fitxer: {}", "the file can't be saved: {}"),
    ("ya no está en el índice", "ja no és a l'índex", "no longer in the index"),
    ("el estado local cambió", "l'estat local ha canviat", "the local state changed"),
    ("el archivo se ha modificado aquí; se conserva", "el fitxer s'ha modificat aquí; es conserva", "the file was modified here; it's kept"),
    ("el archivo está abierto en otra aplicación; se eliminará cuando se cierre", "el fitxer és obert en una altra aplicació; s'eliminarà quan es tanqui", "the file is open in another app; it will be removed when it's closed"),
    ("no se puede eliminar: {}", "no es pot eliminar: {}", "can't be removed: {}"),
    ("la ruta atraviesa un enlace simbólico", "el camí passa per un enllaç simbòlic", "the path goes through a symbolic link"),
    // --- Escaneo y nombres ---
    ("existe otro archivo con el mismo nombre (solo cambian mayúsculas)", "hi ha un altre fitxer amb el mateix nom (només canvien les majúscules)", "another file has the same name (only the letter case differs)"),
    ("no se puede leer: {}", "no es pot llegir: {}", "can't be read: {}"),
    ("el nombre «{}» contiene «{}», que Windows no admite", "el nom «{}» conté «{}», que Windows no admet", "the name “{}” contains “{}”, which Windows doesn't allow"),
    ("el nombre «{}» termina en punto o espacio, que Windows no admite", "el nom «{}» acaba amb punt o espai, que Windows no admet", "the name “{}” ends with a dot or space, which Windows doesn't allow"),
    ("«{}» es un nombre reservado en Windows", "«{}» és un nom reservat a Windows", "“{}” is a reserved name on Windows"),
    // --- Selección de carpeta ---
    ("Carpeta de {}", "Carpeta de {}", "{}'s folder"),
    ("No se puede abrir la carpeta: {}", "No es pot obrir la carpeta: {}", "The folder can't be opened: {}"),
    ("La ruta elegida no es una carpeta.", "El camí triat no és una carpeta.", "The chosen path isn't a folder."),
    ("No se puede sincronizar la raíz del disco. Elige o crea una carpeta concreta.", "No es pot sincronitzar l'arrel del disc. Tria o crea una carpeta concreta.", "The root of a drive can't be synced. Choose or create a specific folder."),
    ("No se puede sincronizar la carpeta de usuario completa. Elige o crea una carpeta concreta.", "No es pot sincronitzar tota la carpeta d'usuari. Tria o crea una carpeta concreta.", "The whole user folder can't be synced. Choose or create a specific folder."),
    ("No se puede sincronizar una carpeta del sistema.", "No es pot sincronitzar una carpeta del sistema.", "A system folder can't be synced."),
    ("No se puede escribir en la carpeta: {}", "No es pot escriure a la carpeta: {}", "Can't write to the folder: {}"),
    ("Esa carpeta coincide o está dentro de otra carpeta compartida ({}). Elige otra.", "Aquesta carpeta coincideix o és dins d'una altra carpeta compartida ({}). Tria'n una altra.", "That folder is the same as, or inside, another shared folder ({}). Choose another one."),
    ("Ponle un nombre a la carpeta compartida.", "Posa un nom a la carpeta compartida.", "Give the shared folder a name."),
    ("Hay varias carpetas aceptando equipos. Elige a cuál unirte.", "Hi ha diverses carpetes acceptant equips. Tria a quina t'hi vols unir.", "Several folders are accepting computers. Choose which one to join."),
    (
        "No se encuentra ninguna carpeta compartida esperando equipos. Pulsa «Invitar» en el otro equipo y comprueba que estáis en la misma red. Si la red no permite el descubrimiento, usa «Conexión manual».",
        "No es troba cap carpeta compartida esperant equips. Prem «Convida» a l'altre equip i comprova que sou a la mateixa xarxa. Si la xarxa no permet la detecció, fes servir «Connexió manual».",
        "No shared folder waiting for computers was found. Click “Invite” on the other computer and check that you're on the same network. If the network doesn't allow discovery, use “Manual connection”.",
    ),
    ("Esa carpeta compartida ya no existe.", "Aquesta carpeta compartida ja no existeix.", "That shared folder no longer exists."),
    ("La recepción por navegador no está disponible en este equipo.", "La recepció pel navegador no està disponible en aquest equip.", "Receiving through the browser isn't available on this computer."),
    ("Esa carpeta compartida no tiene carpeta local.", "Aquesta carpeta compartida no té carpeta local.", "That shared folder has no local folder."),
    ("la carpeta compartida «{}»", "la carpeta compartida «{}»", "the shared folder “{}”"),
    ("la carpeta {}", "la carpeta {}", "the folder {}"),
    ("Recibidos Home File Sync", "Rebuts Home File Sync", "Home File Sync Received"),
    (
        "Carpeta interna de Home File Sync.\ntmp/        descargas en curso\nversiones/  copias de seguridad internas\npapelera/   archivos borrados en otros equipos (se borran a los 14 días)\nNo la borres mientras la aplicación está en uso.\n",
        "Carpeta interna de Home File Sync.\ntmp/        baixades en curs\nversiones/  còpies de seguretat internes\npapelera/   fitxers esborrats en altres equips (s'esborren als 14 dies)\nNo l'esborris mentre l'aplicació estigui en ús.\n",
        "Home File Sync internal folder.\ntmp/        downloads in progress\nversiones/  internal backups\npapelera/   files deleted on other computers (removed after 14 days)\nDon't delete it while the app is in use.\n",
    ),
    // --- Página del móvil ---
    ("Enviar a {}", "Envia a {}", "Send to {}"),
    ("Enviar archivos a «{}»", "Envia fitxers a «{}»", "Send files to “{}”"),
    ("Se guardarán en <b>{}</b>. No hace falta instalar nada.", "Es desaran a <b>{}</b>. No cal instal·lar res.", "They'll be saved in <b>{}</b>. Nothing to install."),
    ("Elegir archivos", "Tria fitxers", "Choose files"),
    ("o arrastrarlos aquí", "o arrossega'ls aquí", "or drag them here"),
    ("Home File Sync · solo dentro de la misma red", "Home File Sync · només dins la mateixa xarxa", "Home File Sync · only within the same network"),
    ("En espera", "En espera", "Waiting"),
    ("Enviando…", "Enviant…", "Sending…"),
    ("Enviado ✓", "Enviat ✓", "Sent ✓"),
    (" como «{}»", " com a «{}»", " as “{}”"),
    ("Sin conexión con el equipo", "Sense connexió amb l'equip", "No connection to the computer"),
    ("Este enlace ha caducado.", "Aquest enllaç ha caducat.", "This link has expired."),
    ("Este enlace no es válido.", "Aquest enllaç no és vàlid.", "This link isn't valid."),
    ("Pide que vuelvan a mostrar el código QR en Home File Sync.", "Demana que tornin a mostrar el codi QR a Home File Sync.", "Ask for the QR code to be shown again in Home File Sync."),
    ("Enlace caducado o no válido", "Enllaç caducat o no vàlid", "Expired or invalid link"),
    ("subida interrumpida", "pujada interrompuda", "upload interrupted"),
    ("la subida se ha detenido", "la pujada s'ha aturat", "the upload stalled"),
    ("demasiados archivos con el mismo nombre", "massa fitxers amb el mateix nom", "too many files with the same name"),
    // --- Aplicación ---
    ("Abrir Home File Sync", "Obre Home File Sync", "Open Home File Sync"),
    ("Salir (detiene la sincronización)", "Surt (atura la sincronització)", "Quit (stops syncing)"),
    ("Home File Sync — sin carpetas compartidas", "Home File Sync — sense carpetes compartides", "Home File Sync — no shared folders"),
    ("hay errores", "hi ha errors", "there are errors"),
    ("sincronizando · faltan {}", "sincronitzant · en falten {}", "syncing · {} left"),
    ("todo al día", "tot al dia", "all up to date"),
    ("Home File Sync — {} carpeta(s) · {}", "Home File Sync — {} carpeta(es) · {}", "Home File Sync — {} folder(s) · {}"),
    ("Elige la carpeta", "Tria la carpeta", "Choose the folder"),
    ("Elige una carpeta.", "Tria una carpeta.", "Choose a folder."),
    ("Esa carpeta no está disponible", "Aquesta carpeta no està disponible", "That folder isn't available"),
    ("Exportar registro de diagnóstico", "Exporta el registre de diagnòstic", "Export diagnostic log"),
    ("El registro en disco no está activo", "El registre al disc no està actiu", "The on-disk log isn't active"),
    ("Acerca de Home File Sync", "Quant a Home File Sync", "About Home File Sync"),
    ("Ocultar Home File Sync", "Amaga Home File Sync", "Hide Home File Sync"),
    ("Ocultar otros", "Amaga els altres", "Hide Others"),
    ("Mostrar todo", "Mostra-ho tot", "Show All"),
    ("Salir de Home File Sync", "Surt de Home File Sync", "Quit Home File Sync"),
    ("Edición", "Edició", "Edit"),
    ("Deshacer", "Desfés", "Undo"),
    ("Rehacer", "Refés", "Redo"),
    ("Cortar", "Retalla", "Cut"),
    ("Copiar", "Copia", "Copy"),
    ("Pegar", "Enganxa", "Paste"),
    ("Seleccionar todo", "Selecciona-ho tot", "Select All"),
    ("Ventana", "Finestra", "Window"),
    ("Minimizar", "Minimitza", "Minimize"),
    ("Cerrar ventana", "Tanca la finestra", "Close Window"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_choice() {
        assert_eq!(Lang::from_tag("ca-ES"), Lang::Ca);
        assert_eq!(Lang::from_tag("ca"), Lang::Ca);
        assert_eq!(Lang::from_tag("es-ES"), Lang::Es);
        assert_eq!(Lang::from_tag("es_MX"), Lang::Es);
        assert_eq!(Lang::from_tag("en-GB"), Lang::En);
        assert_eq!(Lang::from_tag("fr-FR"), Lang::En, "otros idiomas → inglés");
        assert_eq!(Lang::from_tag("eu-ES"), Lang::En);
        assert_eq!(Lang::from_accept_language("fr-FR,ca;q=0.8,es;q=0.6"), Lang::Ca);
        assert_eq!(Lang::from_accept_language("de-DE,de;q=0.9"), Lang::En);
    }

    #[test]
    fn fill_and_lookup() {
        assert_eq!(fill("Recibido {} de {}", &["a.pdf".into(), "Ana".into()]), "Recibido a.pdf de Ana");
        assert_eq!(t_in(Lang::Ca, "Recibido {}"), "Rebut {}");
        assert_eq!(t_in(Lang::En, "Recibido {}"), "Received {}");
        assert_eq!(t_in(Lang::Es, "Recibido {}"), "Recibido {}");
        assert_eq!(crate::tr_in!(Lang::En, "Recibido {}", "foto.jpg"), "Received foto.jpg");
    }

    #[test]
    fn every_translation_has_the_same_placeholders() {
        let mut seen = std::collections::HashSet::new();
        for (es, ca, en) in ENTRIES {
            assert!(seen.insert(es), "clave repetida: {es}");
            let n = es.matches("{}").count();
            assert_eq!(ca.matches("{}").count(), n, "catalán con huecos distintos: {es}");
            assert_eq!(en.matches("{}").count(), n, "inglés con huecos distintos: {es}");
            assert!(!ca.is_empty() && !en.is_empty(), "{es}");
        }
    }

    /// Todo texto marcado con `tr!`/`tr_in!` en el código tiene traducción.
    #[test]
    fn every_marked_text_is_translated() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(root.join("src")).unwrap().flatten().map(|e| e.path()).collect();
        // También la aplicación de escritorio (bandeja, menús, diálogos).
        files.push(root.join("../../app/src-tauri/src/main.rs"));
        let mut missing = Vec::new();
        for path in files.into_iter().filter(|p| p.exists()) {
            let src = std::fs::read_to_string(&path).unwrap();
            for lit in marked_literals(&src) {
                if !catalog().contains_key(lit.as_str()) {
                    missing.push(format!("{}: {lit}", path.file_name().unwrap().to_string_lossy()));
                }
            }
        }
        assert!(missing.is_empty(), "textos sin traducir:\n{}", missing.join("\n"));
    }

    /// La interfaz (app/ui/i18n.js) tiene las mismas claves y huecos en los
    /// tres idiomas, y toda clave usada en app.js e index.html existe.
    #[test]
    fn ui_dictionary_is_complete() {
        let ui = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/ui");
        let Ok(dict) = std::fs::read_to_string(ui.join("i18n.js")) else { return };
        let block = |lang: &str| -> std::collections::BTreeMap<String, String> {
            let start = dict.find(&format!("\n  {lang}: {{")).unwrap_or_else(|| panic!("falta el idioma {lang}"));
            let body = &dict[start..];
            let end = body.find("\n  },").unwrap();
            body[..end]
                .lines()
                .filter_map(|l| {
                    let l = l.trim();
                    let rest = l.strip_prefix('"')?;
                    let (k, v) = rest.split_once("\": ")?;
                    Some((k.to_string(), v.to_string()))
                })
                .collect()
        };
        let (es, ca, en) = (block("es"), block("ca"), block("en"));
        assert!(es.len() > 100);
        for (name, other) in [("ca", &ca), ("en", &en)] {
            let a: Vec<&String> = es.keys().collect();
            let b: Vec<&String> = other.keys().collect();
            assert_eq!(a, b, "claves distintas entre es y {name}");
            for (k, v) in &es {
                assert_eq!(v.matches("{}").count(), other[k].matches("{}").count(), "huecos distintos en {name}: {k}");
            }
        }
        let html = std::fs::read_to_string(ui.join("index.html")).unwrap();
        let js = std::fs::read_to_string(ui.join("app.js")).unwrap();
        let mut used = Vec::new();
        for attr in ["data-i18n=\"", "data-i18n-html=\"", "data-i18n-ph=\"", "data-i18n-title=\""] {
            for part in html.split(attr).skip(1) {
                used.push(part.split('"').next().unwrap().to_string());
            }
        }
        for part in js.split("t(\"").skip(1) {
            let k = part.split('"').next().unwrap();
            if k.contains('.') && !k.contains(' ') && !k.ends_with('.') {
                used.push(k.to_string());
            }
        }
        for state in ["noFolder", "noGroup", "alone", "synced", "syncing", "conflict", "error"] {
            used.push(format!("state.{state}"));
        }
        let missing: Vec<&String> = used.iter().filter(|k| !es.contains_key(*k)).collect();
        assert!(missing.is_empty(), "claves usadas sin definir: {missing:?}");
    }

    /// Literales de `tr!("…"` y `tr_in!(x, "…"` (con escapes \" y \n).
    fn marked_literals(src: &str) -> Vec<String> {
        // Sin comentarios (ejemplos en la documentación).
        let code: String = src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        let src = code.as_str();
        let mut out = Vec::new();
        for start in ["tr!(\"", "tr_in!("] {
            let mut rest = src;
            while let Some(i) = rest.find(start) {
                // Que sea la macro y no parte de otro nombre (p. ej. `include_str!(`).
                let prev = rest[..i].chars().next_back();
                rest = &rest[i + start.len()..];
                if prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let body = if start == "tr_in!(" {
                    match rest.find(", \"") {
                        Some(j) if j < 40 => &rest[j + 3..],
                        _ => continue,
                    }
                } else {
                    rest
                };
                let mut s = String::new();
                let mut chars = body.chars();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some(o) => s.push(o),
                            None => break,
                        },
                        '"' => break,
                        c => s.push(c),
                    }
                }
                if !s.contains("{}\"") {
                    out.push(s);
                }
            }
        }
        out
    }
}
