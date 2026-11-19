//! Cliente HTTP común (TLS del sistema) y conversión de errores.

use reqwest::blocking::{Client, Response};
use std::time::Duration;

pub(crate) fn client() -> std::io::Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(20))
        // Sin límite total (archivos grandes); la conexión se mantiene viva.
        .timeout(None)
        .tcp_keepalive(Duration::from_secs(30))
        .user_agent(concat!("HomeFileSync/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| std::io::Error::other(e.to_string()))
}

pub(crate) fn net(e: reqwest::Error) -> std::io::Error {
    let kind = if e.is_timeout() { std::io::ErrorKind::TimedOut } else { std::io::ErrorKind::Other };
    // Sin la URL: puede llevar tokens o datos del servicio.
    std::io::Error::new(kind, tr!("sin conexión con el servicio externo ({})", e.without_url()))
}

/// Error a partir de una respuesta no satisfactoria.
pub(crate) fn status(resp: Response) -> std::io::Error {
    let code = resp.status().as_u16();
    let body: String = resp.text().unwrap_or_default().chars().take(300).collect();
    match code {
        404 => std::io::Error::new(std::io::ErrorKind::NotFound, "no existe"),
        401 => std::io::Error::new(std::io::ErrorKind::PermissionDenied, tr!("el servicio no acepta el usuario o la contraseña (o la sesión ha caducado)")),
        403 => std::io::Error::new(std::io::ErrorKind::PermissionDenied, tr!("el servicio no da permiso ({})", body.trim())),
        507 => std::io::Error::other(tr!("el servicio externo no tiene espacio")),
        _ => std::io::Error::other(format!("HTTP {code}: {}", body.trim())),
    }
}

/// Espera recomendada por el servicio antes de reintentar (429 / 503).
pub(crate) fn retry_after(resp: &Response) -> Option<Duration> {
    let code = resp.status().as_u16();
    if code != 429 && code != 503 {
        return None;
    }
    let secs = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(5);
    Some(Duration::from_secs(secs.clamp(1, 60)))
}
