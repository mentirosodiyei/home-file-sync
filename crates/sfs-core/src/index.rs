//! Índice local de archivos y su persistencia.

use crate::pathsafe::key_of;
use crate::vv::VersionVector;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Descripción de una versión de un archivo, tal como viaja por la red.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Ruta relativa con `/`, en NFC.
    pub path: String,
    pub size: u64,
    /// Fecha de modificación de origen (nanosegundos Unix).
    pub mtime_ns: i64,
    /// BLAKE3 del contenido, en hexadecimal.
    pub hash: String,
    pub vv: VersionVector,
    /// Borrado. Es una versión más del archivo y se propaga a los demás
    /// equipos (que lo mueven a su papelera interna); también sirve para no volver a
    /// descargar esa misma versión en el equipo donde se borró.
    pub deleted: bool,
    /// Nombre del equipo que hizo el último cambio (para nombrar conflictos).
    pub modified_by: String,
}

impl FileEntry {
    pub fn key(&self) -> String {
        key_of(&self.path)
    }
}

/// Entrada del índice local: la versión más lo que vemos en disco.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalEntry {
    pub entry: FileEntry,
    /// Ruta real en disco relativa a la carpeta (puede diferir en mayúsculas o NFD).
    pub disk: String,
    pub disk_size: u64,
    pub disk_mtime_ns: i64,
}

#[derive(Default, Serialize, Deserialize)]
pub struct LocalIndex {
    pub folder: PathBuf,
    pub entries: HashMap<String, LocalEntry>,
    #[serde(skip)]
    pub file: Option<PathBuf>,
    #[serde(skip)]
    pub dirty: bool,
}

impl LocalIndex {
    /// Carga el índice de `folder` desde `data_dir`, o crea uno vacío.
    pub fn load(data_dir: &Path, folder: &Path) -> Self {
        let id = blake3::hash(folder.to_string_lossy().as_bytes()).to_hex();
        let file = data_dir.join(format!("index-{}.json", &id[..16]));
        let mut idx = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice::<LocalIndex>(&b).ok())
            .filter(|i| i.folder == folder)
            .unwrap_or_else(|| LocalIndex { folder: folder.to_path_buf(), ..Default::default() });
        idx.file = Some(file);
        idx
    }

    pub fn get(&self, key: &str) -> Option<&LocalEntry> {
        self.entries.get(key)
    }

    pub fn put(&mut self, le: LocalEntry) {
        self.entries.insert(le.entry.key(), le);
        self.dirty = true;
    }

    /// Entradas presentes (no borradas), para anunciarlas a otros equipos.
    pub fn present(&self) -> Vec<FileEntry> {
        self.entries.values().filter(|e| !e.entry.deleted).map(|e| e.entry.clone()).collect()
    }

    /// Busca un archivo local presente con este contenido.
    pub fn find_by_hash(&self, hash: &str) -> Option<&LocalEntry> {
        self.entries.values().find(|e| !e.entry.deleted && e.entry.hash == hash)
    }

    pub fn total(&self) -> (usize, u64) {
        self.entries
            .values()
            .filter(|e| !e.entry.deleted)
            .fold((0, 0), |(n, b), e| (n + 1, b + e.entry.size))
    }

    /// Guarda el índice de forma atómica si ha cambiado.
    pub fn save(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if let Some(file) = &self.file {
            write_atomic(file, &serde_json::to_vec(self)?)?;
        }
        self.dirty = false;
        Ok(())
    }
}

/// Escribe un archivo pequeño mediante temporal + fsync + renombrado.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.new");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    restrict_permissions(&tmp);
    std::fs::rename(&tmp, path)
}

/// En Unix deja el archivo legible solo por el usuario (contiene claves).
pub fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}
