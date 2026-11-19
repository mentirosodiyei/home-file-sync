//! Google Drive con inicio de sesión. Permiso mínimo (`drive.file`): la app
//! solo ve los archivos que crea ella, en la carpeta «Home File Sync» de «Mi
//! unidad». Todos los equipos de la carpeta compartida deben iniciar sesión
//! con la misma cuenta de Google (con cuentas distintas, la app de escritorio
//! de Google Drive y una carpeta compartida desde Drive).

use super::oauth::{AccessToken, Provider};
use super::{http, not_found, split, Item, Store, ROOT_NAME};
use reqwest::blocking::{Client, RequestBuilder, Response};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

const API: &str = "https://www.googleapis.com/drive/v3";
const UP: &str = "https://www.googleapis.com/upload/drive/v3";
const FOLDER: &str = "application/vnd.google-apps.folder";
const CHUNK: usize = 8 * 1024 * 1024; // múltiplo de 256 KiB

pub struct Drive {
    client: Client,
    token: AccessToken,
    /// Carpeta relativa → id de Drive ("" = «Home File Sync»).
    dirs: Mutex<HashMap<String, String>>,
}

struct Found {
    id: String,
    size: u64,
    modified: SystemTime,
}

/// Bytes que Google dice haber recibido (cabecera `Range: bytes=0-N`).
fn received(r: &Response) -> u64 {
    r.headers().get("range").and_then(|v| v.to_str().ok()).and_then(|v| v.rsplit('-').next()).and_then(|v| v.parse::<u64>().ok()).map(|e| e + 1).unwrap_or(0)
}

fn q_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn parse_time(s: &str) -> SystemTime {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| SystemTime::UNIX_EPOCH + Duration::from_secs(d.timestamp().max(0) as u64))
        .unwrap_or_else(|_| SystemTime::now())
}

impl Drive {
    pub fn new(refresh_token: &str) -> Result<Self, String> {
        Ok(Drive { client: http::client().map_err(|e| e.to_string())?, token: AccessToken::new(Provider::Google, refresh_token), dirs: Mutex::new(HashMap::new()) })
    }

    /// Envía una petición con el token; renueva el token si caduca y reintenta
    /// si el servicio pide esperar.
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

    fn ok(&self, r: Response) -> std::io::Result<Response> {
        if r.status().is_success() {
            Ok(r)
        } else {
            Err(http::status(r))
        }
    }

    fn children(&self, parent: &str, name: Option<&str>, folders: bool) -> std::io::Result<Vec<(String, Found)>> {
        let mut q = format!("'{}' in parents and trashed = false and mimeType {} '{FOLDER}'", q_str(parent), if folders { "=" } else { "!=" });
        if let Some(n) = name {
            q.push_str(&format!(" and name = '{}'", q_str(n)));
        }
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut params = vec![
                ("q", q.clone()),
                ("fields", "nextPageToken,files(id,name,size,modifiedTime)".to_string()),
                ("pageSize", "1000".to_string()),
                ("spaces", "drive".to_string()),
            ];
            if let Some(p) = &page {
                params.push(("pageToken", p.clone()));
            }
            let r = self.ok(self.call(|c| c.get(format!("{API}/files")).query(&params))?)?;
            let v: serde_json::Value = r.json().map_err(http::net)?;
            for f in v["files"].as_array().into_iter().flatten() {
                out.push((
                    f["name"].as_str().unwrap_or("").to_string(),
                    Found {
                        id: f["id"].as_str().unwrap_or("").to_string(),
                        size: f["size"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
                        modified: parse_time(f["modifiedTime"].as_str().unwrap_or("")),
                    },
                ));
            }
            page = v["nextPageToken"].as_str().map(str::to_string);
            if page.is_none() {
                return Ok(out);
            }
        }
    }

    fn create_folder(&self, parent: &str, name: &str) -> std::io::Result<String> {
        let meta = serde_json::json!({"name": name, "mimeType": FOLDER, "parents": [parent]});
        let r = self.ok(self.call(|c| c.post(format!("{API}/files")).query(&[("fields", "id")]).json(&meta))?)?;
        let v: serde_json::Value = r.json().map_err(http::net)?;
        v["id"].as_str().map(str::to_string).ok_or_else(|| std::io::Error::other("sin id"))
    }

    /// Id de una carpeta relativa (creándola si `create`). `None` si no existe.
    fn dir_id(&self, dir: &str, create: bool) -> std::io::Result<Option<String>> {
        let dir = dir.trim_matches('/');
        if let Some(id) = self.dirs.lock().unwrap().get(dir) {
            return Ok(Some(id.clone()));
        }
        // Raíz: «Home File Sync» en «Mi unidad».
        let mut cur = match self.dirs.lock().unwrap().get("").cloned() {
            Some(id) => id,
            None => {
                let found = self.children("root", Some(ROOT_NAME), true)?;
                let id = match found.into_iter().next() {
                    Some((_, f)) => f.id,
                    None if create => self.create_folder("root", ROOT_NAME)?,
                    None => return Ok(None),
                };
                self.dirs.lock().unwrap().insert(String::new(), id.clone());
                id
            }
        };
        let mut acc = String::new();
        for c in dir.split('/').filter(|c| !c.is_empty()) {
            acc = if acc.is_empty() { c.to_string() } else { format!("{acc}/{c}") };
            if let Some(id) = self.dirs.lock().unwrap().get(&acc).cloned() {
                cur = id;
                continue;
            }
            let found = self.children(&cur, Some(c), true)?;
            cur = match found.into_iter().next() {
                Some((_, f)) => f.id,
                None if create => self.create_folder(&cur, c)?,
                None => return Ok(None),
            };
            self.dirs.lock().unwrap().insert(acc.clone(), cur.clone());
        }
        Ok(Some(cur))
    }

    /// Ids de los archivos con ese nombre (Drive admite nombres repetidos).
    fn file_ids(&self, path: &str) -> std::io::Result<(Option<String>, Vec<String>)> {
        let (dir, name) = split(path);
        let Some(d) = self.dir_id(dir, false)? else { return Ok((None, Vec::new())) };
        let ids = self.children(&d, Some(name), false)?.into_iter().map(|(_, f)| f.id).collect();
        Ok((Some(d), ids))
    }

    fn multipart(meta: &serde_json::Value, data: &[u8]) -> (String, Vec<u8>) {
        let boundary = format!("hfs{}", hex::encode(crate::auth::random_bytes::<8>()));
        let mut body = Vec::with_capacity(data.len() + 512);
        body.extend_from_slice(format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
        (format!("multipart/related; boundary={boundary}"), body)
    }
}

impl Store for Drive {
    fn mkdirs(&self, dir: &str) -> std::io::Result<()> {
        self.dir_id(dir, true).map(|_| ())
    }

    fn list(&self, dir: &str) -> std::io::Result<Vec<Item>> {
        let Some(d) = self.dir_id(dir, false)? else { return Ok(Vec::new()) };
        let mut seen = std::collections::HashSet::new();
        Ok(self
            .children(&d, None, false)?
            .into_iter()
            .filter(|(n, _)| !n.starts_with('.') && seen.insert(n.clone()))
            .map(|(name, f)| Item { name, size: f.size, modified: f.modified })
            .collect())
    }

    fn write(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        let (dir, name) = split(path);
        let d = self.dir_id(dir, true)?.ok_or_else(not_found)?;
        let ids = self.children(&d, Some(name), false)?.into_iter().map(|(_, f)| f.id).collect::<Vec<_>>();
        match ids.split_first() {
            Some((first, rest)) => {
                self.ok(self.call(|c| c.patch(format!("{UP}/files/{first}")).query(&[("uploadType", "media")]).header("Content-Type", "application/octet-stream").body(data.to_vec()))?)?;
                for id in rest {
                    let _ = self.call(|c| c.delete(format!("{API}/files/{id}")));
                }
            }
            None => {
                let (ctype, body) = Self::multipart(&serde_json::json!({"name": name, "parents": [d]}), data);
                self.ok(self.call(|c| c.post(format!("{UP}/files")).query(&[("uploadType", "multipart"), ("fields", "id")]).header("Content-Type", ctype.clone()).body(body.clone()))?)?;
            }
        }
        Ok(())
    }

    fn reader(&self, path: &str) -> std::io::Result<Box<dyn Read + Send>> {
        let (_, ids) = self.file_ids(path)?;
        let id = ids.first().ok_or_else(not_found)?.clone();
        let r = self.ok(self.call(|c| c.get(format!("{API}/files/{id}")).query(&[("alt", "media")]))?)?;
        Ok(Box::new(r))
    }

    fn upload(&self, path: &str, src: &Path) -> std::io::Result<()> {
        let (dir, name) = split(path);
        let d = self.dir_id(dir, true)?.ok_or_else(not_found)?;
        let total = std::fs::metadata(src)?.len();
        // Sesión de subida reanudable: el archivo no aparece hasta que se completa.
        let meta = serde_json::json!({"name": name, "parents": [d]});
        let r = self.ok(self.call(|c| {
            c.post(format!("{UP}/files")).query(&[("uploadType", "resumable")]).header("X-Upload-Content-Type", "application/octet-stream").header("X-Upload-Content-Length", total).json(&meta)
        })?)?;
        let session = r.headers().get("location").and_then(|v| v.to_str().ok()).ok_or_else(|| std::io::Error::other("sin sesión de subida"))?.to_string();
        let mut f = std::fs::File::open(src)?;
        let mut offset = 0u64;
        let mut buf = vec![0u8; CHUNK];
        let mut failures = 0;
        loop {
            use std::io::Seek;
            f.seek(std::io::SeekFrom::Start(offset))?;
            let n = super::read_up_to(&mut f, &mut buf)?;
            let range = if total == 0 { "bytes */0".to_string() } else { format!("bytes {}-{}/{}", offset, offset + n as u64 - 1, total) };
            let chunk = buf[..n].to_vec();
            let res = self.client.put(&session).header("Content-Range", range).body(chunk).send();
            match res {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) if r.status().as_u16() == 308 => {
                    // Lo que ha recibido Google: «bytes=0-N».
                    offset = received(&r);
                    failures = 0;
                }
                Ok(r) if r.status().as_u16() >= 500 && failures < 5 => {
                    failures += 1;
                    std::thread::sleep(Duration::from_secs(2 * failures));
                    // Pregunta cuánto se ha recibido y sigue desde ahí.
                    if let Ok(s) = self.client.put(&session).header("Content-Range", format!("bytes */{total}")).body(Vec::new()).send() {
                        if s.status().is_success() {
                            return Ok(());
                        }
                        offset = received(&s);
                    }
                }
                Ok(r) => return Err(http::status(r)),
                Err(e) if failures < 5 => {
                    failures += 1;
                    tracing::debug!("Google Drive: corte en la subida ({e}); se reanuda");
                    std::thread::sleep(Duration::from_secs(2 * failures));
                    if let Ok(s) = self.client.put(&session).header("Content-Range", format!("bytes */{total}")).body(Vec::new()).send() {
                        if s.status().is_success() {
                            return Ok(());
                        }
                        offset = received(&s);
                    }
                }
                Err(e) => return Err(http::net(e)),
            }
        }
    }

    fn delete(&self, path: &str) -> std::io::Result<()> {
        let (_, ids) = self.file_ids(path)?;
        for id in ids {
            let r = self.call(|c| c.delete(format!("{API}/files/{id}")))?;
            if !r.status().is_success() && r.status().as_u16() != 404 {
                return Err(http::status(r));
            }
        }
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        let (old_parent, ids) = self.file_ids(from)?;
        let id = ids.first().ok_or_else(not_found)?.clone();
        let (to_dir, to_name) = split(to);
        let new_parent = self.dir_id(to_dir, true)?.ok_or_else(not_found)?;
        let old_parent = old_parent.unwrap_or_default();
        let meta = serde_json::json!({"name": to_name});
        self.ok(self.call(|c| c.patch(format!("{API}/files/{id}")).query(&[("addParents", new_parent.as_str()), ("removeParents", old_parent.as_str())]).json(&meta))?)?;
        Ok(())
    }

    fn exists(&self, path: &str) -> std::io::Result<bool> {
        Ok(!self.file_ids(path)?.1.is_empty())
    }
}
