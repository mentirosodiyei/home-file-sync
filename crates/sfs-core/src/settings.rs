//! Ajustes persistentes del equipo: identidad, carpeta, grupo y equipos conocidos.
//!
//! Se guardan en `settings.json` dentro del directorio de datos de la
//! aplicación, con permisos solo para el usuario (contiene la clave privada
//! del equipo y la clave del grupo).

use crate::auth::{device_id, random_bytes};
use crate::index::write_atomic;
use crate::transport::generate_keypair;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    /// Clave secreta del grupo (hex, 32 bytes).
    pub key: String,
}

impl Group {
    pub fn create(device_name: &str) -> Self {
        Group {
            id: hex::encode(random_bytes::<12>()),
            name: format!("Grupo de {device_name}"),
            key: hex::encode(random_bytes::<32>()),
        }
    }

    pub fn key_bytes(&self) -> [u8; 32] {
        let mut k = [0u8; 32];
        if let Ok(v) = hex::decode(&self.key) {
            if v.len() == 32 {
                k.copy_from_slice(&v);
            }
        }
        k
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub private_key: String,
    pub public_key: String,
    pub device_name: String,
    pub folder: Option<PathBuf>,
    pub group: Option<Group>,
    /// Equipos del grupo vistos alguna vez (id → nombre).
    #[serde(default)]
    pub known: BTreeMap<String, String>,
    /// Direcciones de conexión manual que el usuario pidió recordar ("anfitrión|puerto").
    #[serde(default)]
    pub manual_peers: Vec<String>,
    /// Direcciones por las que se ha alcanzado a cada miembro del grupo
    /// (id → "ip:puerto"). Permiten reconectar aunque el descubrimiento falle.
    #[serde(default)]
    pub peer_addrs: BTreeMap<String, Vec<String>>,
}

impl Settings {
    pub fn load_or_create(data_dir: &Path, name_override: Option<&str>) -> std::io::Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let file = data_dir.join("settings.json");
        let mut s = match std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Settings>(&b).ok()) {
            Some(s) => s,
            None => {
                let (private, public) = generate_keypair();
                let s = Settings {
                    private_key: hex::encode(private),
                    public_key: hex::encode(public),
                    device_name: default_device_name(),
                    folder: None,
                    group: None,
                    known: BTreeMap::new(),
                    manual_peers: Vec::new(),
                    peer_addrs: BTreeMap::new(),
                };
                s.save(data_dir)?;
                s
            }
        };
        if let Some(n) = name_override {
            s.device_name = n.to_string();
        }
        Ok(s)
    }

    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        write_atomic(&data_dir.join("settings.json"), &serde_json::to_vec_pretty(self)?)
    }

    pub fn device_id(&self) -> String {
        device_id(&hex::decode(&self.public_key).unwrap_or_default())
    }

    pub fn private_key_bytes(&self) -> Vec<u8> {
        hex::decode(&self.private_key).unwrap_or_default()
    }
}

/// Nombre legible del equipo ("MacBook Pro de Ana", "PC-SALA-2").
pub fn default_device_name() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("/usr/sbin/scutil").args(["--get", "ComputerName"]).output() {
            let n = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() && !n.is_empty() {
                return n;
            }
        }
    }
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(n) = std::env::var(var) {
            if !n.trim().is_empty() {
                return n.trim().to_string();
            }
        }
    }
    "Equipo".to_string()
}

/// Valida la carpeta elegida por el usuario. Rechaza carpetas peligrosas
/// (raíz, carpeta de usuario completa, carpetas del sistema).
pub fn check_folder(path: &Path) -> Result<PathBuf, String> {
    let canon = std::fs::canonicalize(path).map_err(|e| format!("No se puede abrir la carpeta: {e}"))?;
    // Windows devuelve rutas "\\?\C:\...": las simplificamos si es posible.
    let canon = match canon.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(s) if s.as_bytes().get(1) == Some(&b':') => PathBuf::from(s),
        _ => canon,
    };
    if !canon.is_dir() {
        return Err("La ruta elegida no es una carpeta.".into());
    }
    if canon.parent().is_none() {
        return Err("No se puede sincronizar la raíz del disco. Elige o crea una carpeta concreta.".into());
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    if let Some(home) = home.and_then(|h| std::fs::canonicalize(h).ok()) {
        if canon == home {
            return Err("No se puede sincronizar la carpeta de usuario completa. Elige o crea una carpeta concreta.".into());
        }
    }
    let lower = canon.to_string_lossy().to_lowercase().replace('\\', "/");
    // En Windows quitamos la unidad ("c:") y aplicamos la misma comprobación.
    let rest = if lower.len() > 2 && lower.as_bytes()[1] == b':' { &lower[2..] } else { lower.as_str() };
    let forbidden = [
        "/system", "/library", "/applications", "/usr", "/bin", "/sbin", "/etc", "/private/etc", "/windows",
        "/program files", "/program files (x86)", "/programdata",
    ];
    if forbidden.iter().any(|f| rest == *f || rest.starts_with(&format!("{f}/"))) {
        return Err("No se puede sincronizar una carpeta del sistema.".into());
    }
    let probe = canon.join(".showfilesync-probe");
    std::fs::write(&probe, b"ok").map_err(|e| format!("No se puede escribir en la carpeta: {e}"))?;
    let _ = std::fs::remove_file(&probe);
    Ok(canon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_checks() {
        assert!(check_folder(Path::new("/")).is_err());
        let t = tempfile::tempdir().unwrap();
        assert!(check_folder(t.path()).is_ok());
        assert!(check_folder(&t.path().join("no-existe")).is_err());
    }

    #[test]
    fn settings_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let s = Settings::load_or_create(t.path(), Some("Prueba")).unwrap();
        let s2 = Settings::load_or_create(t.path(), None).unwrap();
        assert_eq!(s.private_key, s2.private_key);
        assert_eq!(s.device_id().len(), 32);
    }
}
