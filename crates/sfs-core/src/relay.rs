//! Relevo por un servicio externo (Google Drive, Dropbox, OneDrive o un NAS).
//!
//! Cuando un miembro de la carpeta compartida no está conectado, los cambios
//! que le faltan se dejan cifrados en una carpeta que el propio sistema ya
//! sincroniza con la nube (la de Google Drive, Dropbox…) o en una carpeta de
//! red de un NAS. Cuando ese equipo vuelve, los descarga de ahí; cuando todos
//! los equipos que usan el relevo los tienen, se borran del servicio (o se
//! mueven a `old/`, si se eligió conservarlos).
//!
//! Estructura dentro de la carpeta elegida:
//!
//! ```text
//! Home File Sync/
//!   LEEME.txt
//!   hfs-<id>/            una carpeta por carpeta compartida (nombre opaco)
//!     config.hfs         opciones comunes (conservar en old/)
//!     idx/<equipo>.hfs   índice publicado por cada equipo
//!     blobs/<id>.hfs     contenido de archivos, uno por hash
//!     old/<fecha>/       lo que ya llegó a todos (si se conserva)
//! ```
//!
//! * **Todo va cifrado** con una clave derivada de la del grupo
//!   (XChaCha20-Poly1305 por bloques de 1 MiB, con marca de último bloque para
//!   detectar archivos truncados). Los nombres de archivo también son opacos:
//!   el servicio no ve nombres, rutas ni hashes.
//! * Cada equipo escribe **solo su propio índice**: el servicio nunca tiene que
//!   resolver conflictos de edición. Los blobs son inmutables y se identifican
//!   por su contenido, así que si dos equipos suben el mismo, da igual.
//! * El relevo es una fuente más para el planificador de descargas: se aplican
//!   las mismas reglas de versiones, borrados, conflictos y `old/` que por la
//!   red, y el contenido se verifica con su hash antes de colocarlo.
//! * Solo cuentan como destinatarios los equipos que también usan el relevo
//!   (los que han publicado su índice): un equipo sin relevo no puede leerlo,
//!   así que no impide la limpieza.

use crate::engine::{FolderCtx, Inner};
use crate::index::FileEntry;
use crate::store::{RelayTarget, Store};
use crate::vv::Causality;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Identificador de la fuente «relevo» en la lista de fuentes de una descarga.
pub(crate) const RELAY_SOURCE: &str = "@relay";
/// Nombre de la carpeta que se crea dentro de la elegida.
pub const RELAY_ROOT: &str = "Home File Sync";

const MAGIC: &[u8; 6] = b"HFSR1\n";
const PREFIX: usize = 19;
const HEADER: usize = MAGIC.len() + PREFIX;
const CHUNK: usize = 1 << 20;
const TAG: usize = 16;
/// Un equipo que no actualiza su índice en este tiempo deja de contar como
/// destinatario (salió del grupo o dejó de usar el relevo).
const STALE_AFTER: Duration = Duration::from_secs(30 * 86400);
/// El índice propio se reescribe al menos con esta frecuencia, aunque no cambie.
const HEARTBEAT: Duration = Duration::from_secs(86400);
/// Blobs que no corresponden a ningún índice conocido: se esperan antes de
/// borrarlos, por si su índice aún no ha llegado desde la nube.
const ORPHAN_GRACE: Duration = Duration::from_secs(86400);

// ---------------------------------------------------------------------------
// Claves y nombres
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct Keys {
    enc: [u8; 32],
    names: [u8; 32],
}

impl Keys {
    pub fn new(group_key: &[u8; 32]) -> Self {
        Keys {
            enc: blake3::derive_key("homefilesync relay v1 encryption", group_key),
            names: blake3::derive_key("homefilesync relay v1 names", group_key),
        }
    }

    fn name(&self, kind: &str, value: &str) -> String {
        let mut h = blake3::Hasher::new_keyed(&self.names);
        h.update(kind.as_bytes());
        h.update(&[0]);
        h.update(value.as_bytes());
        h.finalize().to_hex()[..32].to_string()
    }

    pub fn share_dir(&self, group_id: &str) -> String {
        format!("hfs-{}", &self.name("share", group_id)[..16])
    }

    pub fn blob_name(&self, hash: &str) -> String {
        format!("{}.hfs", self.name("blob", hash))
    }

    pub fn index_name(&self, device: &str) -> String {
        format!("{}.hfs", self.name("index", device))
    }
}

// ---------------------------------------------------------------------------
// Cifrado por bloques (construcción STREAM)
// ---------------------------------------------------------------------------

fn bad(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_string())
}

fn read_full<R: Read>(r: &mut R, buf: &mut Vec<u8>, max: usize) -> std::io::Result<()> {
    buf.clear();
    buf.resize(max, 0);
    let mut n = 0;
    while n < max {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    buf.truncate(n);
    Ok(())
}

fn nonce(prefix: &[u8], counter: u32, last: bool) -> XNonce {
    let mut n = [0u8; 24];
    n[..PREFIX].copy_from_slice(prefix);
    n[PREFIX..PREFIX + 4].copy_from_slice(&counter.to_be_bytes());
    n[23] = last as u8;
    XNonce::from(n)
}

/// Cifra `r` en `w`. Devuelve los bytes en claro y su BLAKE3 (para comprobar
/// que lo cifrado es exactamente la versión que se pretendía subir).
pub(crate) fn encrypt_stream<R: Read, W: Write>(key: &[u8; 32], mut r: R, mut w: W, mut progress: impl FnMut(u64)) -> std::io::Result<(u64, String)> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let prefix: [u8; PREFIX] = crate::auth::random_bytes();
    let mut header = Vec::with_capacity(HEADER);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&prefix);
    w.write_all(&header)?;
    let mut hasher = blake3::Hasher::new();
    let (mut cur, mut next) = (Vec::with_capacity(CHUNK), Vec::with_capacity(CHUNK));
    read_full(&mut r, &mut cur, CHUNK)?;
    let mut counter: u32 = 0;
    let mut total = 0u64;
    loop {
        let last = if cur.len() < CHUNK {
            true
        } else {
            read_full(&mut r, &mut next, CHUNK)?;
            next.is_empty()
        };
        hasher.update(&cur);
        let ct = cipher.encrypt(&nonce(&prefix, counter, last), Payload { msg: &cur, aad: &header }).map_err(|_| bad("cifrado"))?;
        w.write_all(&ct)?;
        total += cur.len() as u64;
        progress(total);
        if last {
            break;
        }
        counter = counter.checked_add(1).ok_or_else(|| bad("archivo demasiado grande"))?;
        std::mem::swap(&mut cur, &mut next);
    }
    w.flush()?;
    Ok((total, hasher.finalize().to_hex().to_string()))
}

/// Descifra `r` en `w`. Falla si el archivo está alterado, es de otro grupo o
/// está incompleto (por ejemplo, porque la nube aún no ha terminado de bajarlo).
pub(crate) fn decrypt_stream<R: Read, W: Write>(key: &[u8; 32], mut r: R, mut w: W, mut progress: impl FnMut(u64)) -> std::io::Result<u64> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut header = Vec::new();
    read_full(&mut r, &mut header, HEADER)?;
    if header.len() != HEADER || &header[..MAGIC.len()] != MAGIC {
        return Err(bad("no es un archivo del relevo"));
    }
    let prefix = header[MAGIC.len()..].to_vec();
    let (mut cur, mut next) = (Vec::new(), Vec::new());
    read_full(&mut r, &mut cur, CHUNK + TAG)?;
    let mut counter: u32 = 0;
    let mut total = 0u64;
    loop {
        let last = if cur.len() < CHUNK + TAG {
            true
        } else {
            read_full(&mut r, &mut next, CHUNK + TAG)?;
            next.is_empty()
        };
        let pt = cipher
            .decrypt(&nonce(&prefix, counter, last), Payload { msg: &cur, aad: &header })
            .map_err(|_| bad("datos alterados, incompletos o de otro grupo"))?;
        w.write_all(&pt)?;
        total += pt.len() as u64;
        progress(total);
        if last {
            break;
        }
        counter = counter.checked_add(1).ok_or_else(|| bad("archivo demasiado grande"))?;
        std::mem::swap(&mut cur, &mut next);
    }
    w.flush()?;
    Ok(total)
}

pub(crate) fn seal(key: &[u8; 32], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + HEADER + TAG);
    encrypt_stream(key, data, &mut out, |_| {}).expect("cifrar en memoria no falla");
    out
}

pub(crate) fn open(key: &[u8; 32], data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    decrypt_stream(key, data, &mut out, |_| {})?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Datos publicados
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct PublishedIndex {
    v: u32,
    device: String,
    name: String,
    backup: bool,
    /// Segundos Unix de la última publicación (latido).
    updated: u64,
    entries: Vec<FileEntry>,
}

#[derive(Serialize, Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "camelCase")]
struct RelayConfig {
    #[serde(default)]
    keep_old: bool,
}

pub(crate) struct RemoteIdx {
    pub name: String,
    pub backup: bool,
    pub updated: u64,
    pub entries: HashMap<String, FileEntry>,
}

#[derive(Default)]
pub(crate) struct RelayState {
    pub keys: Option<Keys>,
    /// Servicio abierto y para qué destino (si cambia el destino, se vuelve a abrir).
    store: Option<(RelayTarget, Arc<dyn Store>)>,
    /// Carpeta de esta carpeta compartida dentro del servicio (`hfs-…`).
    share_dir: String,
    /// Índices publicados por los demás equipos (id → índice).
    pub others: HashMap<String, RemoteIdx>,
    /// Blobs presentes (nombre de archivo).
    pub blobs: HashSet<String>,
    pub keep_old: bool,
    pub error: Option<String>,
    pub stored_files: usize,
    pub stored_bytes: u64,
    /// Equipos desconectados a los que les falta algo que está (o estará) en el relevo.
    pub waiting: Vec<String>,
    published: Option<(blake3::Hash, SystemTime)>,
    seen: HashMap<String, (SystemTime, u64)>,
}

impl RelayState {
    fn reset(&mut self) {
        *self = RelayState::default();
    }

    /// ¿Se puede descargar este contenido del relevo?
    pub fn has_blob(&self, hash: &str) -> bool {
        self.keys.as_ref().is_some_and(|k| self.blobs.contains(&k.blob_name(hash)))
    }

    fn open_store(&self) -> Option<(Arc<dyn Store>, String)> {
        self.store.as_ref().map(|(_, s)| (s.clone(), self.share_dir.clone()))
    }
}

/// Estado del relevo para la interfaz.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayView {
    /// folder | smb | webdav | gdrive | dropbox
    pub kind: String,
    /// Dónde está, para mostrarlo (sin contraseñas).
    pub path: String,
    pub target: RelayTarget,
    pub ok: bool,
    pub error: Option<String>,
    pub keep_old: bool,
    pub stored_files: usize,
    pub stored_bytes: u64,
    pub waiting: Vec<String>,
    /// Otros equipos que también usan el relevo.
    pub members: Vec<String>,
}

/// Destino del relevo configurado (con compatibilidad con la 0.4.0, que solo
/// guardaba una carpeta).
pub(crate) fn target_of(s: &crate::settings::Settings) -> Option<RelayTarget> {
    s.relay_target.clone().or_else(|| s.relay.clone().map(|path| RelayTarget::Folder { path }))
}

pub(crate) fn view(inner: &Inner) -> Option<RelayView> {
    let target = target_of(&inner.settings.lock().unwrap())?;
    let r = inner.relay.lock().unwrap();
    let mut members: Vec<String> = r.others.values().map(|o| o.name.clone()).collect();
    members.sort();
    Some(RelayView {
        kind: target.kind().into(),
        path: target.describe(),
        ok: r.store.is_some() && r.error.is_none(),
        error: r.error.clone(),
        keep_old: r.keep_old,
        stored_files: r.stored_files,
        stored_bytes: r.stored_bytes,
        waiting: r.waiting.clone(),
        members,
        target,
    })
}

// ---------------------------------------------------------------------------
// Configuración
// ---------------------------------------------------------------------------

/// Comprueba que la carpeta elegida sirve como relevo.
pub(crate) fn check_relay_folder(path: &Path, share: Option<&Path>) -> Result<PathBuf, String> {
    let p = std::fs::canonicalize(path).map_err(|_| tr!("No se encuentra la carpeta {}", path.display()))?;
    if !p.is_dir() {
        return Err(tr!("No se encuentra la carpeta {}", path.display()));
    }
    if let Some(share) = share.and_then(|s| std::fs::canonicalize(s).ok()) {
        if p.starts_with(&share) || share.starts_with(&p) {
            return Err(tr!("El servicio externo debe estar fuera de la carpeta compartida."));
        }
    }
    let probe = p.join(format!(".hfs-prueba-{}", hex::encode(crate::auth::random_bytes::<4>())));
    std::fs::write(&probe, b"ok").map_err(|e| tr!("No se puede escribir en {}: {}", p.display(), e))?;
    let _ = std::fs::remove_file(&probe);
    Ok(p)
}

fn readme() -> String {
    format!(
        "{}\n\n{}\n\n{}\n",
        crate::i18n::t_in(crate::i18n::Lang::Ca, README_ES),
        crate::i18n::t_in(crate::i18n::Lang::Es, README_ES),
        crate::i18n::t_in(crate::i18n::Lang::En, README_ES)
    )
}

const README_ES: &str = "Home File Sync guarda aquí, cifrados, los cambios para los equipos que no están conectados. Se borran solos cuando todos los han recibido. No modifiques ni borres estos archivos a mano, salvo la carpeta «old».";

impl crate::engine::Engine {
    /// Usa (o deja de usar, con `None`) una carpeta de este equipo como relevo.
    pub fn set_relay(&self, path: Option<&Path>) -> Result<(), String> {
        self.set_relay_target(path.map(|p| RelayTarget::Folder { path: p.to_path_buf() }), None)
    }

    /// Configura el relevo: carpeta, NAS, WebDAV, Google Drive o Dropbox.
    /// `secret` es la contraseña (NAS, WebDAV) o el token de la sesión (Google,
    /// Dropbox); se guarda en el almacén del sistema. Comprueba antes que se
    /// puede escribir. Bloqueante (puede conectar por red).
    pub fn set_relay_target(&self, target: Option<RelayTarget>, secret: Option<&str>) -> Result<(), String> {
        let inner = &self.inner;
        let group = inner.group().ok_or_else(|| tr!("El servicio externo no está disponible."))?;
        let old = target_of(&inner.settings.lock().unwrap());
        let target = match target {
            Some(RelayTarget::Folder { path }) => {
                let share = inner.folder().map(|f| f.root.clone());
                Some(RelayTarget::Folder { path: check_relay_folder(&path, share.as_deref())? })
            }
            Some(t) => {
                let key = crate::store::secret_key(&group.id, t.kind());
                // Sin contraseña nueva: se conserva la guardada (p. ej. al cambiar solo la carpeta).
                let pass = secret.map(str::to_string).or_else(|| crate::secrets::get(&key));
                let store = crate::store::open(&t, pass.as_deref())?;
                crate::store::probe(store.as_ref())?;
                if let Some(p) = secret {
                    crate::secrets::set(&key, p)?;
                }
                Some(t)
            }
            None => None,
        };
        // El secreto del destino anterior ya no hace falta.
        if let Some(o) = old.filter(|o| target.as_ref().is_none_or(|t| t.kind() != o.kind())) {
            if o.kind() != "folder" {
                crate::secrets::delete(&crate::store::secret_key(&group.id, o.kind()));
            }
        }
        match &target {
            Some(t) => tracing::info!("Relevo por servicio externo: {}", t.kind()),
            None => tracing::info!("Relevo por servicio externo desactivado"),
        }
        {
            let mut s = inner.settings.lock().unwrap();
            s.relay = None;
            s.relay_target = target;
        }
        inner.save_settings();
        inner.relay.lock().unwrap().reset();
        inner.relay_wake.notify_one();
        Ok(())
    }

    /// Qué hacer con lo que ya han recibido todos: borrarlo (por defecto) o
    /// moverlo a `old/` dentro del servicio. Es común a todos los equipos.
    pub fn set_relay_keep_old(&self, keep_old: bool) -> Result<(), String> {
        let inner = &self.inner;
        let (open, keys) = {
            let r = inner.relay.lock().unwrap();
            (r.open_store(), r.keys.clone())
        };
        let (Some((store, sd)), Some(keys)) = (open, keys) else {
            return Err(tr!("El servicio externo no está disponible."));
        };
        let data = serde_json::to_vec(&RelayConfig { keep_old }).unwrap_or_default();
        store.write(&format!("{sd}/config.hfs"), &seal(&keys.enc, &data)).map_err(|e| tr!("No se puede escribir en el servicio externo: {}", e))?;
        inner.relay.lock().unwrap().keep_old = keep_old;
        inner.relay_wake.notify_one();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Bucle del relevo
// ---------------------------------------------------------------------------

pub(crate) async fn relay_loop(inner: Arc<Inner>) {
    let tmp = inner.cfg.data_dir.join("relay-tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    loop {
        let every = Duration::from_secs(inner.cfg.relay_secs.max(1));
        tokio::select! {
            _ = inner.relay_wake.notified() => {
                // Agrupa ráfagas de cambios.
                tokio::time::sleep(every.min(Duration::from_secs(3))).await;
            }
            _ = tokio::time::sleep(every) => {}
        }
        if inner.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        let i = inner.clone();
        let _ = tokio::task::spawn_blocking(move || tick(&i)).await;
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// ¿El índice `idx` ya tiene la versión `e` (o una posterior)?
fn covers(idx: &HashMap<String, FileEntry>, key: &str, e: &FileEntry) -> bool {
    match idx.get(key) {
        Some(m) => (m.hash == e.hash && !m.deleted) || matches!(m.vv.compare(&e.vv), Causality::Equal | Causality::Newer),
        None => false,
    }
}

fn fail(inner: &Inner, msg: String) {
    let mut r = inner.relay.lock().unwrap();
    if r.error.as_ref() != Some(&msg) {
        tracing::warn!("Relevo: {msg}");
    }
    r.error = Some(msg);
    // Se vuelve a abrir (reconectar el NAS, renovar la sesión…) en la próxima vuelta.
    r.store = None;
}

fn tick(inner: &Inner) {
    let Some(target) = target_of(&inner.settings.lock().unwrap()) else {
        inner.relay.lock().unwrap().reset();
        return;
    };
    let (Some(group), Some(folder)) = (inner.group(), inner.folder()) else { return };
    if !folder.scanned_once.load(Ordering::SeqCst) || folder.root_error.lock().unwrap().is_some() {
        return;
    }
    let keys = Keys::new(&group.key_bytes());
    let sd = keys.share_dir(&group.id);
    // Abre el servicio (o reutiliza el ya abierto).
    let current = inner.relay.lock().unwrap().store.as_ref().filter(|(t, _)| *t == target).map(|(_, s)| s.clone());
    let store: Arc<dyn Store> = match current {
        Some(s) => s,
        None => {
            let secret = crate::secrets::get(&crate::store::secret_key(&group.id, target.kind()));
            let s: Arc<dyn Store> = match crate::store::open(&target, secret.as_deref()) {
                Ok(s) => Arc::from(s),
                Err(msg) => return fail(inner, msg),
            };
            if let Err(e) = s.mkdirs(&format!("{sd}/idx")).and_then(|_| s.mkdirs(&format!("{sd}/blobs"))) {
                return fail(inner, tr!("No se puede escribir en el servicio externo: {}", e));
            }
            if !s.exists("LEEME - README.txt").unwrap_or(true) {
                let _ = s.write("LEEME - README.txt", readme().as_bytes());
            }
            let mut r = inner.relay.lock().unwrap();
            r.store = Some((target.clone(), s.clone()));
            r.share_dir = sd.clone();
            s
        }
    };
    inner.relay.lock().unwrap().keys = Some(keys.clone());
    if let Err(e) = sync_once(inner, &folder, store.as_ref(), &keys, &sd) {
        fail(inner, tr!("No se puede escribir en el servicio externo: {}", e));
    }
}

fn sync_once(inner: &Inner, folder: &FolderCtx, store: &dyn Store, keys: &Keys, sd: &str) -> std::io::Result<()> {
    // Opciones comunes.
    let cfg = store
        .read(&format!("{sd}/config.hfs"))
        .ok()
        .and_then(|d| open(&keys.enc, &d).ok())
        .and_then(|d| serde_json::from_slice::<RelayConfig>(&d).ok())
        .unwrap_or_default();

    // 1) Publicar el índice propio (si cambió, o como latido).
    let mine: HashMap<String, FileEntry> = folder.index.lock().unwrap().entries.iter().map(|(k, l)| (k.clone(), l.entry.clone())).collect();
    let my_name = keys.index_name(&inner.my_id);
    {
        let mut entries: Vec<FileEntry> = mine.values().cloned().collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let fingerprint = {
            let mut h = blake3::Hasher::new();
            h.update(&serde_json::to_vec(&entries).unwrap_or_default());
            h.update(&[inner.backup() as u8]);
            h.update(inner.name().as_bytes());
            h.finalize()
        };
        let due = {
            let r = inner.relay.lock().unwrap();
            match r.published {
                Some((f, when)) => f != fingerprint || when.elapsed().map_or(true, |e| e > HEARTBEAT),
                None => true,
            }
        };
        if due {
            let pi = PublishedIndex { v: 1, device: inner.my_id.clone(), name: inner.name(), backup: inner.backup(), updated: now_secs(), entries };
            let data = serde_json::to_vec(&pi).unwrap_or_default();
            store.write(&format!("{sd}/idx/{my_name}"), &seal(&keys.enc, &data))?;
            inner.relay.lock().unwrap().published = Some((fingerprint, SystemTime::now()));
        }
    }

    // 2) Leer los índices de los demás (solo los que han cambiado).
    let mut present = HashSet::new();
    for it in store.list(&format!("{sd}/idx"))? {
        let fname = it.name.clone();
        if !fname.ends_with(".hfs") || fname == my_name {
            continue;
        }
        present.insert(fname.clone());
        let stamp = (it.modified, it.size);
        if inner.relay.lock().unwrap().seen.get(&fname) == Some(&stamp) {
            continue;
        }
        let parsed = store
            .read(&format!("{sd}/idx/{fname}"))
            .ok()
            .and_then(|d| open(&keys.enc, &d).ok())
            .and_then(|d| serde_json::from_slice::<PublishedIndex>(&d).ok());
        // El nombre del archivo debe corresponder al equipo que dice ser.
        let Some(pi) = parsed.filter(|p| keys.index_name(&p.device) == fname && p.device != inner.my_id) else {
            continue; // incompleto (la nube aún lo está bajando) o ajeno: se reintenta
        };
        let mut r = inner.relay.lock().unwrap();
        r.seen.insert(fname, stamp);
        let entries = pi.entries.into_iter().map(|e| (e.key(), e)).collect();
        if !r.others.contains_key(&pi.device) {
            tracing::info!("Relevo: «{}» también usa el servicio externo", pi.name);
        }
        r.others.insert(pi.device.clone(), RemoteIdx { name: pi.name.clone(), backup: pi.backup, updated: pi.updated, entries });
        drop(r);
        // Un equipo que publica en el relevo tiene la clave del grupo: es miembro.
        if inner.add_member(&pi.device, &pi.name) {
            inner.event("relay", tr!("«{}» se ha unido a través del servicio externo", pi.name));
        }
    }
    {
        let mut r = inner.relay.lock().unwrap();
        let k = keys.clone();
        r.others.retain(|id, _| present.contains(&k.index_name(id)));
        r.seen.retain(|f, _| present.contains(f));
    }

    // 3) Blobs presentes.
    let mut blobs: HashMap<String, (u64, SystemTime)> =
        store.list(&format!("{sd}/blobs"))?.into_iter().filter(|i| i.name.ends_with(".hfs")).map(|i| (i.name, (i.size, i.modified))).collect();

    // 4) Qué falta a quién.
    let live: HashMap<String, (String, Option<HashMap<String, FileEntry>>)> = inner
        .peers
        .lock()
        .unwrap()
        .iter()
        .map(|(id, p)| (id.clone(), (p.name.clone(), (p.conn.is_some() && p.index_ready).then(|| p.index.clone()))))
        .collect();
    let (needed, waiting, known_names) = {
        let r = inner.relay.lock().unwrap();
        let now = now_secs();
        // Destinatarios: equipos que usan el relevo y lo han actualizado hace poco.
        let recipients: Vec<(&String, &RemoteIdx)> =
            r.others.iter().filter(|(_, o)| now.saturating_sub(o.updated) < STALE_AFTER.as_secs()).collect();
        // Versiones vigentes en algún sitio (posibles contenidos a entregar).
        // Lo de un equipo de copia de seguridad no se entrega a nadie.
        let mut current: Vec<(&String, &FileEntry)> = if inner.backup() { Vec::new() } else { mine.iter().filter(|(_, e)| !e.deleted).collect() };
        for o in r.others.values().filter(|o| !o.backup) {
            current.extend(o.entries.iter().filter(|(_, e)| !e.deleted));
        }
        for (_, idx) in live.values() {
            if let Some(idx) = idx {
                current.extend(idx.iter().filter(|(_, e)| !e.deleted));
            }
        }
        let mut needed: HashMap<String, Vec<String>> = HashMap::new(); // hash → equipos sin conexión que lo necesitan
        let mut waiting: HashSet<String> = HashSet::new();
        let mut known_names: HashSet<String> = HashSet::new();
        for (key, e) in &current {
            known_names.insert(keys.blob_name(&e.hash));
            if !covers(&mine, key, e) {
                needed.entry(e.hash.clone()).or_default();
            }
            for (id, o) in &recipients {
                let live_idx = live.get(*id).and_then(|(_, i)| i.as_ref());
                if covers(&o.entries, key, e) || live_idx.is_some_and(|i| covers(i, key, e)) {
                    continue;
                }
                let v = needed.entry(e.hash.clone()).or_default();
                if live_idx.is_none() {
                    v.push((*id).clone());
                    waiting.insert(o.name.clone());
                }
            }
        }
        (needed, waiting, known_names)
    };

    // 5) Subir lo que falta a equipos desconectados y solo está aquí.
    let mut uploads: Vec<(String, FileEntry)> = mine
        .iter()
        .filter(|(_, e)| !e.deleted && needed.get(&e.hash).is_some_and(|v| !v.is_empty()) && !blobs.contains_key(&keys.blob_name(&e.hash)))
        .map(|(k, e)| (k.clone(), e.clone()))
        .collect();
    uploads.sort_by_key(|(_, e)| e.size);
    let mut seen_hashes = HashSet::new();
    for (key, e) in uploads {
        if !seen_hashes.insert(e.hash.clone()) || inner.shutting_down.load(Ordering::SeqCst) || target_of(&inner.settings.lock().unwrap()).is_none() {
            continue;
        }
        let name = keys.blob_name(&e.hash);
        match upload(inner, folder, store, keys, sd, &key, &e) {
            Ok(size) => {
                blobs.insert(name, (size, SystemTime::now()));
                inner.event("relay", tr!("Guardado en el servicio externo: {}", e.path));
            }
            Err(msg) => {
                tracing::warn!("Relevo: no se pudo subir «{}»: {msg}", e.path);
            }
        }
    }

    // 6) Limpiar lo que ya tienen todos.
    let needed_names: HashSet<String> = needed.keys().map(|h| keys.blob_name(h)).collect();
    let keep_old = cfg.keep_old;
    let old_dir = format!("{sd}/old/{}", chrono::Local::now().format("%Y-%m-%d"));
    let mut stored = (0usize, 0u64);
    let mut removed = HashSet::new();
    for (fname, (size, mtime)) in &blobs {
        let age = mtime.elapsed().unwrap_or(Duration::ZERO);
        let removable = !needed_names.contains(fname) && (known_names.contains(fname) || age > ORPHAN_GRACE);
        if !removable {
            stored.0 += 1;
            stored.1 += size;
            continue;
        }
        let src = format!("{sd}/blobs/{fname}");
        let res = if keep_old { store.mkdirs(&old_dir).and_then(|_| store.rename(&src, &format!("{old_dir}/{fname}"))) } else { store.delete(&src) };
        match res {
            Ok(()) => {
                removed.insert(fname.clone());
                tracing::info!("Relevo: contenido ya entregado a todos {} ({size} bytes)", if keep_old { "movido a old/" } else { "eliminado" });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                removed.insert(fname.clone());
            }
            Err(e) => {
                tracing::warn!("Relevo: no se pudo limpiar {fname}: {e}");
                stored.0 += 1;
                stored.1 += size;
            }
        }
    }
    let present_blobs: HashSet<String> = blobs.keys().filter(|f| !removed.contains(*f)).cloned().collect();
    let had_new_blobs = {
        let mut r = inner.relay.lock().unwrap();
        let grew = present_blobs.iter().any(|b| !r.blobs.contains(b));
        r.blobs = present_blobs;
        r.keep_old = keep_old;
        r.error = None;
        r.stored_files = stored.0;
        r.stored_bytes = stored.1;
        let mut w: Vec<String> = waiting.into_iter().collect();
        w.sort();
        r.waiting = w;
        grew || !r.others.is_empty()
    };
    if had_new_blobs {
        inner.wake.notify_one(); // el planificador puede tener algo que bajar
    }
    Ok(())
}

/// Cifra y sube un archivo local. Comprueba que lo subido es exactamente la
/// versión indexada (si el archivo cambió mientras tanto, se descarta).
fn upload(inner: &Inner, folder: &FolderCtx, store: &dyn Store, keys: &Keys, sd: &str, key: &str, e: &FileEntry) -> Result<u64, String> {
    let (disk, size, mtime) = {
        let idx = folder.index.lock().unwrap();
        let l = idx.get(key).ok_or_else(|| tr!("ya no está en el índice"))?;
        if l.entry.hash != e.hash {
            return Err(tr!("el estado local cambió"));
        }
        (crate::pathsafe::to_local(&folder.root, &l.disk), l.disk_size, l.disk_mtime_ns)
    };
    let f = crate::fsops::open_shared(&disk).map_err(|err| err.to_string())?;
    let m = f.metadata().map_err(|err| err.to_string())?;
    if m.len() != size || crate::scanner::mtime_ns(&m) != mtime {
        folder.request_scan();
        return Err(tr!("el estado local cambió"));
    }
    // Se cifra a un temporal local y después se sube.
    let tmp_dir = inner.cfg.data_dir.join("relay-tmp");
    std::fs::create_dir_all(&tmp_dir).map_err(|err| err.to_string())?;
    let tmp = tmp_dir.join(format!("{}.{}.tmp", keys.blob_name(&e.hash), hex::encode(crate::auth::random_bytes::<4>())));
    let tid = crate::transfer::add_transfer(inner, &e.path, &tr!("servicio externo"), false, e.size, 0);
    let res = (|| -> Result<u64, String> {
        if let Some(free) = crate::fsops::available_space(&tmp_dir) {
            if free < e.size + 64 * 1024 * 1024 {
                return Err(tr!("no hay espacio suficiente en el disco"));
            }
        }
        let out = std::fs::File::create(&tmp).map_err(|err| err.to_string())?;
        let mut w = std::io::BufWriter::with_capacity(CHUNK, out);
        let (n, hash) = encrypt_stream(&keys.enc, std::io::BufReader::with_capacity(CHUNK, f), &mut w, |d| {
            // La mitad del progreso es cifrar; la otra, subir.
            crate::transfer::progress(inner, tid, d / 2, None);
        })
        .map_err(|err| err.to_string())?;
        let out = w.into_inner().map_err(|err| err.to_string())?;
        out.sync_all().map_err(|err| err.to_string())?;
        drop(out);
        if hash != e.hash || n != e.size {
            folder.request_scan();
            return Err(tr!("el estado local cambió"));
        }
        let len = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(n);
        store.upload(&format!("{sd}/blobs/{}", keys.blob_name(&e.hash)), &tmp).map_err(|err| err.to_string())?;
        crate::transfer::progress(inner, tid, e.size, None);
        Ok(len)
    })();
    crate::transfer::remove_transfer(inner, tid);
    let _ = std::fs::remove_file(&tmp);
    if res.is_ok() {
        inner.stats.lock().unwrap().bytes_up += e.size;
    }
    res
}

/// Descarga (descifra) un contenido del relevo en el temporal de la descarga.
pub(crate) fn fetch(inner: &Inner, hash: &str, temp: &Path, tid: u64) -> Result<(), String> {
    let (open_store, keys) = {
        let r = inner.relay.lock().unwrap();
        (r.open_store(), r.keys.clone())
    };
    let (Some((store, sd)), Some(keys)) = (open_store, keys) else { return Err(tr!("El servicio externo no está disponible.")) };
    let src = store.reader(&format!("{sd}/blobs/{}", keys.blob_name(hash))).map_err(|e| e.to_string())?;
    crate::transfer::progress(inner, tid, 0, Some(&tr!("servicio externo")));
    let out = std::fs::File::create(temp).map_err(|e| e.to_string())?;
    let mut w = std::io::BufWriter::with_capacity(CHUNK, out);
    let n = decrypt_stream(&keys.enc, std::io::BufReader::with_capacity(CHUNK + TAG, src), &mut w, |d| crate::transfer::progress(inner, tid, d, None))
        .map_err(|e| e.to_string())?;
    let out = w.into_inner().map_err(|e| e.to_string())?;
    out.sync_all().map_err(|e| e.to_string())?;
    inner.stats.lock().unwrap().bytes_down += n;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_roundtrip_and_tamper() {
        let key = [7u8; 32];
        for size in [0usize, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK] {
            let data: Vec<u8> = (0..size).map(|i| (i * 31 % 251) as u8).collect();
            let mut ct = Vec::new();
            let (n, h) = encrypt_stream(&key, &data[..], &mut ct, |_| {}).unwrap();
            assert_eq!(n as usize, size);
            assert_eq!(h, blake3::hash(&data).to_hex().to_string());
            assert_eq!(open(&key, &ct).unwrap(), data, "tamaño {size}");
            // Cualquier byte alterado: se detecta.
            let mut bad = ct.clone();
            let pos = bad.len() - 1;
            bad[pos] ^= 1;
            assert!(open(&key, &bad).is_err());
            // Otra clave: se detecta.
            assert!(open(&[8u8; 32], &ct).is_err());
        }
    }

    #[test]
    fn truncation_at_chunk_boundary_is_detected() {
        let key = [1u8; 32];
        let data = vec![5u8; 2 * CHUNK + 10];
        let ct = seal(&key, &data);
        assert!(open(&key, &ct[..HEADER + CHUNK + TAG]).is_err());
        assert!(open(&key, &ct[..HEADER + 2 * (CHUNK + TAG)]).is_err());
        assert!(open(&key, &ct[..ct.len() - 3]).is_err());
    }

    #[test]
    fn names_are_opaque_and_stable() {
        let k = Keys::new(&[3u8; 32]);
        let h = "a".repeat(64);
        assert_eq!(k.blob_name(&h), Keys::new(&[3u8; 32]).blob_name(&h));
        assert_ne!(k.blob_name(&h), Keys::new(&[4u8; 32]).blob_name(&h));
        assert!(!k.blob_name(&h).contains(&h[..16]));
        assert!(k.share_dir("grupo").starts_with("hfs-"));
    }

    #[test]
    fn coverage() {
        let mut vv1 = crate::vv::VersionVector::new();
        vv1.bump(1);
        let mut vv2 = vv1.clone();
        vv2.bump(2);
        let e = |h: &str, vv: &crate::vv::VersionVector, deleted| FileEntry {
            path: "a.txt".into(),
            size: 1,
            mtime_ns: 0,
            hash: h.into(),
            vv: vv.clone(),
            deleted,
            modified_by: String::new(),
        };
        let mut idx = HashMap::new();
        assert!(!covers(&idx, "a.txt", &e("x", &vv1, false)));
        idx.insert("a.txt".to_string(), e("x", &vv1, false));
        assert!(covers(&idx, "a.txt", &e("x", &vv1, false)));
        assert!(!covers(&idx, "a.txt", &e("y", &vv2, false)));
        idx.insert("a.txt".to_string(), e("", &vv2, true));
        assert!(covers(&idx, "a.txt", &e("x", &vv1, false)), "un borrado posterior también la cubre");
    }
}
