//! Enlace público temporal para recibir archivos desde fuera de casa.
//!
//! Usa un «Quick Tunnel» de Cloudflare (`cloudflared tunnel --url …`): no
//! necesita cuenta ni abrir puertos en el router, y da una dirección
//! `https://<aleatorio>.trycloudflare.com` que lleva a la página de recepción
//! de este equipo. El enlace solo existe mientras la ventana de recepción está
//! abierta (y como mucho hasta que caduca la sesión); sin el token de la
//! sesión no se acepta nada.
//!
//! * `cloudflared` no se incluye en el instalador: se descarga la primera vez
//!   que se usa, de la publicación oficial de Cloudflare en GitHub, y solo se
//!   usa si su SHA-256 coincide con el fijado aquí para esa versión.
//! * El tráfico entre el móvil y Cloudflare va por HTTPS; Cloudflare lo
//!   reenvía a este equipo por su túnel cifrado. Cloudflare puede ver lo que
//!   pasa por él (como cualquier servicio en la nube): la interfaz lo indica.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};

/// Versión fijada de cloudflared y SHA-256 publicados por Cloudflare.
pub const CLOUDFLARED_VERSION: &str = "2026.10.7";

struct Asset {
    name: &'static str,
    /// SHA-256 del archivo descargado (resumen de GitHub para ese recurso).
    sha256: &'static str,
    /// SHA-256 del ejecutable que contiene un `.tgz` (el que publica Cloudflare).
    binary_sha256: Option<&'static str>,
    size: u64,
}

fn asset() -> Option<Asset> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(Asset {
            name: "cloudflared-darwin-arm64.tgz",
            sha256: "3586ccf397e98e5f437e60e7c307ca6ac463e110580503af8bb8e6f9ebf831bd",
            binary_sha256: Some("2136431d5eb467bf447d2ee514b713e192719b143aec1d9e09bf7dc290d5bb12"),
            size: 19847161,
        })
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some(Asset {
            name: "cloudflared-darwin-amd64.tgz",
            sha256: "02f3edfd87684dd5b705607688e4eb4b323f6e777b81e7b3a85af556aa356848",
            binary_sha256: Some("5c55e77da7012cebca19170357e196176c48c9647cc2ead404702fa125ddbd86"),
            size: 21834416,
        })
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(Asset {
            name: "cloudflared-windows-amd64.exe",
            sha256: "2493dbc168923bb3e59e5e7716f71deced92992eea9acd6e9d74f84ac7751343",
            binary_sha256: None,
            size: 55390512,
        })
    } else {
        None
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TunnelView {
    /// off | downloading | starting | ready | error
    pub status: String,
    pub url: Option<String>,
    pub error: Option<String>,
    /// Progreso de la descarga de cloudflared (0–1).
    pub progress: Option<f32>,
}

impl TunnelView {
    fn off() -> Self {
        TunnelView { status: "off".into(), url: None, error: None, progress: None }
    }
}

struct State {
    view: TunnelView,
    /// Cada arranque tiene un número; un arranque antiguo no pisa el estado de uno nuevo.
    run: u64,
    child: Option<tokio::process::Child>,
    /// Hasta cuándo debe seguir abierto (se amplía si se abre otra sesión).
    deadline: Instant,
}

#[derive(Clone)]
pub struct Tunnel {
    tools_dir: PathBuf,
    state: Arc<Mutex<State>>,
}

impl Tunnel {
    pub fn new(tools_dir: &Path) -> Self {
        Tunnel { tools_dir: tools_dir.to_path_buf(), state: Arc::new(Mutex::new(State { view: TunnelView::off(), run: 0, child: None, deadline: Instant::now() })) }
    }

    pub fn view(&self) -> TunnelView {
        self.state.lock().unwrap().view.clone()
    }

    pub fn public_url(&self) -> Option<String> {
        let st = self.state.lock().unwrap();
        (st.view.status == "ready").then(|| st.view.url.clone()).flatten()
    }

    /// Abre el enlace público hacia `http://127.0.0.1:<port>` hasta `deadline`.
    pub fn start(&self, port: u16, deadline: Instant) {
        let run = {
            let mut st = self.state.lock().unwrap();
            st.deadline = deadline;
            if matches!(st.view.status.as_str(), "downloading" | "starting" | "ready") {
                return;
            }
            st.run += 1;
            st.view = TunnelView { status: "starting".into(), url: None, error: None, progress: None };
            st.run
        };
        let t = self.clone();
        tokio::spawn(async move {
            if let Err(msg) = t.run(run, port, deadline).await {
                tracing::warn!("Enlace público: {msg}");
                let mut st = t.state.lock().unwrap();
                if st.run == run {
                    st.view = TunnelView { status: "error".into(), url: None, error: Some(msg), progress: None };
                    st.child = None;
                }
            }
        });
    }

    pub fn stop(&self) {
        let mut st = self.state.lock().unwrap();
        st.run += 1;
        if let Some(mut c) = st.child.take() {
            let _ = c.start_kill();
            tracing::info!("Enlace público cerrado");
        }
        st.view = TunnelView::off();
    }

    fn set(&self, run: u64, f: impl FnOnce(&mut TunnelView)) -> bool {
        let mut st = self.state.lock().unwrap();
        if st.run != run {
            return false;
        }
        f(&mut st.view);
        true
    }

    async fn run(&self, run: u64, port: u16, _deadline: Instant) -> Result<(), String> {
        let exe = self.ensure_binary(run).await?;
        if !self.set(run, |v| *v = TunnelView { status: "starting".into(), url: None, error: None, progress: None }) {
            return Ok(());
        }
        // Configuración vacía: una configuración propia de cloudflared en el
        // equipo (~/.cloudflared/config.yml) impediría el túnel rápido.
        let empty_cfg = self.tools_dir.join("cloudflared-vacio.yml");
        let _ = std::fs::write(&empty_cfg, b"");
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("tunnel")
            .arg("--config")
            .arg(&empty_cfg)
            .args(["--no-autoupdate", "--url"])
            .arg(format!("http://127.0.0.1:{port}"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().map_err(|e| tr!("No se pudo iniciar el enlace público: {}", e))?;
        let stderr = child.stderr.take().ok_or_else(|| tr!("No se pudo iniciar el enlace público: {}", "stderr"))?;
        {
            let mut st = self.state.lock().unwrap();
            if st.run != run {
                let _ = child.start_kill();
                return Ok(());
            }
            st.child = Some(child);
        }
        let mut lines = BufReader::new(stderr).lines();
        let found = tokio::time::timeout(Duration::from_secs(45), async {
            while let Ok(Some(l)) = lines.next_line().await {
                tracing::debug!("cloudflared: {l}");
                if let Some(url) = find_url(&l) {
                    return Some(url);
                }
            }
            None
        })
        .await
        .ok()
        .flatten();
        let Some(url) = found else {
            self.kill(run);
            return Err(tr!("Cloudflare no ha dado un enlace. Comprueba la conexión a internet e inténtalo de nuevo."));
        };
        tracing::info!("Enlace público abierto (Cloudflare)");
        if !self.set(run, |v| *v = TunnelView { status: "ready".into(), url: Some(url), error: None, progress: None }) {
            return Ok(());
        }
        // Sigue leyendo (para que cloudflared no se bloquee) hasta que termine o caduque.
        let watch = async {
            while let Ok(Some(l)) = lines.next_line().await {
                tracing::debug!("cloudflared: {l}");
            }
        };
        let expired = async {
            loop {
                let left = self.state.lock().unwrap().deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return;
                }
                tokio::time::sleep(left.min(Duration::from_secs(5))).await;
            }
        };
        tokio::select! {
            _ = watch => {
                if self.state.lock().unwrap().run == run {
                    self.kill(run);
                    return Err(tr!("El enlace público se ha cerrado."));
                }
            }
            _ = expired => {
                self.kill(run);
                self.set(run, |v| *v = TunnelView::off());
                tracing::info!("Enlace público cerrado (sesión caducada)");
            }
        }
        Ok(())
    }

    fn kill(&self, run: u64) {
        let mut st = self.state.lock().unwrap();
        if st.run == run {
            if let Some(mut c) = st.child.take() {
                let _ = c.start_kill();
            }
        }
    }

    /// Devuelve la ruta de cloudflared, descargándolo y verificándolo si hace falta.
    async fn ensure_binary(&self, run: u64) -> Result<PathBuf, String> {
        let a = asset().ok_or_else(|| tr!("El enlace público no está disponible en este sistema."))?;
        let exe_name = if cfg!(windows) { format!("cloudflared-{CLOUDFLARED_VERSION}.exe") } else { format!("cloudflared-{CLOUDFLARED_VERSION}") };
        let exe = self.tools_dir.join(&exe_name);
        if exe.is_file() {
            return Ok(exe);
        }
        std::fs::create_dir_all(&self.tools_dir).map_err(|e| e.to_string())?;
        let url = format!("https://github.com/cloudflare/cloudflared/releases/download/{CLOUDFLARED_VERSION}/{}", a.name);
        let dl = self.tools_dir.join(format!("{}.descarga", a.name));
        let _ = std::fs::remove_file(&dl);
        tracing::info!("Descargando cloudflared {CLOUDFLARED_VERSION} de GitHub ({} bytes)", a.size);
        self.set(run, |v| *v = TunnelView { status: "downloading".into(), url: None, error: None, progress: Some(0.0) });
        let mut cmd = tokio::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
        cmd.args(["-fsSL", "--proto", "=https", "--retry", "2", "-o"]).arg(&dl).arg(&url).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let mut child = cmd.spawn().map_err(|e| tr!("No se pudo descargar cloudflared: {}", e))?;
        let status = loop {
            tokio::select! {
                s = child.wait() => break s,
                _ = tokio::time::sleep(Duration::from_millis(400)) => {
                    let got = std::fs::metadata(&dl).map(|m| m.len()).unwrap_or(0);
                    if !self.set(run, |v| v.progress = Some((got as f32 / a.size as f32).min(1.0))) {
                        let _ = child.start_kill();
                        let _ = std::fs::remove_file(&dl);
                        return Err(tr!("Cancelado"));
                    }
                }
            }
        };
        if !status.map(|s| s.success()).unwrap_or(false) {
            let _ = std::fs::remove_file(&dl);
            return Err(tr!("No se pudo descargar cloudflared: {}", tr!("sin conexión a internet o GitHub no responde")));
        }
        // Verificación: solo se usa si es exactamente el archivo publicado.
        let d = dl.clone();
        let digest = tokio::task::spawn_blocking(move || sha256_file(&d)).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        if digest != a.sha256 {
            let _ = std::fs::remove_file(&dl);
            tracing::error!("cloudflared descargado con SHA-256 inesperado: {digest}");
            return Err(tr!("La descarga de cloudflared no supera la verificación; no se usará."));
        }
        let tmp_exe = self.tools_dir.join(format!("{exe_name}.tmp"));
        if a.name.ends_with(".tgz") {
            let out = self.tools_dir.join("extraer");
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            let ok = tokio::process::Command::new("tar").arg("-xzf").arg(&dl).arg("-C").arg(&out).status().await.map(|s| s.success()).unwrap_or(false);
            let extracted = out.join("cloudflared");
            if !ok || !extracted.is_file() {
                let _ = std::fs::remove_dir_all(&out);
                return Err(tr!("No se pudo descargar cloudflared: {}", "tar"));
            }
            let x = extracted.clone();
            let inner_digest = tokio::task::spawn_blocking(move || sha256_file(&x)).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
            if Some(inner_digest.as_str()) != a.binary_sha256 {
                let _ = std::fs::remove_dir_all(&out);
                let _ = std::fs::remove_file(&dl);
                tracing::error!("cloudflared extraído con SHA-256 inesperado: {inner_digest}");
                return Err(tr!("La descarga de cloudflared no supera la verificación; no se usará."));
            }
            std::fs::rename(&extracted, &tmp_exe).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_dir_all(&out);
            let _ = std::fs::remove_file(&dl);
        } else {
            std::fs::rename(&dl, &tmp_exe).map_err(|e| e.to_string())?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp_exe, std::fs::Permissions::from_mode(0o755));
        }
        std::fs::rename(&tmp_exe, &exe).map_err(|e| e.to_string())?;
        // Versiones anteriores ya no hacen falta.
        if let Ok(rd) = std::fs::read_dir(&self.tools_dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.starts_with("cloudflared-") && n != exe_name && n != "cloudflared-vacio.yml" {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        tracing::info!("cloudflared {CLOUDFLARED_VERSION} descargado y verificado");
        Ok(exe)
    }
}

fn sha256_file(p: &Path) -> std::io::Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(p)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Extrae la dirección `https://….trycloudflare.com` de una línea de cloudflared.
fn find_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest.find(|c: char| c.is_whitespace() || c == '|' || c == '"').unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches('/');
    let host = url.strip_prefix("https://")?;
    let ok = host.ends_with(".trycloudflare.com")
        && host != "api.trycloudflare.com"
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    ok.then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_quick_tunnel_url() {
        let l = "2026-11-16T10:00:00Z INF |  https://brave-lion-cool-table.trycloudflare.com                                     |";
        assert_eq!(find_url(l).as_deref(), Some("https://brave-lion-cool-table.trycloudflare.com"));
        assert_eq!(find_url("INF Requesting new quick Tunnel on trycloudflare.com..."), None);
        assert_eq!(find_url("ERR https://api.trycloudflare.com/tunnel failed"), None);
        assert_eq!(find_url("see https://developers.cloudflare.com/x"), None);
    }

    #[test]
    fn every_supported_platform_has_a_pinned_hash() {
        if let Some(a) = asset() {
            assert_eq!(a.sha256.len(), 64);
            assert!(a.size > 1_000_000);
        }
    }
}
