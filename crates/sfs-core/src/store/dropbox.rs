//! Dropbox con inicio de sesión (acceso completo, para poder usar una carpeta
//! compartida desde Dropbox con otras cuentas). El relevo vive en la carpeta
//! elegida (por defecto `/Home File Sync`).

use super::oauth::{AccessToken, Provider};
use super::{http, not_found, Item, Store};
use reqwest::blocking::{Client, RequestBuilder, Response};
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime};

const API: &str = "https://api.dropboxapi.com/2";
const CONTENT: &str = "https://content.dropboxapi.com/2";
const SINGLE_MAX: u64 = 140 * 1024 * 1024;
const CHUNK: usize = 32 * 1024 * 1024;

pub struct Dropbox {
    client: Client,
    token: AccessToken,
    /// Carpeta del relevo en Dropbox, p. ej. `/Home File Sync`.
    root: String,
}

/// Normaliza la carpeta de Dropbox escrita por el usuario.
pub fn normalize_path(p: &str) -> String {
    let parts: Vec<&str> = p.split(['/', '\\']).map(str::trim).filter(|c| !c.is_empty() && *c != "." && *c != "..").collect();
    if parts.is_empty() {
        format!("/{}", super::ROOT_NAME)
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// JSON para la cabecera `Dropbox-API-Arg`, que solo admite ASCII.
fn arg(v: &serde_json::Value) -> String {
    let mut out = String::new();
    for ch in v.to_string().chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            let mut buf = [0u16; 2];
            for u in ch.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

fn parse_time(s: &str) -> SystemTime {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| SystemTime::UNIX_EPOCH + Duration::from_secs(d.timestamp().max(0) as u64))
        .unwrap_or_else(|_| SystemTime::now())
}

/// Error de Dropbox (409 lleva un resumen como `path/not_found/..`).
fn dbx_err(r: Response) -> std::io::Error {
    if r.status().as_u16() == 409 {
        let body = r.text().unwrap_or_default();
        if body.contains("not_found") {
            return not_found();
        }
        if body.contains("insufficient_space") {
            return std::io::Error::other(tr!("el servicio externo no tiene espacio"));
        }
        let summary: String = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["error_summary"].as_str().map(str::to_string))
            .unwrap_or(body)
            .chars()
            .take(200)
            .collect();
        return std::io::Error::new(std::io::ErrorKind::AlreadyExists, summary);
    }
    http::status(r)
}

impl Dropbox {
    pub fn new(refresh_token: &str, root: &str) -> Result<Self, String> {
        Ok(Dropbox { client: http::client().map_err(|e| e.to_string())?, token: AccessToken::new(Provider::Dropbox, refresh_token), root: normalize_path(root) })
    }

    fn p(&self, rel: &str) -> String {
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            self.root.clone()
        } else {
            format!("{}/{rel}", self.root)
        }
    }

    fn call(&self, build: impl Fn(&Client) -> RequestBuilder) -> std::io::Result<Response> {
        let mut tries = 0;
        loop {
            tries += 1;
            let r = build(&self.client).bearer_auth(self.token.get()?).send().map_err(http::net)?;
            let code = r.status().as_u16();
            if code == 401 && tries < 3 {
                self.token.invalidate();
                continue;
            }
            if let Some(wait) = http::retry_after(&r).or((code >= 500).then_some(Duration::from_secs(2))) {
                if tries < 4 {
                    std::thread::sleep(wait);
                    continue;
                }
            }
            return Ok(r);
        }
    }

    fn api(&self, endpoint: &str, body: serde_json::Value) -> std::io::Result<serde_json::Value> {
        let r = self.call(|c| c.post(format!("{API}/{endpoint}")).json(&body))?;
        if !r.status().is_success() {
            return Err(dbx_err(r));
        }
        r.json().map_err(http::net)
    }

    fn content(&self, endpoint: &str, a: serde_json::Value, data: Vec<u8>) -> std::io::Result<Response> {
        let header = arg(&a);
        let r = self.call(|c| c.post(format!("{CONTENT}/{endpoint}")).header("Dropbox-API-Arg", header.clone()).header("Content-Type", "application/octet-stream").body(data.clone()))?;
        if !r.status().is_success() {
            return Err(dbx_err(r));
        }
        Ok(r)
    }
}

impl Store for Dropbox {
    fn mkdirs(&self, dir: &str) -> std::io::Result<()> {
        match self.api("files/create_folder_v2", serde_json::json!({"path": self.p(dir), "autorename": false})) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()), // ya existe
            Err(e) => Err(e),
        }
    }

    fn list(&self, dir: &str) -> std::io::Result<Vec<Item>> {
        let mut v = match self.api("files/list_folder", serde_json::json!({"path": self.p(dir), "limit": 2000})) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        loop {
            for e in v["entries"].as_array().into_iter().flatten() {
                if e[".tag"] == "file" {
                    let name = e["name"].as_str().unwrap_or("").to_string();
                    if !name.starts_with('.') {
                        out.push(Item { name, size: e["size"].as_u64().unwrap_or(0), modified: parse_time(e["server_modified"].as_str().unwrap_or("")) });
                    }
                }
            }
            if v["has_more"].as_bool() != Some(true) {
                return Ok(out);
            }
            let cursor = v["cursor"].as_str().unwrap_or("").to_string();
            v = self.api("files/list_folder/continue", serde_json::json!({"cursor": cursor}))?;
        }
    }

    fn write(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        self.content("files/upload", serde_json::json!({"path": self.p(path), "mode": "overwrite", "mute": true}), data.to_vec()).map(|_| ())
    }

    fn reader(&self, path: &str) -> std::io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(self.content("files/download", serde_json::json!({"path": self.p(path)}), Vec::new())?))
    }

    fn upload(&self, path: &str, src: &Path) -> std::io::Result<()> {
        let total = std::fs::metadata(src)?.len();
        let commit = serde_json::json!({"path": self.p(path), "mode": "add", "autorename": false, "mute": true});
        let conflict_ok = |r: std::io::Result<Response>| match r {
            // Ya estaba (otro equipo subió el mismo contenido): vale.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            r => r.map(|_| ()),
        };
        if total <= SINGLE_MAX {
            return conflict_ok(self.content("files/upload", commit, std::fs::read(src)?));
        }
        // Sesión de subida por partes: el archivo aparece al terminar.
        let mut f = std::fs::File::open(src)?;
        let mut buf = vec![0u8; CHUNK];
        let n = super::read_up_to(&mut f, &mut buf)?;
        let r = self.content("files/upload_session/start", serde_json::json!({"close": false}), buf[..n].to_vec())?;
        let v: serde_json::Value = r.json().map_err(http::net)?;
        let session = v["session_id"].as_str().ok_or_else(|| std::io::Error::other("sin sesión"))?.to_string();
        let mut offset = n as u64;
        loop {
            let n = super::read_up_to(&mut f, &mut buf)?;
            let cursor = serde_json::json!({"session_id": session, "offset": offset});
            if offset + n as u64 >= total {
                return conflict_ok(self.content("files/upload_session/finish", serde_json::json!({"cursor": cursor, "commit": commit}), buf[..n].to_vec()));
            }
            self.content("files/upload_session/append_v2", serde_json::json!({"cursor": cursor, "close": false}), buf[..n].to_vec())?;
            offset += n as u64;
        }
    }

    fn delete(&self, path: &str) -> std::io::Result<()> {
        match self.api("files/delete_v2", serde_json::json!({"path": self.p(path)})) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        self.api("files/move_v2", serde_json::json!({"from_path": self.p(from), "to_path": self.p(to), "autorename": false})).map(|_| ())
    }

    fn exists(&self, path: &str) -> std::io::Result<bool> {
        match self.api("files/get_metadata", serde_json::json!({"path": self.p(path)})) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_header_args() {
        assert_eq!(normalize_path(""), "/Home File Sync");
        assert_eq!(normalize_path("Familia/../Fotos/"), "/Familia/Fotos");
        assert_eq!(arg(&serde_json::json!({"path": "/Mamá"})), r#"{"path":"/Mam\u00e1"}"#);
    }
}
