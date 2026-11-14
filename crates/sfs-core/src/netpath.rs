//! Selección de direcciones y conexión TCP saliente.
//!
//! Un equipo puede anunciar varias direcciones (Ethernet, wifi, VPN, adaptadores
//! virtuales de Hyper-V/VirtualBox, IPv6…). Aquí:
//! * se ordenan las candidatas por probabilidad de ser alcanzables (misma
//!   subred que una interfaz local primero, enlace local y redes ajenas al final);
//! * se conectan de forma escalonada ("happy eyeballs"): se lanza la siguiente
//!   sin esperar a que la anterior agote su tiempo, y gana la primera que responde;
//! * cada intento queda registrado con su fase, interfaz y error del sistema;
//! * los fallos se traducen a un mensaje comprensible, incluido el bloqueo del
//!   permiso de **Red local** de macOS, que se manifiesta como
//!   `EHOSTUNREACH` ("No route to host", error 65) hacia direcciones de la propia subred.
//!
//! Tanto la conexión automática como la manual pasan por aquí.

use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::task::JoinSet;

/// Origen de una dirección candidata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AddrSource {
    /// Escrita por el usuario (conexión manual).
    Manual,
    /// Origen real del paquete de la baliza UDP: hay camino de vuelta probado.
    Beacon,
    /// Funcionó en una conexión anterior con ese equipo.
    Learned,
    /// Anunciada por mDNS (puede incluir adaptadores virtuales o de otras redes).
    Mdns,
    /// Comunicada por el propio equipo o por otro miembro del grupo.
    Reported,
}

impl AddrSource {
    pub fn text(&self) -> String {
        match self {
            AddrSource::Manual => tr!("manual"),
            AddrSource::Beacon => tr!("baliza UDP"),
            AddrSource::Mdns => tr!("mDNS"),
            AddrSource::Learned => tr!("conexión anterior"),
            AddrSource::Reported => tr!("comunicada por el grupo"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub addr: SocketAddr,
    pub source: AddrSource,
}

/// Interfaz de red local con su subred.
#[derive(Clone, Debug)]
pub struct LocalIf {
    pub name: String,
    pub ip: IpAddr,
    pub prefix: u8,
    /// Túnel VPN (WireGuard, OpenVPN, IPsec, PPP…), no una red local física.
    pub tunnel: bool,
}

// ---------------------------------------------------------------------------
// Túneles VPN
// ---------------------------------------------------------------------------

static LISTEN_VPN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Permite (o no) conexiones a través de túneles VPN. Desactivado por
/// defecto: sin este ajuste, el equipo solo atiende a la red local y no
/// comunica a los demás sus direcciones de VPN.
pub fn set_listen_vpn(on: bool) {
    LISTEN_VPN.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn listen_vpn() -> bool {
    LISTEN_VPN.load(std::sync::atomic::Ordering::Relaxed)
}

/// ¿Esta dirección local pertenece a un túnel VPN?
pub fn is_tunnel_addr(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v)),
        v4 => v4,
    };
    local_interfaces().iter().any(|i| i.ip == ip && i.tunnel)
}

/// ¿Debe aceptarse una conexión que llega a la dirección local `local`?
pub fn accept_on(local: IpAddr) -> bool {
    listen_vpn() || !is_tunnel_addr(local)
}

/// Nombres típicos de túneles en macOS/Linux y de adaptadores VPN conocidos.
fn tunnel_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["utun", "tun", "tap", "wg", "ppp", "ipsec", "zt", "tailscale", "wireguard", "wintun", "openvpn", "gpd", "nordlynx", "proton"].iter().any(|p| n.starts_with(p))
}

/// Windows: índices de los adaptadores que son túneles, según su tipo
/// (`IF_TYPE_PPP`, `IF_TYPE_TUNNEL`, `IF_TYPE_PROP_VIRTUAL` como WireGuard/Wintun)
/// o su descripción. El nombre visible lo elige el usuario y no sirve.
#[cfg(windows)]
fn windows_tunnel_indexes() -> std::collections::HashSet<u32> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH};
    let mut set = std::collections::HashSet::new();
    let mut size: u32 = 32 * 1024;
    for _ in 0..3 {
        let mut buf = vec![0u8; size as usize];
        let p = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
        // AF_UNSPEC = 0
        let r = unsafe { GetAdaptersAddresses(0, GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST, std::ptr::null(), p, &mut size) };
        if r == 111 {
            continue; // ERROR_BUFFER_OVERFLOW: reintenta con el tamaño indicado
        }
        if r != 0 {
            break;
        }
        let mut cur = p as *const IP_ADAPTER_ADDRESSES_LH;
        while !cur.is_null() {
            let a = unsafe { &*cur };
            let desc = unsafe { wide_to_string(a.Description) }.to_ascii_lowercase();
            let virt = matches!(a.IfType, 23 | 131 | 53);
            let known = ["wireguard", "wintun", "tap-windows", "openvpn", "tailscale", "zerotier", "vpn"].iter().any(|k| desc.contains(k));
            if virt || known {
                let idx = unsafe { a.Anonymous1.Anonymous.IfIndex };
                set.insert(idx);
                set.insert(a.Ipv6IfIndex);
            }
            cur = a.Next;
        }
        break;
    }
    set
}

#[cfg(windows)]
unsafe fn wide_to_string(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    while *p.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
}

/// Interfaces locales (sin loopback).
pub fn local_interfaces() -> Vec<LocalIf> {
    let Ok(ifs) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    #[cfg(windows)]
    let win_tunnels = windows_tunnel_indexes();
    ifs.into_iter()
        .filter(|i| !i.is_loopback())
        .map(|i| {
            #[cfg(windows)]
            let os_tunnel = i.index.is_some_and(|x| win_tunnels.contains(&x));
            // macOS/Linux: los túneles son punto a punto (sin broadcast).
            #[cfg(not(windows))]
            let os_tunnel = matches!(&i.addr, if_addrs::IfAddr::V4(a) if a.broadcast.is_none());
            let tunnel = os_tunnel || tunnel_name(&i.name);
            match i.addr {
                if_addrs::IfAddr::V4(a) => LocalIf { name: i.name, ip: IpAddr::V4(a.ip), prefix: u32::from(a.netmask).count_ones() as u8, tunnel },
                if_addrs::IfAddr::V6(a) => LocalIf { name: i.name, ip: IpAddr::V6(a.ip), prefix: u128::from(a.netmask).count_ones() as u8, tunnel },
            }
        })
        .collect()
}

fn same_subnet(a: IpAddr, b: IpAddr, prefix: u8) -> bool {
    match (a, b) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let m = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix.min(32))
            };
            u32::from(a) & m == u32::from(b) & m
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let p = if prefix == 0 || prefix > 128 {
                64
            } else {
                prefix
            };
            let m = u128::MAX << (128 - p);
            u128::from(a) & m == u128::from(b) & m
        }
        _ => false,
    }
}

/// Direcciones propias (todas las interfaces de red) con el puerto de escucha.
pub fn own_addresses(port: u16) -> Vec<String> {
    let vpn = listen_vpn();
    let mut ifs: Vec<LocalIf> = local_interfaces().into_iter().filter(|i| usable(i.ip) && (vpn || !i.tunnel)).collect();
    // IPv4 de red primero; enlace local al final.
    ifs.sort_by_key(|i| (is_link_local(i.ip), !i.ip.is_ipv4()));
    let mut v: Vec<String> = ifs.iter().map(|i| SocketAddr::new(i.ip, port).to_string()).collect();
    v.truncate(crate::protocol::EXT_MAX_ADDRS);
    v
}

/// Interfaz local cuya subred contiene `target` (la que usará el sistema).
pub fn interface_for(target: IpAddr, ifs: &[LocalIf]) -> Option<&LocalIf> {
    ifs.iter().find(|i| same_subnet(i.ip, target, i.prefix))
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_link_local(),
        IpAddr::V6(v) => (v.segments()[0] & 0xffc0) == 0xfe80,
    }
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private(),
        IpAddr::V6(v) => (v.segments()[0] & 0xfe00) == 0xfc00, // ULA fc00::/7
    }
}

/// ¿Se puede usar esta dirección para conectar? (descarta multicast, sin
/// especificar, y las IPv6 de enlace local, que requieren identificador de zona).
pub fn usable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => !v.is_unspecified() && !v.is_multicast() && v != Ipv4Addr::BROADCAST,
        IpAddr::V6(v) => !v.is_unspecified() && !v.is_multicast() && !is_link_local(IpAddr::V6(v)),
    }
}

/// Prioridad (menor = antes).
fn rank(c: &Candidate, ifs: &[LocalIf]) -> (u8, u8, u8) {
    let ip = c.addr.ip();
    let reach = if interface_for(ip, ifs).is_some() {
        0 // misma subred que una interfaz local: conexión directa
    } else if ip.is_loopback() {
        1
    } else if is_link_local(ip) {
        5 // 169.254.x.x sin interfaz en esa red: casi nunca alcanzable
    } else if is_private(ip) {
        3 // otra red privada (VPN, adaptador virtual): posible vía router
    } else {
        4
    };
    let family = if ip.is_ipv4() { 0 } else { 1 };
    let source = c.source as u8;
    (reach, family, source)
}

/// Quita duplicadas y no utilizables, y ordena por probabilidad de éxito.
pub fn order(cands: &[Candidate], ifs: &[LocalIf]) -> Vec<Candidate> {
    let mut v: Vec<Candidate> = Vec::new();
    for c in cands {
        if !usable(c.addr.ip()) || c.addr.port() == 0 {
            continue;
        }
        match v.iter_mut().find(|x| x.addr == c.addr) {
            Some(x) => {
                if c.source < x.source {
                    x.source = c.source;
                }
            }
            None => v.push(*c),
        }
    }
    v.sort_by_key(|c| rank(c, ifs));
    v
}

// ---------------------------------------------------------------------------
// Errores
// ---------------------------------------------------------------------------

/// Fase de la conexión en la que ocurre un fallo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Resolve,
    Tcp,
    Encryption,
    Auth,
    Pairing,
}

impl Phase {
    pub fn text(&self) -> String {
        match self {
            Phase::Resolve => tr!("resolución del nombre"),
            Phase::Tcp => tr!("conexión TCP"),
            Phase::Encryption => tr!("cifrado (Noise)"),
            Phase::Auth => tr!("autenticación de grupo"),
            Phase::Pairing => tr!("emparejamiento"),
        }
    }
}

/// Tipo de fallo de red, independiente del sistema operativo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetErrKind {
    HostUnreachable,
    NetworkUnreachable,
    Refused,
    TimedOut,
    Reset,
    Other,
}

pub fn classify(e: &io::Error) -> NetErrKind {
    use io::ErrorKind as K;
    match e.kind() {
        K::HostUnreachable => NetErrKind::HostUnreachable,
        K::NetworkUnreachable => NetErrKind::NetworkUnreachable,
        K::ConnectionRefused => NetErrKind::Refused,
        K::TimedOut => NetErrKind::TimedOut,
        K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe | K::UnexpectedEof => {
            NetErrKind::Reset
        }
        _ => match e.raw_os_error() {
            // macOS / Linux / Windows
            Some(65) | Some(113) | Some(10065) => NetErrKind::HostUnreachable,
            Some(51) | Some(101) | Some(10051) => NetErrKind::NetworkUnreachable,
            Some(61) | Some(111) | Some(10061) => NetErrKind::Refused,
            Some(60) | Some(110) | Some(10060) => NetErrKind::TimedOut,
            _ => NetErrKind::Other,
        },
    }
}

/// Un intento de conexión a una dirección.
#[derive(Clone, Debug)]
pub struct Attempt {
    pub addr: SocketAddr,
    pub source: AddrSource,
    pub iface: Option<String>,
    pub phase: Phase,
    pub kind: Option<NetErrKind>,
    pub os_code: Option<i32>,
    pub detail: String,
    pub elapsed: Duration,
}

impl Attempt {
    pub fn ok(&self) -> bool {
        self.kind.is_none() && self.detail.is_empty()
    }
}

impl fmt::Display for Attempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}", self.addr, self.source.text())?;
        if let Some(i) = &self.iface {
            write!(f, "{}", tr!(", interfaz {}", i))?;
        }
        write!(f, ") TCP")?;
        if self.ok() {
            write!(f, "{}", tr!(" → conectado en {} ms", self.elapsed.as_millis()))
        } else {
            write!(f, "{}", tr!(" → fallo en {}: {}", self.phase.text(), self.detail))?;
            if let Some(c) = self.os_code {
                write!(f, "{}", tr!(" [error del sistema {}]", c))?;
            }
            Ok(())
        }
    }
}

/// ¿Parece que macOS está bloqueando la app por el permiso de Red local?
/// `EHOSTUNREACH` hacia una dirección de la propia subred no tiene otra
/// explicación razonable: hay ruta directa, pero el sistema la deniega.
pub fn looks_like_macos_lnp_block(attempts: &[Attempt]) -> bool {
    cfg!(target_os = "macos")
        && attempts.iter().any(|a| {
            a.phase == Phase::Tcp
                && a.kind == Some(NetErrKind::HostUnreachable)
                && a.iface.is_some()
        })
}

/// Ayuda para el bloqueo del permiso «Red local» de macOS (traducida).
pub fn macos_lnp_help() -> String {
    tr!("macOS está bloqueando las conexiones de Home File Sync hacia la red local. Abre Ajustes del Sistema → Privacidad y seguridad → Red local y activa Home File Sync. Si ya estaba activado, desactívalo y vuelve a activarlo, y reinicia la aplicación.")
}

/// Mensaje comprensible para el usuario a partir de los intentos fallidos.
pub fn explain(attempts: &[Attempt]) -> String {
    if attempts.is_empty() {
        return tr!("No se conoce ninguna dirección del equipo.");
    }
    let main = if looks_like_macos_lnp_block(attempts) {
        macos_lnp_help()
    } else {
        let tcp: Vec<&Attempt> = attempts.iter().filter(|a| a.phase == Phase::Tcp).collect();
        let all = |k: NetErrKind| !tcp.is_empty() && tcp.iter().all(|a| a.kind == Some(k));
        if attempts.iter().any(|a| a.phase == Phase::Resolve) {
            tr!("No se encuentra ese nombre de equipo en la red. Prueba con su dirección IP.")
        } else if all(NetErrKind::Refused) {
            tr!("El equipo rechaza la conexión. Comprueba que Home File Sync está abierto en él y que el puerto es correcto.")
        } else if all(NetErrKind::TimedOut) {
            tr!("El equipo no responde. Probablemente un cortafuegos bloquea la conexión o la red aísla a los equipos entre sí.")
        } else if tcp.iter().all(|a| {
            matches!(
                a.kind,
                Some(NetErrKind::HostUnreachable | NetErrKind::NetworkUnreachable)
            )
        }) && !tcp.is_empty()
        {
            tr!("No hay ruta de red hasta el equipo: está en otra red o subred que este equipo no alcanza.")
        } else if attempts.iter().any(|a| a.phase != Phase::Tcp) {
            tr!("Se conectó con el equipo pero la comunicación se interrumpió. Vuelve a intentarlo.")
        } else {
            tr!("No se puede conectar con el equipo.")
        }
    };
    let detail: Vec<String> = attempts.iter().take(4).map(|a| a.to_string()).collect();
    format!("{main}\n\nDetalle: {}", detail.join("; "))
}

// ---------------------------------------------------------------------------
// Conexión escalonada
// ---------------------------------------------------------------------------

pub const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(4);
pub const STAGGER: Duration = Duration::from_millis(350);

fn failed(
    c: &Candidate,
    iface: Option<String>,
    phase: Phase,
    e: &io::Error,
    elapsed: Duration,
) -> Attempt {
    Attempt {
        addr: c.addr,
        source: c.source,
        iface,
        phase,
        kind: Some(classify(e)),
        os_code: e.raw_os_error(),
        detail: e.to_string(),
        elapsed,
    }
}

/// Conecta con la primera candidata que responda. Las candidatas deben venir
/// ya ordenadas ([`order`]). Devuelve el flujo TCP (si alguno conectó) y el
/// registro de todos los intentos terminados.
pub async fn connect_first(
    cands: &[Candidate],
    ifs: &[LocalIf],
    quiet: bool,
) -> (Option<(TcpStream, Candidate)>, Vec<Attempt>) {
    let mut attempts = Vec::new();
    let mut set: JoinSet<(usize, io::Result<TcpStream>, Duration)> = JoinSet::new();
    let mut next = 0usize;
    let mut next_at = tokio::time::Instant::now();
    loop {
        if next < cands.len() && (set.is_empty() || tokio::time::Instant::now() >= next_at) {
            let i = next;
            let addr = cands[i].addr;
            set.spawn(async move {
                let t = Instant::now();
                let r = match tokio::time::timeout(ATTEMPT_TIMEOUT, TcpStream::connect(addr)).await
                {
                    Ok(r) => r,
                    Err(_) => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        tr!("sin respuesta (tiempo de espera agotado)"),
                    )),
                };
                (i, r, t.elapsed())
            });
            next += 1;
            next_at = tokio::time::Instant::now() + STAGGER;
        }
        if set.is_empty() {
            break;
        }
        let more = next < cands.len();
        tokio::select! {
            Some(res) = set.join_next() => {
                let Ok((i, r, elapsed)) = res else { continue };
                let c = &cands[i];
                let iface = interface_for(c.addr.ip(), ifs).map(|l| l.name.clone());
                match r {
                    Ok(s) => {
                        let _ = s.set_nodelay(true);
                        tracing::info!(
                            "Conexión TCP establecida con {} ({}, interfaz {}) en {} ms",
                            c.addr, c.source.text(), iface.as_deref().unwrap_or("según tabla de rutas"), elapsed.as_millis()
                        );
                        set.abort_all();
                        return (Some((s, *c)), attempts);
                    }
                    Err(e) => {
                        let a = failed(c, iface, Phase::Tcp, &e, elapsed);
                        if quiet {
                            tracing::debug!("Intento de conexión fallido: {a}");
                        } else {
                            tracing::warn!("Intento de conexión fallido: {a}");
                        }
                        attempts.push(a);
                        // Tras un fallo, la siguiente candidata se lanza ya.
                        next_at = tokio::time::Instant::now();
                    }
                }
            }
            _ = tokio::time::sleep_until(next_at), if more => {}
        }
    }
    (None, attempts)
}

/// Registro de un fallo en una fase posterior a TCP (cifrado, autenticación…).
pub fn later_failure(
    c: &Candidate,
    ifs: &[LocalIf],
    phase: Phase,
    detail: impl Into<String>,
    os: Option<&io::Error>,
) -> Attempt {
    Attempt {
        addr: c.addr,
        source: c.source,
        iface: interface_for(c.addr.ip(), ifs).map(|l| l.name.clone()),
        phase,
        kind: os.map(classify),
        os_code: os.and_then(|e| e.raw_os_error()),
        detail: detail.into(),
        elapsed: Duration::ZERO,
    }
}

// ---------------------------------------------------------------------------
// Direcciones escritas por el usuario
// ---------------------------------------------------------------------------

/// Valida "IP", "nombre" o "[IPv6]" y devuelve (anfitrión, puerto si venía incluido).
pub fn parse_host(input: &str) -> Result<(String, Option<u16>), String> {
    let s = input.trim();
    if s.is_empty() {
        return Err(tr!("Escribe la dirección IP o el nombre del equipo."));
    }
    if s.contains("://") || s.contains('/') || s.contains(char::is_whitespace) {
        return Err(
            "Escribe solo la dirección IP o el nombre del equipo, sin «http://» ni rutas.".into(),
        );
    }
    // [IPv6]:puerto o [IPv6]
    if let Some(rest) = s.strip_prefix('[') {
        let (ip, after) = rest
            .split_once(']')
            .ok_or_else(|| tr!("Dirección IPv6 mal escrita: falta «]»."))?;
        let ip: Ipv6Addr = ip
            .parse()
            .map_err(|_| tr!("Dirección IPv6 no válida."))?;
        let port = match after {
            "" => None,
            p => Some(parse_port(
                p.strip_prefix(':').ok_or_else(|| tr!("Dirección IPv6 mal escrita."))?,
            )?),
        };
        return Ok((ip.to_string(), port));
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Ok((ip.to_string(), None));
    }
    // IPv4 o nombre con ":puerto".
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h, Some(parse_port(p)?)),
        Some(_) => return Err(tr!("Dirección IPv6 no válida.")),
        None => (s, None),
    };
    if host.parse::<Ipv4Addr>().is_ok() {
        return Ok((host.to_string(), port));
    }
    if host.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(tr!("Dirección IPv4 no válida."));
    }
    let valid = host.len() <= 253
        && host.trim_end_matches('.').split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if !valid {
        return Err(tr!("Nombre de equipo no válido."));
    }
    Ok((host.to_string(), port))
}

pub fn parse_port(p: &str) -> Result<u16, String> {
    match p.trim().parse::<u16>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(tr!("El puerto debe ser un número entre 1 y 65535.")),
    }
}

/// Resuelve un anfitrión a candidatas (todas sus direcciones IPv4/IPv6).
pub async fn resolve(host: &str, port: u16) -> Result<Vec<Candidate>, io::Error> {
    let target = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let addrs = tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host(target))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "la resolución del nombre no responde",
            )
        })??;
    Ok(addrs
        .map(|addr| Candidate {
            addr,
            source: AddrSource::Manual,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str, src: AddrSource) -> Candidate {
        Candidate {
            addr: s.parse().unwrap(),
            source: src,
        }
    }

    fn ifs() -> Vec<LocalIf> {
        vec![
            LocalIf {
                name: "en8".into(),
                ip: "192.168.1.102".parse().unwrap(),
                prefix: 24,
                tunnel: false,
            },
            LocalIf {
                name: "en0".into(),
                ip: "10.0.0.5".parse().unwrap(),
                prefix: 16,
                tunnel: false,
            },
        ]
    }

    #[test]
    fn orders_reachable_first() {
        // Lo que anunciaría un PC con Hyper-V, VirtualBox y APIPA además de su red real.
        let got = order(
            &[
                c("172.27.160.1:47800", AddrSource::Mdns),
                c("169.254.12.3:47800", AddrSource::Mdns),
                c("192.168.56.1:47800", AddrSource::Mdns),
                c("192.168.1.66:47800", AddrSource::Mdns),
                c("192.168.1.66:47800", AddrSource::Beacon),
                c("[fd00::66]:47800", AddrSource::Mdns),
                c("[fe80::1]:47800", AddrSource::Mdns),
                c("0.0.0.0:47800", AddrSource::Mdns),
            ],
            &ifs(),
        );
        let addrs: Vec<String> = got.iter().map(|c| c.addr.to_string()).collect();
        assert_eq!(addrs[0], "192.168.1.66:47800");
        assert_eq!(
            got[0].source,
            AddrSource::Beacon,
            "se conserva el mejor origen"
        );
        assert_eq!(addrs.last().unwrap(), "169.254.12.3:47800");
        assert!(!addrs
            .iter()
            .any(|a| a.starts_with("[fe80") || a.starts_with("0.0.0.0")));
        assert_eq!(addrs.len(), 5);
    }

    #[test]
    fn tunnel_names() {
        for n in ["utun3", "wg0", "tun0", "ppp0", "ipsec0", "tailscale0", "WireGuard Casa", "ProtonVPN"] {
            assert!(tunnel_name(n), "{n}");
        }
        for n in ["en0", "en8", "eth0", "Wi-Fi", "Ethernet", "bridge100"] {
            assert!(!tunnel_name(n), "{n}");
        }
    }

    #[test]
    fn interface_lookup() {
        let i = ifs();
        assert_eq!(
            interface_for("192.168.1.66".parse().unwrap(), &i)
                .unwrap()
                .name,
            "en8"
        );
        assert_eq!(
            interface_for("10.0.200.1".parse().unwrap(), &i)
                .unwrap()
                .name,
            "en0"
        );
        assert!(interface_for("192.168.3.1".parse().unwrap(), &i).is_none());
    }

    #[test]
    fn host_parsing() {
        assert_eq!(
            parse_host("192.168.1.66").unwrap(),
            ("192.168.1.66".into(), None)
        );
        assert_eq!(
            parse_host(" 192.168.1.66:47900 ").unwrap(),
            ("192.168.1.66".into(), Some(47900))
        );
        assert_eq!(parse_host("fd00::66").unwrap(), ("fd00::66".into(), None));
        assert_eq!(
            parse_host("[fd00::66]:47801").unwrap(),
            ("fd00::66".into(), Some(47801))
        );
        assert_eq!(
            parse_host("PORTATIL-SALON").unwrap(),
            ("PORTATIL-SALON".into(), None)
        );
        assert_eq!(
            parse_host("portatil-salon.local:47800").unwrap(),
            ("portatil-salon.local".into(), Some(47800))
        );
        for bad in [
            "",
            "http://192.168.1.66",
            "192.168.1.300",
            "192.168.2",
            "host name",
            "-bad-",
            "a..b",
            "1.2.3.4:0",
            "1.2.3.4:99999",
            "[fd00::1",
        ] {
            assert!(parse_host(bad).is_err(), "debería rechazar {bad:?}");
        }
    }

    #[test]
    fn explains_macos_local_network_block() {
        let a = Attempt {
            addr: "192.168.1.66:47800".parse().unwrap(),
            source: AddrSource::Beacon,
            iface: Some("en8".into()),
            phase: Phase::Tcp,
            kind: Some(NetErrKind::HostUnreachable),
            os_code: Some(65),
            detail: "No route to host (os error 65)".into(),
            elapsed: Duration::from_millis(1),
        };
        let msg = explain(&[a.clone()]);
        if cfg!(target_os = "macos") {
            assert!(msg.contains("Red local"), "{msg}");
        }
        assert!(
            msg.contains("192.168.1.66:47800")
                && msg.contains("interfaz en8")
                && msg.contains("65"),
            "{msg}"
        );
        // Fuera de la subred no se atribuye al permiso de macOS.
        let mut b = a;
        b.iface = None;
        assert!(!explain(&[b]).contains("Red local"));
    }

    #[test]
    fn classifies_os_errors() {
        assert_eq!(
            classify(&io::Error::from_raw_os_error(if cfg!(windows) {
                10065
            } else if cfg!(target_os = "macos") {
                65
            } else {
                113
            })),
            NetErrKind::HostUnreachable
        );
        assert_eq!(
            classify(&io::Error::from(io::ErrorKind::ConnectionRefused)),
            NetErrKind::Refused
        );
    }

    #[tokio::test]
    async fn staggered_connect_skips_dead_addresses_quickly() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let good = l.local_addr().unwrap();
        // Un puerto cerrado (rechazo inmediato) y una dirección de documentación
        // que no responde (TEST-NET-1) antes de la buena.
        let closed = {
            let t = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            t.local_addr().unwrap()
        };
        let cands = vec![
            Candidate {
                addr: "192.0.2.1:47800".parse().unwrap(),
                source: AddrSource::Mdns,
            },
            Candidate {
                addr: closed,
                source: AddrSource::Mdns,
            },
            Candidate {
                addr: good,
                source: AddrSource::Beacon,
            },
        ];
        let t = Instant::now();
        let (s, attempts) = connect_first(&cands, &[], false).await;
        let (_, won) = s.expect("debe conectar con la buena");
        assert_eq!(won.addr, good);
        assert!(
            t.elapsed() < Duration::from_secs(2),
            "no debe esperar al tiempo de espera de la inaccesible: {:?}",
            t.elapsed()
        );
        assert!(attempts
            .iter()
            .any(|a| a.addr == closed && a.kind == Some(NetErrKind::Refused)));
    }
}
