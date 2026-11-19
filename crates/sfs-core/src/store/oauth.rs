//! Inicio de sesión en Google Drive y Dropbox (OAuth 2 con PKCE).
//!
//! La app abre el navegador con la página de inicio de sesión del servicio;
//! al aceptar, el servicio redirige a `http://127.0.0.1:47835/` en este mismo
//! equipo, donde la app recoge el código y lo cambia por un token de
//! renovación, que se guarda en el almacén del sistema. La contraseña de la
//! cuenta nunca pasa por la app.
//!
//! Los identificadores de la app en Google y Dropbox se fijan al compilar
//! (`HFS_GOOGLE_CLIENT_ID`, `HFS_GOOGLE_CLIENT_SECRET`, `HFS_DROPBOX_APP_KEY`;
//! ver `docs/REGISTRO-SERVICIOS.md`). Si faltan, esa opción no aparece. En una
//! app de escritorio el «secreto» de Google no es secreto (Google lo indica
//! así): la seguridad la dan PKCE y la redirección local.

use super::http;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

pub const REDIRECT_PORT: u16 = 47835;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Google,
    Dropbox,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Provider> {
        match s {
            "gdrive" | "google" => Some(Provider::Google),
            "dropbox" => Some(Provider::Dropbox),
            _ => None,
        }
    }
}

fn cfg(var: &str, built: Option<&'static str>) -> Option<String> {
    std::env::var(var).ok().or(built.map(str::to_string)).filter(|v| !v.trim().is_empty())
}

pub(crate) fn google_client() -> Option<(String, String)> {
    Some((cfg("HFS_GOOGLE_CLIENT_ID", option_env!("HFS_GOOGLE_CLIENT_ID"))?, cfg("HFS_GOOGLE_CLIENT_SECRET", option_env!("HFS_GOOGLE_CLIENT_SECRET")).unwrap_or_default()))
}

pub(crate) fn dropbox_key() -> Option<String> {
    cfg("HFS_DROPBOX_APP_KEY", option_env!("HFS_DROPBOX_APP_KEY"))
}

/// ¿Está disponible el inicio de sesión con este servicio en esta compilación?
pub fn available(p: Provider) -> bool {
    match p {
        Provider::Google => google_client().is_some(),
        Provider::Dropbox => dropbox_key().is_some(),
    }
}

fn redirect_uri() -> String {
    format!("http://127.0.0.1:{REDIRECT_PORT}/")
}

pub(crate) fn b64url(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        let k = c.len() + 1;
        for i in 0..k {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", super::pct(k), super::pct(v))).collect::<Vec<_>>().join("&")
}

/// Inicio de sesión en curso: espera la redirección del navegador.
pub struct Login {
    provider: Provider,
    listener: std::net::TcpListener,
    verifier: String,
    state: String,
}

/// Empieza el inicio de sesión: devuelve la dirección que hay que abrir en el navegador.
pub fn begin(provider: Provider) -> Result<(String, Login), String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", REDIRECT_PORT))
        .map_err(|_| tr!("Hay otro inicio de sesión en curso (o el puerto {} está ocupado).", REDIRECT_PORT))?;
    let verifier = b64url(&crate::auth::random_bytes::<32>());
    let state = b64url(&crate::auth::random_bytes::<16>());
    let challenge = {
        use sha2::Digest;
        b64url(&sha2::Sha256::digest(verifier.as_bytes()))
    };
    let url = match provider {
        Provider::Google => {
            let (id, _) = google_client().ok_or_else(|| tr!("Esta versión no incluye el inicio de sesión con {}.", "Google"))?;
            format!(
                "https://accounts.google.com/o/oauth2/v2/auth?{}",
                form(&[
                    ("client_id", &id),
                    ("redirect_uri", &redirect_uri()),
                    ("response_type", "code"),
                    ("scope", "https://www.googleapis.com/auth/drive.file openid email"),
                    ("code_challenge", &challenge),
                    ("code_challenge_method", "S256"),
                    ("access_type", "offline"),
                    ("prompt", "consent"),
                    ("state", &state),
                ])
            )
        }
        Provider::Dropbox => {
            let key = dropbox_key().ok_or_else(|| tr!("Esta versión no incluye el inicio de sesión con {}.", "Dropbox"))?;
            format!(
                "https://www.dropbox.com/oauth2/authorize?{}",
                form(&[
                    ("client_id", &key),
                    ("redirect_uri", &redirect_uri()),
                    ("response_type", "code"),
                    ("code_challenge", &challenge),
                    ("code_challenge_method", "S256"),
                    ("token_access_type", "offline"),
                    ("state", &state),
                ])
            )
        }
    };
    Ok((url, Login { provider, listener, verifier, state }))
}

/// Resultado de un inicio de sesión: token de renovación y cuenta (para mostrarla).
pub struct Session {
    pub refresh_token: String,
    pub account: String,
}

impl Login {
    /// Espera a que el navegador vuelva (como mucho `timeout`) y obtiene el token.
    pub fn wait(self, timeout: Duration) -> Result<Session, String> {
        let deadline = Instant::now() + timeout;
        self.listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let code = loop {
            if Instant::now() > deadline {
                return Err(tr!("No se ha completado el inicio de sesión a tiempo."));
            }
            match self.listener.accept() {
                Ok((mut s, _)) => {
                    let _ = s.set_nonblocking(false);
                    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut buf = [0u8; 8192];
                    let n = s.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let target = req.split_whitespace().nth(1).unwrap_or("");
                    let q = target.split_once('?').map(|(_, q)| q).unwrap_or("");
                    let param = |k: &str| q.split('&').find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('='))).map(super::pct_decode);
                    if param("state").is_none() && param("code").is_none() && param("error").is_none() {
                        let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        continue; // favicon, etc.
                    }
                    let ok = param("state").is_some_and(|st| crate::auth::ct_eq(st.as_bytes(), self.state.as_bytes())) && param("code").is_some();
                    let msg = if ok { tr!("Listo. Ya puedes volver a Home File Sync.") } else { tr!("No se ha podido iniciar sesión. Vuelve a Home File Sync e inténtalo de nuevo.") };
                    let body = format!(
                        "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'><title>Home File Sync</title><body style='font:17px system-ui;margin:60px 20px;text-align:center'><h2>{}</h2>",
                        msg.replace('<', "&lt;")
                    );
                    let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}", body.len()).as_bytes());
                    if !ok {
                        return Err(match param("error") {
                            Some(e) if e.contains("denied") => tr!("Se ha cancelado el inicio de sesión."),
                            _ => tr!("No se ha podido iniciar sesión. Vuelve a Home File Sync e inténtalo de nuevo."),
                        });
                    }
                    break param("code").unwrap();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(200)),
                Err(e) => return Err(e.to_string()),
            }
        };
        drop(self.listener);
        exchange(self.provider, &code, &self.verifier)
    }
}

fn token_request(provider: Provider, body: String) -> std::io::Result<serde_json::Value> {
    let url = match provider {
        Provider::Google => "https://oauth2.googleapis.com/token",
        Provider::Dropbox => "https://api.dropboxapi.com/oauth2/token",
    };
    let r = http::client()?.post(url).header("Content-Type", "application/x-www-form-urlencoded").body(body).send().map_err(http::net)?;
    if !r.status().is_success() {
        return Err(http::status(r));
    }
    r.json::<serde_json::Value>().map_err(http::net)
}

fn exchange(provider: Provider, code: &str, verifier: &str) -> Result<Session, String> {
    let redirect = redirect_uri();
    let body = match provider {
        Provider::Google => {
            let (id, secret) = google_client().unwrap_or_default();
            form(&[("code", code), ("client_id", &id), ("client_secret", &secret), ("redirect_uri", &redirect), ("grant_type", "authorization_code"), ("code_verifier", verifier)])
        }
        Provider::Dropbox => {
            let key = dropbox_key().unwrap_or_default();
            form(&[("code", code), ("client_id", &key), ("redirect_uri", &redirect), ("grant_type", "authorization_code"), ("code_verifier", verifier)])
        }
    };
    let v = token_request(provider, body).map_err(|e| tr!("No se ha podido iniciar sesión: {}", e))?;
    let refresh = v["refresh_token"].as_str().ok_or_else(|| tr!("No se ha podido iniciar sesión: {}", "sin refresh_token"))?.to_string();
    let access = v["access_token"].as_str().unwrap_or_default().to_string();
    let account = account_label(provider, &access).unwrap_or_else(|_| "?".into());
    tracing::info!("Sesión iniciada en {:?}", provider);
    Ok(Session { refresh_token: refresh, account })
}

fn account_label(provider: Provider, access: &str) -> std::io::Result<String> {
    let c = http::client()?;
    let v: serde_json::Value = match provider {
        Provider::Google => c.get("https://openidconnect.googleapis.com/v1/userinfo").bearer_auth(access).send().map_err(http::net)?.json().map_err(http::net)?,
        Provider::Dropbox => c.post("https://api.dropboxapi.com/2/users/get_current_account").bearer_auth(access).send().map_err(http::net)?.json().map_err(http::net)?,
    };
    Ok(v["email"].as_str().unwrap_or("?").to_string())
}

/// Token de acceso a partir del de renovación. Devuelve el token y su duración.
pub(crate) fn refresh(provider: Provider, refresh_token: &str) -> std::io::Result<(String, Duration)> {
    let body = match provider {
        Provider::Google => {
            let (id, secret) = google_client().unwrap_or_default();
            form(&[("refresh_token", refresh_token), ("client_id", &id), ("client_secret", &secret), ("grant_type", "refresh_token")])
        }
        Provider::Dropbox => form(&[("refresh_token", refresh_token), ("client_id", &dropbox_key().unwrap_or_default()), ("grant_type", "refresh_token")]),
    };
    let v = token_request(provider, body).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied || e.to_string().contains("invalid_grant") {
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, tr!("La sesión del servicio externo ha caducado o se ha retirado el permiso. Vuelve a iniciar sesión."))
        } else {
            e
        }
    })?;
    let access = v["access_token"].as_str().ok_or_else(|| std::io::Error::other("sin access_token"))?.to_string();
    let secs = v["expires_in"].as_u64().unwrap_or(3600);
    Ok((access, Duration::from_secs(secs.saturating_sub(120).max(60))))
}

/// Token de acceso en caché, renovado cuando caduca.
pub(crate) struct AccessToken {
    provider: Provider,
    refresh: String,
    cached: std::sync::Mutex<Option<(String, Instant)>>,
}

impl AccessToken {
    pub fn new(provider: Provider, refresh: &str) -> Self {
        AccessToken { provider, refresh: refresh.to_string(), cached: std::sync::Mutex::new(None) }
    }

    pub fn get(&self) -> std::io::Result<String> {
        let mut c = self.cached.lock().unwrap();
        if let Some((t, until)) = c.as_ref() {
            if Instant::now() < *until {
                return Ok(t.clone());
            }
        }
        let (t, d) = refresh(self.provider, &self.refresh)?;
        *c = Some((t.clone(), Instant::now() + d));
        Ok(t)
    }

    /// El servicio ha rechazado el token: se pedirá otro.
    pub fn invalidate(&self) {
        *self.cached.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_callback_checks_state_and_cancel() {
        std::env::set_var("HFS_DROPBOX_APP_KEY", "clave-de-prueba");
        assert!(available(Provider::Dropbox));
        let (url, login) = begin(Provider::Dropbox).unwrap();
        assert!(url.starts_with("https://www.dropbox.com/oauth2/authorize?") && url.contains("code_challenge_method=S256") && url.contains("token_access_type=offline"));
        assert!(url.contains(&format!("redirect_uri={}", super::super::pct(&redirect_uri()))));
        // Un segundo inicio a la vez no puede usar el mismo puerto.
        assert!(begin(Provider::Dropbox).is_err());
        let browser = std::thread::spawn(|| {
            let get = |path: &str| {
                let mut s = std::net::TcpStream::connect(("127.0.0.1", REDIRECT_PORT)).unwrap();
                s.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes()).unwrap();
                let mut out = String::new();
                let _ = s.read_to_string(&mut out);
                out
            };
            assert!(get("/favicon.ico").starts_with("HTTP/1.1 404"));
            get("/?error=access_denied&state=otro")
        });
        let err = login.wait(Duration::from_secs(10)).err().unwrap();
        assert!(err.contains("cancel") || err.contains("Cancel"), "{err}");
        assert!(browser.join().unwrap().contains("200 OK"));
        // El puerto queda libre para otro intento.
        assert!(begin(Provider::Dropbox).is_ok());
    }

    #[test]
    fn base64url_matches_rfc4648() {
        assert_eq!(b64url(b""), "");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert_eq!(b64url(b"foob"), "Zm9vYg");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
        // Ejemplo de PKCE del RFC 7636 (apéndice B).
        use sha2::Digest;
        assert_eq!(b64url(&sha2::Sha256::digest(b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }
}
