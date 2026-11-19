//! Reglas de exclusión para carpetas que son proyectos de código.
//!
//! En una carpeta marcada como «proyecto de código» no se sincroniza:
//! * el repositorio (`.git`, `.hg`, `.svn`, `.jj`): dos equipos escribiendo a
//!   la vez en él lo corromperían; cada equipo mantiene el suyo;
//! * lo que excluyen los `.gitignore` del proyecto (en cualquier subcarpeta,
//!   con las mismas reglas que git) y el archivo opcional
//!   `.homefilesyncignore` de la raíz, con la misma sintaxis.
//!
//! Así no viajan carpetas de compilación (`target/`, `node_modules/`…) que
//! cada equipo genera por su cuenta y que además dependen del sistema.
//!
//! Las reglas se aplican en los dos sentidos: lo excluido no se indexa aquí y
//! tampoco se descarga aunque otro equipo lo anuncie. Pasar a excluir una ruta
//! nunca se trata como un borrado: simplemente se deja de sincronizar.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::HashMap;
use std::path::Path;

pub const IGNORE_FILE: &str = ".homefilesyncignore";
const GITIGNORE: &str = ".gitignore";
/// Repositorios: nunca se sincronizan en un proyecto de código.
const ALWAYS: &[&str] = &[".git", ".hg", ".svn", ".jj"];

fn matcher(lines: &str) -> Option<Gitignore> {
    let mut b = GitignoreBuilder::new(".");
    let mut any = false;
    for line in lines.lines() {
        if b.add_line(None, line).is_ok() && !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            any = true;
        }
    }
    if !any {
        return None;
    }
    b.build().ok()
}

/// Reglas de una carpeta: los `.gitignore` encontrados, por subcarpeta
/// (`""` es la raíz), más el archivo propio de la raíz.
#[derive(Default)]
pub struct IgnoreRules {
    dirs: HashMap<String, Gitignore>,
    own: Option<Gitignore>,
}

impl IgnoreRules {
    /// Carga las reglas de la raíz. Las de las subcarpetas se añaden con
    /// [`IgnoreRules::load_dir`] al recorrerlas.
    pub fn for_root(root: &Path) -> IgnoreRules {
        let mut r = IgnoreRules::default();
        r.own = std::fs::read_to_string(root.join(IGNORE_FILE)).ok().and_then(|s| matcher(&s));
        r.load_dir(root, "");
        r
    }

    /// Lee el `.gitignore` de la subcarpeta `rel` (con `/`; `""` = raíz).
    pub fn load_dir(&mut self, root: &Path, rel: &str) {
        let path = if rel.is_empty() { root.join(GITIGNORE) } else { crate::pathsafe::to_local(root, rel).join(GITIGNORE) };
        if let Some(m) = std::fs::read_to_string(path).ok().and_then(|s| matcher(&s)) {
            self.dirs.insert(rel.to_string(), m);
        }
    }

    /// ¿Se excluye esta ruta relativa (con `/`)? Como en git, la regla más
    /// cercana al archivo manda y una carpeta excluida excluye todo su contenido.
    pub fn is_ignored(&self, rel: &str, is_dir: bool) -> bool {
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return false;
        }
        // Cada carpeta antecesora, de la raíz hacia abajo: si alguna queda
        // excluida, todo lo de dentro también (git no entra en ellas).
        for depth in 0..parts.len() {
            let here_is_dir = depth + 1 < parts.len() || is_dir;
            if ALWAYS.iter().any(|a| parts[depth].eq_ignore_ascii_case(a)) && here_is_dir {
                return true;
            }
            if self.matches(&parts[..=depth], here_is_dir) {
                return true;
            }
        }
        false
    }

    /// Resultado de las reglas para `parts` (sin mirar los antecesores).
    fn matches(&self, parts: &[&str], is_dir: bool) -> bool {
        let full = parts.join("/");
        let mut verdict = None;
        if let Some(own) = &self.own {
            verdict = decision(own, &full, is_dir).or(verdict);
        }
        // De la raíz a la carpeta del propio archivo: la más profunda gana.
        for depth in 0..parts.len() {
            let dir = parts[..depth].join("/");
            if let Some(gi) = self.dirs.get(&dir) {
                let sub = parts[depth..].join("/");
                verdict = decision(gi, &sub, is_dir).or(verdict);
            }
        }
        verdict.unwrap_or(false)
    }
}

fn decision(gi: &Gitignore, rel: &str, is_dir: bool) -> Option<bool> {
    let m = gi.matched(Path::new(rel), is_dir);
    if m.is_ignore() {
        Some(true)
    } else if m.is_whitelist() {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_gitignore_rules_at_every_level() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n!keep.log\n/build\n").unwrap();
        std::fs::create_dir_all(root.join("web")).unwrap();
        std::fs::write(root.join("web/.gitignore"), "node_modules\ndist/\n").unwrap();
        std::fs::write(root.join(IGNORE_FILE), "secret.txt\n").unwrap();
        let mut r = IgnoreRules::for_root(root);
        r.load_dir(root, "web");

        assert!(r.is_ignored("target/debug/app", false));
        assert!(r.is_ignored("target", true));
        assert!(r.is_ignored("sub/target/x.o", false), "sin / inicial vale a cualquier nivel");
        assert!(r.is_ignored("a.log", false));
        assert!(!r.is_ignored("keep.log", false), "las excepciones con ! funcionan");
        assert!(r.is_ignored("build/out.bin", false));
        assert!(!r.is_ignored("src/build/mod.rs", false), "/build solo en la raíz");
        assert!(r.is_ignored("web/node_modules/react/index.js", false));
        assert!(r.is_ignored("web/dist/app.js", false));
        assert!(!r.is_ignored("dist/app.js", false), "las reglas de web/ solo valen dentro de web/");
        assert!(r.is_ignored("secret.txt", false));
        assert!(!r.is_ignored("src/main.rs", false));
        assert!(!r.is_ignored(".gitignore", false));
        // Repositorios, siempre.
        assert!(r.is_ignored(".git/HEAD", false));
        assert!(r.is_ignored("vendor/lib/.git/config", false));
        assert!(!r.is_ignored(".github/workflows/ci.yml", false));
        assert!(!r.is_ignored(".git", false), "un archivo .git (submódulo) es solo un puntero; se excluye su carpeta");
    }

    #[test]
    fn without_gitignore_only_repositories_are_excluded() {
        let d = tempfile::tempdir().unwrap();
        let r = IgnoreRules::for_root(d.path());
        assert!(r.is_ignored(".git/objects/ab/cdef", false));
        assert!(!r.is_ignored("target/debug/app", false));
    }
}
