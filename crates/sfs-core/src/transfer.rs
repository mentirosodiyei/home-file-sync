//! Transferencias de archivos.
//!
//! **Modelo "pull"**: cada equipo decide qué le falta y lo pide. Así nunca se
//! envía nada que el receptor no necesite, y un archivo puede venir de
//! cualquier equipo que tenga exactamente ese contenido (no solo del que lo
//! creó). Con muchos equipos, los primeros en recibirlo pasan a servirlo
//! también, y cada equipo atiende como mucho `max_uploads` descargas a la vez:
//! la carga se reparte en lugar de saturar al equipo de origen.
//!
//! **Descarga**: por bloques con varias peticiones en vuelo, a un temporal
//! `.showfilesync/tmp/<hash>.sfspart`. Si la conexión se corta, el temporal se
//! conserva y la descarga continúa donde se quedó (con ese u otro equipo).
//! Al terminar se verifica el hash BLAKE3 completo; si no coincide, se
//! descarta y se vuelve a pedir. Solo entonces se coloca con un `rename`.

use crate::decide::{better, conflict_name, decide, Action};
use crate::engine::{ConnHandle, FileError, FolderCtx, Inner, Transfer};
use crate::fsops;
use crate::index::{FileEntry, LocalEntry};
use crate::pathsafe::{key_of, locally_representable, to_local};
use crate::protocol::Msg;
use crate::scanner::hash_file;
use crate::vv::VersionVector;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::time::timeout;

// ---------------------------------------------------------------------------
// Progreso
// ---------------------------------------------------------------------------

fn add_transfer(inner: &Inner, path: &str, peer: &str, download: bool, size: u64, done: u64) -> u64 {
    let id = inner.next_transfer.fetch_add(1, Ordering::Relaxed);
    inner.activity.lock().unwrap().transfers.insert(
        id,
        Transfer { path: path.into(), peer: peer.into(), download, size, done, rate: 0.0, sample: (Instant::now(), done) },
    );
    id
}

fn progress(inner: &Inner, id: u64, done: u64, peer: Option<&str>) {
    let mut a = inner.activity.lock().unwrap();
    if let Some(t) = a.transfers.get_mut(&id) {
        t.done = done;
        if let Some(p) = peer {
            t.peer = p.to_string();
        }
        let dt = t.sample.0.elapsed().as_secs_f64();
        if dt >= 0.5 {
            let inst = done.saturating_sub(t.sample.1) as f64 / dt;
            t.rate = if t.rate == 0.0 { inst } else { 0.6 * inst + 0.4 * t.rate };
            t.sample = (Instant::now(), done);
        }
    }
}

fn remove_transfer(inner: &Inner, id: u64) {
    inner.activity.lock().unwrap().transfers.remove(&id);
}

// ---------------------------------------------------------------------------
// Subidas (servir archivos a otro equipo)
// ---------------------------------------------------------------------------

struct UploadGuard {
    inner: Arc<Inner>,
    tid: u64,
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        self.inner.active_uploads.fetch_sub(1, Ordering::Relaxed);
        remove_transfer(&self.inner, self.tid);
    }
}

/// Sesiones de subida de una conexión.
pub(crate) struct Uploads {
    inner: Arc<Inner>,
    conn: ConnHandle,
    peer_name: String,
    sessions: HashMap<u32, mpsc::UnboundedSender<(u64, u32)>>,
}

impl Uploads {
    pub fn new(inner: Arc<Inner>, conn: ConnHandle, peer_name: String) -> Self {
        Uploads { inner, conn, peer_name, sessions: HashMap::new() }
    }

    pub fn open(&mut self, id: u32, path: String, hash: String) {
        let inner = self.inner.clone();
        let conn = self.conn.clone();
        let reply = |m: Msg| {
            let _ = conn.ctrl.send(m);
        };
        if self.sessions.len() >= 16 {
            return reply(Msg::DlErr { id, msg: "demasiadas sesiones".into() });
        }
        // Límite global: reservamos plaza antes de abrir.
        let max = inner.cfg.max_uploads;
        if inner.active_uploads.fetch_add(1, Ordering::Relaxed) >= max {
            inner.active_uploads.fetch_sub(1, Ordering::Relaxed);
            return reply(Msg::DlBusy { id });
        }
        // Solo se sirve lo que está en el índice con ese hash: nunca una ruta arbitraria.
        let found = inner.folder().and_then(|f| {
            let idx = f.index.lock().unwrap();
            idx.get(&key_of(&path))
                .filter(|le| !le.entry.deleted && le.entry.hash == hash)
                .map(|le| (to_local(&f.root, &le.disk), le.disk_size, le.disk_mtime_ns))
        });
        let Some((local, size, mtime)) = found else {
            inner.active_uploads.fetch_sub(1, Ordering::Relaxed);
            return reply(Msg::DlErr { id, msg: "archivo no disponible".into() });
        };
        let tid = add_transfer(&inner, &path, &self.peer_name, false, size, 0);
        tracing::info!("Envío de «{path}» ({size} bytes) a «{}»", self.peer_name);
        let guard = UploadGuard { inner: inner.clone(), tid };
        let (tx, mut rx) = mpsc::unbounded_channel::<(u64, u32)>();
        self.sessions.insert(id, tx);
        tokio::spawn(async move {
            let _guard = guard;
            let opened = tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
                let f = fsops::open_shared(&local)?;
                let m = f.metadata()?;
                if m.len() != size || crate::scanner::mtime_ns(&m) != mtime {
                    return Err(std::io::Error::other("el archivo ha cambiado"));
                }
                Ok(f)
            })
            .await;
            let file = match opened {
                Ok(Ok(f)) => Arc::new(f),
                Ok(Err(e)) => {
                    let _ = conn.ctrl.send(Msg::DlErr { id, msg: e.to_string() });
                    return;
                }
                Err(_) => return,
            };
            let _ = conn.ctrl.send(Msg::DlOpenOk { id, size });
            let mut sent_max = 0u64;
            // Peticiones atendidas en orden: el receptor escribe secuencialmente.
            while let Some((offset, len)) = timeout(Duration::from_secs(60), rx.recv()).await.ok().flatten() {
                let len = len.min(1 << 20) as usize;
                if offset >= size {
                    let _ = conn.ctrl.send(Msg::DlErr { id, msg: "posición fuera de rango".into() });
                    break;
                }
                let f = file.clone();
                let n = len.min((size - offset) as usize);
                let data = tokio::task::spawn_blocking(move || {
                    let mut buf = vec![0u8; n];
                    let r = fsops::read_at(&f, &mut buf, offset)?;
                    buf.truncate(r);
                    Ok::<_, std::io::Error>(buf)
                })
                .await;
                let data = match data {
                    Ok(Ok(d)) if !d.is_empty() => d,
                    _ => {
                        let _ = conn.ctrl.send(Msg::DlErr { id, msg: "error de lectura".into() });
                        break;
                    }
                };
                let dl = data.len() as u64;
                if conn.data.send(Msg::Block { id, offset, data }).await.is_err() {
                    break;
                }
                inner.stats.lock().unwrap().bytes_up += dl;
                sent_max = sent_max.max(offset + dl);
                progress(&inner, tid, sent_max, None);
            }
        });
    }

    pub fn request(&mut self, id: u32, offset: u64, len: u32) {
        match self.sessions.get(&id) {
            Some(tx) => {
                let _ = tx.send((offset, len));
            }
            None => {
                let _ = self.conn.ctrl.send(Msg::DlErr { id, msg: "sesión desconocida".into() });
            }
        }
    }

    pub fn close(&mut self, id: u32) {
        self.sessions.remove(&id);
    }

    pub fn close_all(&mut self) {
        self.sessions.clear();
    }
}

// ---------------------------------------------------------------------------
// Planificador
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) struct Need {
    pub key: String,
    pub remote: FileEntry,
    pub sources: Vec<String>,
}

/// Calcula qué falta. Aplica en el momento las fusiones de versión (sin datos).
fn plan(inner: &Inner, folder: &FolderCtx) -> (Vec<Need>, Vec<Need>) {
    let mut best: HashMap<String, (FileEntry, Vec<String>)> = HashMap::new();
    {
        let peers = inner.peers.lock().unwrap();
        for (pid, p) in peers.iter() {
            if p.conn.is_none() || !p.index_ready {
                continue;
            }
            for (k, e) in &p.index {
                let src = if e.deleted { vec![] } else { vec![pid.clone()] };
                match best.get_mut(k) {
                    None => {
                        best.insert(k.clone(), (e.clone(), src));
                    }
                    Some((b, srcs)) => {
                        if e.hash == b.hash && e.deleted == b.deleted {
                            srcs.push(pid.clone());
                            if better(e, b) {
                                let vv = b.vv.clone();
                                *b = e.clone();
                                b.vv.merge(&vv);
                            } else {
                                b.vv.merge(&e.vv);
                            }
                        } else if better(e, b) {
                            *b = e.clone();
                            *srcs = src;
                        }
                    }
                }
            }
        }
    }
    let mut needs = Vec::new();
    let mut deletes = Vec::new();
    let mut merged = Vec::new();
    {
        let mut idx = folder.index.lock().unwrap();
        for (k, (remote, sources)) in best {
            let local = idx.get(&k).map(|l| &l.entry);
            match decide(local, &remote) {
                Action::Nothing => {}
                Action::MergeVersion => {
                    if let Some(le) = idx.entries.get_mut(&k) {
                        le.entry.vv.merge(&remote.vv);
                        merged.push(le.entry.clone());
                        idx.dirty = true;
                    }
                }
                Action::Download | Action::DownloadConflict => needs.push(Need { key: k, remote, sources }),
                Action::Delete => deletes.push(Need { key: k, remote, sources }),
                Action::AdoptTombstone => {
                    idx.put(LocalEntry { entry: remote, disk: String::new(), disk_size: 0, disk_mtime_ns: 0 });
                }
            }
        }
    }
    if !merged.is_empty() {
        inner.broadcast_entries(merged);
    }
    needs.sort_by(|a, b| a.remote.size.cmp(&b.remote.size).then_with(|| a.remote.path.cmp(&b.remote.path)));
    (needs, deletes)
}

/// Aplica un borrado recibido: mueve el archivo a la papelera interna y
/// registra el borrado con la versión remota. Comprueba antes que el archivo
/// local no ha cambiado desde que se decidió (si cambió, gana la edición).
fn apply_delete(inner: &Inner, folder: &FolderCtx, need: &Need) -> Result<FileEntry, String> {
    let root = &folder.root;
    let mut idx = folder.index.lock().unwrap();
    let cur = idx.get(&need.key).cloned().ok_or("ya no está en el índice")?;
    if decide(Some(&cur.entry), &need.remote) != Action::Delete {
        return Err("el estado local cambió".into());
    }
    let m = std::fs::metadata(to_local(root, &cur.disk)).ok();
    if m.as_ref().map(|m| (m.len(), crate::scanner::mtime_ns(m))) != Some((cur.disk_size, cur.disk_mtime_ns)) {
        folder.request_scan();
        return Err("el archivo se ha modificado aquí; se conserva".into());
    }
    let trashed = fsops::move_to_trash(root, &cur.disk).map_err(|e| {
        if is_locked(&e) {
            "el archivo está abierto en otra aplicación; se eliminará cuando se cierre".to_string()
        } else {
            format!("no se puede eliminar: {e}")
        }
    })?;
    fsops::remove_empty_parents(root, &cur.disk);
    {
        let mut t = inner.recent_trash.lock().unwrap();
        t.retain(|_, (_, _, when)| when.elapsed() < Duration::from_secs(600));
        t.insert(cur.entry.hash.clone(), (trashed, cur.entry.size, Instant::now()));
    }
    let mut entry = need.remote.clone();
    entry.vv.merge(&cur.entry.vv);
    idx.put(LocalEntry { entry: entry.clone(), disk: String::new(), disk_size: 0, disk_mtime_ns: 0 });
    Ok(entry)
}

pub(crate) async fn scheduler_loop(inner: Arc<Inner>) {
    loop {
        tokio::select! {
            _ = inner.wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
        let Some(folder) = inner.folder() else { continue };
        if inner.group().is_none() || !folder.scanned_once.load(Ordering::SeqCst) || folder.root_error.lock().unwrap().is_some() {
            continue;
        }
        let (needs, deletes) = plan(&inner, &folder);
        let now = Instant::now();
        // Borrados: si el mismo contenido hace falta para una descarga pendiente
        // (archivo renombrado o movido), se espera a que se copie en local.
        let wanted: std::collections::HashSet<String> = needs.iter().map(|n| n.remote.hash.clone()).collect();
        let mut done = Vec::new();
        for d in deletes {
            let local_hash = folder.index.lock().unwrap().get(&d.key).map(|l| l.entry.hash.clone());
            if local_hash.is_some_and(|h| wanted.contains(&h)) {
                continue;
            }
            if inner.activity.lock().unwrap().errors.get(&d.key).is_some_and(|e| e.retry_at > now) {
                continue;
            }
            let (i, f, dd) = (inner.clone(), folder.clone(), d.clone());
            match tokio::task::spawn_blocking(move || apply_delete(&i, &f, &dd)).await {
                Ok(Ok(entry)) => {
                    inner.event("delete", format!("Eliminado {} (borrado en «{}»; copia en la papelera interna)", d.remote.path, d.remote.modified_by));
                    done.push(entry);
                }
                Ok(Err(msg)) => set_error(&inner, &d, msg, Duration::from_secs(10), true),
                Err(_) => {}
            }
        }
        inner.broadcast_entries(done);
        {
            let mut a = inner.activity.lock().unwrap();
            // Olvidamos errores de archivos que ya no hacen falta.
            let needed: std::collections::HashSet<&String> = needs.iter().map(|n| &n.key).collect();
            a.errors.retain(|k, _| needed.contains(k));
            a.pending_files = needs.len() as u32;
            a.pending_bytes = needs.iter().map(|n| n.remote.size).sum();
        }
        for need in needs {
            if inner.active_downloads.lock().unwrap().len() >= inner.cfg.max_downloads {
                break;
            }
            if inner.active_downloads.lock().unwrap().contains_key(&need.key) {
                continue;
            }
            if inner.activity.lock().unwrap().errors.get(&need.key).is_some_and(|e| e.retry_at > now) {
                continue;
            }
            if let Err(msg) = locally_representable(&need.remote.path) {
                set_error(&inner, &need, msg, Duration::from_secs(600), true);
                continue;
            }
            // Si en disco hay algo que el índice aún no refleja (archivo recién
            // copiado o editado que el escáner está esperando a que se estabilice),
            // se espera: puede ser el mismo contenido y no hace falta transferirlo.
            if pending_local_change(&folder, &need) {
                folder.request_scan();
                continue;
            }
            inner.active_downloads.lock().unwrap().insert(need.key.clone(), need.remote.hash.clone());
            let i = inner.clone();
            let f = folder.clone();
            tokio::spawn(async move {
                let key = need.key.clone();
                download_job(i.clone(), f, need).await;
                i.active_downloads.lock().unwrap().remove(&key);
                i.wake.notify_one();
            });
        }
    }
}

fn pending_local_change(folder: &FolderCtx, need: &Need) -> bool {
    let idx = folder.index.lock().unwrap();
    match idx.get(&need.key).filter(|l| !l.entry.deleted) {
        None => to_local(&folder.root, &need.remote.path).exists(),
        Some(l) => {
            let m = std::fs::metadata(to_local(&folder.root, &l.disk)).ok();
            m.map(|m| (m.len(), crate::scanner::mtime_ns(&m))) != Some((l.disk_size, l.disk_mtime_ns))
        }
    }
}

fn set_error(inner: &Inner, need: &Need, message: String, base: Duration, visible: bool) {
    let mut a = inner.activity.lock().unwrap();
    let attempts = a.errors.get(&need.key).map(|e| e.attempts + 1).unwrap_or(1);
    let backoff = if visible { (base * 2u32.saturating_pow(attempts.min(6) - 1)).min(Duration::from_secs(300)) } else { base };
    if visible && attempts == 1 {
        a.event("error", format!("{}: {message}", need.remote.path));
    }
    a.errors.insert(
        need.key.clone(),
        FileError { path: need.remote.path.clone(), message, retry_at: Instant::now() + backoff, attempts, visible },
    );
}

// ---------------------------------------------------------------------------
// Descarga
// ---------------------------------------------------------------------------

enum DlError {
    /// Todos los equipos ocupados o desconectados: reintentar pronto, sin mostrar error.
    Busy,
    /// El estado local cambió: se vuelve a evaluar.
    Stale,
    Fail(String),
}

async fn download_job(inner: Arc<Inner>, folder: Arc<FolderCtx>, need: Need) {
    let first_peer = need.sources.first().and_then(|p| inner.peers.lock().unwrap().get(p).map(|p| p.name.clone())).unwrap_or_default();
    let temp = fsops::temp_path(&folder.root, &need.remote.hash);
    let already = std::fs::metadata(&temp).map(|m| m.len()).unwrap_or(0).min(need.remote.size);
    let tid = add_transfer(&inner, &need.remote.path, &first_peer, true, need.remote.size, already);
    tracing::info!(
        "Descarga de «{}» ({} bytes, {} fuente(s)){}",
        need.remote.path,
        need.remote.size,
        need.sources.len(),
        if already > 0 { format!(", reanudando desde {already} bytes") } else { String::new() }
    );
    let result = download(&inner, &folder, &need, tid).await;
    remove_transfer(&inner, tid);
    match result {
        Ok(conflict) => {
            inner.activity.lock().unwrap().errors.remove(&need.key);
            let mut a = inner.activity.lock().unwrap();
            match conflict {
                Some(copy) => {
                    a.conflicts += 1;
                    a.event(
                        "conflict",
                        format!("Conflicto en «{}»: se ha recibido la versión más reciente y la de este equipo se conserva como «{copy}»", need.remote.path),
                    );
                }
                None => a.event("recv", format!("Recibido {}", need.remote.path)),
            }
        }
        Err(DlError::Busy) => set_error(&inner, &need, "esperando a un equipo libre".into(), Duration::from_millis(1500), false),
        Err(DlError::Stale) => {
            folder.request_scan();
            set_error(&inner, &need, "cambió en este equipo; se vuelve a evaluar".into(), Duration::from_secs(3), false);
        }
        Err(DlError::Fail(msg)) => {
            tracing::warn!("Descarga de «{}» fallida: {msg}", need.remote.path);
            set_error(&inner, &need, msg, Duration::from_secs(5), true)
        }
    }
}

async fn download(inner: &Arc<Inner>, folder: &Arc<FolderCtx>, need: &Need, tid: u64) -> Result<Option<String>, DlError> {
    let temp = fsops::temp_path(&folder.root, &need.remote.hash);

    // 1) ¿Tenemos ya ese contenido en otra ruta? Copia local, sin red.
    let local_src = {
        let idx = folder.index.lock().unwrap();
        idx.find_by_hash(&need.remote.hash).map(|le| (to_local(&folder.root, &le.disk), le.disk_size, le.disk_mtime_ns))
    };
    let mut have_data = false;
    // ¿Acaba de ir a la papelera con otro nombre? (renombrado o movido)
    let trashed = inner.recent_trash.lock().unwrap().remove(&need.remote.hash);
    if let Some((src, size, _)) = trashed {
        let t = temp.clone();
        let ok = tokio::task::spawn_blocking(move || {
            std::fs::metadata(&src).is_ok_and(|m| m.len() == size) && std::fs::rename(&src, &t).is_ok()
        })
        .await
        .unwrap_or(false);
        if ok {
            have_data = true;
            inner.stats.lock().unwrap().local_copies += 1;
            tracing::info!("«{}» recuperado de la papelera interna (renombrado o movido): sin transferencia", need.remote.path);
        }
    }
    if let (false, Some((src, size, mtime))) = (have_data, local_src) {
        let t = temp.clone();
        let ok = tokio::task::spawn_blocking(move || -> std::io::Result<bool> {
            let m = std::fs::metadata(&src)?;
            if m.len() != size || crate::scanner::mtime_ns(&m) != mtime {
                return Ok(false);
            }
            std::fs::copy(&src, &t)?;
            Ok(true)
        })
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or(false);
        if ok {
            have_data = true;
            inner.stats.lock().unwrap().local_copies += 1;
        }
    }

    // 2) Por la red.
    if !have_data {
        let already = std::fs::metadata(&temp).map(|m| m.len()).unwrap_or(0);
        let remaining = need.remote.size.saturating_sub(already);
        if let Some(free) = fsops::available_space(&folder.root) {
            if free < remaining + 64 * 1024 * 1024 {
                return Err(DlError::Fail("no hay espacio suficiente en el disco".into()));
            }
        }
        // Fuentes ordenadas por carga (menos subidas en curso primero).
        let mut sources: Vec<(u32, String, String, ConnHandle)> = {
            let peers = inner.peers.lock().unwrap();
            need.sources
                .iter()
                .filter_map(|id| {
                    let p = peers.get(id)?;
                    Some((p.status.uploads, id.clone(), p.name.clone(), p.conn.clone()?))
                })
                .collect()
        };
        sources.sort_by_key(|s| (s.0, rand_u32()));
        let mut last_err = None;
        let mut done = false;
        for (_, _, name, conn) in sources {
            match fetch(inner, conn, need, &temp, tid, &name).await {
                Ok(()) => {
                    done = true;
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        if !done {
            return Err(match last_err {
                Some(DlError::Fail(m)) => DlError::Fail(m),
                _ => DlError::Busy,
            });
        }
    }

    // 3) Verificación de integridad.
    let t = temp.clone();
    let hash = tokio::task::spawn_blocking(move || hash_file(&t)).await.map_err(|e| DlError::Fail(e.to_string()))?;
    match hash {
        Ok(h) if h == need.remote.hash => {}
        _ => {
            let _ = std::fs::remove_file(&temp);
            inner.stats.lock().unwrap().hash_failures += 1;
            return Err(DlError::Fail("los datos recibidos no superan la verificación; se descargará de nuevo".into()));
        }
    }

    // 4) Colocación atómica.
    let i = inner.clone();
    let f = folder.clone();
    let n = need.clone();
    let res = tokio::task::spawn_blocking(move || place(&i, &f, &n, &temp)).await.map_err(|e| DlError::Fail(e.to_string()))?;
    let (changed, conflict) = res?;
    inner.stats.lock().unwrap().files_received += 1;
    inner.broadcast_entries(changed);
    Ok(conflict)
}

fn rand_u32() -> u32 {
    u32::from_le_bytes(crate::auth::random_bytes::<4>())
}

/// Descarga el temporal desde un equipo concreto, reanudando si ya existe.
async fn fetch(inner: &Arc<Inner>, conn: ConnHandle, need: &Need, temp: &std::path::Path, tid: u64, peer_name: &str) -> Result<(), DlError> {
    let id = conn.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::unbounded_channel();
    conn.routes.lock().unwrap().insert(id, tx);
    struct Route(ConnHandle, u32);
    impl Drop for Route {
        fn drop(&mut self) {
            self.0.routes.lock().unwrap().remove(&self.1);
            let _ = self.0.ctrl.send(Msg::DlClose { id: self.1 });
        }
    }
    let _route = Route(conn.clone(), id);
    let lost = || DlError::Fail("conexión perdida".into());

    conn.ctrl.send(Msg::DlOpen { id, path: need.remote.path.clone(), hash: need.remote.hash.clone() }).map_err(|_| lost())?;
    let size = match timeout(Duration::from_secs(20), rx.recv()).await {
        Ok(Some(Msg::DlOpenOk { size, .. })) => size,
        Ok(Some(Msg::DlBusy { .. })) => return Err(DlError::Busy),
        Ok(Some(Msg::DlErr { msg, .. })) => {
            tracing::debug!("{peer_name} no sirve {}: {msg}", need.remote.path);
            return Err(DlError::Busy);
        }
        _ => return Err(DlError::Busy),
    };
    if size != need.remote.size {
        return Err(DlError::Busy);
    }

    let mut file = tokio::fs::OpenOptions::new().create(true).append(true).open(temp).await.map_err(|e| DlError::Fail(e.to_string()))?;
    let mut offset = file.metadata().await.map_err(|e| DlError::Fail(e.to_string()))?.len();
    if offset > size {
        file.set_len(0).await.map_err(|e| DlError::Fail(e.to_string()))?;
        offset = 0;
    }
    progress(inner, tid, offset, Some(peer_name));
    let bs = inner.cfg.block_size as u64;
    let mut next = offset;
    let mut inflight = 0usize;
    while offset < size {
        while inflight < inner.cfg.window && next < size {
            let len = bs.min(size - next) as u32;
            conn.ctrl.send(Msg::BlockReq { id, offset: next, len }).map_err(|_| lost())?;
            next += len as u64;
            inflight += 1;
        }
        match timeout(Duration::from_secs(30), rx.recv()).await {
            Ok(Some(Msg::Block { offset: o, data, .. })) => {
                if o != offset || data.is_empty() || o + data.len() as u64 > size {
                    return Err(DlError::Fail("bloque inesperado".into()));
                }
                file.write_all(&data).await.map_err(|e| DlError::Fail(format!("error de escritura: {e}")))?;
                offset += data.len() as u64;
                inflight -= 1;
                inner.stats.lock().unwrap().bytes_down += data.len() as u64;
                progress(inner, tid, offset, None);
                if let Some(rate) = inner.cfg.throttle_bytes_per_sec.filter(|r| *r > 0) {
                    tokio::time::sleep(Duration::from_secs_f64(data.len() as f64 / rate as f64)).await;
                }
            }
            Ok(Some(Msg::DlErr { msg, .. })) => {
                file.flush().await.ok();
                return Err(DlError::Fail(format!("{peer_name}: {msg}")));
            }
            _ => {
                file.flush().await.ok();
                return Err(lost());
            }
        }
    }
    file.flush().await.map_err(|e| DlError::Fail(e.to_string()))?;
    file.sync_all().await.map_err(|e| DlError::Fail(e.to_string()))?;
    Ok(())
}

/// Coloca el temporal verificado. Comprueba antes que el estado local no ha
/// cambiado desde que se decidió la descarga (si cambió, se reevalúa).
fn place(inner: &Inner, folder: &FolderCtx, need: &Need, temp: &std::path::Path) -> Result<(Vec<FileEntry>, Option<String>), DlError> {
    if !folder.alive.load(Ordering::SeqCst) {
        return Err(DlError::Stale);
    }
    let root = &folder.root;
    let mut idx = folder.index.lock().unwrap();
    let cur = idx.get(&need.key).cloned();
    let action = decide(cur.as_ref().map(|c| &c.entry), &need.remote);
    if !matches!(action, Action::Download | Action::DownloadConflict) {
        return Err(DlError::Stale);
    }
    let present = cur.as_ref().filter(|c| !c.entry.deleted);
    match present {
        Some(c) => {
            let m = std::fs::metadata(to_local(root, &c.disk)).ok();
            if m.as_ref().map(|m| (m.len(), crate::scanner::mtime_ns(m))) != Some((c.disk_size, c.disk_mtime_ns)) {
                return Err(DlError::Stale);
            }
        }
        None => {
            if to_local(root, &need.remote.path).exists() {
                return Err(DlError::Stale); // apareció un archivo local aún sin indexar
            }
        }
    }
    let dest = present.map(|c| c.disk.clone()).unwrap_or_else(|| need.remote.path.clone());
    locally_representable(&dest).map_err(DlError::Fail)?;
    let mut changed = Vec::new();
    let mut conflict = None;
    let fail = |e: std::io::Error| {
        if is_locked(&e) {
            DlError::Fail("el archivo está abierto en otra aplicación; se actualizará cuando se cierre".into())
        } else {
            DlError::Fail(format!("no se puede colocar el archivo: {e}"))
        }
    };

    // La versión local que se sustituye (por una más reciente o por la
    // ganadora de un conflicto) pasa a la carpeta visible `old/`, que se
    // sincroniza como las demás. Dentro de `old/` no se generan más versiones:
    // ahí la anterior va a la copia oculta de seguridad.
    let mut backup = None;
    let mut parked: Option<String> = None;
    if let Some(c) = present.filter(|_| matches!(action, Action::Download | Action::DownloadConflict)) {
        if fsops::is_in_old(&c.disk) {
            backup = fsops::backup_existing(root, &dest).map_err(fail)?;
        } else {
            let mut copy = if action == Action::DownloadConflict {
                conflict_name(&c.entry)
            } else {
                fsops::old_version_name(&c.entry.path, c.entry.mtime_ns)
            };
            let taken = |p: &str, idx: &crate::index::LocalIndex| {
                idx.get(&key_of(p)).is_some_and(|e| !e.entry.deleted && e.entry.hash != c.entry.hash)
                    || (to_local(root, p).exists() && idx.get(&key_of(p)).is_none_or(|e| e.entry.hash != c.entry.hash))
            };
            if taken(&copy, &idx) {
                let (d, s, e) = crate::pathsafe::split_name(&copy);
                copy = format!("{d}{s} {}{e}", &c.entry.hash[..6]);
            }
            if to_local(root, &copy).exists() {
                // Ya está (la ha creado otro equipo con el mismo contenido).
                backup = fsops::backup_existing(root, &c.disk).map_err(fail)?;
            } else {
                if let Some(parent) = to_local(root, &copy).parent() {
                    std::fs::create_dir_all(parent).map_err(fail)?;
                }
                crate::pathsafe::check_no_symlinks(root, &copy).map_err(fail)?;
                std::fs::rename(to_local(root, &c.disk), to_local(root, &copy)).map_err(fail)?;
                backup = Some(to_local(root, &copy));
                let m = std::fs::metadata(to_local(root, &copy)).map_err(fail)?;
                let mut vv = VersionVector::new();
                vv.bump(inner.my_num);
                let entry = FileEntry {
                    path: copy.clone(),
                    size: c.entry.size,
                    mtime_ns: c.entry.mtime_ns,
                    hash: c.entry.hash.clone(),
                    vv,
                    deleted: false,
                    modified_by: c.entry.modified_by.clone(),
                };
                changed.push(entry.clone());
                idx.put(LocalEntry { entry, disk: copy.clone(), disk_size: m.len(), disk_mtime_ns: crate::scanner::mtime_ns(&m) });
                parked = Some(copy.clone());
            }
            if action == Action::DownloadConflict {
                idx.entries.remove(&need.key);
                conflict = Some(copy);
            }
        }
    }
    match fsops::install(root, temp, &dest, need.remote.mtime_ns) {
        Ok((size, mtime)) => {
            let mut entry = need.remote.clone();
            if let Some(c) = &cur {
                if c.entry.hash == entry.hash {
                    entry.vv.merge(&c.entry.vv);
                }
            }
            changed.push(entry.clone());
            idx.put(LocalEntry { entry, disk: dest, disk_size: size, disk_mtime_ns: mtime });
            Ok((changed, conflict))
        }
        Err(e) => {
            // Se deshace: la versión local vuelve a su sitio y no queda en old/.
            if let Some(b) = backup {
                let _ = std::fs::rename(b, to_local(root, &dest));
            }
            if let Some(p) = parked {
                idx.entries.remove(&key_of(&p));
                if let Some(c) = &cur {
                    idx.put(c.clone());
                }
            }
            Err(fail(e))
        }
    }
}

fn is_locked(e: &std::io::Error) -> bool {
    // ERROR_SHARING_VIOLATION (32) / ERROR_LOCK_VIOLATION (33) en Windows.
    matches!(e.raw_os_error(), Some(32) | Some(33)) && cfg!(windows)
}
