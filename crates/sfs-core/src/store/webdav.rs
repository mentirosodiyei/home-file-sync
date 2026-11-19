//! Servidor WebDAV: Nextcloud, ownCloud, Infomaniak kDrive, pCloud, Synology,
//! QNAP… Usuario y contraseña (en Nextcloud, mejor una «contraseña de
//! aplicación»). Se exige HTTPS salvo en la red local.

use super::{http, pct, pct_decode, split, Item, Store, ROOT_NAME};
use reqwest::blocking::{Body, Client};
use reqwest::Method;
use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

pub struct WebDav {
    client: Client,
    /// URL de la carpeta «Home File Sync», terminada en `/`.
    root: String,
    user: String,
    pass: String,
}

/// ¿La dirección es de la red local (donde se admite http://)?
fn is_local_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    if h == "localhost" || h.ends_with(".local") || h.ends_with(".lan") || h.ends_with(".home.arpa") {
        return true;
    }
    match h.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v)) => v.is_private() || v.is_loopback() || v.is_link_local(),
        Ok(std::net::IpAddr::V6(v)) => v.is_loopback() || (v.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// Normaliza la dirección escrita por el usuario. Devuelve la URL base con `/` final.
pub fn normalize_url(url: &str) -> Result<String, String> {
    let u = url.trim();
    let u = if u.contains("://") { u.to_string() } else { format!("https://{u}") };
    let (scheme, rest) = u.split_once("://").unwrap();
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_only = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    let host_only = if host_only.starts_with('[') { host_only.split(']').next().unwrap_or("").to_string() + "]" } else { host_only.split(':').next().unwrap_or("").to_string() };
    if host_only.is_empty() {
        return Err(tr!("La dirección del servidor no es válida."));
    }
    match scheme.to_ascii_lowercase().as_str() {
        "https" => {}
        "http" if is_local_host(&host_only) => {}
        "http" => return Err(tr!("Usa una dirección https:// (sin cifrar, la contraseña viajaría a la vista).")),
        _ => return Err(tr!("La dirección del servidor no es válida.")),
    }
    Ok(if u.ends_with('/') { u } else { format!("{u}/") })
}

impl WebDav {
    pub fn new(url: &str, user: &str, pass: &str) -> Result<Self, String> {
        let base = normalize_url(url)?;
        Ok(WebDav {
            client: http::client().map_err(|e| e.to_string())?,
            root: format!("{base}{}/", pct(ROOT_NAME)),
            user: user.to_string(),
            pass: pass.to_string(),
        })
    }

    fn url(&self, rel: &str) -> String {
        let enc: Vec<String> = rel.split('/').filter(|c| !c.is_empty()).map(pct).collect();
        format!("{}{}", self.root, enc.join("/"))
    }

    fn req(&self, m: Method, url: &str) -> reqwest::blocking::RequestBuilder {
        self.client.request(m, url).basic_auth(&self.user, Some(&self.pass))
    }

    fn send(&self, rb: reqwest::blocking::RequestBuilder) -> std::io::Result<reqwest::blocking::Response> {
        rb.send().map_err(http::net)
    }

    fn mkcol(&self, url: &str) -> std::io::Result<()> {
        let r = self.send(self.req(Method::from_bytes(b"MKCOL").unwrap(), &format!("{url}/")))?;
        match r.status().as_u16() {
            200..=299 | 405 => Ok(()), // 405: ya existe
            _ => Err(http::status(r)),
        }
    }

    fn put_file(&self, url: &str, src: &Path) -> std::io::Result<()> {
        let f = std::fs::File::open(src)?;
        let len = f.metadata()?.len();
        let r = self.send(self.req(Method::PUT, url).header("Content-Type", "application/octet-stream").body(Body::sized(f, len)))?;
        if r.status().is_success() {
            Ok(())
        } else {
            Err(http::status(r))
        }
    }

    fn mv(&self, from: &str, to: &str, overwrite: bool) -> std::io::Result<()> {
        let r = self.send(
            self.req(Method::from_bytes(b"MOVE").unwrap(), from)
                .header("Destination", to)
                .header("Overwrite", if overwrite { "T" } else { "F" }),
        )?;
        if r.status().is_success() {
            Ok(())
        } else {
            Err(http::status(r))
        }
    }

    fn tmp_url(&self, rel: &str) -> String {
        let (dir, name) = split(rel);
        self.url(&format!("{dir}/.{name}.{}.tmp", hex::encode(crate::auth::random_bytes::<4>())))
    }
}

impl Store for WebDav {
    fn mkdirs(&self, dir: &str) -> std::io::Result<()> {
        self.mkcol(self.root.trim_end_matches('/'))?;
        let mut acc = String::new();
        for c in dir.split('/').filter(|c| !c.is_empty()) {
            acc = if acc.is_empty() { c.to_string() } else { format!("{acc}/{c}") };
            self.mkcol(&self.url(&acc))?;
        }
        Ok(())
    }

    fn list(&self, dir: &str) -> std::io::Result<Vec<Item>> {
        let url = format!("{}/", self.url(dir).trim_end_matches('/'));
        let body = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;
        let r = self.send(
            self.req(Method::from_bytes(b"PROPFIND").unwrap(), &url).header("Depth", "1").header("Content-Type", "application/xml").body(body),
        )?;
        if r.status().as_u16() == 404 {
            return Ok(Vec::new());
        }
        if !r.status().is_success() {
            return Err(http::status(r));
        }
        let xml = r.text().map_err(http::net)?;
        Ok(parse_propfind(&xml))
    }

    fn write(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        let tmp = self.tmp_url(path);
        let r = self.send(self.req(Method::PUT, &tmp).header("Content-Type", "application/octet-stream").body(data.to_vec()))?;
        if !r.status().is_success() {
            return Err(http::status(r));
        }
        let res = self.mv(&tmp, &self.url(path), true);
        if res.is_err() {
            let _ = self.send(self.req(Method::DELETE, &tmp));
        }
        res
    }

    fn reader(&self, path: &str) -> std::io::Result<Box<dyn Read + Send>> {
        let r = self.send(self.req(Method::GET, &self.url(path)))?;
        if !r.status().is_success() {
            return Err(http::status(r));
        }
        Ok(Box::new(r))
    }

    fn upload(&self, path: &str, src: &Path) -> std::io::Result<()> {
        let tmp = self.tmp_url(path);
        self.put_file(&tmp, src)?;
        let res = self.mv(&tmp, &self.url(path), false);
        if res.is_err() {
            let _ = self.send(self.req(Method::DELETE, &tmp));
        }
        res
    }

    fn delete(&self, path: &str) -> std::io::Result<()> {
        let r = self.send(self.req(Method::DELETE, &self.url(path)))?;
        match r.status().as_u16() {
            200..=299 | 404 => Ok(()),
            _ => Err(http::status(r)),
        }
    }

    fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        self.mv(&self.url(from), &self.url(to), false)
    }

    fn exists(&self, path: &str) -> std::io::Result<bool> {
        let r = self.send(self.req(Method::HEAD, &self.url(path)))?;
        match r.status().as_u16() {
            200..=299 => Ok(true),
            404 => Ok(false),
            _ => Err(http::status(r)),
        }
    }
}

/// Extrae los archivos (no carpetas) de una respuesta PROPFIND.
fn parse_propfind(xml: &str) -> Vec<Item> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut items = Vec::new();
    let (mut href, mut size, mut modified, mut is_dir) = (String::new(), 0u64, SystemTime::now(), false);
    let mut field = "";
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(e.name().as_ref()) == "response" => {
                href.clear();
                size = 0;
                modified = SystemTime::now();
                is_dir = false;
            }
            Ok(Event::Start(e)) => {
                field = match local(e.name().as_ref()) {
                    "href" => "href",
                    "getcontentlength" => "len",
                    "getlastmodified" => "mod",
                    "collection" => {
                        is_dir = true;
                        ""
                    }
                    _ => "",
                };
            }
            Ok(Event::Empty(e)) => {
                if local(e.name().as_ref()) == "collection" {
                    is_dir = true;
                }
            }
            Ok(Event::Text(t)) => {
                let v = t.unescape().map(|c| c.into_owned()).unwrap_or_default();
                match field {
                    "href" => href.push_str(&v),
                    "len" => size = v.trim().parse().unwrap_or(0),
                    "mod" => {
                        if let Ok(d) = chrono::DateTime::parse_from_rfc2822(v.trim()) {
                            modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(d.timestamp().max(0) as u64);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                if local(e.name().as_ref()) == "response" && !is_dir && !href.is_empty() {
                    let name = pct_decode(href.trim_end_matches('/').rsplit('/').next().unwrap_or(""));
                    if !name.is_empty() && !name.starts_with('.') {
                        items.push(Item { name, size, modified });
                    }
                }
                field = "";
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    items
}

fn local(name: &[u8]) -> &str {
    let s = std::str::from_utf8(name).unwrap_or("");
    s.rsplit(':').next().unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_checked() {
        assert_eq!(normalize_url("nube.ejemplo.com/remote.php/dav/files/ana").unwrap(), "https://nube.ejemplo.com/remote.php/dav/files/ana/");
        assert!(normalize_url("http://nube.ejemplo.com/dav").is_err(), "http fuera de casa: no");
        assert!(normalize_url("http://192.168.1.10:5005/").is_ok());
        assert!(normalize_url("http://nas.local/webdav").is_ok());
        assert!(normalize_url("ftp://x").is_err());
    }

    #[test]
    fn parses_nextcloud_propfind() {
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">
<d:response><d:href>/remote.php/dav/files/ana/Home%20File%20Sync/hfs-1/blobs/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/ana/Home%20File%20Sync/hfs-1/blobs/abc.hfs</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>1234</d:getcontentlength><d:getlastmodified>Mon, 16 Nov 2026 10:00:00 GMT</d:getlastmodified></d:prop></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/ana/Home%20File%20Sync/hfs-1/blobs/.abc.hfs.1a2b.tmp</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>5</d:getcontentlength></d:prop></d:propstat></d:response>
</d:multistatus>"#;
        let items = parse_propfind(xml);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "abc.hfs");
        assert_eq!(items[0].size, 1234);
    }
}
