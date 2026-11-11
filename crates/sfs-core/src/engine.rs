//! Motor de sincronización: une carpeta, índice, red y transferencias.
//!
//! Tareas en segundo plano (todas en el runtime de tokio salvo el escáner):
//! * escucha TCP y acepta conexiones,
//! * descubrimiento (mDNS + baliza) y conexión automática a equipos del grupo,
//! * planificador de descargas,
//! * difusión periódica del estado propio,
//! * mantenimiento (guardar índice, limpiar temporales y versiones),
//! * un hilo de escaneo por carpeta, despertado por el vigilante de archivos.

use crate::discovery::{has_lan_ipv4, Advert, Discovery, DiscoveryEvent, DEFAULT_BEACON_PORT};
use crate::fsops;
use crate::index::{FileEntry, LocalIndex};
use crate::netpath::{AddrSource, Candidate};
use crate::peer::LinkError;
use crate::protocol::{Msg, PeerStatus, PROTOCOL_VERSION};
use crate::scanner::{Me, Scanner};
use crate::settings::{check_folder, Group, Settings};
use crate::status::*;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Notify};
use tokio::task::AbortHandle;

pub const DEFAULT_PORT: u16 = 47820;

/// Clave de un equipo descubierto: un mismo equipo se anuncia una vez por
/// cada carpeta compartida (grupo) que tiene.
pub fn nkey(device_id: &str, group_id: &str) -> String {
    format!("{device_id}/{group_id}")
}
const PAIRING_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_PAIR_FAILURES: u32 = 5;

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub data_dir: PathBuf,
    pub device_name: Option<String>,
    /// Puerto TCP. `None` = 47820 si está libre, si no uno cualquiera.
    pub listen_port: Option<u16>,
    pub discovery: bool,
    pub beacon_port: u16,
    /// Tiempo que un archivo debe permanecer sin cambios antes de copiarlo.
    pub stable_ms: u64,
    pub rescan_secs: u64,
    pub max_downloads: usize,
    pub max_uploads: usize,
    pub block_size: u32,
    pub window: usize,
    /// Límite de velocidad de descarga (bytes/s). Solo para pruebas y diagnóstico.
    pub throttle_bytes_per_sec: Option<u64>,
}

impl EngineConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        EngineConfig {
            data_dir: data_dir.into(),
            device_name: None,
            listen_port: None,
            discovery: true,
            beacon_port: DEFAULT_BEACON_PORT,
            stable_ms: 2500,
            rescan_secs: 60,
            max_downloads: 3,
            max_uploads: 4,
            block_size: 512 * 1024,
            window: 8,
            throttle_bytes_per_sec: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Estado interno
// ---------------------------------------------------------------------------

pub(crate) struct FolderCtx {
    pub root: PathBuf,
    pub index: Mutex<LocalIndex>,
    pub alive: AtomicBool,
    pub scanned_once: AtomicBool,
    pub scan_tx: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    pub root_error: Mutex<Option<String>>,
    pub scan_errors: Mutex<Vec<(String, String)>>,
    _watcher: Mutex<Option<notify::RecommendedWatcher>>,
}

impl FolderCtx {
    pub fn request_scan(&self) {
        if let Some(tx) = self.scan_tx.lock().unwrap().as_ref() {
            let _ = tx.send(());
        }
    }
}

#[derive(Clone)]
pub(crate) struct ConnHandle {
    pub conn_id: u64,
    pub dialer: String,
    pub ctrl: mpsc::UnboundedSender<Msg>,
    pub data: mpsc::Sender<Msg>,
    pub routes: Arc<Mutex<HashMap<u32, mpsc::UnboundedSender<Msg>>>>,
    pub next_id: Arc<AtomicU32>,
    pub close: Arc<watch::Sender<bool>>,
}

pub(crate) struct Peer {
    pub name: String,
    pub conn: Option<ConnHandle>,
    pub index: HashMap<String, FileEntry>,
    pub index_ready: bool,
    pub status: PeerStatus,
}

pub(crate) struct Nearby {
    pub advert: Advert,
    /// Direcciones conocidas del equipo con su origen y la última vez que se vieron.
    pub addrs: Vec<(Candidate, Instant)>,
    pub last_seen: Instant,
}

/// Estado de los intentos de conexión con un miembro del grupo.
pub(crate) struct DialState {
    /// Desde cuándo hay direcciones para intentarlo (reparto de quién marca).
    pub since: Instant,
    pub next: Instant,
    pub fails: u32,
    pub error: Option<String>,
    pub dialing: bool,
}

pub(crate) struct ManualTarget {
    pub id: Option<String>,
    pub next: Instant,
    pub fails: u32,
}

pub(crate) struct Pairing {
    pub code: String,
    pub expires: Instant,
    pub failures: u32,
}

pub(crate) struct Transfer {
    pub path: String,
    pub peer: String,
    pub download: bool,
    pub size: u64,
    pub done: u64,
    pub rate: f64,
    pub sample: (Instant, u64),
}

pub(crate) struct FileError {
    pub path: String,
    pub message: String,
    pub retry_at: Instant,
    pub attempts: u32,
    /// Si es `false` no se muestra (p. ej. "todos los equipos ocupados").
    pub visible: bool,
}

#[derive(Default)]
pub(crate) struct Activity {
    pub transfers: HashMap<u64, Transfer>,
    pub events: VecDeque<EventView>,
    pub errors: HashMap<String, FileError>,
    pub conflicts: u32,
    pub pending_files: u32,
    pub pending_bytes: u64,
}

impl Activity {
    pub fn event(&mut self, kind: &str, text: String) {
        tracing::info!("[{kind}] {text}");
        self.events.push_front(EventView { time_ms: crate::scanner::now_ns() / 1_000_000, kind: kind.into(), text });
        self.events.truncate(50);
    }
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub bytes_down: u64,
    pub bytes_up: u64,
    pub files_received: u64,
    pub local_copies: u64,
    pub hash_failures: u64,
}

pub(crate) struct Inner {
    pub cfg: EngineConfig,
    pub settings: Mutex<Settings>,
    pub my_id: String,
    pub my_num: u64,
    pub private_key: Vec<u8>,
    pub port: u16,
    pub folder: Mutex<Option<Arc<FolderCtx>>>,
    pub peers: Mutex<HashMap<String, Peer>>,
    pub nearby: Mutex<HashMap<String, Nearby>>,
    /// Direcciones manuales: id del equipo (si ya se sabe), próximo intento y fallos.
    pub manual: Mutex<HashMap<SocketAddr, ManualTarget>>,
    /// Último bloqueo detectado del permiso de Red local de macOS.
    pub lnp_blocked: Mutex<Option<Instant>>,
    /// Direcciones aprendidas o comunicadas por el grupo, por equipo.
    pub book: Mutex<HashMap<String, Vec<(Candidate, Instant)>>>,
    pub book_dirty: AtomicBool,
    pub dial: Mutex<HashMap<String, DialState>>,
    /// Archivos enviados a la papelera hace poco (hash → ruta, tamaño, cuándo):
    /// si el mismo contenido llega con otro nombre (renombrado, movido), se
    /// recupera de ahí en lugar de descargarlo.
    pub recent_trash: Mutex<HashMap<String, (PathBuf, u64, Instant)>>,
    /// Problemas del descubrimiento automático (para el diagnóstico).
    pub discovery_health: Mutex<Vec<String>>,
    pub pairing: Mutex<Option<Pairing>>,
    pub activity: Mutex<Activity>,
    pub active_downloads: Mutex<HashMap<String, String>>,
    pub active_uploads: AtomicUsize,
    pub wake: Notify,
    pub discovery: Mutex<Option<Discovery>>,
    pub next_conn: AtomicU64,
    pub next_transfer: AtomicU64,
    pub started: Instant,
    pub stats: Mutex<Stats>,
    pub tasks: Mutex<Vec<AbortHandle>>,
    pub shutting_down: AtomicBool,
    pub last_status: Mutex<PeerStatus>,
}

impl Inner {
    pub fn name(&self) -> String {
        self.settings.lock().unwrap().device_name.clone()
    }

    pub fn group(&self) -> Option<Group> {
        self.settings.lock().unwrap().group.clone()
    }

    pub fn folder(&self) -> Option<Arc<FolderCtx>> {
        self.folder.lock().unwrap().clone()
    }

    pub fn save_settings(&self) {
        let s = self.settings.lock().unwrap();
        if let Err(e) = s.save(&self.cfg.data_dir) {
            tracing::error!("no se pudieron guardar los ajustes: {e}");
        }
    }

    pub fn event(&self, kind: &str, text: String) {
        self.activity.lock().unwrap().event(kind, text);
    }

    /// Recuerda una dirección por la que se alcanza (o se puede alcanzar) a un equipo.
    pub fn learn_addr(&self, id: &str, c: Candidate) {
        if id == self.my_id || !crate::netpath::usable(c.addr.ip()) || c.source == AddrSource::Manual {
            return;
        }
        let now = Instant::now();
        let mut book = self.book.lock().unwrap();
        let v = book.entry(id.to_string()).or_default();
        match v.iter_mut().find(|(x, _)| x.addr == c.addr) {
            Some((x, seen)) => {
                *seen = now;
                if c.source < x.source {
                    x.source = c.source;
                    self.book_dirty.store(true, Ordering::Relaxed);
                }
            }
            None => {
                tracing::debug!("Dirección de {} aprendida: {} ({})", &id[..8.min(id.len())], c.addr, c.source.text());
                v.push((c, now));
                self.book_dirty.store(true, Ordering::Relaxed);
            }
        }
        v.sort_by_key(|(x, seen)| (x.source, std::cmp::Reverse(*seen)));
        v.truncate(8);
    }

    /// Todas las direcciones conocidas de un equipo: descubrimiento + aprendidas.
    pub fn candidates_for(&self, id: &str, group_id: &str) -> Vec<Candidate> {
        let mut v: Vec<Candidate> = Vec::new();
        if let Some(n) = self.nearby.lock().unwrap().get(&nkey(id, group_id)) {
            if n.advert.group_id == group_id && n.advert.proto == PROTOCOL_VERSION {
                v.extend(n.addrs.iter().map(|(c, _)| *c));
            }
        }
        if let Some(b) = self.book.lock().unwrap().get(id) {
            v.extend(b.iter().map(|(c, _)| *c));
        }
        v
    }

    /// Añade un miembro del grupo comunicado por otro. Devuelve `true` si es nuevo.
    pub fn add_member(&self, id: &str, name: &str) -> bool {
        let mut peers = self.peers.lock().unwrap();
        if peers.contains_key(id) {
            return false;
        }
        peers.insert(id.to_string(), Peer { name: name.to_string(), conn: None, index: HashMap::new(), index_ready: false, status: PeerStatus::default() });
        drop(peers);
        self.settings.lock().unwrap().known.insert(id.to_string(), name.to_string());
        self.save_settings();
        true
    }

    /// Envía a cada equipo conectado las direcciones de los demás.
    pub fn broadcast_members(&self) {
        let group_id = self.group().map(|g| g.id).unwrap_or_default();
        let peers = self.peers.lock().unwrap();
        let ifs = crate::netpath::local_interfaces();
        let connected: Vec<(String, String, ConnHandle)> =
            peers.iter().filter_map(|(id, p)| p.conn.clone().map(|c| (id.clone(), p.name.clone(), c))).collect();
        drop(peers);
        let members: Vec<crate::protocol::MemberAddrs> = connected
            .iter()
            .filter_map(|(id, name, _)| {
                let addrs: Vec<String> = crate::netpath::order(&self.candidates_for(id, &group_id), &ifs)
                    .iter()
                    .filter(|c| !c.addr.ip().is_loopback())
                    .take(crate::protocol::EXT_MAX_ADDRS)
                    .map(|c| c.addr.to_string())
                    .collect();
                (!addrs.is_empty()).then(|| crate::protocol::MemberAddrs { id: id.clone(), name: name.clone(), addrs })
            })
            .collect();
        for (id, _, c) in &connected {
            let others: Vec<_> = members.iter().filter(|m| &m.id != id).cloned().collect();
            if !others.is_empty() {
                let _ = c.ctrl.send(crate::protocol::Ext::Members { members: others }.to_msg());
            }
        }
    }

    /// Guarda en disco las direcciones aprendidas (si han cambiado).
    pub fn persist_book(&self) {
        if !self.book_dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let map: std::collections::BTreeMap<String, Vec<String>> = self
            .book
            .lock()
            .unwrap()
            .iter()
            .map(|(id, v)| (id.clone(), v.iter().take(6).map(|(c, _)| c.addr.to_string()).collect()))
            .collect();
        self.settings.lock().unwrap().peer_addrs = map;
        self.save_settings();
    }

    /// Olvida todo lo aprendido sobre los miembros (al cambiar de grupo).
    pub fn forget_members(&self) {
        self.book.lock().unwrap().clear();
        self.dial.lock().unwrap().clear();
        self.settings.lock().unwrap().peer_addrs.clear();
        self.book_dirty.store(false, Ordering::Relaxed);
    }

    /// Anota un fallo de conexión: detecta el bloqueo de Red local de macOS.
    pub fn note_link_error(&self, e: &LinkError) {
        if crate::netpath::looks_like_macos_lnp_block(e.attempts()) {
            tracing::error!("macOS deniega a Home File Sync las conexiones salientes a la red local (permiso «Red local»)");
            *self.lnp_blocked.lock().unwrap() = Some(Instant::now());
        } else if e.attempts().iter().any(|a| a.ok() || a.phase != crate::netpath::Phase::Tcp) {
            *self.lnp_blocked.lock().unwrap() = None;
        }
    }

    pub fn me(&self) -> Me {
        Me { num: self.my_num, name: self.name() }
    }

    /// Envía un mensaje de control a todos los equipos conectados.
    pub fn broadcast(&self, msg: Msg) {
        for p in self.peers.lock().unwrap().values() {
            if let Some(c) = &p.conn {
                let _ = c.ctrl.send(msg.clone());
            }
        }
    }

    /// Copia de seguridad: solo recibe. No anuncia sus archivos (nadie los
    /// descarga de aquí) y conserva lo que se borra en el original.
    pub fn backup(&self) -> bool {
        self.settings.lock().unwrap().mode == crate::settings::ShareMode::Backup
    }

    /// Índice que se anuncia a los demás (vacío en modo copia de seguridad).
    pub fn announced_entries(&self) -> Vec<FileEntry> {
        if self.backup() {
            return Vec::new();
        }
        self.folder()
            .filter(|f| f.scanned_once.load(Ordering::SeqCst))
            .map(|f| f.index.lock().unwrap().entries.values().map(|e| e.entry.clone()).collect())
            .unwrap_or_default()
    }

    pub fn broadcast_entries(&self, entries: Vec<FileEntry>) {
        if entries.is_empty() {
            return; // sin cambios: no despierta al planificador (evita un bucle activo)
        }
        if self.backup() {
            self.wake.notify_one();
            return;
        }
        for chunk in entries.chunks(1000) {
            self.broadcast(Msg::IndexUpdate { entries: chunk.to_vec() });
        }
        self.wake.notify_one();
    }

    pub fn advert(&self) -> Advert {
        let s = self.settings.lock().unwrap();
        let pairing = self.pairing.lock().unwrap().as_ref().is_some_and(|p| p.expires > Instant::now() && p.failures < MAX_PAIR_FAILURES);
        Advert {
            device_id: self.my_id.clone(),
            name: s.device_name.clone(),
            port: self.port,
            group_id: s.group.as_ref().map(|g| g.id.clone()).unwrap_or_default(),
            group_name: s.group.as_ref().map(|g| g.name.clone()).unwrap_or_default(),
            pairing,
            proto: PROTOCOL_VERSION,
        }
    }

    pub fn refresh_advert(&self) {
        if let Some(d) = self.discovery.lock().unwrap().as_ref() {
            d.update(self.advert());
        }
    }

    pub fn spawn<F>(self: &Arc<Self>, f: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let h = tokio::spawn(f);
        let mut t = self.tasks.lock().unwrap();
        t.retain(|h| !h.is_finished());
        t.push(h.abort_handle());
    }

    pub fn disconnect_all(&self) {
        let mut peers = self.peers.lock().unwrap();
        for p in peers.values_mut() {
            if let Some(c) = p.conn.take() {
                let _ = c.close.send(true);
            }
            p.index.clear();
            p.index_ready = false;
        }
    }
}

// ---------------------------------------------------------------------------
// API pública
// ---------------------------------------------------------------------------

/// Punto de entrada. Clonar es barato (comparte el estado).
#[derive(Clone)]
pub struct Engine {
    pub(crate) inner: Arc<Inner>,
}

impl Engine {
    /// Arranca el motor. Debe llamarse dentro de un runtime de tokio.
    pub async fn start(cfg: EngineConfig) -> std::io::Result<Engine> {
        let settings = Settings::load_or_create(&cfg.data_dir, cfg.device_name.as_deref())?;
        let my_id = settings.device_id();
        let listener = bind_listener(cfg.listen_port).await?;
        let port = listener.local_addr()?.port();
        let inner = Arc::new(Inner {
            my_num: crate::auth::device_num(&my_id),
            private_key: settings.private_key_bytes(),
            my_id,
            port,
            settings: Mutex::new(settings),
            folder: Mutex::new(None),
            peers: Mutex::new(HashMap::new()),
            nearby: Mutex::new(HashMap::new()),
            manual: Mutex::new(HashMap::new()),
            lnp_blocked: Mutex::new(None),
            book: Mutex::new(HashMap::new()),
            book_dirty: AtomicBool::new(false),
            dial: Mutex::new(HashMap::new()),
            discovery_health: Mutex::new(Vec::new()),
            recent_trash: Mutex::new(HashMap::new()),
            pairing: Mutex::new(None),
            activity: Mutex::new(Activity::default()),
            active_downloads: Mutex::new(HashMap::new()),
            active_uploads: AtomicUsize::new(0),
            wake: Notify::new(),
            discovery: Mutex::new(None),
            next_conn: AtomicU64::new(1),
            next_transfer: AtomicU64::new(1),
            started: Instant::now(),
            stats: Mutex::new(Stats::default()),
            tasks: Mutex::new(Vec::new()),
            shutting_down: AtomicBool::new(false),
            last_status: Mutex::new(PeerStatus::default()),
            cfg,
        });
        // Direcciones aprendidas en sesiones anteriores.
        {
            let saved = inner.settings.lock().unwrap().peer_addrs.clone();
            let mut book = inner.book.lock().unwrap();
            for (id, addrs) in saved {
                let v: Vec<(Candidate, Instant)> = addrs
                    .iter()
                    .filter_map(|a| a.parse().ok())
                    .map(|addr| (Candidate { addr, source: AddrSource::Learned }, Instant::now()))
                    .collect();
                if !v.is_empty() {
                    book.insert(id, v);
                }
            }
        }
        // Equipos conocidos: aparecen como "Desconectado" hasta que conecten.
        {
            let known = inner.settings.lock().unwrap().known.clone();
            let mut peers = inner.peers.lock().unwrap();
            for (id, name) in known {
                peers.insert(id, Peer { name, conn: None, index: HashMap::new(), index_ready: false, status: PeerStatus::default() });
            }
        }
        let engine = Engine { inner: inner.clone() };

        let folder = inner.settings.lock().unwrap().folder.clone();
        if let Some(f) = folder {
            engine.open_folder(&f);
        }

        let i = inner.clone();
        inner.spawn(async move { crate::peer::accept_loop(i, listener).await });
        // También IPv6 en el mismo puerto (si el sistema lo permite).
        match bind_listener_v6(port) {
            Ok(l6) => {
                let i = inner.clone();
                inner.spawn(async move { crate::peer::accept_loop(i, l6).await });
            }
            Err(e) => tracing::debug!("Sin escucha IPv6: {e}"),
        }
        tracing::info!("Home File Sync escuchando en el puerto TCP {port} (equipo {})", &inner.my_id[..8]);
        let i = inner.clone();
        inner.spawn(async move { crate::transfer::scheduler_loop(i).await });
        let i = inner.clone();
        inner.spawn(async move { connector_loop(i).await });
        let i = inner.clone();
        inner.spawn(async move { status_loop(i).await });
        let i = inner.clone();
        inner.spawn(async move { maintenance_loop(i).await });

        if inner.cfg.discovery {
            let (tx, rx) = mpsc::unbounded_channel();
            let d = Discovery::start(inner.advert(), inner.cfg.beacon_port, tx);
            let mut health = Vec::new();
            if let Some(e) = &d.mdns_error {
                tracing::warn!("mDNS no disponible: {e}");
                health.push(format!("mDNS no disponible: {e}"));
            }
            if let Some(e) = &d.beacon_error {
                tracing::warn!("baliza UDP no disponible: {e}");
                health.push(format!("baliza UDP no disponible: {e}"));
            }
            *inner.discovery_health.lock().unwrap() = health;
            *inner.discovery.lock().unwrap() = Some(d);
            let i = inner.clone();
            inner.spawn(async move { discovery_loop(i, rx).await });
        }
        Ok(engine)
    }

    pub fn device_id(&self) -> String {
        self.inner.my_id.clone()
    }

    pub fn port(&self) -> u16 {
        self.inner.port
    }

    pub fn stats(&self) -> Stats {
        self.inner.stats.lock().unwrap().clone()
    }

    pub fn folder(&self) -> Option<PathBuf> {
        self.inner.folder().map(|f| f.root.clone())
    }

    /// Cambia (o fija por primera vez) la carpeta sincronizada.
    pub fn set_folder(&self, path: &Path) -> Result<PathBuf, String> {
        let canon = check_folder(path)?;
        {
            let mut s = self.inner.settings.lock().unwrap();
            s.folder = Some(canon.clone());
        }
        self.inner.save_settings();
        self.open_folder(&canon);
        Ok(canon)
    }

    fn open_folder(&self, root: &Path) {
        let inner = &self.inner;
        if let Some(old) = inner.folder.lock().unwrap().take() {
            old.alive.store(false, Ordering::SeqCst);
            *old.scan_tx.lock().unwrap() = None;
            let _ = old.index.lock().unwrap().save();
        }
        let mut index = LocalIndex::load(&inner.cfg.data_dir, root);
        let mut root_error = None;
        match fsops::ensure_meta(root) {
            Ok(existed) => {
                if !existed && !index.entries.is_empty() {
                    // La carpeta interna ha desaparecido (carpeta sustituida o
                    // disco desmontado): se empieza de cero. Lo que falte aquí se
                    // vuelve a recibir; nunca se interpreta como borrado.
                    tracing::warn!("La carpeta {} no tiene su carpeta interna: se trata como carpeta nueva", root.display());
                    index.entries.clear();
                    index.dirty = true;
                }
            }
            Err(e) => root_error = Some(format!("No se puede preparar la carpeta: {e}")),
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = Arc::new(FolderCtx {
            root: root.to_path_buf(),
            index: Mutex::new(index),
            alive: AtomicBool::new(true),
            scanned_once: AtomicBool::new(false),
            scan_tx: Mutex::new(Some(tx.clone())),
            root_error: Mutex::new(root_error),
            scan_errors: Mutex::new(Vec::new()),
            _watcher: Mutex::new(make_watcher(root, tx)),
        });
        *inner.folder.lock().unwrap() = Some(ctx.clone());
        inner.active_downloads.lock().unwrap().clear();
        inner.activity.lock().unwrap().errors.clear();
        let i = inner.clone();
        std::thread::Builder::new()
            .name("sfs-scanner".into())
            .spawn(move || scanner_thread(i, ctx, rx))
            .expect("hilo de escaneo");
    }

    /// Crea un grupo nuevo y abre el emparejamiento. Devuelve el código.
    pub fn create_group(&self) -> String {
        let name = format!("Carpeta de {}", self.inner.name());
        self.create_group_named(&name)
    }

    /// Crea un grupo (carpeta compartida) con ese nombre. Devuelve el código.
    pub fn create_group_named(&self, group_name: &str) -> String {
        {
            let mut s = self.inner.settings.lock().unwrap();
            s.group = Some(Group::create_named(group_name));
            s.known.clear();
        }
        self.inner.forget_members();
        self.inner.save_settings();
        self.inner.disconnect_all();
        self.inner.peers.lock().unwrap().clear();
        self.inner.event("info", "Grupo nuevo creado".into());
        self.open_pairing().unwrap_or_default()
    }

    /// Muestra un código para añadir equipos al grupo actual.
    pub fn open_pairing(&self) -> Result<String, String> {
        if self.inner.group().is_none() {
            return Err("Este equipo no pertenece a ningún grupo.".into());
        }
        let code = crate::auth::new_code();
        *self.inner.pairing.lock().unwrap() = Some(Pairing { code: code.clone(), expires: Instant::now() + PAIRING_TTL, failures: 0 });
        self.inner.refresh_advert();
        Ok(code)
    }

    pub fn close_pairing(&self) {
        *self.inner.pairing.lock().unwrap() = None;
        self.inner.refresh_advert();
    }

    /// Se une al grupo que está mostrando `code`. Si hay varios grupos
    /// aceptando equipos, `device_id` indica cuál.
    pub async fn join_group(&self, code: &str, device_id: Option<String>) -> Result<(), String> {
        let code = crate::auth::normalize_code(code).ok_or("El código debe tener 8 cifras.")?;
        let devices: Vec<(String, String, Vec<Candidate>)> = {
            let nearby = self.inner.nearby.lock().unwrap();
            let open: Vec<&Nearby> = nearby.values().filter(|n| n.advert.pairing).collect();
            let chosen: Vec<&Nearby> = match &device_id {
                Some(id) => open.into_iter().filter(|n| &n.advert.device_id == id).collect(),
                None => {
                    let groups: std::collections::HashSet<&str> = open.iter().map(|n| n.advert.group_id.as_str()).collect();
                    if groups.len() > 1 {
                        return Err("Hay varios grupos aceptando equipos. Elige a cuál unirte.".into());
                    }
                    open
                }
            };
            chosen
                .iter()
                .map(|n| (n.advert.name.clone(), n.advert.group_name.clone(), n.addrs.iter().map(|(c, _)| *c).collect()))
                .collect()
        };
        if devices.is_empty() {
            return Err("No se encuentra ningún equipo mostrando un código. Pulsa «Añadir equipo» en un equipo del grupo y comprueba que ambos están en la misma red. Si la red no permite el descubrimiento, usa «Conexión manual».".into());
        }
        let mut last = None;
        for (name, group_name, cands) in devices {
            tracing::info!("Unirse a «{group_name}» a través de «{name}» (descubierto)");
            match self.join_with(&cands, &code).await {
                Ok(_) => return Ok(()),
                // Un rechazo (código incorrecto, bloqueado…) es definitivo: no se
                // prueba con otras direcciones ni equipos para no gastar intentos.
                Err(e @ LinkError::Rejected(..)) => return Err(e.user_message()),
                Err(e) => last = Some(e),
            }
        }
        Err(last.map(|e| e.user_message()).unwrap_or_default())
    }

    /// Unión manual: dirección IP o nombre del equipo, puerto opcional y código.
    /// Usa exactamente el mismo emparejamiento y autenticación que la automática.
    /// La dirección solo se guarda si `remember` es verdadero.
    pub async fn join_group_manual(&self, host: &str, port: Option<u16>, code: &str, remember: bool) -> Result<(), String> {
        let code = crate::auth::normalize_code(code).ok_or("El código debe tener 8 cifras.")?;
        let (host, port_in_host) = crate::netpath::parse_host(host)?;
        let port = port.or(port_in_host).unwrap_or(DEFAULT_PORT);
        tracing::info!("Unión manual: anfitrión «{host}», puerto TCP {port}");
        let cands = match crate::netpath::resolve(&host, port).await {
            Ok(c) if !c.is_empty() => c,
            Ok(_) | Err(_) => {
                tracing::warn!("Unión manual: no se pudo resolver «{host}»");
                return Err(format!("No se encuentra el equipo «{host}» en la red. Prueba con su dirección IP."));
            }
        };
        let (via, remote) = self.join_with(&cands, &code).await.map_err(|e| e.user_message())?;
        self.report_discovery_gap(&host, via, &remote);
        if remember {
            let mut s = self.inner.settings.lock().unwrap();
            let entry = format!("{host}|{port}");
            if !s.manual_peers.contains(&entry) {
                s.manual_peers.push(entry);
            }
            drop(s);
            self.inner.save_settings();
        }
        Ok(())
    }

    /// Unión con direcciones ya conocidas (las de una invitación descubierta).
    pub async fn join_group_candidates(&self, cands: Vec<Candidate>, code: &str) -> Result<(), String> {
        let code = crate::auth::normalize_code(code).ok_or("El código debe tener 8 cifras.")?;
        self.join_with(&cands, &code).await.map(|_| ()).map_err(|e| e.user_message())
    }

    /// Cambia el modo de esta carpeta (sincronizada o copia de seguridad).
    pub fn set_mode(&self, mode: crate::settings::ShareMode) {
        self.inner.settings.lock().unwrap().mode = mode;
        self.inner.save_settings();
        let entries = self.inner.announced_entries();
        for p in self.inner.peers.lock().unwrap().values() {
            if let Some(c) = &p.conn {
                crate::peer::send_full_index(&c.ctrl, entries.clone());
            }
        }
        self.inner.wake.notify_one();
    }

    pub fn mode(&self) -> crate::settings::ShareMode {
        self.inner.settings.lock().unwrap().mode
    }

    /// Nombre local de la carpeta compartida (no cambia el de los demás).
    pub fn rename_group(&self, name: &str) {
        let name: String = name.trim().chars().filter(|c| !c.is_control()).take(60).collect();
        if name.is_empty() {
            return;
        }
        if let Some(g) = self.inner.settings.lock().unwrap().group.as_mut() {
            g.name = name;
        }
        self.inner.save_settings();
        self.inner.refresh_advert();
    }

    /// Lo que anuncia este motor en la red (para un descubrimiento compartido).
    pub fn advert(&self) -> Advert {
        self.inner.advert()
    }

    /// Procesa un evento de un descubrimiento externo (el del hub).
    pub fn feed_discovery(&self, ev: DiscoveryEvent) {
        handle_discovery_event(&self.inner, ev);
    }

    /// Emparejamiento contra una dirección concreta (pruebas y diagnóstico).
    pub async fn join_group_at(&self, addr: SocketAddr, code: &str) -> Result<(), String> {
        let code = crate::auth::normalize_code(code).ok_or("El código debe tener 8 cifras.")?;
        self.join_with(&[Candidate { addr, source: AddrSource::Manual }], &code).await.map(|_| ()).map_err(|e| e.user_message())
    }

    /// Tras una unión manual que ha funcionado, deja constancia en el registro
    /// de por qué no lo consiguió la automática: cada uso de la conexión
    /// manual es un caso que el descubrimiento debería resolver.
    fn report_discovery_gap(&self, host: &str, via: Candidate, remote: &str) {
        let inner = &self.inner;
        let health = inner.discovery_health.lock().unwrap().clone();
        let disc = if !inner.cfg.discovery {
            "el descubrimiento automático está desactivado".to_string()
        } else if health.is_empty() {
            "mDNS y baliza UDP activos en este equipo".to_string()
        } else {
            health.join("; ")
        };
        let ifs: Vec<String> =
            crate::netpath::local_interfaces().iter().filter(|i| i.ip.is_ipv4()).map(|i| format!("{} {}/{}", i.name, i.ip, i.prefix)).collect();
        let gid = inner.group().map(|g| g.id).unwrap_or_default();
        let nearby = inner.nearby.lock().unwrap();
        let msg = match nearby.get(&nkey(remote, &gid)) {
            None => format!(
                "el equipo no se había descubierto: no ha llegado ningún anuncio suyo por mDNS ni por la baliza UDP. Probablemente la red bloquea multicast/broadcast entre equipos o están en VLAN distintas ({disc}; interfaces locales: {})",
                ifs.join(", ")
            ),
            Some(n) if !n.addrs.iter().any(|(c, _)| c.addr == via.addr) => format!(
                "el equipo se había descubierto, pero con otras direcciones ({}) y no con la que ha funcionado",
                n.addrs.iter().map(|(c, _)| format!("{} vía {}", c.addr, c.source.text())).collect::<Vec<_>>().join(", ")
            ),
            Some(n) if !n.advert.pairing => {
                "el equipo se había descubierto con esta dirección, pero no anunciaba un código abierto (¿se pulsó «Añadir equipo» justo antes?)".to_string()
            }
            Some(_) => "el equipo se había descubierto con esta misma dirección: la unión automática debería haber funcionado; revisa los intentos anteriores en este registro".to_string(),
        };
        tracing::warn!("Unión manual con «{host}» correcta por {}: la conexión automática no lo consiguió porque {msg}", via.addr);
    }

    async fn join_with(&self, cands: &[Candidate], code: &str) -> Result<(Candidate, String), LinkError> {
        let (group, via, remote) = match crate::peer::pair_as_joiner(&self.inner, cands, code).await {
            Ok(r) => r,
            Err(e) => {
                self.inner.note_link_error(&e);
                tracing::warn!("Emparejamiento fallido: {}", e.user_message().replace('\n', " "));
                return Err(e);
            }
        };
        *self.inner.lnp_blocked.lock().unwrap() = None;
        {
            let mut s = self.inner.settings.lock().unwrap();
            s.group = Some(group.clone());
            s.known.clear();
        }
        self.inner.forget_members();
        self.inner.save_settings();
        self.inner.disconnect_all();
        self.inner.peers.lock().unwrap().clear();
        self.inner.refresh_advert();
        self.inner.event("info", format!("Unido a «{}»", group.name));
        // Conectamos ya con el equipo que nos ha emparejado, por la misma dirección.
        if via.source == AddrSource::Manual {
            self.connect_to(via.addr);
        } else {
            self.inner.learn_addr(&remote, Candidate { addr: via.addr, source: AddrSource::Learned });
        }
        Ok((via, remote))
    }

    pub fn leave_group(&self) {
        {
            let mut s = self.inner.settings.lock().unwrap();
            s.group = None;
            s.known.clear();
        }
        self.inner.save_settings();
        *self.inner.pairing.lock().unwrap() = None;
        self.inner.disconnect_all();
        self.inner.peers.lock().unwrap().clear();
        self.inner.manual.lock().unwrap().clear();
        self.inner.forget_members();
        self.inner.settings.lock().unwrap().manual_peers.clear();
        self.inner.save_settings();
        self.inner.refresh_advert();
        self.inner.event("info", "Has salido del grupo".into());
    }

    /// Conecta (y reconecta si se pierde) con una dirección concreta.
    pub fn connect_to(&self, addr: SocketAddr) {
        self.inner.manual.lock().unwrap().insert(addr, ManualTarget { id: None, next: Instant::now(), fails: 0 });
    }

    /// Direcciones manuales guardadas a petición del usuario.
    pub fn remembered_addresses(&self) -> Vec<String> {
        self.inner.settings.lock().unwrap().manual_peers.iter().map(|e| e.replace('|', ":")).collect()
    }

    pub fn forget_addresses(&self) {
        self.inner.settings.lock().unwrap().manual_peers.clear();
        self.inner.save_settings();
    }

    pub fn ack_conflicts(&self) {
        self.inner.activity.lock().unwrap().conflicts = 0;
    }

    pub fn rescan(&self) {
        if let Some(f) = self.inner.folder() {
            f.request_scan();
        }
    }

    pub fn set_device_name(&self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.inner.settings.lock().unwrap().device_name = name.chars().take(60).collect();
        self.inner.save_settings();
        self.inner.refresh_advert();
    }

    pub async fn shutdown(&self) {
        let inner = &self.inner;
        inner.shutting_down.store(true, Ordering::SeqCst);
        if let Some(mut d) = inner.discovery.lock().unwrap().take() {
            d.stop();
        }
        for t in inner.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
        inner.disconnect_all();
        if let Some(f) = inner.folder.lock().unwrap().take() {
            f.alive.store(false, Ordering::SeqCst);
            *f.scan_tx.lock().unwrap() = None;
            let _ = f.index.lock().unwrap().save();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    /// Estado completo para la interfaz.
    pub fn snapshot(&self) -> UiState {
        build_snapshot(&self.inner)
    }
}

fn bind_listener_v6(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    s.set_only_v6(true)?;
    #[cfg(unix)]
    s.set_reuse_address(true)?;
    s.bind(&SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)).into())?;
    s.listen(128)?;
    s.set_nonblocking(true)?;
    tokio::net::TcpListener::from_std(s.into())
}

async fn bind_listener(port: Option<u16>) -> std::io::Result<tokio::net::TcpListener> {
    if let Some(port) = port {
        return tokio::net::TcpListener::bind(("0.0.0.0", port)).await;
    }
    match tokio::net::TcpListener::bind(("0.0.0.0", DEFAULT_PORT)).await {
        Ok(l) => Ok(l),
        Err(_) => tokio::net::TcpListener::bind(("0.0.0.0", 0)).await,
    }
}

// ---------------------------------------------------------------------------
// Escaneo
// ---------------------------------------------------------------------------

fn make_watcher(root: &Path, tx: std::sync::mpsc::Sender<()>) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher;
    let meta = root.join(crate::pathsafe::META_DIR);
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            // Ignoramos nuestra propia carpeta interna (descargas en curso).
            if ev.paths.iter().any(|p| !p.starts_with(&meta)) {
                let _ = tx.send(());
            }
        }
    })
    .ok()?;
    w.watch(root, notify::RecursiveMode::Recursive).ok()?;
    Some(w)
}

fn scanner_thread(inner: Arc<Inner>, ctx: Arc<FolderCtx>, rx: std::sync::mpsc::Receiver<()>) {
    let mut scanner = Scanner::new(ctx.root.clone(), Duration::from_millis(inner.cfg.stable_ms));
    let full = Duration::from_secs(inner.cfg.rescan_secs.max(1));
    let quick = Duration::from_millis((inner.cfg.stable_ms / 2).clamp(100, 1500));
    let mut wait = Duration::from_millis(0);
    let mut first = true;
    loop {
        match rx.recv_timeout(wait) {
            Ok(()) => {
                // Agrupa ráfagas de eventos del sistema de archivos.
                std::thread::sleep(Duration::from_millis(250));
                while rx.try_recv().is_ok() {}
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if !first {
                    break;
                }
            }
        }
        if !ctx.alive.load(Ordering::SeqCst) || inner.shutting_down.load(Ordering::SeqCst) {
            break;
        }
        first = false;
        let out = scanner.scan(&ctx.index, &inner.me());
        if out.meta_missing {
            tracing::warn!(
                "Ha desaparecido la carpeta interna de {}: la carpeta se ha sustituido o el disco se ha desmontado. No se propaga ningún borrado; se vuelve a recibir lo que falte.",
                ctx.root.display()
            );
            inner.event("warn", "La carpeta sincronizada ha cambiado por completo (¿disco desconectado?). No se borrará nada en los demás equipos.".into());
            {
                let mut idx = ctx.index.lock().unwrap();
                idx.entries.clear();
                idx.dirty = true;
            }
            let _ = fsops::ensure_meta(&ctx.root);
            // Los demás deben dejar de contar con nuestro índice anterior.
            for p in inner.peers.lock().unwrap().values() {
                if let Some(c) = &p.conn {
                    crate::peer::send_full_index(&c.ctrl, Vec::new());
                }
            }
            wait = quick;
            continue;
        }
        let root_missing = out.errors.iter().any(|(p, _)| p.is_empty());
        *ctx.root_error.lock().unwrap() =
            root_missing.then(|| "La carpeta sincronizada no está disponible (¿disco desconectado?).".to_string());
        *ctx.scan_errors.lock().unwrap() = out.errors.into_iter().filter(|(p, _)| !p.is_empty()).collect();
        if !out.changed.is_empty() {
            let n = out.changed.len();
            if ctx.scanned_once.load(Ordering::SeqCst) {
                let text = if n == 1 { format!("Nuevo cambio local: {}", out.changed[0].path) } else { format!("{n} cambios locales detectados") };
                inner.event("local", text);
            }
            inner.broadcast_entries(out.changed);
        }
        if out.tombstoned > 0 {
            inner.event("local", format!("{} archivo(s) borrado(s) en este equipo: se eliminan también en los demás (copia en su papelera interna)", out.tombstoned));
            inner.broadcast_entries(out.tombstones);
        }
        let _ = ctx.index.lock().unwrap().save();
        if !ctx.scanned_once.swap(true, Ordering::SeqCst) {
            // Los equipos que conectaron antes del primer escaneo recibieron un
            // índice vacío: les enviamos el completo.
            let entries = inner.announced_entries();
            for p in inner.peers.lock().unwrap().values() {
                if let Some(c) = &p.conn {
                    crate::peer::send_full_index(&c.ctrl, entries.clone());
                }
            }
            inner.wake.notify_one();
        }
        wait = if out.unstable { quick } else { full };
    }
}

// ---------------------------------------------------------------------------
// Descubrimiento y conexión automática
// ---------------------------------------------------------------------------

async fn discovery_loop(inner: Arc<Inner>, mut rx: mpsc::UnboundedReceiver<DiscoveryEvent>) {
    while let Some(ev) = rx.recv().await {
        handle_discovery_event(&inner, ev);
    }
}

fn handle_discovery_event(inner: &Inner, ev: DiscoveryEvent) {
    {
        let now = Instant::now();
        let mut reappeared = None;
        let mut nearby = inner.nearby.lock().unwrap();
        match ev {
            DiscoveryEvent::Seen(s) => {
                let key = nkey(&s.advert.device_id, &s.advert.group_id);
                let is_new = !nearby.contains_key(&key);
                let n = nearby.entry(key).or_insert_with(|| Nearby {
                    advert: s.advert.clone(),
                    addrs: Vec::new(),
                    last_seen: now,
                });
                if is_new {
                    reappeared = Some(s.advert.device_id.clone());
                }
                if is_new || n.advert.group_id != s.advert.group_id || n.advert.pairing != s.advert.pairing {
                    tracing::info!(
                        "Descubierto «{}» (id {}, grupo «{}», {}acepta equipos, protocolo v{})",
                        s.advert.name,
                        &s.advert.device_id[..8.min(s.advert.device_id.len())],
                        if s.advert.group_name.is_empty() { "ninguno" } else { &s.advert.group_name },
                        if s.advert.pairing { "" } else { "no " },
                        s.advert.proto
                    );
                }
                n.advert = s.advert;
                n.last_seen = now;
                for c in s.addrs {
                    match n.addrs.iter_mut().find(|(x, _)| x.addr == c.addr) {
                        Some((x, seen)) => {
                            *seen = now;
                            if c.source < x.source {
                                x.source = c.source;
                            }
                        }
                        None => {
                            let ifs = crate::netpath::local_interfaces();
                            let iface = crate::netpath::interface_for(c.addr.ip(), &ifs).map(|i| i.name.clone());
                            tracing::info!(
                                "«{}» anuncia {} vía {}{}, interfaz local: {}",
                                n.advert.name,
                                c.addr,
                                c.source.text(),
                                s.from.map(|f| format!(" (paquete recibido desde {f})")).unwrap_or_default(),
                                iface.as_deref().unwrap_or("ninguna en esa subred")
                            );
                            n.addrs.push((c, now));
                        }
                    }
                }
                // Olvida direcciones que llevan mucho sin anunciarse y limita la lista.
                n.addrs.retain(|(_, seen)| now.duration_since(*seen) < Duration::from_secs(300));
                n.addrs.sort_by_key(|(_, seen)| std::cmp::Reverse(*seen));
                n.addrs.truncate(16);
            }
            DiscoveryEvent::Gone(id) => {
                if let Some(n) = nearby.remove(&id) {
                    tracing::info!("«{}» ya no se anuncia en la red", n.advert.name);
                }
            }
        }
        drop(nearby);
        // Reaparece: se reintenta ya, sin esperar a la espera creciente.
        // (Orden de cerrojos: `dial` nunca se toma con `nearby` tomado.)
        if let Some(id) = reappeared {
            if let Some(d) = inner.dial.lock().unwrap().get_mut(&id) {
                d.next = now;
                d.fails = 0;
            }
        }
    }
}

async fn connector_loop(inner: Arc<Inner>) {
    let mut tick = 0u64;
    loop {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        tick += 1;
        let Some(group) = inner.group() else { continue };
        let now = Instant::now();
        let connected: std::collections::HashSet<String> =
            inner.peers.lock().unwrap().iter().filter(|(_, p)| p.conn.is_some()).map(|(id, _)| id.clone()).collect();

        // Miembros del grupo con alguna dirección conocida: descubiertos ahora,
        // aprendidos en conexiones anteriores o comunicados por otros miembros.
        let mut members: std::collections::HashSet<String> = inner.book.lock().unwrap().keys().cloned().collect();
        {
            let mut nearby = inner.nearby.lock().unwrap();
            nearby.retain(|_, n| now.duration_since(n.last_seen) < Duration::from_secs(120) || n.advert.group_id == group.id);
            members.extend(
                nearby.values().filter(|n| n.advert.group_id == group.id && n.advert.proto == PROTOCOL_VERSION).map(|n| n.advert.device_id.clone()),
            );
        }
        members.remove(&inner.my_id);
        let mut to_dial = Vec::new();
        {
            let mut dial = inner.dial.lock().unwrap();
            for id in members {
                if connected.contains(&id) {
                    if let Some(d) = dial.get_mut(&id) {
                        d.fails = 0;
                        d.error = None;
                    }
                    continue;
                }
                let cands = inner.candidates_for(&id, &group.id);
                if cands.is_empty() {
                    continue;
                }
                let d = dial.entry(id.clone()).or_insert(DialState { since: now, next: now, fails: 0, error: None, dialing: false });
                if d.dialing || now < d.next {
                    continue;
                }
                // Para no abrir dos conexiones a la vez, marca el equipo de id
                // menor; el otro solo marca si no hay conexión pasado un rato.
                let my_turn = inner.my_id < id || now.duration_since(d.since) > Duration::from_secs(6);
                if my_turn {
                    d.dialing = true;
                    to_dial.push((id, cands, d.fails >= 2));
                }
            }
        }
        for (id, addrs, quiet) in to_dial {
            let i = inner.clone();
            tokio::spawn(async move {
                let (ok, err) = match crate::peer::dial(i.clone(), &addrs, quiet).await {
                    Ok(_) => (true, String::new()),
                    Err(e) => {
                        i.note_link_error(&e);
                        (false, e.user_message().split("\n").next().unwrap_or_default().to_string())
                    }
                };
                let mut dial = i.dial.lock().unwrap();
                if let Some(d) = dial.get_mut(&id) {
                    d.dialing = false;
                    if ok {
                        d.fails = 0;
                        d.error = None;
                        d.next = Instant::now() + Duration::from_secs(2);
                    } else {
                        d.fails += 1;
                        d.error = Some(err);
                        d.next = Instant::now() + Duration::from_secs((1u64 << d.fails.min(5)).min(30));
                    }
                }
            });
        }

        // Direcciones guardadas por el usuario (se resuelven de nuevo cada vez).
        let remembered = inner.settings.lock().unwrap().manual_peers.clone();
        if !remembered.is_empty() && tick % 15 == 1 {
            let i = inner.clone();
            tokio::spawn(async move {
                for e in remembered {
                    let (host, port) = e.split_once('|').unwrap_or((&e, "47800"));
                    let port = port.parse().unwrap_or(DEFAULT_PORT);
                    if let Ok(cands) = crate::netpath::resolve(host, port).await {
                        let mut m = i.manual.lock().unwrap();
                        for c in cands {
                            m.entry(c.addr).or_insert(ManualTarget { id: None, next: Instant::now(), fails: 0 });
                        }
                    }
                }
            });
        }

        // Direcciones manuales (con espera creciente si fallan).
        let manual: Vec<SocketAddr> = {
            let mut m = inner.manual.lock().unwrap();
            m.iter_mut()
                .filter(|(_, t)| t.id.as_ref().is_none_or(|id| !connected.contains(id)) && now >= t.next)
                .map(|(a, t)| {
                    t.next = now + Duration::from_secs(60); // se ajusta al terminar el intento
                    *a
                })
                .collect()
        };
        for a in manual {
            let i = inner.clone();
            tokio::spawn(async move {
                let quiet = i.manual.lock().unwrap().get(&a).is_some_and(|t| t.fails >= 2);
                let r = crate::peer::dial(i.clone(), &[Candidate { addr: a, source: AddrSource::Manual }], quiet).await;
                if let Err(e) = &r {
                    i.note_link_error(e);
                }
                let mut m = i.manual.lock().unwrap();
                if let Some(t) = m.get_mut(&a) {
                    match r {
                        Ok(id) => {
                            t.id = Some(id);
                            t.fails = 0;
                            t.next = Instant::now() + Duration::from_secs(3);
                        }
                        Err(_) => {
                            t.fails += 1;
                            t.next = Instant::now() + Duration::from_secs((3u64 << t.fails.min(4)).min(60));
                        }
                    }
                }
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Estado propio y mantenimiento
// ---------------------------------------------------------------------------

pub(crate) fn own_status(inner: &Inner) -> PeerStatus {
    let a = inner.activity.lock().unwrap();
    PeerStatus {
        pending_files: a.pending_files,
        pending_bytes: a.pending_bytes,
        receiving: a.transfers.values().any(|t| t.download),
        conflicts: a.conflicts,
        errors: a.errors.values().filter(|e| e.visible).count() as u32,
        uploads: inner.active_uploads.load(Ordering::Relaxed) as u32,
        backup: inner.backup(),
    }
}

async fn status_loop(inner: Arc<Inner>) {
    let mut ticks = 0u32;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        ticks += 1;
        let st = own_status(&inner);
        let changed = *inner.last_status.lock().unwrap() != st;
        if changed || ticks % 20 == 0 {
            *inner.last_status.lock().unwrap() = st.clone();
            inner.broadcast(Msg::Status(st));
        }
        if ticks % 120 == 0 {
            inner.broadcast_members();
        }
        // Caducidad del código de emparejamiento.
        let expired = inner.pairing.lock().unwrap().as_ref().is_some_and(|p| p.expires <= Instant::now());
        if expired {
            *inner.pairing.lock().unwrap() = None;
            inner.refresh_advert();
        }
    }
}

async fn maintenance_loop(inner: Arc<Inner>) {
    let mut last_cleanup = Instant::now() - Duration::from_secs(3600);
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        inner.persist_book();
        if let Some(f) = inner.folder() {
            let _ = f.index.lock().unwrap().save();
            if last_cleanup.elapsed() > Duration::from_secs(3600) {
                last_cleanup = Instant::now();
                let root = f.root.clone();
                tokio::task::spawn_blocking(move || {
                    fsops::cleanup(&root, Duration::from_secs(14 * 86400), Duration::from_secs(7 * 86400))
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Instantánea para la interfaz
// ---------------------------------------------------------------------------

fn human_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn build_snapshot(inner: &Inner) -> UiState {
    let now = Instant::now();
    let s = inner.settings.lock().unwrap().clone();
    let folder = inner.folder();
    let (file_count, total_bytes) = folder.as_ref().map(|f| f.index.lock().unwrap().total()).unwrap_or((0, 0));
    let folder_error = folder.as_ref().and_then(|f| f.root_error.lock().unwrap().clone());
    let pairing = inner.pairing.lock().unwrap().as_ref().map(|p| PairingView {
        code: crate::auth::format_code(&p.code),
        expires_in_secs: p.expires.saturating_duration_since(now).as_secs(),
        locked: p.failures >= MAX_PAIR_FAILURES,
    });

    let dial = inner.dial.lock().unwrap();
    let nearby = inner.nearby.lock().unwrap();
    let group_id = s.group.as_ref().map(|g| g.id.clone());
    let mut devices: Vec<DeviceView> = inner
        .peers
        .lock()
        .unwrap()
        .iter()
        .map(|(id, p)| {
            let near = nearby.get(&nkey(id, group_id.as_deref().unwrap_or("")));
            let (state, detail) = if p.conn.is_none() {
                let d = dial.get(id);
                match near {
                    Some(n) if n.advert.group_id == group_id.clone().unwrap_or_default() => match d.and_then(|d| d.error.as_ref().filter(|_| d.fails >= 2)) {
                        Some(e) => (DeviceState::Error, format!("Detectado pero sin conexión: {e}")),
                        None => (DeviceState::Connecting, "Conectando…".into()),
                    },
                    // Sin anuncios pero con direcciones conocidas: se sigue intentando.
                    _ if d.is_some_and(|d| d.dialing && d.fails < 2) => (DeviceState::Connecting, "Conectando…".into()),
                    _ => (DeviceState::Offline, "Desconectado".into()),
                }
            } else if !p.index_ready {
                (DeviceState::Syncing, "Comparando archivos…".into())
            } else if p.status.receiving || p.status.pending_files > 0 {
                (
                    if p.status.receiving { DeviceState::Receiving } else { DeviceState::Syncing },
                    format!("Recibiendo archivos · faltan {} ({})", p.status.pending_files, human_bytes(p.status.pending_bytes)),
                )
            } else if p.status.errors > 0 {
                (DeviceState::Error, format!("{} archivo(s) con error", p.status.errors))
            } else if p.status.conflicts > 0 {
                (DeviceState::Conflict, format!("{} conflicto(s) resuelto(s) conservando ambas copias", p.status.conflicts))
            } else {
                (DeviceState::Synced, "Sincronizado".into())
            };
            DeviceView {
                id: id.clone(),
                name: p.name.clone(),
                state,
                detail,
                pending_files: p.status.pending_files,
                pending_bytes: p.status.pending_bytes,
                backup: p.status.backup,
            }
        })
        .collect();
    // Equipos del grupo detectados que todavía no conocemos.
    if let Some(gid) = &group_id {
        let known: std::collections::HashSet<String> = devices.iter().map(|d| d.id.clone()).collect();
        for n in nearby.values() {
            let id = &n.advert.device_id;
            if &n.advert.group_id == gid && !known.contains(id) && id != &inner.my_id {
                let (state, detail) = match dial.get(id).and_then(|d| d.error.as_ref().filter(|_| d.fails >= 2)) {
                    Some(e) => (DeviceState::Error, format!("Detectado pero sin conexión: {e}")),
                    None => (DeviceState::Connecting, "Conectando…".into()),
                };
                devices.push(DeviceView { id: id.clone(), name: n.advert.name.clone(), state, detail, pending_files: 0, pending_bytes: 0, backup: false });
            }
        }
    }
    devices.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let open_groups = nearby
        .values()
        .filter(|n| n.advert.pairing && Some(&n.advert.group_id) != group_id.as_ref())
        .map(|n| OpenGroupView {
            group_id: n.advert.group_id.clone(),
            group_name: n.advert.group_name.clone(),
            device_id: n.advert.device_id.clone(),
            device_name: n.advert.name.clone(),
        })
        .collect();

    let any_connected = devices.iter().any(|d| !matches!(d.state, DeviceState::Offline | DeviceState::Connecting | DeviceState::Error));
    let detected_unreachable = devices.iter().any(|d| d.state == DeviceState::Error && d.detail.starts_with("Detectado"));
    let in_group_nearby = group_id.as_ref().is_some_and(|g| nearby.values().any(|n| &n.advert.group_id == g));
    drop(nearby);
    drop(dial);

    let lnp = inner.lnp_blocked.lock().unwrap().is_some_and(|t| t.elapsed() < Duration::from_secs(120));
    let network_warning = if lnp {
        Some(crate::netpath::MACOS_LNP_HELP.to_string())
    } else if !has_lan_ipv4() {
        Some("Este equipo no tiene conexión a una red local.".to_string())
    } else if detected_unreachable {
        Some("Se detectan otros equipos pero no se puede conectar con ellos. Probablemente un cortafuegos o la red bloquea las conexiones directas entre equipos (aislamiento de clientes). Permite Home File Sync en el cortafuegos o usa una red sin aislamiento.".to_string())
    } else if group_id.is_some() && !any_connected && !in_group_nearby && now.duration_since(inner.started) > Duration::from_secs(20) {
        Some("No se detecta ningún otro equipo del grupo. Comprueba que están encendidos, con Home File Sync abierto y en la misma red. Algunas redes wifi aíslan a los clientes y no permiten que los equipos se vean.".to_string())
    } else {
        None
    };

    let a = inner.activity.lock().unwrap();
    let transfers: Vec<TransferView> = a
        .transfers
        .values()
        .map(|t| TransferView { path: t.path.clone(), peer: t.peer.clone(), download: t.download, size: t.size, done: t.done, rate: t.rate })
        .collect();
    let down_rate = transfers.iter().filter(|t| t.download).map(|t| t.rate).sum();
    let up_rate = transfers.iter().filter(|t| !t.download).map(|t| t.rate).sum();
    let mut errors: Vec<ErrorView> =
        a.errors.values().filter(|e| e.visible).map(|e| ErrorView { path: e.path.clone(), message: e.message.clone() }).collect();
    if let Some(f) = &folder {
        for (p, m) in f.scan_errors.lock().unwrap().iter() {
            errors.push(ErrorView { path: p.clone(), message: m.clone() });
        }
    }
    errors.sort_by(|x, y| x.path.cmp(&y.path));

    let overall = if s.folder.is_none() {
        Overall::NoFolder
    } else if s.group.is_none() {
        Overall::NoGroup
    } else if folder_error.is_some() {
        Overall::Error
    } else if a.pending_files > 0 || !transfers.is_empty() {
        Overall::Syncing
    } else if !errors.is_empty() {
        Overall::Error
    } else if a.conflicts > 0 {
        Overall::Conflict
    } else if !any_connected {
        Overall::Alone
    } else {
        Overall::Synced
    };

    UiState {
        device_name: s.device_name.clone(),
        device_id: inner.my_id.clone(),
        folder: s.folder.as_ref().map(|p| p.display().to_string()),
        folder_error,
        group: s.group.as_ref().map(|g| GroupView { id: g.id.clone(), name: g.name.clone() }),
        pairing,
        overall,
        devices,
        transfers,
        pending_files: a.pending_files,
        pending_bytes: a.pending_bytes,
        down_rate,
        up_rate,
        file_count,
        total_bytes,
        conflicts: a.conflicts,
        errors,
        events: a.events.iter().take(30).cloned().collect(),
        open_groups,
        network_warning,
        mode: s.mode,
    }
}
