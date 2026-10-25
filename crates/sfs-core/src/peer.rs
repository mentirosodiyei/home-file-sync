//! Conexiones entre equipos: aceptar, marcar, autenticar, emparejar y la
//! sesión de sincronización (índice, estado, keep-alive y enrutado de bloques).

use crate::auth::{ct_eq, device_id, group_proof, pair_confirm, pair_start_joiner, pair_start_member};
use crate::engine::{ConnHandle, Inner, Peer};
use crate::index::FileEntry;
use crate::pathsafe::validate_wire_path;
use crate::protocol::{Msg, PeerStatus, RejectReason, PROTOCOL_VERSION};
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
        std::io::ErrorKind::ConnectionRefused => "conexión rechazada".into(),
        std::io::ErrorKind::TimedOut => "tiempo de espera agotado".into(),
        _ => e.to_string(),
    }
}

async fn recv_timeout(r: &mut SecureReader, d: Duration) -> Result<Msg, String> {
    match timeout(d, r.recv()).await {
        Ok(Ok(m)) => Ok(m),
        Ok(Err(e)) => Err(io_text(e)),
        Err(_) => Err("tiempo de espera agotado".into()),
    }
}

async fn connect(inner: &Inner, addr: SocketAddr) -> Result<Handshaken, String> {
    let stream = timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| "tiempo de espera agotado al conectar".to_string())?
        .map_err(io_text)?;
    let hs = timeout(HANDSHAKE_TIMEOUT, handshake(stream, &inner.private_key, true))
        .await
        .map_err(|_| "tiempo de espera agotado en el cifrado".to_string())?
        .map_err(io_text)?;
    if device_id(&hs.remote_static) == inner.my_id {
        return Err("conexión con este mismo equipo".into());
    }
    Ok(hs)
}

// ---------------------------------------------------------------------------
// Lado que inicia
// ---------------------------------------------------------------------------

/// Conecta con un equipo del grupo y ejecuta la sesión hasta que se cierre.
/// Devuelve en cuanto la sesión queda establecida (o falla).
pub(crate) async fn dial(inner: Arc<Inner>, addr: SocketAddr) -> Result<(), String> {
    let group = inner.group().ok_or("sin grupo")?;
    let mut hs = connect(&inner, addr).await?;
    let remote = device_id(&hs.remote_static);
    if inner.peers.lock().unwrap().get(&remote).is_some_and(|p| p.conn.is_some()) {
        return Ok(()); // ya conectados por otra vía
    }
    let key = group.key_bytes();
    hs.writer
        .send(&Msg::Hello {
            proto: PROTOCOL_VERSION,
            name: inner.name(),
            group_id: group.id.clone(),
            proof: group_proof(&key, "client", &hs.handshake_hash),
        })
        .await
        .map_err(io_text)?;
    match recv_timeout(&mut hs.reader, HANDSHAKE_TIMEOUT).await? {
        Msg::Welcome { name, proof } => {
            if !ct_eq(&proof, &group_proof(&key, "server", &hs.handshake_hash)) {
                return Err("el otro equipo no ha demostrado pertenecer al grupo".into());
            }
            if let Some(m) = inner.manual.lock().unwrap().get_mut(&addr) {
                m.0 = Some(remote.clone());
            }
            let my_id = inner.my_id.clone();
            tokio::spawn(run_session(inner, hs, remote, name, my_id));
            Ok(())
        }
        Msg::Reject { reason } => Err(reason.text().into()),
        _ => Err("respuesta inesperada".into()),
    }
}

/// Emparejamiento desde el equipo nuevo. Devuelve el grupo recibido.
pub(crate) async fn pair_as_joiner(inner: &Arc<Inner>, addr: SocketAddr, code: &str) -> Result<Group, String> {
    let mut hs = connect(inner, addr).await.map_err(|e| format!("No se puede conectar: {e}"))?;
    let (state, msg) = pair_start_joiner(code);
    hs.writer.send(&Msg::PairStart { proto: PROTOCOL_VERSION, name: inner.name(), spake: msg }).await.map_err(io_text)?;
    match recv_timeout(&mut hs.reader, Duration::from_secs(20)).await? {
        Msg::PairReply { spake, confirm } => {
            let key = state.finish(&spake).ok_or("Código incorrecto.")?;
            if !ct_eq(&confirm, &pair_confirm(&key, "member", &hs.handshake_hash)) {
                return Err("Código incorrecto.".into());
            }
            hs.writer.send(&Msg::PairConfirm { confirm: pair_confirm(&key, "joiner", &hs.handshake_hash) }).await.map_err(io_text)?;
            match recv_timeout(&mut hs.reader, Duration::from_secs(20)).await? {
                Msg::PairGroup { group_id, group_name, group_key } if group_key.len() == 32 && !group_id.is_empty() => {
                    Ok(Group { id: group_id, name: group_name, key: hex::encode(group_key) })
                }
                Msg::Reject { reason } => Err(capitalize(reason.text())),
                _ => Err("Respuesta inesperada durante el emparejamiento.".into()),
            }
        }
        Msg::Reject { reason } => Err(capitalize(reason.text())),
        _ => Err("Respuesta inesperada durante el emparejamiento.".into()),
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
        let i = inner.clone();
        tokio::spawn(async move {
            if let Err(e) = accept_one(i, stream).await {
                tracing::debug!("conexión entrante de {addr} rechazada: {e}");
            }
        });
    }
}

async fn accept_one(inner: Arc<Inner>, stream: TcpStream) -> Result<(), String> {
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
            run_session(inner, hs, remote.clone(), name, remote).await;
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
            inner.event("info", format!("{name} se ha unido al grupo"));
            // Esperamos a que el otro lado cierre para no cortar el último mensaje.
            let _ = timeout(Duration::from_secs(5), hs.reader.recv()).await;
            Ok(())
        }
        _ => {
            fail(&inner);
            inner.refresh_advert();
            let _ = hs.writer.send(&reject(RejectReason::PairingFailed)).await;
            inner.event("warn", format!("Intento de emparejamiento fallido desde «{name}»"));
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

pub(crate) async fn run_session(inner: Arc<Inner>, hs: Handshaken, remote: String, name: String, dialer: String) {
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
    };
    if !register(&inner, &remote, &name, handle.clone()) {
        return;
    }
    inner.event("net", format!("Conectado con {name}"));
    tokio::spawn(writer_task(writer, ctrl_rx, data_rx, close_rx.clone()));

    // Índice completo y estado actual.
    let entries: Vec<FileEntry> = inner
        .folder()
        .filter(|f| f.scanned_once.load(Ordering::SeqCst))
        .map(|f| f.index.lock().unwrap().entries.values().map(|e| e.entry.clone()).collect())
        .unwrap_or_default();
    send_full_index(&ctrl_tx, entries);
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
        if !handle_msg(&inner, &remote, &handle, &mut uploads, msg) {
            break;
        }
    }
    let _ = handle.close.send(true);
    uploads.close_all();
    handle.routes.lock().unwrap().clear();
    unregister(&inner, &remote, conn_id);
    if !inner.shutting_down.load(Ordering::SeqCst) {
        inner.event("net", format!("Desconectado de {name}"));
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

/// Procesa un mensaje. Devuelve `false` si hay que cerrar la conexión.
fn handle_msg(inner: &Arc<Inner>, remote: &str, handle: &ConnHandle, uploads: &mut crate::transfer::Uploads, msg: Msg) -> bool {
    match msg {
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
