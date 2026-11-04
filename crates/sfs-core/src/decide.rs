//! Decisiones de sincronización. Funciones puras, fáciles de probar.
//!
//! Reglas (las carpetas se comportan como una sola):
//! * Un borrado es una versión más del archivo: si es posterior a la que hay
//!   aquí, el archivo se elimina (se mueve a `.showfilesync/papelera`).
//! * Si un equipo edita mientras otro borra (cambios independientes), gana la
//!   edición: nunca se pierde contenido modificado.
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
    /// Eliminar el archivo local (a la papelera): se borró en otro equipo.
    Delete,
    /// Registrar el borrado aunque aquí no exista, para no recibirlo de nuevo
    /// de un equipo que aún tenga la versión antigua.
    AdoptTombstone,
}

/// ¿Qué hacer con la versión remota `remote` dada la local `local`?
pub fn decide(local: Option<&FileEntry>, remote: &FileEntry) -> Action {
    if remote.deleted {
        return match local {
            None => Action::AdoptTombstone,
            Some(l) if l.deleted => match remote.vv.compare(&l.vv) {
                Causality::Newer | Causality::Concurrent => Action::MergeVersion,
                _ => Action::Nothing,
            },
            // Solo se borra si el borrado es posterior a lo que hay aquí; si
            // aquí se editó a la vez, gana la edición.
            Some(l) => match remote.vv.compare(&l.vv) {
                Causality::Newer => Action::Delete,
                _ => Action::Nothing,
            },
        };
    }
    let Some(local) = local else { return Action::Download };
    let causality = remote.vv.compare(&local.vv);
    if local.deleted {
        // Borrado aquí: vuelve si la versión remota es posterior al borrado o
        // se editó a la vez que se borraba (gana la edición).
        return match causality {
            Causality::Newer | Causality::Concurrent => Action::Download,
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
        // Entre un borrado y una edición independientes, gana la edición.
        _ if a.deleted != b.deleted => !a.deleted,
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

    fn deleted(mut x: FileEntry) -> FileEntry {
        x.deleted = true;
        x
    }

    #[test]
    fn newer_deletion_propagates() {
        // A tenía v{1:1}; lo borra → tombstone v{1:2}.
        let r = deleted(e("aa", 1, &[(1, 2)]));
        assert_eq!(decide(Some(&e("aa", 1, &[(1, 1)])), &r), Action::Delete);
        assert_eq!(decide(None, &r), Action::AdoptTombstone);
        // Un borrado antiguo no elimina una versión posterior.
        assert_eq!(decide(Some(&e("bb", 2, &[(1, 3)])), &r), Action::Nothing);
    }

    #[test]
    fn edit_wins_over_concurrent_deletion() {
        let tomb = deleted(e("aa", 1, &[(1, 2)])); // A borra
        let edit = e("bb", 5, &[(1, 1), (2, 1)]); // B edita a la vez
        assert_eq!(decide(Some(&edit), &tomb), Action::Nothing, "en B se conserva la edición");
        assert_eq!(decide(Some(&tomb), &edit), Action::Download, "en A vuelve la versión editada");
        assert!(better(&edit, &tomb) && !better(&tomb, &edit));
    }

    #[test]
    fn deleted_here_returns_only_with_a_newer_version() {
        let l = deleted(e("aa", 1, &[(1, 2)]));
        assert_eq!(decide(Some(&l), &e("aa", 1, &[(1, 1)])), Action::Nothing, "versión anterior al borrado");
        assert_eq!(decide(Some(&l), &e("aa", 1, &[(1, 3)])), Action::Download, "restaurado con el mismo contenido");
        assert_eq!(decide(Some(&l), &deleted(e("aa", 1, &[(1, 2), (2, 1)]))), Action::MergeVersion);
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
