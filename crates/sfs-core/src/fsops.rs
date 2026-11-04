//! Operaciones de disco seguras: temporales, sustitución atómica, copias de
//! versiones anteriores y limpieza.
//!
//! Estructura interna dentro de la carpeta sincronizada:
//! ```text
//! .showfilesync/
//!   tmp/<hash>.sfspart        descargas en curso (reanudables)
//!   versiones/<fecha>/<ruta>  versiones sustituidas por una actualización
//! ```
//! Todo vive en el mismo volumen que los archivos, así que el paso final es
//! un `rename`, atómico en APFS, HFS+ y NTFS.

use crate::pathsafe::{check_no_symlinks, to_local, META_DIR};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn meta_dir(root: &Path) -> PathBuf {
    root.join(META_DIR)
}

/// Crea la carpeta interna. Devuelve `false` si no existía.
pub fn ensure_meta(root: &Path) -> std::io::Result<bool> {
    let meta = meta_dir(root);
    let existed = meta.is_dir();
    std::fs::create_dir_all(meta.join("tmp"))?;
    std::fs::create_dir_all(meta.join("versiones"))?;
    if !existed {
        let _ = std::fs::write(
            meta.join("LEEME.txt"),
            "Carpeta interna de Show File Sync.\n\
             tmp/        descargas en curso\n\
             versiones/  copias de archivos sustituidos por versiones nuevas (se borran a los 14 días)\n\
             No la borres mientras la aplicación está en uso.\n",
        );
        set_hidden(&meta);
    }
    Ok(existed)
}

#[cfg(windows)]
fn set_hidden(p: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN};
    let w: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        SetFileAttributesW(w.as_ptr(), FILE_ATTRIBUTE_HIDDEN);
    }
}

#[cfg(not(windows))]
fn set_hidden(_p: &Path) {}

pub fn temp_path(root: &Path, hash: &str) -> PathBuf {
    meta_dir(root).join("tmp").join(format!("{hash}.sfspart"))
}

/// Fecha UTC compacta para nombres de carpeta: `2026-10-25_183005Z`.
pub fn stamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Algoritmo civil_from_days (H. Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}_{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Mueve el archivo actual a `versiones/` antes de sustituirlo.
pub fn backup_existing(root: &Path, rel: &str) -> std::io::Result<Option<PathBuf>> {
    let src = to_local(root, rel);
    if !src.exists() {
        return Ok(None);
    }
    let dst = to_local(&meta_dir(root).join("versiones").join(stamp(SystemTime::now())), rel);
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::rename(&src, &dst)?;
    Ok(Some(dst))
}

/// Mueve un archivo borrado en otro equipo a `papelera/<fecha>/` (se conserva 14 días).
pub fn move_to_trash(root: &Path, rel: &str) -> std::io::Result<PathBuf> {
    let src = to_local(root, rel);
    let dst = to_local(&meta_dir(root).join("papelera").join(stamp(SystemTime::now())), rel);
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::rename(&src, &dst)?;
    Ok(dst)
}

/// Elimina las carpetas que han quedado vacías (o solo con basura del sistema)
/// al borrar `rel`, sin tocar nunca la carpeta raíz.
pub fn remove_empty_parents(root: &Path, rel: &str) {
    let mut p = to_local(root, rel);
    while let Some(parent) = p.parent().map(Path::to_path_buf) {
        if parent == root || !parent.starts_with(root) {
            break;
        }
        let only_junk = std::fs::read_dir(&parent)
            .map(|rd| rd.flatten().all(|e| matches!(e.file_name().to_str(), Some(".DS_Store" | "Thumbs.db" | "desktop.ini"))))
            .unwrap_or(false);
        if !only_junk {
            break;
        }
        for junk in [".DS_Store", "Thumbs.db", "desktop.ini"] {
            let _ = std::fs::remove_file(parent.join(junk));
        }
        if std::fs::remove_dir(&parent).is_err() {
            break;
        }
        p = parent;
    }
}

/// Coloca un temporal ya verificado en su sitio definitivo.
/// Devuelve (tamaño, mtime) tal como quedan en disco.
pub fn install(root: &Path, temp: &Path, rel: &str, mtime_ns: i64) -> std::io::Result<(u64, i64)> {
    check_no_symlinks(root, rel)?;
    let dst = to_local(root, rel);
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)?;
    }
    check_no_symlinks(root, rel)?;
    let ft = filetime::FileTime::from_unix_time(mtime_ns.div_euclid(1_000_000_000), mtime_ns.rem_euclid(1_000_000_000) as u32);
    filetime::set_file_mtime(temp, ft)?;
    #[cfg(unix)]
    {
        // Los archivos recibidos nunca son ejecutables.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp, std::fs::Permissions::from_mode(0o644))?;
    }
    std::fs::rename(temp, &dst)?;
    let m = std::fs::metadata(&dst)?;
    Ok((m.len(), crate::scanner::mtime_ns(&m)))
}

/// Lectura posicional (no mueve el cursor; segura entre hilos).
pub fn read_at(f: &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = f.read_at(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                break;
            }
            done += n;
        }
        Ok(done)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = f.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                break;
            }
            done += n;
        }
        Ok(done)
    }
}

/// Abre un archivo para servirlo a otros equipos sin bloquearlo para nadie.
pub fn open_shared(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        std::fs::OpenOptions::new().read(true).share_mode(7).open(path)
    }
    #[cfg(not(windows))]
    std::fs::File::open(path)
}

pub fn available_space(root: &Path) -> Option<u64> {
    fs4::available_space(root).ok()
}

/// Borra versiones y temporales antiguos.
pub fn cleanup(root: &Path, versions_max_age: Duration, temp_max_age: Duration) {
    let now = SystemTime::now();
    let old = |p: &Path, age: Duration| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|d| d > age)
    };
    for sub in ["versiones", "papelera"] {
        if let Ok(rd) = std::fs::read_dir(meta_dir(root).join(sub)) {
            for e in rd.flatten() {
                if old(&e.path(), versions_max_age) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(meta_dir(root).join("tmp")) {
        for e in rd.flatten() {
            if old(&e.path(), temp_max_age) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_format() {
        assert_eq!(stamp(UNIX_EPOCH + Duration::from_secs(1_761_417_005)), "2025-10-25_183005Z");
        assert_eq!(stamp(UNIX_EPOCH), "1970-01-01_000000Z");
    }

    #[test]
    fn install_with_backup() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        ensure_meta(root).unwrap();
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("d/f.txt"), b"viejo").unwrap();
        let tmp = temp_path(root, "abc");
        std::fs::write(&tmp, b"nuevo").unwrap();
        let b = backup_existing(root, "d/f.txt").unwrap().unwrap();
        assert_eq!(std::fs::read(b).unwrap(), b"viejo");
        let (size, mtime) = install(root, &tmp, "d/f.txt", 1_600_000_000_123_000_000).unwrap();
        assert_eq!(size, 5);
        assert_eq!(mtime / 1_000_000_000, 1_600_000_000);
        assert_eq!(std::fs::read(root.join("d/f.txt")).unwrap(), b"nuevo");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_escape() {
        let t = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = t.path();
        ensure_meta(root).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
        let tmp = temp_path(root, "x");
        std::fs::write(&tmp, b"x").unwrap();
        assert!(install(root, &tmp, "link/evil.txt", 0).is_err());
        assert!(!outside.path().join("evil.txt").exists());
    }
}
