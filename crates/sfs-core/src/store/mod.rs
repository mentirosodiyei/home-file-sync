//! Dónde vive el relevo: una carpeta del equipo (la de la app de Google Drive,
//! Dropbox, OneDrive, Proton Drive, iCloud…), un NAS por SMB, un servidor
//! WebDAV (Nextcloud, kDrive…), o Google Drive / Dropbox con inicio de sesión.
//!
//! Todos ofrecen la misma interfaz sencilla de archivos ([`Store`]); el relevo
//! (`relay.rs`) no sabe cuál usa. Las rutas son relativas a la carpeta
//! «Home File Sync» del servicio y usan `/`.

pub mod dropbox;
pub mod gdrive;
pub(crate) mod http;
pub mod oauth;
pub mod smb;
pub mod webdav;

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Nombre de la carpeta que se crea en el servicio.
pub const ROOT_NAME: &str = "Home File Sync";

/// Dónde está el relevo de una carpeta compartida (sin contraseñas: esas van
/// al almacén del sistema, ver `secrets`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RelayTarget {
    /// Carpeta de este equipo (la que sincroniza la app del servicio, o un disco).
    #[serde(rename_all = "camelCase")]
    Folder { path: PathBuf },
    /// Carpeta compartida de un NAS por SMB, conectada con usuario y contraseña.
    #[serde(rename_all = "camelCase")]
    Smb { host: String, share: String, #[serde(default)] subpath: String, user: String },
    /// Servidor WebDAV (Nextcloud, ownCloud, kDrive, NAS…).
    #[serde(rename_all = "camelCase")]
    WebDav { url: String, user: String },
    /// Google Drive con inicio de sesión (permiso mínimo: solo lo que crea la app).
    #[serde(rename_all = "camelCase")]
    GoogleDrive { account: String },
    /// Dropbox con inicio de sesión. `path` es la carpeta del relevo en Dropbox
    /// (se puede compartir desde Dropbox con otras cuentas).
    #[serde(rename_all = "camelCase")]
    Dropbox { account: String, path: String },
}

impl RelayTarget {
    pub fn kind(&self) -> &'static str {
        match self {
            RelayTarget::Folder { .. } => "folder",
            RelayTarget::Smb { .. } => "smb",
            RelayTarget::WebDav { .. } => "webdav",
            RelayTarget::GoogleDrive { .. } => "gdrive",
            RelayTarget::Dropbox { .. } => "dropbox",
        }
    }

    /// Texto para mostrar dónde está (sin secretos).
    pub fn describe(&self) -> String {
        match self {
            RelayTarget::Folder { path } => path.display().to_string(),
            RelayTarget::Smb { host, share, subpath, user } => {
                let sub = if subpath.is_empty() { String::new() } else { format!("/{}", subpath.trim_matches('/')) };
                format!("smb://{user}@{host}/{share}{sub}")
            }
            RelayTarget::WebDav { url, user } => format!("{url} ({user})"),
            RelayTarget::GoogleDrive { account } => format!("Google Drive · {account}"),
            RelayTarget::Dropbox { account, path } => format!("Dropbox · {account} · {path}"),
        }
    }
}

/// Un archivo del servicio.
#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub size: u64,
    pub modified: SystemTime,
}

/// Operaciones de archivos que necesita el relevo. Bloqueantes (se llaman
/// desde hilos de bloqueo). `NotFound` se indica con `io::ErrorKind::NotFound`.
pub trait Store: Send + Sync {
    /// Crea la carpeta (y las intermedias) si no existe.
    fn mkdirs(&self, dir: &str) -> std::io::Result<()>;
    /// Archivos (no carpetas) de una carpeta. Carpeta inexistente: lista vacía.
    fn list(&self, dir: &str) -> std::io::Result<Vec<Item>>;
    /// Lee un archivo pequeño entero.
    fn read(&self, path: &str) -> std::io::Result<Vec<u8>> {
        let mut v = Vec::new();
        self.reader(path)?.read_to_end(&mut v)?;
        Ok(v)
    }
    /// Escribe (crea o sustituye) un archivo pequeño, de forma que nadie vea uno a medias.
    fn write(&self, path: &str, data: &[u8]) -> std::io::Result<()>;
    /// Abre un archivo para leerlo por partes.
    fn reader(&self, path: &str) -> std::io::Result<Box<dyn Read + Send>>;
    /// Sube un archivo nuevo desde un archivo local, sin que se vea a medias.
    fn upload(&self, path: &str, src: &Path) -> std::io::Result<()>;
    /// Borra (si no existe, no es un error).
    fn delete(&self, path: &str) -> std::io::Result<()>;
    /// Mueve o renombra (la carpeta de destino debe existir).
    fn rename(&self, from: &str, to: &str) -> std::io::Result<()>;
    fn exists(&self, path: &str) -> std::io::Result<bool>;
}

pub(crate) fn not_found() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotFound, "no existe")
}

pub(crate) fn split(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((d, n)) => (d, n),
        None => ("", path),
    }
}

/// Clave del secreto (contraseña o token) de un relevo en el almacén del sistema.
pub fn secret_key(group_id: &str, kind: &str) -> String {
    format!("relay/{group_id}/{kind}")
}

/// Abre el servicio de un relevo. Para NAS conecta la carpeta compartida si
/// hace falta; para los demás comprueba la sesión.
pub fn open(target: &RelayTarget, secret: Option<&str>) -> Result<Box<dyn Store>, String> {
    let need = || tr!("Falta la contraseña o la sesión del servicio externo. Vuelve a configurarlo.");
    match target {
        RelayTarget::Folder { path } => {
            if !path.is_dir() {
                return Err(tr!(
                    "No se encuentra la carpeta del servicio externo ({}). ¿Está conectado el disco o abierta la app de la nube?",
                    path.display()
                ));
            }
            Ok(Box::new(FolderStore::new(path.join(ROOT_NAME))))
        }
        RelayTarget::Smb { host, share, subpath, user } => {
            let base = smb::connect(host, share, user, secret.ok_or_else(need)?)?;
            let mut p = base;
            for c in subpath.split(['/', '\\']).filter(|c| !c.is_empty()) {
                p.push(c);
            }
            if !p.is_dir() {
                return Err(tr!("No se encuentra la carpeta {}", p.display()));
            }
            Ok(Box::new(FolderStore::new(p.join(ROOT_NAME))))
        }
        RelayTarget::WebDav { url, user } => Ok(Box::new(webdav::WebDav::new(url, user, secret.ok_or_else(need)?)?)),
        RelayTarget::GoogleDrive { .. } => Ok(Box::new(gdrive::Drive::new(secret.ok_or_else(need)?)?)),
        RelayTarget::Dropbox { path, .. } => Ok(Box::new(dropbox::Dropbox::new(secret.ok_or_else(need)?, path)?)),
    }
}

/// Comprueba que se puede escribir y borrar en el servicio.
pub fn probe(store: &dyn Store) -> Result<(), String> {
    let name = format!("prueba-{}.tmp", hex::encode(crate::auth::random_bytes::<4>()));
    let fail = |e: std::io::Error| tr!("No se puede escribir en el servicio externo: {}", e);
    store.mkdirs("").map_err(fail)?;
    store.write(&name, b"ok").map_err(fail)?;
    let back = store.read(&name).map_err(fail)?;
    store.delete(&name).map_err(fail)?;
    if back != b"ok" {
        return Err(tr!("No se puede escribir en el servicio externo: {}", "lectura distinta"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Carpeta del equipo (o de un NAS ya conectado)
// ---------------------------------------------------------------------------

pub struct FolderStore {
    root: PathBuf,
}

impl FolderStore {
    pub fn new(root: PathBuf) -> Self {
        FolderStore { root }
    }

    fn p(&self, rel: &str) -> PathBuf {
        let mut p = self.root.clone();
        for c in rel.split('/').filter(|c| !c.is_empty()) {
            p.push(c);
        }
        p
    }

    fn tmp_for(&self, rel: &str) -> PathBuf {
        let target = self.p(rel);
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        target.with_file_name(format!(".{name}.{}.tmp", hex::encode(crate::auth::random_bytes::<4>())))
    }
}

impl Store for FolderStore {
    fn mkdirs(&self, dir: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(self.p(dir))
    }

    fn list(&self, dir: &str) -> std::io::Result<Vec<Item>> {
        let rd = match std::fs::read_dir(self.p(dir)) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut v = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if let Ok(m) = e.metadata() {
                if m.is_file() {
                    v.push(Item { name, size: m.len(), modified: m.modified().unwrap_or(SystemTime::now()) });
                }
            }
        }
        Ok(v)
    }

    fn read(&self, path: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.p(path))
    }

    fn write(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        crate::index::write_atomic(&self.p(path), data)
    }

    fn reader(&self, path: &str) -> std::io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(std::fs::File::open(self.p(path))?))
    }

    fn upload(&self, path: &str, src: &Path) -> std::io::Result<()> {
        let tmp = self.tmp_for(path);
        let res = (|| {
            std::fs::copy(src, &tmp)?;
            std::fs::File::open(&tmp)?.sync_all()?;
            std::fs::rename(&tmp, self.p(path))
        })();
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        res
    }

    fn delete(&self, path: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.p(path)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }

    fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        std::fs::rename(self.p(from), self.p(to))
    }

    fn exists(&self, path: &str) -> std::io::Result<bool> {
        Ok(self.p(path).exists())
    }
}

/// Lee hasta llenar `buf` (o hasta el final). Devuelve los bytes leídos.
pub(crate) fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Codifica un componente de ruta para una URL.
pub(crate) fn pct(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub(crate) fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_store_basics() {
        let d = tempfile::tempdir().unwrap();
        let s = FolderStore::new(d.path().join(ROOT_NAME));
        probe(&s).unwrap();
        s.mkdirs("a/b").unwrap();
        s.write("a/b/x.hfs", b"uno").unwrap();
        s.write("a/b/x.hfs", b"dos").unwrap();
        assert_eq!(s.read("a/b/x.hfs").unwrap(), b"dos");
        let src = d.path().join("src");
        std::fs::write(&src, b"grande").unwrap();
        s.upload("a/b/y.hfs", &src).unwrap();
        let mut names: Vec<String> = s.list("a/b").unwrap().into_iter().map(|i| i.name).collect();
        names.sort();
        assert_eq!(names, vec!["x.hfs", "y.hfs"]);
        s.mkdirs("a/old").unwrap();
        s.rename("a/b/y.hfs", "a/old/y.hfs").unwrap();
        assert!(s.exists("a/old/y.hfs").unwrap() && !s.exists("a/b/y.hfs").unwrap());
        s.delete("a/b/x.hfs").unwrap();
        s.delete("a/b/x.hfs").unwrap();
        assert!(s.list("no/existe").unwrap().is_empty());
    }

    #[test]
    fn targets_serialize_without_secrets() {
        let t = RelayTarget::Smb { host: "192.168.1.5".into(), share: "Familia".into(), subpath: "".into(), user: "ana".into() };
        let j = serde_json::to_string(&t).unwrap();
        assert!(j.contains("\"kind\":\"smb\"") && !j.contains("password"));
        assert_eq!(serde_json::from_str::<RelayTarget>(&j).unwrap(), t);
        assert_eq!(t.describe(), "smb://ana@192.168.1.5/Familia");
        assert_eq!(pct_decode(&pct("Mamá y Papá/1")), "Mamá y Papá/1");
    }
}
