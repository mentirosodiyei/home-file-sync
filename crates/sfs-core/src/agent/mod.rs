//! Órdenes de las IA entre equipos.
//!
//! Una IA (Claude, Codex, Cursor, Gemini…) que trabaja en una carpeta
//! compartida de un equipo puede pedir que otro equipo de esa carpeta ejecute
//! una orden —normalmente las pruebas o la compilación— y recibir la salida.
//! Así se comprueba en Windows lo que se ha escrito en un Mac, y al revés.
//!
//! * **Quién decide es el equipo que ejecuta.** Por defecto no acepta nada.
//!   En cada carpeta se activa a mano, con una lista de órdenes permitidas
//!   (`cargo test`, `npm test`…) que se ejecutan sin preguntar; cualquier otra
//!   se puede aprobar o rechazar en su pantalla. Estos ajustes son locales:
//!   nunca llegan de otro equipo.
//! * **Solo miembros del grupo.** Las peticiones viajan por la conexión
//!   cifrada y autenticada de la carpeta compartida (ver `peer`), como
//!   extensiones que las versiones anteriores ignoran.
//! * **Con los archivos al día.** El que pide termina antes de escanear sus
//!   cambios; el que ejecuta espera a tenerlos todos. Al acabar, lo que la
//!   orden haya generado vuelve por la sincronización normal.
//! * **Sin intérprete de órdenes** (ver [`exec`]).
//!
//! Las IA llegan aquí a través de un servidor MCP (ver [`mcp`] y [`control`]).

pub mod api;
pub mod control;
pub mod exec;
pub mod mcp;

use crate::engine::{Engine, Inner};
use crate::protocol::{AgentCaps, ExecEvent, ExecReq, Ext, Msg};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, watch, Notify, Semaphore};

pub const AGENT_VERSION: u32 = 1;
/// Órdenes a la vez por carpeta en el equipo que ejecuta.
const MAX_PARALLEL: usize = 2;
/// Tiempo para aprobar una orden en pantalla.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
/// Tiempo máximo esperando a recibir los cambios del que pide.
const SYNC_TIMEOUT: Duration = Duration::from_secs(180);
/// Salida máxima que se envía por ejecución (el resto se descarta).
const MAX_STREAMED: usize = 8 * 1024 * 1024;
/// Salida que se guarda en el que pide (lo último).
const KEEP_OUTPUT: usize = 1024 * 1024;
const FLUSH_EVERY: Duration = Duration::from_millis(200);
const FLUSH_BYTES: usize = 32 * 1024;
const HISTORY: usize = 30;

/// Qué pueden ejecutar en esta carpeta las IA de los otros equipos.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPolicy {
    /// Se aceptan peticiones.
    #[serde(default)]
    pub enabled: bool,
    /// Órdenes que se ejecutan sin preguntar (prefijos: `cargo test`).
    #[serde(default)]
    pub allowed: Vec<String>,
    /// Las demás se pueden aprobar en pantalla (si no, se rechazan).
    #[serde(default = "yes")]
    pub ask: bool,
}

fn yes() -> bool {
    true
}

impl Default for AgentPolicy {
    fn default() -> Self {
        AgentPolicy { enabled: false, allowed: Vec::new(), ask: true }
    }
}

impl AgentPolicy {
    /// Limpia la lista: sin vacías ni repetidas, cada una separable.
    pub fn normalized(mut self) -> Self {
        let mut seen = std::collections::HashSet::new();
        self.allowed = self
            .allowed
            .into_iter()
            .filter_map(|a| exec::split_command(a.trim()).ok().filter(|t| !t.is_empty()).map(|t| exec::join_command(&t)))
            .filter(|a| seen.insert(a.clone()))
            .collect();
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RunState {
    /// Terminando de escanear los cambios locales antes de enviar la petición.
    Preparing,
    Queued,
    /// Esperando a que alguien la apruebe en el otro equipo.
    Approval,
    /// El otro equipo está recibiendo los cambios.
    Syncing,
    Running,
    /// Ha terminado; llegan los archivos que ha generado.
    SyncingBack,
    Finished,
    Refused,
    Cancelled,
    /// Se ha perdido la conexión con el equipo.
    Lost,
}

impl RunState {
    pub fn is_final(self) -> bool {
        matches!(self, RunState::Finished | RunState::Refused | RunState::Cancelled | RunState::Lost)
    }
}

/// Una ejecución pedida desde este equipo.
pub(crate) struct OutRun {
    pub peer: String,
    pub peer_name: String,
    pub argv: Vec<String>,
    pub subdir: String,
    pub client: String,
    pub sync_back: bool,
    pub state: RunState,
    pub output: Vec<u8>,
    pub output_total: u64,
    pub truncated: bool,
    pub code: Option<i32>,
    pub reason: Option<String>,
    pub duration_ms: Option<u64>,
    pub sync: Option<(u32, u64)>,
    #[allow(dead_code)]
    pub created: Instant,
    pub finished: Option<Instant>,
}

/// Una ejecución pedida por otro equipo.
pub(crate) struct InRun {
    pub peer_name: String,
    pub client: String,
    pub argv: Vec<String>,
    pub subdir: String,
    pub state: RunState,
    pub approve: Option<oneshot::Sender<Approval>>,
    pub cancel: watch::Sender<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Approval {
    Once,
    /// Aprobar y añadirla a las permitidas.
    Always,
    Deny,
}

#[derive(Default)]
pub(crate) struct AgentData {
    pub caps: HashMap<String, AgentCaps>,
    pub outgoing: HashMap<u64, OutRun>,
    pub incoming: HashMap<(String, u64), InRun>,
    pub history: VecDeque<RunRecord>,
}

pub(crate) struct AgentState {
    pub data: Mutex<AgentData>,
    pub changed: Notify,
    pub slots: Arc<Semaphore>,
}

impl AgentState {
    pub fn new() -> Self {
        AgentState { data: Mutex::new(AgentData::default()), changed: Notify::new(), slots: Arc::new(Semaphore::new(MAX_PARALLEL)) }
    }
}

// ---------------------------------------------------------------------------
// Vistas
// ---------------------------------------------------------------------------

/// Estado de una ejecución pedida desde aquí (para la IA).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub run: u64,
    pub machine: String,
    pub command: String,
    pub subdir: String,
    pub state: RunState,
    pub exit_code: Option<i32>,
    pub reason: Option<String>,
    pub duration_ms: Option<u64>,
    /// Salida (lo último, sin códigos de color).
    pub output: String,
    pub output_bytes: u64,
    /// Se ha descartado parte de la salida por ser demasiado larga.
    pub truncated: bool,
    /// Archivos que le faltan por recibir al otro equipo (o a este, al volver).
    pub sync_files: Option<u32>,
}

/// Entrada del historial (interfaz).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    /// La pidió otro equipo para ejecutarla aquí.
    pub incoming: bool,
    pub peer: String,
    pub client: String,
    pub command: String,
    pub state: RunState,
    pub exit_code: Option<i32>,
    pub reason: Option<String>,
    pub time_ms: i64,
    pub duration_ms: Option<u64>,
}

/// Petición esperando aprobación en este equipo.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalView {
    pub peer_id: String,
    pub run: String,
    pub peer: String,
    pub client: String,
    pub command: String,
    pub subdir: String,
}

/// Ejecución de otro equipo en marcha aquí.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveView {
    pub peer_id: String,
    pub run: String,
    pub peer: String,
    pub client: String,
    pub command: String,
    pub state: RunState,
}

/// Lo que sabe este equipo de otro para las IA.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerAgentView {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub os: Option<String>,
    /// Acepta órdenes en esta carpeta.
    pub exec: bool,
    pub allowed: Vec<String>,
    pub ask: bool,
    /// Tiene una versión con esta función.
    pub supported: bool,
}

/// Sección «IA» de una carpeta en la interfaz.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentView {
    pub policy: AgentPolicy,
    pub code_project: bool,
    pub suggestions: Vec<String>,
    pub approvals: Vec<ApprovalView>,
    pub active: Vec<ActiveView>,
    pub history: Vec<RunRecord>,
}

fn now_ms() -> i64 {
    crate::scanner::now_ns() / 1_000_000
}

/// Quita los códigos de color y control de terminal.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            match it.peek() {
                Some('[') => {
                    it.next();
                    for n in it.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    it.next();
                    while let Some(n) = it.next() {
                        if n == '\u{7}' || (n == '\u{1b}' && it.peek() == Some(&'\\')) {
                            if n == '\u{1b}' {
                                it.next();
                            }
                            break;
                        }
                    }
                }
                _ => {
                    it.next();
                }
            }
        } else if c == '\r' {
            if it.peek() != Some(&'\n') {
                out.push('\n');
            }
        } else if !c.is_control() || c == '\n' || c == '\t' {
            out.push(c);
        }
    }
    out
}

/// Últimos `max` caracteres del texto, empezando en una línea completa.
pub fn tail_chars(s: &str, max: usize) -> (String, bool) {
    let n = s.chars().count();
    if n <= max {
        return (s.to_string(), false);
    }
    let start = s.char_indices().nth(n - max).map(|(i, _)| i).unwrap_or(0);
    let rest = &s[start..];
    let rest = rest.find('\n').map(|i| &rest[i + 1..]).filter(|r| !r.is_empty()).unwrap_or(rest);
    (rest.to_string(), true)
}

// ---------------------------------------------------------------------------
// Conexiones
// ---------------------------------------------------------------------------

pub(crate) fn caps(inner: &Inner) -> AgentCaps {
    let p = inner.settings.lock().unwrap().agent.clone();
    AgentCaps {
        version: AGENT_VERSION,
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        exec: p.enabled,
        allowed: if p.enabled { p.allowed } else { Vec::new() },
        ask: p.enabled && p.ask,
    }
}

/// Envía un mensaje a un equipo por su conexión actual.
fn send_to(inner: &Inner, peer: &str, msg: Msg) -> bool {
    let conn = inner.peers.lock().unwrap().get(peer).and_then(|p| p.conn.clone());
    conn.is_some_and(|c| c.ctrl.send(msg).is_ok())
}

pub(crate) fn on_connect(inner: &Inner, ctrl: &tokio::sync::mpsc::UnboundedSender<Msg>) {
    let _ = ctrl.send(Ext::Agent(caps(inner)).to_msg());
}

pub(crate) fn broadcast_caps(inner: &Inner) {
    inner.broadcast(Ext::Agent(caps(inner)).to_msg());
}

/// El equipo se ha desconectado del todo.
pub(crate) fn on_disconnect(inner: &Inner, peer: &str) {
    let mut d = inner.agent.data.lock().unwrap();
    d.caps.remove(peer);
    let mut lost = Vec::new();
    for r in d.outgoing.values_mut().filter(|r| r.peer == peer && !r.state.is_final()) {
        // Lo que ya terminó y estaba esperando los archivos se da por acabado.
        if r.state == RunState::SyncingBack {
            r.state = RunState::Finished;
        } else {
            r.state = RunState::Lost;
            r.reason = Some(tr!("se ha perdido la conexión con {}", r.peer_name));
        }
        r.finished = Some(Instant::now());
        lost.push(record_out(r));
    }
    for (k, r) in d.incoming.iter_mut() {
        if k.0 == peer {
            let _ = r.cancel.send(true);
        }
    }
    for rec in lost {
        push_history(&mut d, rec);
    }
    drop(d);
    inner.agent.changed.notify_waiters();
}

pub(crate) fn shutdown(inner: &Inner) {
    for r in inner.agent.data.lock().unwrap().incoming.values() {
        let _ = r.cancel.send(true);
    }
}

fn push_history(d: &mut AgentData, rec: RunRecord) {
    d.history.push_front(rec);
    d.history.truncate(HISTORY);
}

fn record_out(r: &OutRun) -> RunRecord {
    RunRecord {
        incoming: false,
        peer: r.peer_name.clone(),
        client: r.client.clone(),
        command: exec::join_command(&r.argv),
        state: r.state,
        exit_code: r.code,
        reason: r.reason.clone(),
        time_ms: now_ms(),
        duration_ms: r.duration_ms,
    }
}

/// Extensiones de órdenes recibidas de un miembro autenticado del grupo.
pub(crate) fn handle_ext(inner: &Arc<Inner>, remote: &str, ext: Ext) {
    match ext {
        Ext::Agent(mut c) => {
            c.os = c.os.chars().filter(|c| c.is_ascii_alphanumeric()).take(16).collect();
            c.arch = c.arch.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(16).collect();
            c.allowed.truncate(64);
            c.allowed.iter_mut().for_each(|a| *a = a.chars().filter(|c| !c.is_control()).take(200).collect());
            inner.agent.data.lock().unwrap().caps.insert(remote.to_string(), c);
        }
        Ext::ExecReq(req) => {
            if inner.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            let (i, r) = (inner.clone(), remote.to_string());
            inner.spawn(async move { serve(i, r, req).await });
        }
        Ext::ExecCancel { run } => {
            if let Some(r) = inner.agent.data.lock().unwrap().incoming.get_mut(&(remote.to_string(), run)) {
                let _ = r.cancel.send(true);
                if let Some(a) = r.approve.take() {
                    let _ = a.send(Approval::Deny);
                }
            }
        }
        Ext::ExecEvent { run, event } => on_event(inner, remote, run, event),
        Ext::Caps { .. } | Ext::Members { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Equipo que ejecuta
// ---------------------------------------------------------------------------

/// Atiende una petición de otro equipo de principio a fin.
async fn serve(inner: Arc<Inner>, remote: String, req: ExecReq) {
    let run = req.run;
    let ev = |event: ExecEvent| {
        send_to(&inner, &remote, Ext::ExecEvent { run, event }.to_msg());
    };
    let peer_name = inner.peers.lock().unwrap().get(&remote).map(|p| p.name.clone()).unwrap_or_else(|| remote[..8.min(remote.len())].to_string());
    let client: String = req.client.chars().filter(|c| !c.is_control()).take(60).collect();
    let command = exec::join_command(&req.argv);
    let refuse = |reason: String| {
        tracing::info!("Orden de «{peer_name}» rechazada ({command}): {reason}");
        ev(ExecEvent::Refused { reason });
    };
    let policy = inner.settings.lock().unwrap().agent.clone();
    let Some(folder) = inner.folder() else { return refuse(tr!("Este equipo no tiene la carpeta disponible.")) };
    if !policy.enabled {
        return refuse(tr!("Este equipo no acepta órdenes de las IA en esta carpeta. Se activa en Home File Sync, en la carpeta, botón «IA»."));
    }
    if let Err(e) = exec::check_request(&req.argv, &req.subdir) {
        return refuse(e);
    }
    let (cancel_tx, mut cancel) = watch::channel(false);
    {
        let mut d = inner.agent.data.lock().unwrap();
        if d.incoming.contains_key(&(remote.clone(), run)) {
            return; // repetida
        }
        d.incoming.insert(
            (remote.clone(), run),
            InRun { peer_name: peer_name.clone(), client: client.clone(), argv: req.argv.clone(), subdir: req.subdir.clone(), state: RunState::Queued, approve: None, cancel: cancel_tx },
        );
    }
    let set_state = |s: RunState| {
        if let Some(r) = inner.agent.data.lock().unwrap().incoming.get_mut(&(remote.clone(), run)) {
            r.state = s;
        }
    };
    let started = Instant::now();
    let result = serve_inner(&inner, &remote, &req, &peer_name, &client, &command, &policy, &folder.root, &mut cancel, &ev, &set_state).await;
    let (state, code, reason) = match result {
        Ok((code, reason, truncated)) => {
            ev(ExecEvent::Done { code, reason: reason.clone(), duration_ms: started.elapsed().as_millis() as u64, truncated });
            (RunState::Finished, code, reason)
        }
        Err((state, reason)) => {
            if state == RunState::Refused {
                refuse(reason.clone());
            } else {
                ev(ExecEvent::Done { code: None, reason: Some(reason.clone()), duration_ms: started.elapsed().as_millis() as u64, truncated: false });
            }
            (state, None, Some(reason))
        }
    };
    let mut d = inner.agent.data.lock().unwrap();
    d.incoming.remove(&(remote.clone(), run));
    push_history(
        &mut d,
        RunRecord { incoming: true, peer: peer_name, client, command, state, exit_code: code, reason, time_ms: now_ms(), duration_ms: Some(started.elapsed().as_millis() as u64) },
    );
}

type Outcome = Result<(Option<i32>, Option<String>, bool), (RunState, String)>;

#[allow(clippy::too_many_arguments)]
async fn serve_inner(
    inner: &Arc<Inner>,
    remote: &str,
    req: &ExecReq,
    peer_name: &str,
    client: &str,
    command: &str,
    policy: &AgentPolicy,
    root: &std::path::Path,
    cancel: &mut watch::Receiver<bool>,
    ev: &impl Fn(ExecEvent),
    set_state: &impl Fn(RunState),
) -> Outcome {
    let cancelled = || (RunState::Cancelled, tr!("cancelado por quien lo pidió"));
    // 1. Permiso.
    if !exec::is_allowed(&policy.allowed, &req.argv) {
        if !policy.ask {
            return Err((RunState::Refused, tr!("«{}» no está entre las órdenes permitidas en este equipo.", command)));
        }
        let (tx, rx) = oneshot::channel();
        if let Some(r) = inner.agent.data.lock().unwrap().incoming.get_mut(&(remote.to_string(), req.run)) {
            r.approve = Some(tx);
            r.state = RunState::Approval;
        }
        ev(ExecEvent::Approval);
        let who = if client.is_empty() { peer_name.to_string() } else { format!("{client} ({peer_name})") };
        inner.event("agent", tr!("{} pide ejecutar «{}». Apruébalo o recházalo en la carpeta, botón «IA».", who, command));
        let answer = tokio::select! {
            a = tokio::time::timeout(APPROVAL_TIMEOUT, rx) => match a { Ok(Ok(a)) => a, Ok(Err(_)) => Approval::Deny, Err(_) => {
                return Err((RunState::Refused, tr!("nadie lo ha aprobado a tiempo en el otro equipo")));
            } },
            _ = wait_cancel(cancel) => return Err(cancelled()),
        };
        match answer {
            Approval::Deny => return Err((RunState::Refused, tr!("rechazado en el otro equipo"))),
            Approval::Always => {
                {
                    let mut s = inner.settings.lock().unwrap();
                    s.agent.allowed.push(command.to_string());
                    s.agent = s.agent.clone().normalized();
                }
                inner.save_settings();
                broadcast_caps(inner);
            }
            Approval::Once => {}
        }
    }
    // 2. Turno.
    set_state(RunState::Queued);
    if inner.agent.slots.available_permits() == 0 {
        ev(ExecEvent::Queued);
    }
    let _permit = tokio::select! {
        p = inner.agent.slots.clone().acquire_owned() => p.map_err(|_| (RunState::Cancelled, tr!("detenido")))?,
        _ = wait_cancel(cancel) => return Err(cancelled()),
    };
    // 3. Archivos al día.
    if req.wait_sync {
        set_state(RunState::Syncing);
        let deadline = Instant::now() + SYNC_TIMEOUT;
        let mut last = None;
        loop {
            let b = crate::transfer::behind(inner, remote);
            if let Some((0, _)) = b {
                break;
            }
            if b != last {
                if let Some((files, bytes)) = b {
                    ev(ExecEvent::Syncing { files, bytes });
                }
                last = b;
            }
            if Instant::now() > deadline {
                let n = b.map(|x| x.0).unwrap_or(0);
                return Err((RunState::Refused, tr!("no se han recibido todos los cambios a tiempo ({} archivo(s) pendientes; mira si hay errores de sincronización)", n)));
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(300)) => {}
                _ = wait_cancel(cancel) => return Err(cancelled()),
            }
        }
    }
    // 4. Carpeta y programa.
    let cwd = if req.subdir.is_empty() { root.to_path_buf() } else { crate::pathsafe::to_local(root, &req.subdir) };
    let cwd = crate::settings::canonical(&cwd).ok().filter(|c| c.starts_with(root) && c.is_dir()).ok_or_else(|| (RunState::Refused, tr!("La subcarpeta «{}» no existe en el otro equipo.", req.subdir)))?;
    let path_var = tokio::task::spawn_blocking(exec::user_path).await.unwrap_or_default();
    let program = exec::resolve_program(&req.argv[0], root, &cwd, &path_var).map_err(|e| (RunState::Refused, e))?;
    let env = vec![
        ("PATH".to_string(), path_var),
        ("HOMEFILESYNC_REQUESTED_BY".to_string(), peer_name.to_string()),
        ("NO_COLOR".to_string(), "1".to_string()),
        ("CARGO_TERM_COLOR".to_string(), "never".to_string()),
        ("TERM".to_string(), "dumb".to_string()),
    ];
    let timeout = Duration::from_secs(req.timeout_s.clamp(1, exec::MAX_TIMEOUT_S) as u64);
    // 5. Ejecución.
    set_state(RunState::Running);
    let mut rx = exec::spawn(&program, &req.argv[1..], &cwd, &env, timeout, cancel.clone()).map_err(|e| (RunState::Refused, e))?;
    ev(ExecEvent::Started);
    inner.event("agent", tr!("Ejecutando «{}», pedido por {}", command, peer_name));
    let mut pending: Vec<u8> = Vec::new();
    let mut sent = 0usize;
    let mut truncated = false;
    let mut tick = tokio::time::interval(FLUSH_EVERY);
    let flush = |pending: &mut Vec<u8>, sent: &mut usize, truncated: &mut bool| {
        if pending.is_empty() {
            return;
        }
        let room = MAX_STREAMED.saturating_sub(*sent);
        if room < pending.len() {
            *truncated = true;
            pending.truncate(room);
        }
        if !pending.is_empty() {
            *sent += pending.len();
            ev(ExecEvent::Output { data: std::mem::take(pending) });
        }
        pending.clear();
    };
    let (code, reason) = loop {
        tokio::select! {
            m = rx.recv() => match m {
                Some(exec::Progress::Output(b)) => {
                    pending.extend_from_slice(&b);
                    if pending.len() >= FLUSH_BYTES {
                        flush(&mut pending, &mut sent, &mut truncated);
                    }
                }
                Some(exec::Progress::Exited { code, reason }) => break (code, reason),
                None => break (None, Some(tr!("detenido"))),
            },
            _ = tick.tick() => flush(&mut pending, &mut sent, &mut truncated),
        }
    };
    // Lo que llegue aún de las tuberías (procesos hijos que terminan después).
    while let Ok(exec::Progress::Output(b)) = rx.try_recv() {
        pending.extend_from_slice(&b);
    }
    flush(&mut pending, &mut sent, &mut truncated);
    inner.event(
        "agent",
        match code {
            Some(c) => tr!("«{}» ha terminado con código {}", command, c),
            None => tr!("«{}» se ha detenido: {}", command, reason.clone().unwrap_or_default()),
        },
    );
    // 6. Que lo generado empiece a viajar antes de avisar del final.
    flush_scan(inner, Duration::from_secs(15)).await;
    Ok((code, reason, truncated))
}

async fn wait_cancel(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Espera a que el escáner haya visto los cambios hechos hasta ahora
/// (incluidos los archivos recién guardados, que necesitan estabilizarse).
pub(crate) async fn flush_scan(inner: &Arc<Inner>, max: Duration) -> bool {
    let Some(f) = inner.folder() else { return false };
    let deadline = Instant::now() + max;
    loop {
        let g = f.scan_gen.load(Ordering::SeqCst);
        f.request_scan();
        while f.scan_gen.load(Ordering::SeqCst) == g {
            if Instant::now() > deadline || !f.alive.load(Ordering::SeqCst) {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !f.last_unstable.load(Ordering::SeqCst) && f.scanned_once.load(Ordering::SeqCst) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ---------------------------------------------------------------------------
// Equipo que pide
// ---------------------------------------------------------------------------

fn on_event(inner: &Arc<Inner>, remote: &str, run: u64, event: ExecEvent) {
    let mut start_sync_back = false;
    {
        let mut d = inner.agent.data.lock().unwrap();
        let Some(r) = d.outgoing.get_mut(&run) else { return };
        if r.peer != remote || r.state.is_final() {
            return; // solo el equipo al que se pidió
        }
        match event {
            ExecEvent::Queued => r.state = RunState::Queued,
            ExecEvent::Approval => r.state = RunState::Approval,
            ExecEvent::Syncing { files, bytes } => {
                r.state = RunState::Syncing;
                r.sync = Some((files, bytes));
            }
            ExecEvent::Started => {
                r.state = RunState::Running;
                r.sync = None;
            }
            ExecEvent::Output { data } => {
                r.output_total += data.len() as u64;
                r.output.extend_from_slice(&data);
                if r.output.len() > KEEP_OUTPUT {
                    let cut = r.output.len() - KEEP_OUTPUT;
                    r.output.drain(..cut);
                    r.truncated = true;
                }
            }
            ExecEvent::Done { code, reason, duration_ms, truncated } => {
                r.code = code;
                r.reason = reason;
                r.duration_ms = Some(duration_ms);
                r.truncated |= truncated;
                if r.sync_back {
                    r.state = RunState::SyncingBack;
                    start_sync_back = true;
                } else {
                    r.state = RunState::Finished;
                    r.finished = Some(Instant::now());
                    let rec = record_out(r);
                    push_history(&mut d, rec);
                }
            }
            ExecEvent::Refused { reason } => {
                r.state = RunState::Refused;
                r.reason = Some(reason);
                r.finished = Some(Instant::now());
                let rec = record_out(r);
                push_history(&mut d, rec);
            }
        }
    }
    inner.agent.changed.notify_waiters();
    if start_sync_back {
        let (i, peer) = (inner.clone(), remote.to_string());
        inner.spawn(async move {
            // Que lleguen aquí los archivos que ha generado la orden.
            let deadline = Instant::now() + Duration::from_secs(60);
            tokio::time::sleep(Duration::from_millis(300)).await;
            loop {
                let b = crate::transfer::behind(&i, &peer);
                if let Some(r) = i.agent.data.lock().unwrap().outgoing.get_mut(&run) {
                    r.sync = b;
                }
                if matches!(b, Some((0, _)) | None) || Instant::now() > deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            let mut d = i.agent.data.lock().unwrap();
            if let Some(r) = d.outgoing.get_mut(&run) {
                if r.state == RunState::SyncingBack {
                    r.state = RunState::Finished;
                    r.finished = Some(Instant::now());
                    r.sync = None;
                    let rec = record_out(r);
                    push_history(&mut d, rec);
                }
            }
            drop(d);
            i.agent.changed.notify_waiters();
        });
    }
}

/// Opciones de una ejecución remota.
#[derive(Clone, Debug)]
pub struct RunRequest {
    pub argv: Vec<String>,
    pub subdir: String,
    pub timeout_s: u32,
    pub client: String,
    /// Esperar a que el otro equipo tenga los cambios de este.
    pub wait_sync: bool,
    /// Al terminar, esperar a recibir lo que haya generado.
    pub sync_back: bool,
}

fn view_of(run: u64, r: &OutRun, max_chars: usize) -> RunView {
    let text = strip_ansi(&String::from_utf8_lossy(&r.output));
    let (output, cut) = tail_chars(&text, max_chars);
    RunView {
        run,
        machine: r.peer_name.clone(),
        command: exec::join_command(&r.argv),
        subdir: r.subdir.clone(),
        state: r.state,
        exit_code: r.code,
        reason: r.reason.clone(),
        duration_ms: r.duration_ms,
        output,
        output_bytes: r.output_total,
        truncated: r.truncated || cut,
        sync_files: r.sync.map(|s| s.0),
    }
}

impl Engine {
    pub fn agent_policy(&self) -> AgentPolicy {
        self.inner.settings.lock().unwrap().agent.clone()
    }

    pub fn set_agent_policy(&self, p: AgentPolicy) {
        self.inner.settings.lock().unwrap().agent = p.normalized();
        self.inner.save_settings();
        broadcast_caps(&self.inner);
    }

    pub fn code_project(&self) -> bool {
        self.inner.settings.lock().unwrap().code_project
    }

    /// Activa las reglas de proyecto de código (`.gitignore`, sin repositorio).
    pub fn set_code_project(&self, on: bool) {
        self.inner.settings.lock().unwrap().code_project = on;
        self.inner.save_settings();
        if let Some(f) = self.inner.folder() {
            if !on {
                *f.rules.lock().unwrap() = None;
            }
            f.request_scan();
        }
    }

    /// Los demás equipos de la carpeta y qué aceptan.
    pub fn agent_peers(&self) -> Vec<PeerAgentView> {
        let caps = self.inner.agent.data.lock().unwrap().caps.clone();
        let peers = self.inner.peers.lock().unwrap();
        peers
            .iter()
            .map(|(id, p)| {
                let c = caps.get(id);
                PeerAgentView {
                    id: id.clone(),
                    name: p.name.clone(),
                    online: p.conn.is_some(),
                    os: c.map(|c| c.os.clone()),
                    exec: c.is_some_and(|c| c.exec),
                    allowed: c.map(|c| c.allowed.clone()).unwrap_or_default(),
                    ask: c.is_some_and(|c| c.ask),
                    supported: c.is_some(),
                }
            })
            .collect()
    }

    /// Pide a otro equipo que ejecute una orden. Devuelve el número de ejecución.
    pub fn run_remote(&self, peer: &str, req: RunRequest) -> Result<u64, String> {
        let inner = &self.inner;
        exec::check_request(&req.argv, &req.subdir)?;
        let peer_name = {
            let peers = inner.peers.lock().unwrap();
            let p = peers.get(peer).ok_or_else(|| tr!("Ese equipo no está en esta carpeta compartida."))?;
            if p.conn.is_none() {
                return Err(tr!("«{}» no está conectado ahora.", p.name));
            }
            p.name.clone()
        };
        match inner.agent.data.lock().unwrap().caps.get(peer) {
            None => return Err(tr!("«{}» tiene una versión de Home File Sync sin órdenes para las IA (o acaba de conectarse; vuelve a intentarlo en unos segundos).", peer_name)),
            Some(c) if !c.exec => return Err(tr!("«{}» no acepta órdenes de las IA en esta carpeta. Hay que activarlo en ese equipo: carpeta, botón «IA».", peer_name)),
            Some(c) if !c.ask && !exec::is_allowed(&c.allowed, &req.argv) => {
                return Err(tr!("«{}» solo acepta estas órdenes: {}", peer_name, c.allowed.join(", ")));
            }
            Some(_) => {}
        }
        let run = loop {
            let r = u64::from_le_bytes(crate::auth::random_bytes::<8>()) >> 11; // cabe en un número de JSON
            if r != 0 {
                break r;
            }
        };
        {
            let mut d = inner.agent.data.lock().unwrap();
            // Limpieza: lo terminado hace más de una hora y, como mucho, 100.
            d.outgoing.retain(|_, r| r.finished.is_none_or(|f| f.elapsed() < Duration::from_secs(3600)));
            if d.outgoing.len() >= 100 {
                let mut done: Vec<(u64, Instant)> = d.outgoing.iter().filter_map(|(k, r)| r.finished.map(|f| (*k, f))).collect();
                done.sort_by_key(|x| x.1);
                for (k, _) in done.into_iter().take(20) {
                    d.outgoing.remove(&k);
                }
            }
            d.outgoing.insert(
                run,
                OutRun {
                    peer: peer.to_string(),
                    peer_name,
                    argv: req.argv.clone(),
                    subdir: req.subdir.clone(),
                    client: req.client.clone(),
                    sync_back: req.sync_back,
                    state: RunState::Preparing,
                    output: Vec::new(),
                    output_total: 0,
                    truncated: false,
                    code: None,
                    reason: None,
                    duration_ms: None,
                    sync: None,
                    created: Instant::now(),
                    finished: None,
                },
            );
        }
        let (i, peer) = (inner.clone(), peer.to_string());
        inner.spawn(async move {
            if req.wait_sync {
                // Los cambios recién guardados aquí deben anunciarse antes que la petición
                // (van por la misma conexión, en orden).
                flush_scan(&i, Duration::from_secs(20)).await;
            }
            let msg = Ext::ExecReq(ExecReq {
                run,
                argv: req.argv,
                subdir: req.subdir,
                timeout_s: req.timeout_s,
                client: req.client,
                wait_sync: req.wait_sync,
            })
            .to_msg();
            let mut d = i.agent.data.lock().unwrap();
            if let Some(r) = d.outgoing.get_mut(&run) {
                if r.state == RunState::Preparing {
                    if send_to(&i, &peer, msg) {
                        r.state = RunState::Queued;
                    } else {
                        r.state = RunState::Lost;
                        r.reason = Some(tr!("se ha perdido la conexión con {}", r.peer_name));
                        r.finished = Some(Instant::now());
                    }
                }
            }
            drop(d);
            i.agent.changed.notify_waiters();
        });
        Ok(run)
    }

    pub fn run_view(&self, run: u64, max_chars: usize) -> Option<RunView> {
        self.inner.agent.data.lock().unwrap().outgoing.get(&run).map(|r| view_of(run, r, max_chars))
    }

    /// Espera hasta que la ejecución termine o pase `wait`.
    pub async fn run_wait(&self, run: u64, wait: Duration, max_chars: usize) -> Option<RunView> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.inner.agent.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let v = self.run_view(run, max_chars)?;
            if v.state.is_final() {
                return Some(v);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.run_view(run, max_chars);
            }
        }
    }

    /// Cancela una ejecución pedida desde aquí.
    pub fn run_cancel(&self, run: u64) -> Result<(), String> {
        let mut d = self.inner.agent.data.lock().unwrap();
        let r = d.outgoing.get_mut(&run).ok_or_else(|| tr!("No existe esa ejecución."))?;
        if r.state.is_final() {
            return Ok(());
        }
        send_to(&self.inner, &r.peer, Ext::ExecCancel { run }.to_msg());
        if r.state == RunState::Preparing {
            r.state = RunState::Cancelled;
            r.finished = Some(Instant::now());
        }
        drop(d);
        self.inner.agent.changed.notify_waiters();
        Ok(())
    }

    /// Respuesta a una petición que espera aprobación en este equipo.
    pub fn approve_run(&self, peer: &str, run: u64, answer: Approval) -> Result<(), String> {
        let mut d = self.inner.agent.data.lock().unwrap();
        let r = d.incoming.get_mut(&(peer.to_string(), run)).ok_or_else(|| tr!("Esa petición ya no está pendiente."))?;
        let tx = r.approve.take().ok_or_else(|| tr!("Esa petición ya no está pendiente."))?;
        let _ = tx.send(answer);
        Ok(())
    }

    /// Detiene una orden de otro equipo que se está ejecutando aquí.
    pub fn stop_incoming(&self, peer: &str, run: u64) -> Result<(), String> {
        let d = self.inner.agent.data.lock().unwrap();
        let r = d.incoming.get(&(peer.to_string(), run)).ok_or_else(|| tr!("Esa orden ya no se está ejecutando."))?;
        let _ = r.cancel.send(true);
        Ok(())
    }

    /// Escanea los cambios locales y espera a que los equipos conectados
    /// los tengan y a tener los suyos. Devuelve lo que falta por equipo.
    pub async fn sync_wait(&self, wait: Duration) -> Vec<(String, u32, u32)> {
        let deadline = Instant::now() + wait;
        flush_scan(&self.inner, wait.min(Duration::from_secs(20))).await;
        // Que el estado que anuncian los demás refleje nuestros últimos cambios.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        loop {
            let ids: Vec<(String, String, u32)> = {
                let peers = self.inner.peers.lock().unwrap();
                peers.iter().filter(|(_, p)| p.conn.is_some()).map(|(id, p)| (id.clone(), p.name.clone(), p.status.pending_files)).collect()
            };
            let rows: Vec<(String, u32, u32)> = ids
                .into_iter()
                .map(|(id, name, theirs)| (name, crate::transfer::behind(&self.inner, &id).map(|b| b.0).unwrap_or(0), theirs))
                .collect();
            if rows.iter().all(|r| r.1 == 0 && r.2 == 0) || Instant::now() > deadline {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub fn agent_view(&self) -> AgentView {
        view(&self.inner)
    }
}


pub(crate) fn view(inner: &Inner) -> AgentView {
    let s = inner.settings.lock().unwrap();
    let (policy, code_project) = (s.agent.clone(), s.code_project);
    drop(s);
    let suggestions = inner.folder().map(|f| exec::suggest_commands(&f.root)).unwrap_or_default();
    let d = inner.agent.data.lock().unwrap();
    let mut approvals = Vec::new();
    let mut active = Vec::new();
    for ((peer, run), r) in d.incoming.iter() {
        let command = exec::join_command(&r.argv);
        if r.approve.is_some() {
            approvals.push(ApprovalView { peer_id: peer.clone(), run: run.to_string(), peer: r.peer_name.clone(), client: r.client.clone(), command, subdir: r.subdir.clone() });
        } else {
            active.push(ActiveView { peer_id: peer.clone(), run: run.to_string(), peer: r.peer_name.clone(), client: r.client.clone(), command, state: r.state });
        }
    }
    AgentView { policy, code_project, suggestions, approvals, active, history: d.history.iter().cloned().collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_terminal_codes() {
        assert_eq!(strip_ansi("\u{1b}[1;32mok\u{1b}[0m done\r\n"), "ok done\n");
        assert_eq!(strip_ansi("50%\r100%\n"), "50%\n100%\n");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}x"), "x");
    }

    #[test]
    fn tail_starts_at_a_line() {
        let (t, cut) = tail_chars("line1\nline2\nline3\n", 9);
        assert!(cut);
        assert_eq!(t, "line3\n");
        assert_eq!(tail_chars("abc", 10), ("abc".into(), false));
    }

    #[test]
    fn policy_is_normalized() {
        let p = AgentPolicy { enabled: true, allowed: vec![" cargo   test ".into(), "cargo test".into(), "".into(), "npm \"run x".into()], ask: true }.normalized();
        assert_eq!(p.allowed, vec!["cargo test".to_string()]);
    }
}
