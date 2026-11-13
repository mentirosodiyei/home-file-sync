//! Varias carpetas compartidas en un mismo equipo.
//!
//! Cada carpeta compartida es un grupo independiente (su clave, sus miembros,
//! su código de invitación) y la gestiona un [`Engine`] propio. El hub:
//! * mantiene una identidad única del equipo para todas (mismo id y nombre);
//! * ejecuta **un solo** descubrimiento de red y reparte lo que ve entre los
//!   motores; cada motor se anuncia como una instancia distinta;
//! * guarda los nombres locales que el usuario da a otros equipos (alias):
//!   solo se ven en este equipo y no cambian los de nadie más;
//! * lleva la recepción de archivos por navegador (QR).
//!
//! Así, en una familia, la pareja puede compartir una carpeta, el padre y el
//! hijo otra, y cada equipo participa en tantas como quiera, cada una en modo
//! sincronizada o copia de seguridad.

use crate::discovery::{Advert, Discovery, DiscoveryEvent, DEFAULT_BEACON_PORT};
use crate::engine::{nkey, Engine, EngineConfig};
use crate::inbox::{Inbox, ReceiveView};
use crate::netpath::Candidate;
use crate::settings::{check_folder, default_device_name, Settings, ShareMode};
use crate::status::UiState;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

#[derive(Clone, Debug)]
pub struct HubConfig {
    pub data_dir: PathBuf,
    pub device_name: Option<String>,
    pub discovery: bool,
    pub beacon_port: u16,
    /// Plantilla para los motores de cada carpeta (tiempos, límites…).
    pub engine: EngineConfig,
    /// Puerto de la recepción por navegador (`None` = 47830 o uno libre; `Some(0)` = cualquiera).
    pub inbox_port: Option<u16>,
    /// Carpeta para lo recibido por navegador cuando no se elige una compartida.
    pub received_dir: Option<PathBuf>,
}

impl HubConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        HubConfig {
            engine: EngineConfig::new(&data_dir),
            data_dir,
            device_name: None,
            discovery: true,
            beacon_port: DEFAULT_BEACON_PORT,
            inbox_port: None,
            received_dir: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HubSettings {
    private_key: String,
    public_key: String,
    device_name: String,
    /// Nombres locales de otros equipos (id → alias).
    #[serde(default)]
    aliases: BTreeMap<String, String>,
    /// Carpetas compartidas de este equipo (ids locales, en orden).
    #[serde(default)]
    shares: Vec<String>,
    /// Aceptar conexiones que llegan por túneles VPN (WireGuard…).
    #[serde(default)]
    listen_vpn: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareState {
    pub id: String,
    #[serde(flatten)]
    pub state: UiState,
}

/// Una carpeta compartida de otro equipo que está aceptando equipos ahora.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteView {
    pub key: String,
    pub device_id: String,
    pub device_name: String,
    pub group_id: String,
    pub group_name: String,
}

/// Una dirección de red de este equipo (para configurar una VPN o una conexión manual).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressView {
    pub interface: String,
    pub ip: String,
    pub tunnel: bool,
}

/// Puerto de cada carpeta compartida (para una conexión manual a través de VPN).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortView {
    pub share: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkView {
    pub listen_vpn: bool,
    pub addresses: Vec<AddressView>,
    pub ports: Vec<PortView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubState {
    pub device_name: String,
    pub device_id: String,
    pub shares: Vec<ShareState>,
    pub invites: Vec<InviteView>,
    pub receive: Option<ReceiveView>,
    pub network_warning: Option<String>,
}

struct Invite {
    advert: Advert,
    addrs: Vec<Candidate>,
    last_seen: Instant,
}

struct HubInner {
    cfg: HubConfig,
    my_id: String,
    settings: Mutex<HubSettings>,
    shares: Mutex<BTreeMap<String, Engine>>,
    invites: Mutex<HashMap<String, Invite>>,
    discovery: Mutex<Option<Discovery>>,
    inbox: Option<Inbox>,
    tasks: Mutex<Vec<AbortHandle>>,
}

#[derive(Clone)]
pub struct Hub {
    inner: Arc<HubInner>,
}

fn io_err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Hub {
    pub async fn start(cfg: HubConfig) -> std::io::Result<Hub> {
        std::fs::create_dir_all(&cfg.data_dir)?;
        let file = cfg.data_dir.join("hub.json");
        let mut settings = match std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<HubSettings>(&b).ok()) {
            Some(s) => s,
            None => {
                let (private, public) = crate::transport::generate_keypair();
                HubSettings {
                    private_key: hex::encode(private),
                    public_key: hex::encode(public),
                    device_name: default_device_name(),
                    aliases: BTreeMap::new(),
                    shares: Vec::new(),
                    listen_vpn: false,
                }
            }
        };
        if let Some(n) = &cfg.device_name {
            settings.device_name = n.clone();
        }
        let my_id = crate::auth::device_id(&hex::decode(&settings.public_key).unwrap_or_default());
        crate::netpath::set_listen_vpn(settings.listen_vpn);
        let inbox = match Inbox::start(cfg.inbox_port, &settings.device_name).await {
            Ok(i) => Some(i),
            Err(e) => {
                tracing::warn!("Recepción por navegador no disponible: {e}");
                None
            }
        };
        let inner = Arc::new(HubInner {
            cfg,
            my_id,
            settings: Mutex::new(settings.clone()),
            shares: Mutex::new(BTreeMap::new()),
            invites: Mutex::new(HashMap::new()),
            discovery: Mutex::new(None),
            inbox,
            tasks: Mutex::new(Vec::new()),
        });
        let hub = Hub { inner: inner.clone() };
        hub.save();
        for id in settings.shares {
            match hub.start_engine(&id).await {
                Ok(e) => {
                    inner.shares.lock().unwrap().insert(id, e);
                }
                Err(e) => tracing::error!("No se pudo abrir la carpeta compartida {id}: {e}"),
            }
        }
        if inner.cfg.discovery {
            let (tx, rx) = mpsc::unbounded_channel();
            let d = Discovery::start_many(inner.my_id.clone(), hub.adverts(), inner.cfg.beacon_port, tx);
            *inner.discovery.lock().unwrap() = Some(d);
            let h = hub.clone();
            hub.spawn(async move { h.discovery_loop(rx).await });
            let h = hub.clone();
            hub.spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let adverts = h.adverts();
                    if let Some(d) = h.inner.discovery.lock().unwrap().as_ref() {
                        d.update_many(adverts);
                    }
                }
            });
        }
        tracing::info!("Home File Sync: {} carpeta(s) compartida(s)", inner.shares.lock().unwrap().len());
        Ok(hub)
    }

    fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, f: F) {
        let h = tokio::spawn(f);
        self.inner.tasks.lock().unwrap().push(h.abort_handle());
    }

    fn save(&self) {
        let s = self.inner.settings.lock().unwrap().clone();
        if let Ok(b) = serde_json::to_vec_pretty(&s) {
            if let Err(e) = crate::index::write_atomic(&self.inner.cfg.data_dir.join("hub.json"), &b) {
                tracing::error!("No se pudieron guardar los ajustes: {e}");
            }
        }
    }

    fn share_dir(&self, id: &str) -> PathBuf {
        self.inner.cfg.data_dir.join("shares").join(id)
    }

    async fn start_engine(&self, id: &str) -> std::io::Result<Engine> {
        let dir = self.share_dir(id);
        std::fs::create_dir_all(&dir)?;
        let s = self.inner.settings.lock().unwrap().clone();
        if !dir.join("settings.json").exists() {
            Settings::with_identity(&s.private_key, &s.public_key, &s.device_name).save(&dir)?;
        }
        let mut cfg = self.inner.cfg.engine.clone();
        cfg.data_dir = dir;
        cfg.device_name = Some(s.device_name);
        cfg.discovery = false; // lo lleva el hub
        Engine::start(cfg).await
    }

    fn adverts(&self) -> Vec<Advert> {
        self.inner.shares.lock().unwrap().values().map(|e| e.advert()).filter(|a| !a.group_id.is_empty()).collect()
    }

    async fn discovery_loop(&self, mut rx: mpsc::UnboundedReceiver<DiscoveryEvent>) {
        while let Some(ev) = rx.recv().await {
            // Invitaciones abiertas de otros equipos.
            {
                let mut inv = self.inner.invites.lock().unwrap();
                let now = Instant::now();
                match &ev {
                    DiscoveryEvent::Seen(s) => {
                        let key = nkey(&s.advert.device_id, &s.advert.group_id);
                        if s.advert.pairing && s.advert.device_id != self.inner.my_id && s.advert.proto == crate::protocol::PROTOCOL_VERSION {
                            let e = inv.entry(key).or_insert(Invite { advert: s.advert.clone(), addrs: Vec::new(), last_seen: now });
                            e.advert = s.advert.clone();
                            e.last_seen = now;
                            for c in &s.addrs {
                                if !e.addrs.iter().any(|x| x.addr == c.addr) {
                                    e.addrs.push(*c);
                                }
                            }
                        } else {
                            inv.remove(&key);
                        }
                    }
                    DiscoveryEvent::Gone(key) => {
                        inv.remove(key);
                    }
                }
                inv.retain(|_, i| now.duration_since(i.last_seen) < Duration::from_secs(60));
            }
            let engines: Vec<Engine> = self.inner.shares.lock().unwrap().values().cloned().collect();
            for e in engines {
                e.feed_discovery(clone_event(&ev));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Carpetas compartidas
    // -----------------------------------------------------------------------

    /// Comprueba la carpeta y que no coincida ni se solape con otra compartida.
    fn validate_folder(&self, folder: &Path, except: Option<&str>) -> Result<PathBuf, String> {
        let canon = check_folder(folder)?;
        for (id, e) in self.inner.shares.lock().unwrap().iter() {
            if Some(id.as_str()) == except {
                continue;
            }
            if let Some(other) = e.folder() {
                if canon.starts_with(&other) || other.starts_with(&canon) {
                    return Err(format!("Esa carpeta coincide o está dentro de otra carpeta compartida ({}). Elige otra.", other.display()));
                }
            }
        }
        Ok(canon)
    }

    fn new_share_id(&self) -> String {
        hex::encode(crate::auth::random_bytes::<6>())
    }

    async fn prepare(&self, folder: &Path, mode: ShareMode) -> Result<(String, Engine), String> {
        let canon = self.validate_folder(folder, None)?;
        let id = self.new_share_id();
        let engine = self.start_engine(&id).await.map_err(io_err)?;
        if let Err(e) = engine.set_folder(&canon) {
            self.discard(&id, &engine).await;
            return Err(e);
        }
        engine.set_mode(mode);
        Ok((id, engine))
    }

    async fn discard(&self, id: &str, engine: &Engine) {
        engine.shutdown().await;
        let _ = std::fs::remove_dir_all(self.share_dir(id));
    }

    fn register(&self, id: &str, engine: Engine) {
        self.inner.shares.lock().unwrap().insert(id.to_string(), engine);
        self.inner.settings.lock().unwrap().shares.push(id.to_string());
        self.save();
    }

    /// Crea una carpeta compartida nueva. Devuelve (id, código de invitación).
    pub async fn create_share(&self, name: &str, folder: &Path, mode: ShareMode) -> Result<(String, String), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Ponle un nombre a la carpeta compartida.".into());
        }
        let (id, engine) = self.prepare(folder, mode).await?;
        let code = engine.create_group_named(name);
        tracing::info!("Carpeta compartida «{name}» creada");
        self.register(&id, engine);
        Ok((id, code))
    }

    /// Invitaciones abiertas (carpetas de otros equipos aceptando equipos).
    pub fn invites(&self) -> Vec<InviteView> {
        let mine: Vec<String> = self.inner.shares.lock().unwrap().values().filter_map(|e| e.snapshot().group.map(|g| g.id)).collect();
        let aliases = self.inner.settings.lock().unwrap().aliases.clone();
        let mut v: Vec<InviteView> = self
            .inner
            .invites
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, i)| !mine.contains(&i.advert.group_id))
            .map(|(k, i)| InviteView {
                key: k.clone(),
                device_id: i.advert.device_id.clone(),
                device_name: aliases.get(&i.advert.device_id).cloned().unwrap_or_else(|| i.advert.name.clone()),
                group_id: i.advert.group_id.clone(),
                group_name: i.advert.group_name.clone(),
            })
            .collect();
        v.sort_by(|a, b| a.group_name.cmp(&b.group_name).then(a.device_name.cmp(&b.device_name)));
        v
    }

    /// Se une a una carpeta compartida descubierta (`invite`, o la única abierta).
    pub async fn join_share(&self, invite: Option<&str>, code: &str, folder: &Path, mode: ShareMode) -> Result<String, String> {
        let open = self.invites();
        let chosen = match invite {
            Some(k) => open.iter().find(|i| i.key == k).cloned(),
            None => {
                let groups: std::collections::HashSet<&str> = open.iter().map(|i| i.group_id.as_str()).collect();
                if groups.len() > 1 {
                    return Err("Hay varias carpetas aceptando equipos. Elige a cuál unirte.".into());
                }
                open.first().cloned()
            }
        };
        let Some(inv) = chosen else {
            return Err("No se encuentra ninguna carpeta compartida esperando equipos. Pulsa «Invitar» en el otro equipo y comprueba que estáis en la misma red. Si la red no permite el descubrimiento, usa «Conexión manual».".into());
        };
        let cands: Vec<Candidate> = self.inner.invites.lock().unwrap().get(&inv.key).map(|i| i.addrs.clone()).unwrap_or_default();
        let (id, engine) = self.prepare(folder, mode).await?;
        match engine.join_group_candidates(cands, code).await {
            Ok(()) => {
                self.register(&id, engine);
                Ok(id)
            }
            Err(e) => {
                self.discard(&id, &engine).await;
                Err(e)
            }
        }
    }

    /// Unión manual (recuperación cuando el descubrimiento no funciona).
    pub async fn join_share_manual(&self, host: &str, port: Option<u16>, code: &str, folder: &Path, mode: ShareMode, remember: bool) -> Result<String, String> {
        let (id, engine) = self.prepare(folder, mode).await?;
        match engine.join_group_manual(host, port, code, remember).await {
            Ok(()) => {
                self.register(&id, engine);
                Ok(id)
            }
            Err(e) => {
                self.discard(&id, &engine).await;
                Err(e)
            }
        }
    }

    /// Unión contra una dirección concreta (pruebas y diagnóstico).
    pub async fn join_share_at(&self, addr: SocketAddr, code: &str, folder: &Path, mode: ShareMode) -> Result<String, String> {
        let (id, engine) = self.prepare(folder, mode).await?;
        match engine.join_group_at(addr, code).await {
            Ok(()) => {
                self.register(&id, engine);
                Ok(id)
            }
            Err(e) => {
                self.discard(&id, &engine).await;
                Err(e)
            }
        }
    }

    fn engine(&self, id: &str) -> Result<Engine, String> {
        self.inner.shares.lock().unwrap().get(id).cloned().ok_or_else(|| "Esa carpeta compartida ya no existe.".to_string())
    }

    /// Motor de una carpeta compartida (para pruebas y diagnóstico).
    pub fn share(&self, id: &str) -> Option<Engine> {
        self.engine(id).ok()
    }

    pub fn share_ids(&self) -> Vec<String> {
        self.inner.settings.lock().unwrap().shares.clone()
    }

    /// Deja de compartir esta carpeta. Los archivos de la carpeta no se tocan.
    pub async fn leave_share(&self, id: &str) -> Result<(), String> {
        let engine = self.engine(id)?;
        engine.leave_group();
        self.inner.shares.lock().unwrap().remove(id);
        self.inner.settings.lock().unwrap().shares.retain(|s| s != id);
        self.save();
        self.discard(id, &engine).await;
        Ok(())
    }

    pub fn open_invite(&self, id: &str) -> Result<String, String> {
        self.engine(id)?.open_pairing()
    }

    pub fn close_invite(&self, id: &str) -> Result<(), String> {
        self.engine(id)?.close_pairing();
        Ok(())
    }

    pub fn set_mode(&self, id: &str, mode: ShareMode) -> Result<(), String> {
        self.engine(id)?.set_mode(mode);
        Ok(())
    }

    pub fn rename_share(&self, id: &str, name: &str) -> Result<(), String> {
        self.engine(id)?.rename_group(name);
        Ok(())
    }

    pub fn ack_conflicts(&self, id: &str) -> Result<(), String> {
        self.engine(id)?.ack_conflicts();
        Ok(())
    }

    pub fn folder(&self, id: &str) -> Option<PathBuf> {
        self.engine(id).ok().and_then(|e| e.folder())
    }

    pub fn change_folder(&self, id: &str, folder: &Path) -> Result<PathBuf, String> {
        let canon = self.validate_folder(folder, Some(id))?;
        self.engine(id)?.set_folder(&canon)
    }

    // -----------------------------------------------------------------------
    // Nombres
    // -----------------------------------------------------------------------

    pub fn set_device_name(&self, name: &str) {
        let name: String = name.trim().chars().filter(|c| !c.is_control()).take(60).collect();
        if name.is_empty() {
            return;
        }
        self.inner.settings.lock().unwrap().device_name = name.clone();
        self.save();
        for e in self.inner.shares.lock().unwrap().values() {
            e.set_device_name(&name);
        }
        if let Some(i) = &self.inner.inbox {
            i.set_device_name(&name);
        }
    }

    /// Nombre local para otro equipo. Vacío = volver a su nombre propio.
    /// Solo afecta a lo que se ve en este equipo.
    pub fn set_alias(&self, device_id: &str, alias: &str) {
        let alias: String = alias.trim().chars().filter(|c| !c.is_control()).take(60).collect();
        {
            let mut s = self.inner.settings.lock().unwrap();
            if alias.is_empty() {
                s.aliases.remove(device_id);
            } else {
                s.aliases.insert(device_id.to_string(), alias);
            }
        }
        self.save();
    }

    // -----------------------------------------------------------------------
    // VPN
    // -----------------------------------------------------------------------

    /// Permite o impide las conexiones que llegan por túneles VPN.
    pub fn set_listen_vpn(&self, on: bool) {
        self.inner.settings.lock().unwrap().listen_vpn = on;
        self.save();
        crate::netpath::set_listen_vpn(on);
        tracing::info!("Conexiones por VPN {}", if on { "permitidas" } else { "no permitidas" });
        for e in self.inner.shares.lock().unwrap().values() {
            e.apply_vpn_policy();
        }
    }

    /// Direcciones de este equipo y puerto de cada carpeta.
    pub fn network(&self) -> NetworkView {
        let mut addresses: Vec<AddressView> = crate::netpath::local_interfaces()
            .into_iter()
            .filter(|i| i.ip.is_ipv4() && crate::netpath::usable(i.ip))
            .map(|i| AddressView { interface: i.name, ip: i.ip.to_string(), tunnel: i.tunnel })
            .collect();
        addresses.sort_by_key(|a| a.tunnel);
        let map = self.inner.shares.lock().unwrap();
        let ports = self
            .inner
            .settings
            .lock()
            .unwrap()
            .shares
            .iter()
            .filter_map(|id| map.get(id))
            .map(|e| PortView { share: e.snapshot().group.map(|g| g.name).unwrap_or_default(), port: e.port() })
            .collect();
        NetworkView { listen_vpn: crate::netpath::listen_vpn(), addresses, ports }
    }

    // -----------------------------------------------------------------------
    // Recepción por navegador (QR)
    // -----------------------------------------------------------------------

    fn default_received_dir(&self) -> PathBuf {
        if let Some(d) = &self.inner.cfg.received_dir {
            return d.clone();
        }
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        home.join("Downloads").join("Recibidos Home File Sync")
    }

    /// Abre la recepción: en una carpeta compartida (`Some(id)`) o en la de recibidos.
    pub fn start_receive(&self, share: Option<&str>) -> Result<ReceiveView, String> {
        let inbox = self.inner.inbox.as_ref().ok_or("La recepción por navegador no está disponible en este equipo.")?;
        let (dest, temp, label) = match share {
            Some(id) => {
                let e = self.engine(id)?;
                let root = e.folder().ok_or("Esa carpeta compartida no tiene carpeta local.")?;
                let name = e.snapshot().group.map(|g| g.name).unwrap_or_default();
                (root.clone(), crate::fsops::meta_dir(&root).join("tmp"), format!("la carpeta compartida «{name}»"))
            }
            None => {
                let d = self.default_received_dir();
                let t = d.join(".recibiendo");
                (d.clone(), t, format!("la carpeta {}", d.display()))
            }
        };
        inbox.open(&dest, &temp, &label, crate::inbox::DEFAULT_TTL).map_err(io_err)
    }

    pub fn stop_receive(&self) {
        if let Some(i) = &self.inner.inbox {
            i.close();
        }
    }

    pub fn inbox_port(&self) -> Option<u16> {
        self.inner.inbox.as_ref().map(|i| i.port())
    }

    // -----------------------------------------------------------------------
    // Estado
    // -----------------------------------------------------------------------

    pub fn device_id(&self) -> String {
        self.inner.my_id.clone()
    }

    pub fn snapshot(&self) -> HubState {
        let s = self.inner.settings.lock().unwrap().clone();
        let engines: Vec<(String, Engine)> = {
            let map = self.inner.shares.lock().unwrap();
            s.shares.iter().filter_map(|id| map.get(id).map(|e| (id.clone(), e.clone()))).collect()
        };
        let mut shares = Vec::new();
        let mut warning = None;
        for (id, e) in engines {
            let mut st = e.snapshot();
            // Nombres locales.
            let mut renamed: HashMap<String, String> = HashMap::new();
            for d in st.devices.iter_mut() {
                if let Some(a) = s.aliases.get(&d.id) {
                    renamed.insert(d.name.clone(), a.clone());
                    d.name = a.clone();
                }
            }
            for t in st.transfers.iter_mut() {
                if let Some(a) = renamed.get(&t.peer) {
                    t.peer = a.clone();
                }
            }
            st.devices.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            if warning.is_none() {
                warning = st.network_warning.clone();
            }
            shares.push(ShareState { id, state: st });
        }
        HubState {
            device_name: s.device_name.clone(),
            device_id: self.inner.my_id.clone(),
            shares,
            invites: self.invites(),
            receive: self.inner.inbox.as_ref().and_then(|i| i.view()),
            network_warning: warning,
        }
    }

    pub async fn shutdown(&self) {
        for t in self.inner.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
        if let Some(mut d) = self.inner.discovery.lock().unwrap().take() {
            d.stop();
        }
        let engines: Vec<Engine> = self.inner.shares.lock().unwrap().values().cloned().collect();
        for e in engines {
            e.shutdown().await;
        }
    }
}

fn clone_event(ev: &DiscoveryEvent) -> DiscoveryEvent {
    match ev {
        DiscoveryEvent::Seen(s) => DiscoveryEvent::Seen(crate::discovery::Sighting { advert: s.advert.clone(), addrs: s.addrs.clone(), from: s.from }),
        DiscoveryEvent::Gone(k) => DiscoveryEvent::Gone(k.clone()),
    }
}
