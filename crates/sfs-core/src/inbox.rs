//! Recepción de archivos sin tener la app: «enviar a este equipo» desde el
//! móvil (o cualquier navegador) de la misma red.
//!
//! La app muestra un código QR con una dirección como
//! `http://192.168.1.20:47830/r/<token>`. Al abrirla se ve una página sencilla
//! para elegir archivos; se suben a la carpeta que eligió quien recibe.
//!
//! * El token es aleatorio (128 bits), solo hay una sesión abierta a la vez y
//!   caduca (15 minutos por defecto). Sin token válido no se acepta nada.
//! * Los nombres se limpian: nunca se escribe fuera de la carpeta de destino
//!   y nunca se sobrescribe un archivo existente (se añade « (2)», « (3)»…).
//! * Cada archivo se escribe en un temporal y se coloca con un `rename`
//!   atómico al terminar; una subida interrumpida no deja nada a medias.
//! * Es HTTP sin cifrar dentro de la red local (un navegador no aceptaría un
//!   certificado propio sin avisos). No usar en redes públicas.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const DEFAULT_INBOX_PORT: u16 = 47830;
pub const DEFAULT_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_HEAD: usize = 16 * 1024;
const IDLE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceivedView {
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveView {
    /// Dirección principal (la del QR).
    pub url: String,
    /// Otras direcciones posibles (si el equipo tiene varias redes).
    pub other_urls: Vec<String>,
    /// Código QR en SVG.
    pub qr_svg: String,
    /// Dónde se guardan los archivos (para mostrarlo).
    pub label: String,
    pub expires_in_secs: u64,
    pub received: Vec<ReceivedView>,
}

struct Session {
    token: String,
    dest: PathBuf,
    /// Carpeta para los temporales (en el mismo volumen que el destino).
    temp_dir: PathBuf,
    label: String,
    /// Nombre de la carpeta compartida (o None si es una carpeta normal).
    share: Option<String>,
    expires: Instant,
    received: Vec<ReceivedView>,
}

struct State {
    session: Option<Session>,
    device_name: String,
}

#[derive(Clone)]
pub struct Inbox {
    port: u16,
    state: Arc<Mutex<State>>,
}

impl Inbox {
    /// Arranca el servidor (en `port` si está libre; si no, en uno cualquiera).
    pub async fn start(port: Option<u16>, device_name: &str) -> std::io::Result<Inbox> {
        let listener = match port {
            Some(p) => TcpListener::bind(("0.0.0.0", p)).await?,
            None => match TcpListener::bind(("0.0.0.0", DEFAULT_INBOX_PORT)).await {
                Ok(l) => l,
                Err(_) => TcpListener::bind(("0.0.0.0", 0)).await?,
            },
        };
        let port = listener.local_addr()?.port();
        let inbox = Inbox { port, state: Arc::new(Mutex::new(State { session: None, device_name: device_name.into() })) };
        let i = inbox.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, from)) = listener.accept().await else { continue };
                if stream.local_addr().is_ok_and(|l| !crate::netpath::accept_on(l.ip())) {
                    tracing::info!("Recepción web desde {from} por VPN rechazada (VPN no permitida)");
                    continue;
                }
                let i = i.clone();
                tokio::spawn(async move {
                    if let Err(e) = i.handle(stream).await {
                        tracing::debug!("Recepción web desde {from}: {e}");
                    }
                });
            }
        });
        tracing::info!("Recepción de archivos por navegador disponible en el puerto {port}");
        Ok(inbox)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn set_device_name(&self, name: &str) {
        self.state.lock().unwrap().device_name = name.into();
    }

    /// Abre una sesión de recepción hacia `dest`. Sustituye a la anterior.
    /// `share`: nombre de la carpeta compartida de destino, si lo es (para
    /// describirla en el idioma del móvil que abre el enlace).
    pub fn open(&self, dest: &Path, temp_dir: &Path, label: &str, share: Option<&str>, ttl: Duration) -> std::io::Result<ReceiveView> {
        std::fs::create_dir_all(dest)?;
        std::fs::create_dir_all(temp_dir)?;
        let token = hex::encode(crate::auth::random_bytes::<16>());
        self.state.lock().unwrap().session = Some(Session {
            token,
            dest: dest.to_path_buf(),
            temp_dir: temp_dir.to_path_buf(),
            label: label.into(),
            share: share.map(str::to_string),
            expires: Instant::now() + ttl,
            received: Vec::new(),
        });
        tracing::info!("Recepción de archivos abierta hacia «{label}» ({} min)", ttl.as_secs() / 60);
        Ok(self.view().expect("sesión recién abierta"))
    }

    pub fn close(&self) {
        if self.state.lock().unwrap().session.take().is_some() {
            tracing::info!("Recepción de archivos cerrada");
        }
    }

    /// Estado de la sesión abierta (None si no hay o ha caducado).
    pub fn view(&self) -> Option<ReceiveView> {
        let mut st = self.state.lock().unwrap();
        if st.session.as_ref().is_some_and(|s| s.expires <= Instant::now()) {
            st.session = None;
        }
        let s = st.session.as_ref()?;
        let urls: Vec<String> = local_urls(self.port, &s.token);
        let url = urls.first().cloned().unwrap_or_else(|| format!("http://127.0.0.1:{}/r/{}", self.port, s.token));
        Some(ReceiveView {
            qr_svg: qr_svg(&url),
            other_urls: urls.into_iter().skip(1).collect(),
            url,
            label: s.label.clone(),
            expires_in_secs: s.expires.saturating_duration_since(Instant::now()).as_secs(),
            received: s.received.clone(),
        })
    }

    async fn handle(&self, mut stream: TcpStream) -> std::io::Result<()> {
        let (head, rest) = read_head(&mut stream).await?;
        let mut lines = head.split("\r\n");
        let request = lines.next().unwrap_or("");
        let mut parts = request.split(' ');
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let mut content_length: Option<u64> = None;
        let mut lang = crate::i18n::Lang::En;
        let mut chunked = false;
        for l in lines {
            if let Some((k, v)) = l.split_once(':') {
                match k.trim().to_ascii_lowercase().as_str() {
                    "content-length" => content_length = v.trim().parse().ok(),
                    "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
                    "accept-language" => lang = crate::i18n::Lang::from_accept_language(v),
                    _ => {}
                }
            }
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let segs: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match (method, segs.as_slice()) {
            ("GET", ["r", token]) => match self.check(token) {
                Ok((dest, device)) => respond(&mut stream, 200, "text/html; charset=utf-8", page(lang, &dest.describe(lang), &device).as_bytes()).await,
                Err(code) => respond(&mut stream, code, "text/html; charset=utf-8", error_page(lang, code).as_bytes()).await,
            },
            ("POST", ["r", token, "upload"]) => {
                if let Err(code) = self.check(token) {
                    let body = serde_json::json!({"ok": false, "error": tr_in!(lang, "Enlace caducado o no válido")}).to_string();
                    return respond(&mut stream, code, "application/json", body.as_bytes()).await;
                }
                if chunked || content_length.is_none() {
                    return respond(&mut stream, 411, "application/json", br#"{"ok":false,"error":"Falta Content-Length"}"#).await;
                }
                let name = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("name="))
                    .map(percent_decode)
                    .map(|n| sanitize_name(&n))
                    .unwrap_or_else(|| "archivo".into());
                match self.receive(lang, token, &name, content_length.unwrap(), rest, &mut stream).await {
                    Ok((saved, size)) => {
                        let body = serde_json::json!({"ok": true, "name": saved, "size": size}).to_string();
                        respond(&mut stream, 200, "application/json", body.as_bytes()).await
                    }
                    Err(e) => {
                        tracing::warn!("Recepción web: «{name}» no se pudo guardar: {e}");
                        let body = serde_json::json!({"ok": false, "error": e.to_string()}).to_string();
                        respond(&mut stream, 500, "application/json", body.as_bytes()).await
                    }
                }
            }
            _ => respond(&mut stream, 404, "text/plain; charset=utf-8", b"No encontrado").await,
        }
    }

    /// Comprueba el token: Ok((destino, nombre del equipo)) o código HTTP.
    fn check(&self, token: &str) -> Result<(Dest, String), u16> {
        let st = self.state.lock().unwrap();
        match &st.session {
            Some(s) if crate::auth::ct_eq(s.token.as_bytes(), token.as_bytes()) => {
                if s.expires <= Instant::now() {
                    Err(410)
                } else {
                    Ok((Dest { share: s.share.clone(), path: s.dest.display().to_string() }, st.device_name.clone()))
                }
            }
            _ => Err(404),
        }
    }

    async fn receive(&self, lang: crate::i18n::Lang, token: &str, name: &str, len: u64, already: Vec<u8>, stream: &mut TcpStream) -> std::io::Result<(String, u64)> {
        let (dest, temp_dir) = {
            let st = self.state.lock().unwrap();
            let s = st.session.as_ref().filter(|s| s.token == token).ok_or_else(|| std::io::Error::other(tr_in!(lang, "Enlace caducado o no válido")))?;
            (s.dest.clone(), s.temp_dir.clone())
        };
        if let Some(free) = crate::fsops::available_space(&dest) {
            if free < len + 64 * 1024 * 1024 {
                return Err(std::io::Error::other(tr_in!(lang, "no hay espacio suficiente en el disco")));
            }
        }
        let temp = temp_dir.join(format!("upload-{}.tmp", hex::encode(crate::auth::random_bytes::<8>())));
        let result = async {
            let mut f = tokio::fs::File::create(&temp).await?;
            let mut written = 0u64;
            let first = &already[..already.len().min(len as usize)];
            f.write_all(first).await?;
            written += first.len() as u64;
            let mut buf = vec![0u8; 256 * 1024];
            while written < len {
                let want = buf.len().min((len - written) as usize);
                let n = tokio::time::timeout(IDLE, stream.read(&mut buf[..want]))
                    .await
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, tr_in!(lang, "la subida se ha detenido")))??;
                if n == 0 {
                    return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, tr_in!(lang, "subida interrumpida")));
                }
                f.write_all(&buf[..n]).await?;
                written += n as u64;
            }
            f.sync_all().await?;
            Ok::<_, std::io::Error>(())
        }
        .await;
        if let Err(e) = result {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
        let final_name = place_unique(&temp, &dest, name)?;
        tracing::info!("Recibido por el navegador: «{final_name}» ({len} bytes)");
        if let Some(s) = self.state.lock().unwrap().session.as_mut() {
            s.received.push(ReceivedView { name: final_name.clone(), size: len });
        }
        Ok((final_name, len))
    }
}

/// Coloca el temporal en `dest/name` sin sobrescribir: « (2)», « (3)»…
fn place_unique(temp: &Path, dest: &Path, name: &str) -> std::io::Result<String> {
    let (_, stem, ext) = crate::pathsafe::split_name(name);
    for i in 1..10_000 {
        let candidate = if i == 1 { name.to_string() } else { format!("{stem} ({i}){ext}") };
        let target = dest.join(&candidate);
        if target.exists() {
            continue;
        }
        // Para no pisar uno creado justo ahora, se comprueba de nuevo tras enlazar.
        match std::fs::hard_link(temp, &target) {
            Ok(()) => {
                let _ = std::fs::remove_file(temp);
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            // Sistemas sin enlaces duros (FAT/exFAT): renombrado normal.
            Err(_) => {
                std::fs::rename(temp, &target)?;
                return Ok(candidate);
            }
        }
    }
    Err(std::io::Error::other(tr!("demasiados archivos con el mismo nombre")))
}

/// Limpia un nombre de archivo recibido: solo el nombre (sin carpetas),
/// sin caracteres que Windows no admite ni nombres reservados.
pub fn sanitize_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    s = crate::pathsafe::nfc(s.trim().trim_end_matches(['.', ' ']));
    if s.is_empty() || s == "." || s == ".." {
        s = "archivo".into();
    }
    if crate::pathsafe::is_ignored_name(&s) || s.starts_with('.') || crate::pathsafe::windows_representable(&s).is_err() {
        s = format!("_{s}");
    }
    if s.len() > 200 {
        let (_, stem, ext) = crate::pathsafe::split_name(&s);
        let stem: String = stem.chars().take(150).collect();
        s = format!("{stem}{ext}");
    }
    s
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn read_head(stream: &mut TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 2048];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(15), stream.read(&mut tmp))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "petición incompleta"))??;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "conexión cerrada"));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).into_owned();
            return Ok((head, buf[pos + 4..].to_vec()));
        }
        if buf.len() > MAX_HEAD {
            return Err(std::io::Error::other("cabecera demasiado grande"));
        }
    }
}

async fn respond(stream: &mut TcpStream, code: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        404 => "Not Found",
        410 => "Gone",
        411 => "Length Required",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}

/// Direcciones con las que otros equipos de la red pueden llegar a este.
fn local_urls(port: u16, token: &str) -> Vec<String> {
    let mut ifs: Vec<crate::netpath::LocalIf> = crate::netpath::local_interfaces()
        .into_iter()
        .filter(|i| i.ip.is_ipv4() && crate::netpath::usable(i.ip) && (!i.tunnel || crate::netpath::listen_vpn()))
        .filter(|i| !matches!(i.ip, std::net::IpAddr::V4(v) if v.is_link_local()))
        .collect();
    // Redes privadas primero; VPN y túneles al final.
    let virt = |n: &str| ["vEthernet", "VirtualBox", "vmnet", "bridge"].iter().any(|p| n.starts_with(p));
    ifs.sort_by_key(|i| (i.tunnel, virt(&i.name), !matches!(i.ip, std::net::IpAddr::V4(v) if v.is_private())));
    ifs.iter().map(|i| format!("http://{}:{port}/r/{token}", i.ip)).collect()
}

fn qr_svg(url: &str) -> String {
    match qrcode::QrCode::new(url.as_bytes()) {
        Ok(code) => code.render::<qrcode::render::svg::Color>().min_dimensions(220, 220).quiet_zone(true).build(),
        Err(_) => String::new(),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Destino de la sesión, para describirlo en el idioma de quien abre el enlace.
struct Dest {
    share: Option<String>,
    path: String,
}

impl Dest {
    fn describe(&self, l: crate::i18n::Lang) -> String {
        match &self.share {
            Some(n) => tr_in!(l, "la carpeta compartida «{}»", n),
            None => tr_in!(l, "la carpeta {}", self.path),
        }
    }
}

fn error_page(l: crate::i18n::Lang, code: u16) -> String {
    let msg = if code == 410 { tr_in!(l, "Este enlace ha caducado.") } else { tr_in!(l, "Este enlace no es válido.") };
    format!(
        "<!doctype html><html lang='{}'><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>Home File Sync</title>\
         <body style='font:16px system-ui;margin:40px 20px;text-align:center'><h2>{}</h2><p>{}</p>",
        l.code(),
        esc(&msg),
        esc(&tr_in!(l, "Pide que vuelvan a mostrar el código QR en Home File Sync."))
    )
}

/// Página del móvil en su idioma. Los textos de la parte en JavaScript van en un objeto JSON.
fn page(l: crate::i18n::Lang, dest: &str, device: &str) -> String {
    let js = serde_json::json!({
        "waiting": tr_in!(l, "En espera"),
        "sending": tr_in!(l, "Enviando…"),
        "sent": tr_in!(l, "Enviado ✓"),
        "as": tr_in!(l, " como «{}»"),
        "noconn": tr_in!(l, "Sin conexión con el equipo"),
    });
    include_str!("inbox_page.html")
        .replace("{{LANG}}", l.code())
        .replace("{{TITLE}}", &esc(&tr_in!(l, "Enviar a {}", device)))
        .replace("{{H1}}", &esc(&tr_in!(l, "Enviar archivos a «{}»", device)))
        .replace("{{SUB}}", &tr_in!(l, "Se guardarán en <b>{}</b>. No hace falta instalar nada.", esc(dest)))
        .replace("{{CHOOSE}}", &esc(&tr_in!(l, "Elegir archivos")))
        .replace("{{DRAG}}", &esc(&tr_in!(l, "o arrastrarlos aquí")))
        .replace("{{FOOTER}}", &esc(&tr_in!(l, "Home File Sync · solo dentro de la misma red")))
        .replace("{{JS}}", &js.to_string().replace("</", "<\\/"))
}

/// Página del móvil con datos de ejemplo, para las capturas del README.
#[doc(hidden)]
pub fn sample_page(l: crate::i18n::Lang, share: &str, device: &str) -> String {
    let dest = Dest { share: Some(share.to_string()), path: String::new() };
    page(l, &dest.describe(l), device)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name("C:\\Users\\x\\Foto 1.jpg"), "Foto 1.jpg");
        assert_eq!(sanitize_name("vídeo: final?.mov"), "vídeo_ final_.mov");
        assert_eq!(sanitize_name(""), "archivo");
        assert_eq!(sanitize_name(".."), "archivo");
        assert_eq!(sanitize_name(".DS_Store"), "_.DS_Store");
        assert_eq!(sanitize_name("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_name("nota. "), "nota");
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("Foto%20de%20Ana%C3%B1.jpg"), "Foto de Anañ.jpg");
        assert_eq!(percent_decode("a+b%"), "a b%");
        assert_eq!(percent_decode("100%25"), "100%");
    }
}
