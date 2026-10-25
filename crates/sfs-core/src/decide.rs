//! Decisiones de sincronización. Funciones puras, fáciles de probar.
//!
//! Reglas:
//! * Los borrados nunca se propagan.
//! * Mismo contenido (hash) = nada que transferir; solo se fusionan vectores.
//! * Versión remota posterior = actualización (la anterior se guarda en
//!   `.showfilesync/versiones`).
//! * Versiones independientes con distinto contenido = conflicto: gana una de
//!   forma determinista (la misma en todos los equipos) y la otra se conserva
//!   con un nombre de conflicto. Nunca se pierde ninguna.

use crate::index::FileEntry;
use crate::pathsafe::split_name;
use crate::vv::Causality;
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Solo cambia el vector de versión (mismo contenido).
    MergeVersion,
    /// Descargar: el archivo no existe aquí o la versión remota es posterior.
    Download,
    /// Descargar la versión remota y conservar la local como copia de conflicto.
    DownloadConflict,
}

/// ¿Qué hacer con la versión remota `remote` dada la local `local`?
pub fn decide(local: Option<&FileEntry>, remote: &FileEntry) -> Action {
    if remote.deleted {
        return Action::Nothing;
    }
    let Some(local) = local else { return Action::Download };
    let causality = remote.vv.compare(&local.vv);
    if local.deleted {
        // Borrado aquí: solo vuelve si aparece una versión nueva.
        return match causality {
            Causality::Newer | Causality::Concurrent if remote.hash != local.hash => Action::Download,
            _ => Action::Nothing,
        };
    }
    if remote.hash == local.hash {
        return match causality {
            Causality::Newer | Causality::Concurrent => Action::MergeVersion,
            _ => Action::Nothing,
        };
    }
    match causality {
        Causality::Newer => Action::Download,
        Causality::Older => Action::Nothing,
        Causality::Equal | Causality::Concurrent => {
            if tiebreak(remote, local) == Ordering::Greater {
                Action::DownloadConflict
            } else {
                Action::Nothing
            }
        }
    }
}

/// Orden determinista entre dos versiones independientes: gana la más
/// reciente; si empatan, el hash mayor. Todos los equipos llegan al mismo
/// resultado porque solo usa datos que viajan con la entrada.
pub fn tiebreak(a: &FileEntry, b: &FileEntry) -> Ordering {
    a.mtime_ns.cmp(&b.mtime_ns).then_with(|| a.hash.cmp(&b.hash))
}

/// De varias versiones remotas de la misma ruta, ¿es `a` preferible a `b`?
pub fn better(a: &FileEntry, b: &FileEntry) -> bool {
    match a.vv.compare(&b.vv) {
        Causality::Newer => true,
        Causality::Older => false,
        _ => a.hash != b.hash && tiebreak(a, b) == Ordering::Greater,
    }
}

/// Nombre de la copia de conflicto, determinista en todos los equipos:
/// `Presentación (conflicto - PC Sala 2 - 3fa2c1).pptx`
pub fn conflict_name(entry: &FileEntry) -> String {
    let (dir, stem, ext) = split_name(&entry.path);
    let who: String = entry
        .modified_by
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') && !c.is_control())
        .take(40)
        .collect();
    let who = who.trim();
    let who = if who.is_empty() { "otro equipo" } else { who };
    let short = &entry.hash[..entry.hash.len().min(6)];
    format!("{dir}{stem} (conflicto - {who} - {short}){ext}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vv::VersionVector;

    fn e(hash: &str, mtime: i64, vv: &[(u64, u64)]) -> FileEntry {
        FileEntry {
            path: "Carpeta/Final.pptx".into(),
            size: 10,
            mtime_ns: mtime,
            hash: hash.into(),
            vv: VersionVector(vv.iter().copied().collect()),
            deleted: false,
            modified_by: "PC Sala 2".into(),
        }
    }

    #[test]
    fn new_file_downloads() {
        assert_eq!(decide(None, &e("aa", 1, &[(1, 1)])), Action::Download);
    }

    #[test]
    fn remote_deletion_never_propagates() {
        let mut r = e("aa", 1, &[(1, 2)]);
        r.deleted = true;
        assert_eq!(decide(Some(&e("aa", 1, &[(1, 1)])), &r), Action::Nothing);
    }

    #[test]
    fn local_deletion_blocks_same_version_but_not_newer() {
        let mut l = e("aa", 1, &[(1, 1)]);
        l.deleted = true;
        assert_eq!(decide(Some(&l), &e("aa", 1, &[(1, 1)])), Action::Nothing);
        assert_eq!(decide(Some(&l), &e("bb", 2, &[(1, 2)])), Action::Download);
    }

    #[test]
    fn update_and_older() {
        let l = e("aa", 1, &[(1, 1)]);
        assert_eq!(decide(Some(&l), &e("bb", 2, &[(1, 1), (2, 1)])), Action::Download);
        let l2 = e("bb", 2, &[(1, 1), (2, 1)]);
        assert_eq!(decide(Some(&l2), &e("aa", 1, &[(1, 1)])), Action::Nothing);
    }

    #[test]
    fn same_content_merges() {
        assert_eq!(decide(Some(&e("aa", 1, &[(1, 1)])), &e("aa", 5, &[(2, 1)])), Action::MergeVersion);
        assert_eq!(decide(Some(&e("aa", 1, &[(1, 1)])), &e("aa", 1, &[(1, 1)])), Action::Nothing);
    }

    #[test]
    fn conflict_is_symmetric() {
        let a = e("aa", 100, &[(1, 1)]);
        let b = e("bb", 200, &[(2, 1)]);
        // b es más reciente: en el equipo de A se descarga b como conflicto;
        // en el equipo de B no se hace nada (B gana).
        assert_eq!(decide(Some(&a), &b), Action::DownloadConflict);
        assert_eq!(decide(Some(&b), &a), Action::Nothing);
        // Empate de fecha: decide el hash, igual en ambos lados.
        let c = e("cc", 100, &[(3, 1)]);
        assert_eq!(decide(Some(&a), &c), Action::DownloadConflict);
        assert_eq!(decide(Some(&c), &a), Action::Nothing);
    }

    #[test]
    fn conflict_names() {
        let mut x = e("3fa2c1d8ee", 1, &[]);
        assert_eq!(conflict_name(&x), "Carpeta/Final (conflicto - PC Sala 2 - 3fa2c1).pptx");
        x.modified_by = "Mac: Juan/2".into();
        x.path = "LEEME".into();
        assert_eq!(conflict_name(&x), "LEEME (conflicto - Mac Juan2 - 3fa2c1)");
    }
}
