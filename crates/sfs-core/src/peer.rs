//! Conexiones entre equipos: aceptar, marcar, autenticar, emparejar y la
//! sesión de sincronización (índice, estado, keep-alive y enrutado de bloques).

use crate::auth::{ct_eq, device_id, group_proof, pair_confirm, pair_start_joiner, pair_start_member};
use crate::engine::{ConnHandle, Inner, Peer};
use crate::index::FileEntry;
use crate::netpath::{connect_first, explain, interface_for, later_failure, local_interfaces, order, AddrSource, Attempt, Candidate, Phase};
use crate::pathsafe::validate_wire_path;
use crate::protocol::{Ext, Msg, PeerStatus, RejectReason, EXT_CHANNEL, EXT_MAX_ADDRS, EXT_MAX_MEMBERS, EXT_VERSION, PROTOCOL_VERSION};
use crate::settings::Group;
use crate::transport::{handshake, Handshaken, SecureReader, SecureWriter};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(35);
const PING_EVERY: Duration = Duration::from_secs(10);

fn io_text(e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::ConnectionRefused => tr!("conexión rechazada"),
        std::io::ErrorKind::TimedOut => tr!("tiempo de espera agotado"),
        _ => e.to_string(),
    }
}

async fn recv_timeout(r: &mut SecureReader, d: Duration) -> Result<Msg, String> {
    match timeout(d, r.recv()).await {
        Ok(Ok(m)) => Ok(m),
        Ok(Err(e)) => Err(io_text(e)),
        Err(_) => Err(tr!("tiempo de espera agotado")),
    }
}

/// Fallo al conectar o emparejar con un equipo.
#[derive(Debug)]
pub(crate) enum LinkError {
    /// No se pudo establecer o mantener el canal: se puede probar otra dirección u otro equipo.
    Network(Vec<Attempt>),
    /// El otro equipo respondió y rechazó la petición (código incorrecto, otro grupo…).
    /// No tiene sentido insistir con otras direcciones.
    Rejected(String, Vec<Attempt>),
}

impl LinkError {
    pub fn attempts(&self) -> &[Attempt] {
        match self {
            LinkError::Network(a) | LinkError::Rejected(_, a) => a,
        }
    }

    /// Mensaje para el usuario.
    pub fn user_message(&self) -> String {
        match self {
            LinkError::Network(a) => explain(a),
            LinkError::Rejected(m, _) => m.clone(),
        }
    }
}

/// Abre un canal cifrado Noise con la primera candidata accesible.
/// Es el único punto de conexión saliente: lo usan la sincronización y el
/// emparejamiento, tanto automáticos como manuales.
async fn open_secure(inner: &Inner, cands: &[Candidate], purpose: &str, quiet: bool) -> Result<(Handshaken, Candidate, Vec<Attempt>), Vec<Attempt>> {
    let ifs = local_interfaces();
    let mut remaining = order(cands, &ifs);
    let list: Vec<String> = remaining
        .iter()
        .map(|c| match interface_for(c.addr.ip(), &ifs) {
            Some(i) => format!("{} ({}, interfaz {})", c.addr, c.source.text(), i.name),
            None => format!("{} ({}, fuera de las subredes locales)", c.addr, c.source.text()),
        })
        .collect();
    if quiet {
        tracing::debug!("{purpose}: direcciones candidatas en orden: [{}]", list.join(", "));
    } else {
        tracing::info!("{purpose}: direcciones candidatas en orden: [{}]", list.join(", "));
    }
    let mut attempts = Vec::new();
    loop {
        if remaining.is_empty() {
            return Err(attempts);
        }
        let (won, mut tried) = connect_first(&remaining, &ifs, quiet).await;
        let failed: Vec<SocketAddr> = tried.iter().map(|a| a.addr).collect();
        attempts.append(&mut tried);
        let Some((stream, c)) = won else { return Err(attempts) };
        remaining.retain(|x| x.addr != c.addr && !failed.contains(&x.addr));
        match timeout(HANDSHAKE_TIMEOUT, handshake(stream, &inner.private_key, true)).await {
            Ok(Ok(hs)) if device_id(&hs.remote_static) == inner.my_id => {
                tracing::info!("{} es este mismo equipo; se descarta", c.addr);
                attempts.push(later_failure(&c, &ifs, Phase::Encryption, tr!("es este mismo equipo"), None));
            }
            Ok(Ok(hs)) => return Ok((hs, c, attempts)),
            Ok(Err(e)) => {
                let a = later_failure(&c, &ifs, Phase::Encryption, e.to_string(), Some(&e));
                tracing::warn!("Fallo tras conectar: {a}");
                attempts.push(a);
            }
            Err(_) => {
                let a = later_failure(&c, &ifs, Phase::Encryption, tr!("sin respuesta al cifrado (¿es Home File Sync?)"), None);
                tracing::warn!("Fallo tras conectar: {a}");
                attempts.push(a);
            }
        }
    }
}

fn phase_err(c: &Candidate, phase: Phase, detail: impl Into<String>, os: Option<&std::io::Error>) -> Attempt {
    let a = later_failure(c, &local_interfaces(), phase, detail, os);
    tracing::warn!("Fallo: {a}");
    a
}

// ---------------------------------------------------------------------------
// Lado que inicia
// ---------------------------------------------------------------------------

/// Conecta con un equipo del grupo y lanza la sesión de sincronización.
/// Devuelve el id del equipo en cuanto la sesión queda establecida.
/// Con `quiet`, los reintentos periódicos se registran a nivel DEBUG.
pub(crate) async fn dial(inner: Arc<Inner>, cands: &[Candidate], quiet: bool) -> Result<String, LinkError> {
    let group = inner.group().ok_or_else(|| LinkError::Rejected(tr!("Este equipo no pertenece a ningún grupo."), vec![]))?;
    let (mut hs, c, mut attempts) = open_secure(&inner, cands, "Conexión de sincronización", quiet).await.map_err(LinkError::Network)?;
    let remote = device_id(&hs.remote_static);
    if inner.peers.lock().unwrap().get(&remote).is_some_and(|p| p.conn.is_some()) {
        return Ok(remote); // ya conectados por otra vía
    }
    let key = group.key_bytes();
    let hello = Msg::Hello { proto: PROTOCOL_VERSION, name: inner.name(), group_id: group.id.clone(), proof: group_proof(&key, "client", &hs.handshake_hash) };
    if let Err(e) = hs.writer.send(&hello).await {
        attempts.push(phase_err(&c, Phase::Auth, e.to_string(), Some(&e)));
        return Err(LinkError::Network(attempts));
    }
    match recv_timeout(&mut hs.reader, HANDSHAKE_TIMEOUT).await {
        Ok(Msg::Welcome { name, proof }) => {
            if !ct_eq(&proof, &group_proof(&key, "server", &hs.handshake_hash)) {
                attempts.push(phase_err(&c, Phase::Auth, tr!("el otro equipo no ha demostrado pertenecer al grupo"), None));
                return Err(LinkError::Rejected(tr!("El otro equipo no ha demostrado pertenecer al grupo."), attempts));
            }
            tracing::info!("Autenticado con «{name}» ({}) en {}", &remote[..8], c.addr);
            // La dirección que ha funcionado se recuerda para reconectar sin
            // descubrimiento (las escritas a mano solo si el usuario lo pidió).
            if c.source != AddrSource::Manual {
                inner.learn_addr(&remote, Candidate { addr: c.addr, source: AddrSource::Learned });
            }
            let my_id = inner.my_id.clone();
            tokio::spawn(run_session(inner, hs, remote.clone(), name, my_id, None, false));
            Ok(remote)
        }
        Ok(Msg::Reject { reason }) => {
            attempts.push(phase_err(&c, Phase::Auth, reason.text(), None));
            Err(LinkError::Rejected(capitalize(&reason.text()), attempts))
        }
        Ok(_) => {
            attempts.push(phase_err(&c, Phase::Auth, tr!("respuesta inesperada"), None));
            Err(LinkError::Network(attempts))
        }
        Err(e) => {
            attempts.push(phase_err(&c, Phase::Auth, e, None));
            Err(LinkError::Network(attempts))
        }
    }
}

/// Emparejamiento desde el equipo nuevo. Devuelve el grupo recibido, la
/// dirección por la que se ha conseguido y el id del equipo que lo ha dado.
pub(crate) async fn pair_as_joiner(inner: &Arc<Inner>, cands: &[Candidate], code: &str) -> Result<(Group, Candidate, String), LinkError> {
    let (mut hs, c, mut attempts) = open_secure(inner, cands, "Emparejamiento", false).await.map_err(LinkError::Network)?;
    let remote = device_id(&hs.remote_static);
    let net = |mut attempts: Vec<Attempt>, detail: String| {
        attempts.push(phase_err(&c, Phase::Pairing, detail, None));
        LinkError::Network(attempts)
    };
    let rejected = |mut attempts: Vec<Attempt>, msg: String| {
        attempts.push(phase_err(&c, Phase::Pairing, msg.clone(), None));
        LinkError::Rejected(msg, attempts)
    };
    let (state, msg) = pair_start_joiner(code);
    if let Err(e) = hs.writer.send(&Msg::PairStart { proto: PROTOCOL_VERSION, name: inner.name(), spake: msg }).await {
        return Err(net(attempts, e.to_string()));
    }
    match recv_timeout(&mut hs.reader, Duration::from_secs(20)).await {
        Ok(Msg::PairReply { spake, confirm }) => {
            let Some(key) = state.finish(&spake) else { return Err(rejected(attempts, tr!("Código incorrecto."))) };
            if !ct_eq(&confirm, &pair_confirm(&key, "member", &hs.handshake_hash)) {
                return Err(rejected(attempts, tr!("Código incorrecto.")));
            }
            if let Err(e) = hs.writer.send(&Msg::PairConfirm { confirm: pair_confirm(&key, "joiner", &hs.handshake_hash) }).await {
                return Err(net(attempts, e.to_string()));
            }
            match recv_timeout(&mut hs.reader, Duration::from_secs(20)).await {
                Ok(Msg::PairGroup { group_id, group_name, group_key }) if group_key.len() == 32 && !group_id.is_empty() => {
                    tracing::info!("Emparejamiento completado con {} ({}): grupo «{group_name}»", c.addr, c.source.text());
                    attempts.clear();
                    Ok((Group { id: group_id, name: group_name, key: hex::encode(group_key) }, c, remote))
                }
                Ok(Msg::Reject { reason }) => Err(rejected(attempts, capitalize(&reason.text()))),
                Ok(_) => Err(net(attempts, tr!("respuesta inesperada"))),
                Err(e) => Err(net(attempts, e)),
            }
        }
        Ok(Msg::Reject { reason }) => Err(rejected(attempts, capitalize(&reason.text()))),
        Ok(_) => Err(net(attempts, tr!("respuesta inesperada"))),
        Err(e) => Err(net(attempts, e)),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str() + ".",
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Lado que acepta
// ---------------------------------------------------------------------------

pub(crate) async fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    loop {
        let Ok((stream, addr)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        // Conexiones que llegan por un túnel VPN: solo si el usuario lo permite.
        if let Ok(local) = stream.local_addr() {
            if !crate::netpath::accept_on(local.ip()) {
                tracing::info!("Conexión desde {addr} por VPN ({}) rechazada: «Permitir conexiones por VPN» está desactivado", local.ip());
                continue;
            }
        }
        let i = inner.clone();
        tokio::spawn(async move {
            tracing::debug!("Conexión entrante desde {addr}");
            if let Err(e) = accept_one(i, stream).await {
                tracing::info!("Conexión entrante desde {addr} no establecida: {e}");
            }
        });
    }
}

async fn accept_one(inner: Arc<Inner>, stream: TcpStream) -> Result<(), String> {
    let observed = stream.peer_addr().ok();
    let via_tunnel = stream.local_addr().is_ok_and(|l| crate::netpath::is_tunnel_addr(l.ip()));
    let mut hs = timeout(HANDSHAKE_TIMEOUT, handshake(stream, &inner.private_key, false))
        .await
        .map_err(|_| "timeout")?
        .map_err(io_text)?;
    let remote = device_id(&hs.remote_static);
    if remote == inner.my_id {
        return Err("conexión con este mismo equipo".into());
    }
    match recv_timeout(&mut hs.reader, HANDSHAKE_TIMEOUT).await? {
        Msg::Hello { proto, name, group_id, proof } => {
            let reject = |r| Msg::Reject { reason: r };
            if proto != PROTOCOL_VERSION {
                let _ = hs.writer.send(&reject(RejectReason::Version)).await;
                return Err("versión".into());
            }
            let Some(group) = inner.group().filter(|g| g.id == group_id) else {
                let _ = hs.writer.send(&reject(RejectReason::NotInGroup)).await;
                return Err("otro grupo".into());
            };
            let key = group.key_bytes();
            if !ct_eq(&proof, &group_proof(&key, "client", &hs.handshake_hash)) {
                let _ = hs.writer.send(&reject(RejectReason::BadProof)).await;
                return Err("prueba de grupo no válida".into());
            }
            hs.writer
                .send(&Msg::Welcome { name: inner.name(), proof: group_proof(&key, "server", &hs.handshake_hash) })
                .await
                .map_err(io_text)?;
            run_session(inner, hs, remote.clone(), name, remote, observed, via_tunnel).await;
            Ok(())
        }
        Msg::PairStart { proto, name, spake } => pair_as_member(inner, hs, proto, name, spake).await,
        _ => Err("mensaje inicial inesperado".into()),
    }
}

async fn pair_as_member(inner: Arc<Inner>, mut hs: Handshaken, proto: u32, name: String, spake: Vec<u8>) -> Result<(), String> {
    let reject = |r| Msg::Reject { reason: r };
    if proto != PROTOCOL_VERSION {
        let _ = hs.writer.send(&reject(RejectReason::Version)).await;
        return Err("versión".into());
    }
    let window: Result<(String, Group), RejectReason> = {
        let p = inner.pairing.lock().unwrap();
        match (p.as_ref(), inner.group()) {
            (Some(p), Some(_)) if p.expires > std::time::Instant::now() && p.failures >= 5 => Err(RejectReason::PairingLocked),
            (Some(p), Some(g)) if p.expires > std::time::Instant::now() => Ok((p.code.clone(), g)),
            _ => Err(RejectReason::PairingClosed),
        }
    };
    let (code, group) = match window {
        Ok(w) => w,
        Err(r) => {
            let _ = hs.writer.send(&reject(r)).await;
            return Err(r.text().into());
        }
    };
    let fail = |inner: &Inner| {
        let mut p = inner.pairing.lock().unwrap();
        if let Some(p) = p.as_mut() {
            p.failures += 1;
        }
    };
    let (state, msg) = pair_start_member(&code);
    let Some(key) = state.finish(&spake) else {
        fail(&inner);
        let _ = hs.writer.send(&reject(RejectReason::PairingFailed)).await;
        return Err("SPAKE2 inválido".into());
    };
    hs.writer
        .send(&Msg::PairReply { spake: msg, confirm: pair_confirm(&key, "member", &hs.handshake_hash) })
        .await
        .map_err(io_text)?;
    match recv_timeout(&mut hs.reader, Duration::from_secs(20)).await {
        Ok(Msg::PairConfirm { confirm }) if ct_eq(&confirm, &pair_confirm(&key, "joiner", &hs.handshake_hash)) => {
            hs.writer
                .send(&Msg::PairGroup { group_id: group.id.clone(), group_name: group.name.clone(), group_key: group.key_bytes().to_vec() })
                .await
                .map_err(io_text)?;
            inner.event("info", tr!("{} se ha unido al grupo", name));
            // Esperamos a que el otro lado cierre para no cortar el último mensaje.
            let _ = timeout(Duration::from_secs(5), hs.reader.recv()).await;
            Ok(())
        }
        _ => {
            fail(&inner);
            inner.refresh_advert();
            let _ = hs.writer.send(&reject(RejectReason::PairingFailed)).await;
            inner.event("warn", tr!("Intento de emparejamiento fallido desde «{}»", name));
            Err("código incorrecto".into())
        }
    }
}

// ---------------------------------------------------------------------------
// Sesión de sincronización
// ---------------------------------------------------------------------------

/// Registra la conexión. Si ya había otra con el mismo equipo, se queda la
/// iniciada por el equipo de id menor (ambos lados deciden igual).
fn register(inner: &Inner, remote: &str, name: &str, handle: ConnHandle) -> bool {
    let mut peers = inner.peers.lock().unwrap();
    let peer = peers.entry(remote.to_string()).or_insert_with(|| Peer {
        name: name.to_string(),
        conn: None,
        index: HashMap::new(),
        index_ready: false,
        status: PeerStatus::default(),
    });
    if let Some(existing) = &peer.conn {
        let keep_new = existing.dialer == handle.dialer || handle.dialer < existing.dialer;
        if !keep_new {
            return false;
        }
        let _ = existing.close.send(true);
    }
    peer.name = name.to_string();
    peer.conn = Some(handle);
    peer.index.clear();
    peer.index_ready = false;
    peer.status = PeerStatus::default();
    drop(peers);
    let is_new = inner.settings.lock().unwrap().known.insert(remote.to_string(), name.to_string()).as_deref() != Some(name);
    if is_new {
        inner.save_settings();
    }
    true
}

fn unregister(inner: &Inner, remote: &str, conn_id: u64) {
    let mut peers = inner.peers.lock().unwrap();
    if let Some(p) = peers.get_mut(remote) {
        if p.conn.as_ref().is_some_and(|c| c.conn_id == conn_id) {
            p.conn = None;
            p.index.clear();
            p.index_ready = false;
            p.status = PeerStatus::default();
        }
    }
}

/// `observed` es la dirección de origen de una conexión entrante (para
/// aprender por dónde se alcanza a ese equipo).
pub(crate) async fn run_session(inner: Arc<Inner>, hs: Handshaken, remote: String, name: String, dialer: String, observed: Option<SocketAddr>, via_tunnel: bool) {
    let Handshaken { mut reader, writer, .. } = hs;
    let name: String = name.chars().filter(|c| !c.is_control()).take(60).collect();
    let conn_id = inner.next_conn.fetch_add(1, Ordering::Relaxed);
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let (data_tx, data_rx) = mpsc::channel(8);
    let (close_tx, close_rx) = watch::channel(false);
    let handle = ConnHandle {
        conn_id,
        dialer,
        ctrl: ctrl_tx.clone(),
        data: data_tx.clone(),
        routes: Arc::new(Mutex::new(HashMap::new())),
        next_id: Arc::new(AtomicU32::new(1)),
        close: Arc::new(close_tx),
        via_tunnel,
    };
    if !register(&inner, &remote, &name, handle.clone()) {
        return;
    }
    inner.event("net", tr!("Conectado con {}", name));
    let gid = inner.group().map(|g| g.id).unwrap_or_default();
    if !inner.nearby.lock().unwrap().contains_key(&crate::engine::nkey(&remote, &gid)) {
        tracing::info!(
            "«{name}» conectado sin que el descubrimiento automático lo haya visto ({})",
            match observed {
                Some(a) => format!("conexión entrante desde {a}"),
                None => "dirección aprendida, comunicada por el grupo o manual".into(),
            }
        );
    }
    tokio::spawn(writer_task(writer, ctrl_rx, data_rx, close_rx.clone()));
    // Extensiones: nuestras direcciones y las de los demás miembros conectados.
    // Los equipos de versiones anteriores las ignoran.
    let _ = ctrl_tx.send(Ext::Caps { version: EXT_VERSION, listen_port: inner.port, addrs: crate::netpath::own_addresses(inner.port) }.to_msg());
    inner.broadcast_members();

    // Índice completo y estado actual.
    send_full_index(&ctrl_tx, inner.announced_entries());
    let _ = ctrl_tx.send(Msg::Status(crate::engine::own_status(&inner)));

    let mut uploads = crate::transfer::Uploads::new(inner.clone(), handle.clone(), name.clone());
    let mut close_rx = close_rx;
    loop {
        let msg = tokio::select! {
            r = timeout(IDLE_TIMEOUT, reader.recv()) => match r {
                Ok(Ok(m)) => m,
                Ok(Err(e)) => { tracing::debug!("{name}: {e}"); break; }
                Err(_) => { tracing::debug!("{name}: sin respuesta"); break; }
            },
            _ = close_rx.changed() => break,
        };
        if !handle_msg(&inner, &remote, observed, &handle, &mut uploads, msg) {
            break;
        }
    }
    let _ = handle.close.send(true);
    uploads.close_all();
    handle.routes.lock().unwrap().clear();
    unregister(&inner, &remote, conn_id);
    if !inner.shutting_down.load(Ordering::SeqCst) {
        inner.event("net", tr!("Desconectado de {}", name));
    }
    inner.wake.notify_one();
}

pub(crate) fn send_full_index(ctrl: &mpsc::UnboundedSender<Msg>, entries: Vec<FileEntry>) {
    if entries.is_empty() {
        let _ = ctrl.send(Msg::IndexPage { reset: true, last: true, entries: vec![] });
        return;
    }
    let pages: Vec<&[FileEntry]> = entries.chunks(1000).collect();
    let n = pages.len();
    for (i, p) in pages.into_iter().enumerate() {
        let _ = ctrl.send(Msg::IndexPage { reset: i == 0, last: i + 1 == n, entries: p.to_vec() });
    }
}

async fn writer_task(
    mut w: SecureWriter,
    mut ctrl: mpsc::UnboundedReceiver<Msg>,
    mut data: mpsc::Receiver<Msg>,
    mut close: watch::Receiver<bool>,
) {
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        // Prioridad a los mensajes de control frente a los bloques de datos.
        let msg = tokio::select! {
            biased;
            _ = close.changed() => break,
            m = ctrl.recv() => match m { Some(m) => m, None => break },
            m = data.recv() => match m { Some(m) => m, None => break },
            _ = ping.tick() => Msg::Ping,
        };
        match timeout(Duration::from_secs(30), w.send(&msg)).await {
            Ok(Ok(())) => {}
            _ => break,
        }
    }
    w.shutdown().await;
}

fn valid_entries(entries: Vec<FileEntry>, from: &str) -> impl Iterator<Item = FileEntry> + '_ {
    entries.into_iter().filter(move |e| {
        let ok = validate_wire_path(&e.path).is_ok() && e.hash.len() == 64 && e.hash.bytes().all(|b| b.is_ascii_hexdigit());
        if !ok {
            tracing::warn!("entrada rechazada de {from}: {:?}", e.path);
        }
        ok
    })
}

fn parse_addrs(list: &[String]) -> Vec<SocketAddr> {
    list.iter()
        .take(EXT_MAX_ADDRS)
        .filter_map(|a| a.parse::<SocketAddr>().ok())
        .filter(|a| a.port() != 0 && crate::netpath::usable(a.ip()))
        .collect()
}

/// Extensiones recibidas de un miembro autenticado del grupo. Solo aportan
/// direcciones donde intentar conectar: la autenticación sigue siendo
/// obligatoria, así que una dirección errónea no da acceso a nada.
fn handle_ext(inner: &Arc<Inner>, remote: &str, observed: Option<SocketAddr>, ext: Ext) {
    match ext {
        Ext::Caps { version, listen_port, addrs } => {
            let addrs = parse_addrs(&addrs);
            tracing::debug!("{} usa extensiones v{version}, puerto {listen_port}, direcciones {:?}", &remote[..8], addrs);
            if let (Some(o), true) = (observed, listen_port != 0) {
                inner.learn_addr(remote, Candidate { addr: SocketAddr::new(o.ip(), listen_port), source: AddrSource::Reported });
            }
            for a in addrs {
                inner.learn_addr(remote, Candidate { addr: a, source: AddrSource::Reported });
            }
        }
        Ext::Members { members } => {
            for m in members.into_iter().take(EXT_MAX_MEMBERS) {
                if m.id == inner.my_id || m.id == remote || m.id.len() != 32 || !m.id.bytes().all(|b| b.is_ascii_hexdigit()) {
                    continue;
                }
                let addrs = parse_addrs(&m.addrs);
                if addrs.is_empty() {
                    continue;
                }
                let name: String = m.name.chars().filter(|c| !c.is_control()).take(60).collect();
                if inner.add_member(&m.id, &name) {
                    tracing::info!("El grupo comunica el equipo «{name}» ({}) en {:?}", &m.id[..8], addrs);
                }
                for a in addrs {
                    inner.learn_addr(&m.id, Candidate { addr: a, source: AddrSource::Reported });
                }
            }
        }
    }
}

/// Procesa un mensaje. Devuelve `false` si hay que cerrar la conexión.
fn handle_msg(inner: &Arc<Inner>, remote: &str, observed: Option<SocketAddr>, handle: &ConnHandle, uploads: &mut crate::transfer::Uploads, msg: Msg) -> bool {
    match msg {
        Msg::Block { id: EXT_CHANNEL, data, .. } => {
            if let Some(ext) = Ext::from_block(&data) {
                handle_ext(inner, remote, observed, ext);
            }
        }
        Msg::IndexPage { reset, last, entries } => {
            let mut peers = inner.peers.lock().unwrap();
            if let Some(p) = peers.get_mut(remote) {
                if reset {
                    p.index.clear();
                }
                for e in valid_entries(entries, remote) {
                    p.index.insert(e.key(), e);
                }
                if last {
                    p.index_ready = true;
                }
            }
            drop(peers);
            if last {
                inner.wake.notify_one();
            }
        }
        Msg::IndexUpdate { entries } => {
            let mut peers = inner.peers.lock().unwrap();
            if let Some(p) = peers.get_mut(remote) {
                for e in valid_entries(entries, remote) {
                    p.index.insert(e.key(), e);
                }
            }
            drop(peers);
            inner.wake.notify_one();
        }
        Msg::Status(s) => {
            if let Some(p) = inner.peers.lock().unwrap().get_mut(remote) {
                p.status = s;
            }
        }
        Msg::Ping => {
            let _ = handle.ctrl.send(Msg::Pong);
        }
        Msg::Pong => {}
        Msg::DlOpen { id, path, hash } => uploads.open(id, path, hash),
        Msg::BlockReq { id, offset, len } => uploads.request(id, offset, len),
        Msg::DlClose { id } => uploads.close(id),
        Msg::DlOpenOk { id, .. } | Msg::DlBusy { id } | Msg::DlErr { id, .. } | Msg::Block { id, .. } => {
            let route = handle.routes.lock().unwrap().get(&id).cloned();
            if let Some(tx) = route {
                let _ = tx.send(msg);
            }
        }
        // Mensajes de autenticación fuera de lugar: protocolo roto.
        _ => return false,
    }
    true
}
