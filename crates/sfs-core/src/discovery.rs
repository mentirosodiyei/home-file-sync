//! Descubrimiento automático de equipos en la LAN.
//!
//! Dos mecanismos complementarios, ninguno requiere configuración:
//! * **mDNS/DNS-SD** (`_showfilesync._tcp`), el estándar de Bonjour, implementado
//!   en Rust puro (no requiere instalar Bonjour en Windows).
//! * **Baliza UDP broadcast** cada pocos segundos en el puerto 47801, como
//!   respaldo para redes o equipos donde mDNS no funciona bien.
//!
//! Lo anunciado no es secreto ni se usa para autenticar: solo sirve para
//! saber a quién conectar. La identidad real se comprueba en la conexión.

use crate::netpath::{AddrSource, Candidate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

pub const SERVICE_TYPE: &str = "_showfilesync._tcp.local.";
pub const DEFAULT_BEACON_PORT: u16 = 47801;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Advert {
    #[serde(rename = "id")]
    pub device_id: String,
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "p")]
    pub port: u16,
    #[serde(rename = "g", default)]
    pub group_id: String,
    #[serde(rename = "gn", default)]
    pub group_name: String,
    /// El equipo está mostrando un código de emparejamiento.
    #[serde(rename = "o", default)]
    pub pairing: bool,
    #[serde(rename = "v", default)]
    pub proto: u32,
}

#[derive(Clone, Debug)]
pub struct Sighting {
    pub advert: Advert,
    pub addrs: Vec<Candidate>,
    /// Dirección de origen del anuncio (solo baliza UDP).
    pub from: Option<SocketAddr>,
}

#[derive(Debug)]
pub enum DiscoveryEvent {
    Seen(Sighting),
    Gone(String),
}

pub struct Discovery {
    advert: Arc<Mutex<Advert>>,
    mdns: Option<mdns_sd::ServiceDaemon>,
    stop: Arc<AtomicBool>,
    pub mdns_error: Option<String>,
    pub beacon_error: Option<String>,
}

impl Discovery {
    pub fn start(advert: Advert, beacon_port: u16, tx: UnboundedSender<DiscoveryEvent>) -> Self {
        let advert = Arc::new(Mutex::new(advert));
        let stop = Arc::new(AtomicBool::new(false));
        let mut d = Discovery { advert, mdns: None, stop, mdns_error: None, beacon_error: None };
        match mdns_sd::ServiceDaemon::new() {
            Ok(daemon) => {
                d.mdns = Some(daemon);
                d.register_mdns();
                if let Err(e) = d.browse_mdns(tx.clone()) {
                    d.mdns_error = Some(e);
                }
            }
            Err(e) => d.mdns_error = Some(e.to_string()),
        }
        if let Err(e) = d.start_beacon(beacon_port, tx) {
            d.beacon_error = Some(e.to_string());
        }
        d
    }

    fn register_mdns(&self) {
        let Some(daemon) = &self.mdns else { return };
        let a = self.advert.lock().unwrap().clone();
        let proto = a.proto.to_string();
        let props = [
            ("id", a.device_id.as_str()),
            ("n", a.name.as_str()),
            ("g", a.group_id.as_str()),
            ("gn", a.group_name.as_str()),
            ("o", if a.pairing { "1" } else { "0" }),
            ("v", proto.as_str()),
        ];
        let host = format!("sfs-{}.local.", &a.device_id[..12.min(a.device_id.len())]);
        match mdns_sd::ServiceInfo::new(SERVICE_TYPE, &a.device_id, &host, "", a.port, &props[..]) {
            Ok(info) => {
                if let Err(e) = daemon.register(info.enable_addr_auto()) {
                    tracing::warn!("mDNS register: {e}");
                }
            }
            Err(e) => tracing::warn!("mDNS info: {e}"),
        }
    }

    fn browse_mdns(&self, tx: UnboundedSender<DiscoveryEvent>) -> Result<(), String> {
        let daemon = self.mdns.as_ref().unwrap();
        let rx = daemon.browse(SERVICE_TYPE).map_err(|e| e.to_string())?;
        let me = self.advert.lock().unwrap().device_id.clone();
        let stop = self.stop.clone();
        std::thread::Builder::new()
            .name("sfs-mdns".into())
            .spawn(move || {
                let mut names: HashMap<String, String> = HashMap::new();
                while !stop.load(Ordering::Relaxed) {
                    let Ok(ev) = rx.recv_timeout(Duration::from_millis(500)) else { continue };
                    match ev {
                        mdns_sd::ServiceEvent::ServiceResolved(info) => {
                            let p = info.get_properties();
                            let get = |k: &str| p.get_property_val_str(k).unwrap_or("").to_string();
                            let advert = Advert {
                                device_id: get("id"),
                                name: get("n"),
                                port: info.get_port(),
                                group_id: get("g"),
                                group_name: get("gn"),
                                pairing: get("o") == "1",
                                proto: get("v").parse().unwrap_or(0),
                            };
                            if advert.device_id.is_empty() || advert.device_id == me {
                                continue;
                            }
                            names.insert(info.get_fullname().to_string(), advert.device_id.clone());
                            let addrs = info
                                .get_addresses()
                                .iter()
                                .filter(|ip| crate::netpath::usable(**ip))
                                .map(|ip| Candidate { addr: SocketAddr::new(*ip, advert.port), source: AddrSource::Mdns })
                                .collect();
                            let _ = tx.send(DiscoveryEvent::Seen(Sighting { advert, addrs, from: None }));
                        }
                        mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                            if let Some(id) = names.remove(&fullname) {
                                let _ = tx.send(DiscoveryEvent::Gone(id));
                            }
                        }
                        _ => {}
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn start_beacon(&self, port: u16, tx: UnboundedSender<DiscoveryEvent>) -> std::io::Result<()> {
        use socket2::{Domain, Protocol, Socket, Type};
        let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        sock.set_reuse_address(true)?;
        #[cfg(unix)]
        sock.set_reuse_port(true)?;
        sock.set_broadcast(true)?;
        sock.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).into())?;
        let recv: UdpSocket = sock.into();
        recv.set_read_timeout(Some(Duration::from_millis(500)))?;
        let send = UdpSocket::bind("0.0.0.0:0")?;
        send.set_broadcast(true)?;

        let stop = self.stop.clone();
        let advert = self.advert.clone();
        std::thread::Builder::new().name("sfs-beacon-tx".into()).spawn(move || {
            let mut tick = 0u32;
            while !stop.load(Ordering::Relaxed) {
                if tick % 6 == 0 {
                    let msg = serde_json::to_vec(&*advert.lock().unwrap()).unwrap_or_default();
                    for target in broadcast_targets() {
                        let _ = send.send_to(&msg, SocketAddrV4::new(target, port));
                    }
                }
                tick = tick.wrapping_add(1);
                std::thread::sleep(Duration::from_millis(500));
            }
        })?;

        let stop = self.stop.clone();
        let me = self.advert.lock().unwrap().device_id.clone();
        std::thread::Builder::new().name("sfs-beacon-rx".into()).spawn(move || {
            let mut buf = [0u8; 2048];
            while !stop.load(Ordering::Relaxed) {
                let Ok((n, from)) = recv.recv_from(&mut buf) else { continue };
                let Ok(advert) = serde_json::from_slice::<Advert>(&buf[..n]) else { continue };
                if advert.device_id == me || advert.device_id.len() != 32 || advert.port == 0 {
                    continue;
                }
                let addrs = vec![Candidate { addr: SocketAddr::new(from.ip(), advert.port), source: AddrSource::Beacon }];
                let _ = tx.send(DiscoveryEvent::Seen(Sighting { advert, addrs, from: Some(from) }));
            }
        })?;
        Ok(())
    }

    /// Actualiza lo anunciado (grupo, emparejamiento abierto, nombre…).
    pub fn update(&self, advert: Advert) {
        let changed = {
            let mut a = self.advert.lock().unwrap();
            let changed = *a != advert;
            *a = advert;
            changed
        };
        if changed {
            self.register_mdns();
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(d) = self.mdns.take() {
            let _ = d.shutdown();
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Direcciones de broadcast de todas las interfaces IPv4 (más la global).
fn broadcast_targets() -> Vec<Ipv4Addr> {
    let mut v = vec![Ipv4Addr::BROADCAST];
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        for i in ifs {
            if let if_addrs::IfAddr::V4(a) = i.addr {
                if let Some(b) = a.broadcast {
                    if !a.ip.is_loopback() && !v.contains(&b) {
                        v.push(b);
                    }
                }
            }
        }
    }
    v
}

/// ¿Tiene el equipo alguna IPv4 de red (no loopback)?
pub fn has_lan_ipv4() -> bool {
    if_addrs::get_if_addrs()
        .map(|ifs| ifs.iter().any(|i| matches!(&i.addr, if_addrs::IfAddr::V4(a) if !a.ip.is_loopback() && !a.ip.is_link_local())))
        .unwrap_or(false)
}
