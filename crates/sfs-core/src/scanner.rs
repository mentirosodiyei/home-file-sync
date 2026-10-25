//! Escaneo de la carpeta local: detecta archivos nuevos, modificados y borrados.
//!
//! Para no copiar archivos que todavía se están escribiendo, un archivo nuevo
//! o modificado solo se indexa cuando su tamaño y fecha no cambian entre dos
//! observaciones separadas al menos `stable_ms`, y además:
//! * en Windows, se puede abrir sin que otro proceso lo tenga abierto para escritura;
//! * en macOS, no tiene la marca de "copia en curso" del Finder.
//!
//! El hash (BLAKE3) se calcula sin bloquear el índice y se comprueba que el
//! archivo no cambió mientras se calculaba.

use crate::index::{FileEntry, LocalEntry, LocalIndex};
use crate::pathsafe::{from_local, is_ignored_name, key_of, nfc, META_DIR};
use crate::vv::VersionVector;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct Me {
    pub num: u64,
    pub name: String,
}

#[derive(Default, Debug)]
pub struct ScanOutcome {
    /// Entradas nuevas o modificadas (para anunciar a los demás).
    pub changed: Vec<FileEntry>,
    /// Hay archivos aún inestables: conviene volver a escanear pronto.
    pub unstable: bool,
    /// Archivos que no se han podido leer (ruta, motivo).
    pub errors: Vec<(String, String)>,
    pub tombstoned: usize,
    /// Marcas de borrado nuevas: se anuncian para que nadie pida el archivo a
    /// este equipo (los demás nunca borran por ello).
    pub tombstones: Vec<FileEntry>,
}

#[derive(Debug)]
struct DiskFile {
    key: String,
    rel: String,
    size: u64,
    mtime_ns: i64,
    busy: bool,
}

pub struct Scanner {
    root: PathBuf,
    stable: Duration,
    observed: HashMap<String, (u64, i64, Instant)>,
}

pub fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos().min(i64::MAX as u128) as i64,
            Err(e) => -(e.duration().as_nanos().min(i64::MAX as u128) as i64),
        },
        Err(_) => 0,
    }
}

/// Copia en curso del Finder: fecha de creación 24-01-1984 08:00 UTC.
fn finder_busy(meta: &std::fs::Metadata) -> bool {
    cfg!(target_os = "macos")
        && meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() == 443_779_200)
            .unwrap_or(false)
}

/// Abre un archivo para calcular su hash. En Windows falla si otro proceso
/// lo tiene abierto para escritura (copia en curso, guardado a medias).
fn open_for_hash(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path)
    }
    #[cfg(not(windows))]
    std::fs::File::open(path)
}

/// Hash BLAKE3 en hexadecimal de todo el archivo.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    hash_reader(open_for_hash(path)?)
}

pub fn hash_reader(mut f: impl Read) -> std::io::Result<String> {
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex().to_string())
}

impl Scanner {
    pub fn new(root: PathBuf, stable: Duration) -> Self {
        Scanner { root, stable, observed: HashMap::new() }
    }

    fn walk(&self) -> (Vec<DiskFile>, bool) {
        let mut out = Vec::new();
        let mut complete = true;
        let it = walkdir::WalkDir::new(&self.root).follow_links(false).into_iter().filter_entry(|e| {
            e.depth() == 0 || e.file_name().to_str().map(|n| !is_ignored_name(n) && n != META_DIR).unwrap_or(false)
        });
        for item in it {
            let item = match item {
                Ok(i) => i,
                Err(_) => {
                    complete = false;
                    continue;
                }
            };
            if !item.file_type().is_file() {
                continue;
            }
            let Some(rel) = from_local(&self.root, item.path()) else { continue };
            let Ok(meta) = item.metadata() else {
                complete = false;
                continue;
            };
            out.push(DiskFile {
                key: key_of(&rel),
                size: meta.len(),
                mtime_ns: mtime_ns(&meta),
                busy: finder_busy(&meta),
                rel,
            });
        }
        (out, complete)
    }

    /// Escanea la carpeta y actualiza el índice.
    pub fn scan(&mut self, index: &Mutex<LocalIndex>, me: &Me) -> ScanOutcome {
        let mut out = ScanOutcome::default();
        if !self.root.is_dir() {
            out.errors.push((String::new(), "La carpeta sincronizada no está disponible".into()));
            return out;
        }
        let (files, complete) = self.walk();

        // Fase 1: qué ha cambiado respecto al índice (bloqueo breve).
        let mut candidates = Vec::new();
        {
            let mut idx = index.lock().unwrap();
            let mut seen = HashSet::new();
            for f in files {
                if !seen.insert(f.key.clone()) {
                    out.errors.push((f.rel.clone(), "existe otro archivo con el mismo nombre (solo cambian mayúsculas)".into()));
                    continue;
                }
                let unchanged = idx.get(&f.key).is_some_and(|le| {
                    !le.entry.deleted && le.disk == f.rel && le.disk_size == f.size && le.disk_mtime_ns == f.mtime_ns
                });
                if !unchanged {
                    candidates.push(f);
                }
            }
            if complete {
                let gone: Vec<String> = idx
                    .entries
                    .iter()
                    .filter(|(k, e)| !e.entry.deleted && !seen.contains(*k))
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in gone {
                    if let Some(le) = idx.entries.get_mut(&k) {
                        le.entry.deleted = true;
                        out.tombstoned += 1;
                        out.tombstones.push(le.entry.clone());
                    }
                }
                if out.tombstoned > 0 {
                    idx.dirty = true;
                }
            }
        }
        let live: HashSet<&String> = candidates.iter().map(|c| &c.key).collect();
        self.observed.retain(|k, _| live.contains(k));

        // Fase 2: estabilidad y hash (sin bloquear el índice).
        let now = Instant::now();
        let mut hashed = Vec::new();
        for f in candidates {
            let stable = match self.observed.get(&f.key) {
                Some((s, m, t)) => *s == f.size && *m == f.mtime_ns && now.duration_since(*t) >= self.stable,
                None => false,
            };
            if !stable || f.busy {
                if self.observed.get(&f.key).map(|(s, m, _)| (*s, *m)) != Some((f.size, f.mtime_ns)) {
                    self.observed.insert(f.key.clone(), (f.size, f.mtime_ns, now));
                }
                out.unstable = true;
                continue;
            }
            let path = crate::pathsafe::to_local(&self.root, &f.rel);
            match hash_file(&path) {
                Ok(hash) => {
                    let after = std::fs::metadata(&path).ok();
                    if after.as_ref().map(|m| (m.len(), mtime_ns(m))) != Some((f.size, f.mtime_ns)) {
                        self.observed.remove(&f.key);
                        out.unstable = true;
                        continue;
                    }
                    self.observed.remove(&f.key);
                    hashed.push((f, hash));
                }
                Err(e) => {
                    out.unstable = true;
                    out.errors.push((f.rel.clone(), format!("no se puede leer: {e}")));
                }
            }
        }

        // Fase 3: aplicar al índice.
        let mut idx = index.lock().unwrap();
        for (f, hash) in hashed {
            let existing = idx.get(&f.key).cloned();
            if let Some(le) = &existing {
                if !le.entry.deleted && le.disk == f.rel && le.disk_size == f.size && le.disk_mtime_ns == f.mtime_ns {
                    continue; // ya actualizado por otra vía (p. ej. una descarga)
                }
            }
            let entry = match existing {
                Some(le) if le.entry.hash == hash => {
                    let mut e = le.entry;
                    let was_deleted = e.deleted;
                    e.deleted = false;
                    if was_deleted {
                        out.changed.push(e.clone());
                    }
                    e
                }
                other => {
                    let mut vv = other.map(|le| le.entry.vv).unwrap_or_else(VersionVector::new);
                    vv.bump(me.num);
                    let e = FileEntry {
                        path: nfc(&f.rel),
                        size: f.size,
                        mtime_ns: f.mtime_ns,
                        hash,
                        vv,
                        deleted: false,
                        modified_by: me.name.clone(),
                    };
                    out.changed.push(e.clone());
                    e
                }
            };
            idx.put(LocalEntry { entry, disk: f.rel, disk_size: f.size, disk_mtime_ns: f.mtime_ns });
        }
        out
    }
}

/// Instante actual en nanosegundos Unix.
pub fn now_ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Me {
        Me { num: 1, name: "A".into() }
    }

    #[test]
    fn detects_new_modified_deleted_and_waits_for_stability() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        std::fs::create_dir_all(root.join("sub dir")).unwrap();
        std::fs::write(root.join("sub dir/Ñandú.pptx"), b"hola").unwrap();
        std::fs::write(root.join(".DS_Store"), b"x").unwrap();
        std::fs::write(root.join("~$lock.pptx"), b"x").unwrap();
        let idx = Mutex::new(LocalIndex::default());
        let mut s = Scanner::new(root.clone(), Duration::from_millis(0));

        // Primera pasada: solo observa.
        let o = s.scan(&idx, &me());
        assert!(o.unstable && o.changed.is_empty());
        // Segunda: estable, se indexa.
        let o = s.scan(&idx, &me());
        assert_eq!(o.changed.len(), 1);
        assert_eq!(o.changed[0].path, "sub dir/Ñandú.pptx");
        let v1 = o.changed[0].vv.clone();

        // Sin cambios: nada.
        let o = s.scan(&idx, &me());
        assert!(o.changed.is_empty() && !o.unstable);

        // Modificación: nueva versión con el contador incrementado.
        std::fs::write(root.join("sub dir/Ñandú.pptx"), b"hola mundo").unwrap();
        s.scan(&idx, &me());
        let o = s.scan(&idx, &me());
        assert_eq!(o.changed.len(), 1);
        assert_eq!(o.changed[0].vv.compare(&v1), crate::vv::Causality::Newer);

        // Borrado: queda como marca local, no se anuncia.
        std::fs::remove_file(root.join("sub dir/Ñandú.pptx")).unwrap();
        let o = s.scan(&idx, &me());
        assert_eq!(o.tombstoned, 1);
        assert!(o.changed.is_empty());
        assert!(idx.lock().unwrap().present().is_empty());
    }

    #[test]
    fn growing_file_is_not_indexed() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        let idx = Mutex::new(LocalIndex::default());
        let mut s = Scanner::new(root.clone(), Duration::from_millis(0));
        for i in 0..4 {
            std::fs::write(root.join("video.mp4"), vec![0u8; 1000 * (i + 1)]).unwrap();
            let o = s.scan(&idx, &me());
            assert!(o.changed.is_empty(), "no debe indexar un archivo que crece");
        }
    }
}
