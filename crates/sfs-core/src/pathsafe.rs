//! Rutas relativas seguras.
//!
//! En el protocolo, una ruta es siempre relativa a la carpeta sincronizada,
//! usa `/` como separador y está normalizada en Unicode NFC. Todo lo que llega
//! de la red pasa por [`validate_wire_path`] antes de tocar el disco.
//!
//! La *clave* de un archivo ([`key_of`]) es la ruta NFC en minúsculas: Windows
//! y macOS usan por defecto sistemas de archivos que no distinguen mayúsculas,
//! así que `Show.pptx` y `show.pptx` son el mismo archivo para nosotros.

use std::path::{Component, Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

/// Carpeta interna dentro de la carpeta sincronizada (temporales, versiones).
pub const META_DIR: &str = ".homefilesync";

/// Longitud máxima aceptada para una ruta del protocolo (en bytes).
pub const MAX_PATH_BYTES: usize = 4096;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
    #[error("ruta vacía")]
    Empty,
    #[error("ruta demasiado larga")]
    TooLong,
    #[error("ruta absoluta o con unidad")]
    Absolute,
    #[error("componente no permitido: {0:?}")]
    BadComponent(String),
    #[error("carácter no permitido en la ruta")]
    BadChar,
    #[error("ruta reservada para uso interno")]
    Reserved,
}

/// Normaliza una ruta relativa a NFC (los nombres que vienen de macOS pueden estar en NFD).
pub fn nfc(path: &str) -> String {
    path.nfc().collect()
}

/// Clave de identidad de un archivo: NFC + minúsculas.
pub fn key_of(path: &str) -> String {
    nfc(path).to_lowercase()
}

/// Valida una ruta recibida por la red. No toca el disco.
pub fn validate_wire_path(p: &str) -> Result<(), PathError> {
    if p.is_empty() {
        return Err(PathError::Empty);
    }
    if p.len() > MAX_PATH_BYTES {
        return Err(PathError::TooLong);
    }
    if p.starts_with('/') {
        return Err(PathError::Absolute);
    }
    if p.contains('\\') || p.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}') {
        return Err(PathError::BadChar);
    }
    // "C:algo" o "C:/algo": unidad de Windows.
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return Err(PathError::Absolute);
    }
    for (i, comp) in p.split('/').enumerate() {
        if comp.is_empty() || comp == "." || comp == ".." {
            return Err(PathError::BadComponent(comp.to_string()));
        }
        if i == 0 && comp.eq_ignore_ascii_case(META_DIR) {
            return Err(PathError::Reserved);
        }
        if is_ignored_name(comp) {
            return Err(PathError::BadComponent(comp.to_string()));
        }
    }
    Ok(())
}

/// Nombres que nunca se sincronizan: basura del sistema, bloqueos de Office,
/// descargas a medias y nuestra carpeta interna.
pub fn is_ignored_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    const EXACT: &[&str] = &[
        ".ds_store",
        "thumbs.db",
        "ehthumbs.db",
        "desktop.ini",
        ".localized",
        ".spotlight-v100",
        ".trashes",
        ".fseventsd",
        ".temporaryitems",
        ".documentrevisions-v100",
        "$recycle.bin",
        "system volume information",
        "icon\r",
        META_DIR,
    ];
    if EXACT.contains(&lower.as_str()) {
        return true;
    }
    if lower.starts_with("~$") || lower.starts_with(".~lock.") || lower.starts_with("._") || lower.starts_with(".trash") {
        return true;
    }
    const SUFFIX: &[&str] = &[".tmp", ".temp", ".crdownload", ".part", ".partial", ".download", ".swp", ".sfspart"];
    SUFFIX.iter().any(|s| lower.ends_with(s))
}

/// Comprueba si el nombre se puede crear en este sistema operativo.
/// En Windows hay caracteres y nombres reservados que macOS sí permite.
pub fn locally_representable(rel: &str) -> Result<(), String> {
    if cfg!(windows) {
        windows_representable(rel)
    } else {
        Ok(())
    }
}

/// Reglas de nombres de Windows (expuesto para pruebas en cualquier plataforma).
pub fn windows_representable(rel: &str) -> Result<(), String> {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1",
        "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    for comp in rel.split('/') {
        if let Some(c) = comp.chars().find(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')) {
            return Err(format!("el nombre «{comp}» contiene «{c}», que Windows no admite"));
        }
        if comp.ends_with('.') || comp.ends_with(' ') {
            return Err(format!("el nombre «{comp}» termina en punto o espacio, que Windows no admite"));
        }
        let stem = comp.split('.').next().unwrap_or("").to_lowercase();
        if RESERVED.contains(&stem.as_str()) {
            return Err(format!("«{comp}» es un nombre reservado en Windows"));
        }
    }
    Ok(())
}

/// Convierte una ruta del protocolo (ya validada) en ruta local bajo `root`.
pub fn to_local(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for comp in rel.split('/') {
        p.push(comp);
    }
    p
}

/// Convierte una ruta local en ruta de protocolo relativa a `root`.
/// Devuelve `None` si no está dentro de `root` o tiene componentes raros.
pub fn from_local(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?.to_string()),
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Rechaza escribir a través de enlaces simbólicos: ningún componente
/// intermedio existente de `rel` bajo `root` puede ser un symlink.
pub fn check_no_symlinks(root: &Path, rel: &str) -> std::io::Result<()> {
    let mut p = root.to_path_buf();
    let comps: Vec<&str> = rel.split('/').collect();
    for comp in comps.iter() {
        p.push(comp);
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "la ruta atraviesa un enlace simbólico",
                ))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Divide `dir/nombre.ext` en (`dir/`, `nombre`, `.ext`).
pub fn split_name(rel: &str) -> (&str, &str, &str) {
    let (dir, file) = match rel.rfind('/') {
        Some(i) => (&rel[..=i], &rel[i + 1..]),
        None => ("", rel),
    };
    match file.rfind('.') {
        Some(i) if i > 0 => (dir, &file[..i], &file[i..]),
        _ => (dir, file, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_absolute() {
        for bad in [
            "../x", "a/../../x", "/etc/passwd", "C:/Windows", "c:x", "a//b", "./a", "a/./b", "a\\b", "", "a/\0b",
            ".homefilesync/tmp/x", ".HomeFileSync/x", "a/..", "x/.DS_Store",
        ] {
            assert!(validate_wire_path(bad).is_err(), "debería rechazar {bad:?}");
        }
    }

    #[test]
    fn accepts_normal_names() {
        for ok in [
            "Presentación Final.pptx",
            "Día 1/Sala A/Ponencia (v2) #3.key",
            "vídeo ñandú 😀.mp4",
            "a/b/c/d.pdf",
            "sub/.hidden-but-ok",
        ] {
            assert_eq!(validate_wire_path(ok), Ok(()), "debería aceptar {ok:?}");
        }
    }

    #[test]
    fn nfc_and_keys() {
        let nfd = "Presentacio\u{301}n.pptx";
        assert_eq!(nfc(nfd), "Presentación.pptx");
        assert_eq!(key_of(nfd), key_of("PRESENTACIÓN.pptx"));
    }

    #[test]
    fn windows_names() {
        assert!(windows_representable("a:b.pptx").is_err());
        assert!(windows_representable("CON.txt").is_err());
        assert!(windows_representable("dir./x").is_err());
        assert!(windows_representable("Ponencia ñ (1).pptx").is_ok());
    }

    #[test]
    fn ignored() {
        assert!(is_ignored_name("~$Presentacion.pptx"));
        assert!(is_ignored_name(".DS_Store"));
        assert!(is_ignored_name("video.mp4.crdownload"));
        assert!(!is_ignored_name("Presentacion.pptx"));
    }

    #[test]
    fn split() {
        assert_eq!(split_name("a/b/Final.v2.pptx"), ("a/b/", "Final.v2", ".pptx"));
        assert_eq!(split_name("README"), ("", "README", ""));
        assert_eq!(split_name(".bashrc"), ("", ".bashrc", ""));
    }
}
