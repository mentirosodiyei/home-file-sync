//! Canal local entre el servidor MCP (que arranca la IA) y la app abierta.
//!
//! La app escucha solo en `127.0.0.1`, en un puerto cualquiera, y escribe en
//! su directorio de datos `agent.json` con ese puerto y un token aleatorio
//! (archivo legible solo por el usuario). Cada llamada es una conexión con
//! una línea JSON de petición y otra de respuesta; sin el token no se atiende.
//! Así, otros usuarios del mismo equipo no pueden usarlo.

use super::api::{self, CallCtx};
use crate::auth::ct_eq;
use crate::hub::Hub;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub const CONTROL_FILE: &str = "agent.json";
/// Identificador de la app (el mismo que usa Tauri para su carpeta de datos).
pub const APP_ID: &str = "org.homefilesync.app";
const MAX_REQUEST: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct ControlInfo {
    port: u16,
    token: String,
    pid: u32,
}

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    tool: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    client: String,
}

#[derive(Serialize, Deserialize)]
struct Response {
    ok: bool,
    text: String,
}

/// Directorio de datos de la app de escritorio en este sistema.
pub fn default_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        home.join("Library/Application Support").join(APP_ID)
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default().join(APP_ID)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".local/share"));
        base.join(APP_ID)
    }
}

/// Arranca el canal en la app. Devuelve el puerto.
pub async fn start(hub: Hub, data_dir: &Path) -> std::io::Result<(u16, tokio::task::AbortHandle)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let token = hex::encode(crate::auth::random_bytes::<32>());
    let info = ControlInfo { port, token: token.clone(), pid: std::process::id() };
    crate::index::write_atomic(&data_dir.join(CONTROL_FILE), &serde_json::to_vec(&info)?)?;
    let token = Arc::new(token);
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, addr)) = listener.accept().await else {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            };
            if !addr.ip().is_loopback() {
                continue;
            }
            let (hub, token) = (hub.clone(), token.clone());
            tokio::spawn(async move {
                if let Err(e) = serve_one(hub, &token, stream).await {
                    tracing::debug!("Canal de las IA: {e}");
                }
            });
        }
    });
    tracing::info!("Canal local para las IA en 127.0.0.1:{port}");
    Ok((port, task.abort_handle()))
}

async fn serve_one(hub: Hub, token: &str, stream: TcpStream) -> std::io::Result<()> {
    let (r, mut w) = stream.into_split();
    let mut line = String::new();
    let mut reader = BufReader::new(r.take(MAX_REQUEST as u64));
    tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "sin petición"))??;
    let req: Request = serde_json::from_str(&line).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if !ct_eq(req.token.as_bytes(), token.as_bytes()) {
        tracing::warn!("Canal de las IA: petición con un token incorrecto");
        return Ok(());
    }
    let ctx = CallCtx { cwd: req.cwd, client: req.client };
    let resp = match api::call(&hub, &req.tool, &req.args, &ctx).await {
        Ok(text) => Response { ok: true, text },
        Err(text) => Response { ok: false, text },
    };
    let mut out = serde_json::to_vec(&resp)?;
    out.push(b'\n');
    w.write_all(&out).await?;
    w.shutdown().await
}

const NOT_RUNNING: &str = "Home File Sync is not open on this computer. Ask the user to open it (it keeps running in the menu bar / system tray) and try again.";

/// Llamada desde el servidor MCP a la app.
pub async fn call_app(data_dir: &Path, tool: &str, args: Value, cwd: Option<PathBuf>, client: String) -> Result<String, String> {
    let info: ControlInfo = std::fs::read(data_dir.join(CONTROL_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).ok_or(NOT_RUNNING)?;
    let stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(("127.0.0.1", info.port))).await.map_err(|_| NOT_RUNNING)?.map_err(|_| NOT_RUNNING)?;
    let (r, mut w) = stream.into_split();
    let req = Request { token: info.token, tool: tool.into(), args, cwd, client };
    let mut line = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
    line.push(b'\n');
    w.write_all(&line).await.map_err(|_| NOT_RUNNING)?;
    let mut resp = String::new();
    BufReader::new(r).read_line(&mut resp).await.map_err(|e| format!("Home File Sync closed the connection: {e}"))?;
    if resp.trim().is_empty() {
        return Err("Home File Sync rejected the request (the app may have restarted; try again).".into());
    }
    let resp: Response = serde_json::from_str(&resp).map_err(|e| format!("Invalid answer from Home File Sync: {e}"))?;
    if resp.ok {
        Ok(resp.text)
    } else {
        Err(resp.text)
    }
}

/// `home-file-sync --mcp [--data DIR]`: servidor MCP por stdio.
pub async fn run_bridge(data_dir: Option<PathBuf>) {
    let data_dir = Arc::new(data_dir.unwrap_or_else(default_data_dir));
    let cwd = std::env::current_dir().ok();
    let caller: super::mcp::Caller = Arc::new(move |tool, args, client| {
        let (d, cwd) = (data_dir.clone(), cwd.clone());
        Box::pin(async move { call_app(&d, &tool, args, cwd, client).await })
    });
    super::mcp::serve_stdio(super::mcp::Session::new(caller)).await;
}

/// Ruta del ejecutable para configurar el servidor MCP en las apps de IA.
pub fn mcp_command() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "home-file-sync".into())
}
