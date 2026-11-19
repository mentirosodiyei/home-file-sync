//! Ejecución local de una orden pedida desde otro equipo.
//!
//! * Nunca pasa por un intérprete de órdenes (`sh -c`, `cmd /c`): el programa
//!   y sus argumentos llegan separados, así que no hay comillas, tuberías ni
//!   redirecciones que interpretar.
//! * El programa se busca en el `PATH` del usuario (en macOS, el de su shell
//!   de inicio de sesión, que es donde están Homebrew, cargo, nvm…) o es una
//!   ruta relativa dentro de la carpeta compartida (`./gradlew`).
//! * Al detenerla se termina también todo lo que haya lanzado (grupo de
//!   procesos en macOS/Linux, árbol de procesos en Windows).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

/// Límites de una petición.
pub const MAX_ARGS: usize = 256;
pub const MAX_ARG_LEN: usize = 8192;
pub const MAX_TIMEOUT_S: u32 = 4 * 3600;
pub const DEFAULT_TIMEOUT_S: u32 = 1800;

/// Separa una orden escrita como texto en programa y argumentos, con comillas
/// simples o dobles y `\` para escapar (sin variables ni comodines: no hay shell).
pub fn split_command(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => match chars.peek() {
                Some('"') | Some('\\') => cur.push(chars.next().unwrap()),
                _ => cur.push('\\'),
            },
            (Some(_), c) => cur.push(c),
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, '\\') if !cfg!(windows) => match chars.next() {
                Some(n) => {
                    cur.push(n);
                    has = true;
                }
                None => cur.push('\\'),
            },
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => {
                cur.push(c);
                has = true;
            }
        }
    }
    if quote.is_some() {
        return Err(tr!("Falta cerrar unas comillas en la orden."));
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// Vuelve a escribir una orden como texto (para mostrarla).
pub fn join_command(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty() && !a.chars().any(|c| c.is_whitespace() || "\"'\\$`|&;<>()*?#".contains(c)) {
                a.clone()
            } else {
                format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn program_eq(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        let strip = |s: &str| {
            let l = s.to_ascii_lowercase();
            for ext in [".exe", ".cmd", ".bat", ".com"] {
                if let Some(x) = l.strip_suffix(ext) {
                    return x.to_string();
                }
            }
            l
        };
        strip(a) == strip(b)
    } else {
        a == b
    }
}

/// ¿La orden empieza exactamente por alguna de las permitidas?
/// `cargo test` permite `cargo test`, `cargo test --release`… pero no `cargo run`.
pub fn is_allowed(allowed: &[String], argv: &[String]) -> bool {
    allowed.iter().any(|entry| {
        let Ok(tokens) = split_command(entry) else { return false };
        !tokens.is_empty()
            && argv.len() >= tokens.len()
            && program_eq(&tokens[0], &argv[0])
            && tokens[1..].iter().zip(&argv[1..]).all(|(t, a)| t == a)
    })
}

/// Comprueba la forma de una petición (no si está permitida).
pub fn check_request(argv: &[String], subdir: &str) -> Result<(), String> {
    if argv.is_empty() || argv[0].trim().is_empty() {
        return Err(tr!("La orden está vacía."));
    }
    if argv.len() > MAX_ARGS || argv.iter().any(|a| a.len() > MAX_ARG_LEN || a.contains('\0')) {
        return Err(tr!("La orden es demasiado larga."));
    }
    let prog = &argv[0];
    if Path::new(prog).is_absolute() || prog.starts_with('/') || prog.starts_with('\\') || prog.contains(':') {
        return Err(tr!("Usa el nombre del programa o una ruta dentro de la carpeta compartida, no una ruta absoluta."));
    }
    if has_separator(prog) {
        crate::pathsafe::validate_wire_path(prog.trim_start_matches("./").trim_start_matches(".\\").replace('\\', "/").as_str())
            .map_err(|_| tr!("La ruta del programa no es válida."))?;
    }
    if !subdir.is_empty() {
        crate::pathsafe::validate_wire_path(subdir).map_err(|_| tr!("La subcarpeta no es válida."))?;
    }
    Ok(())
}

fn has_separator(p: &str) -> bool {
    p.contains('/') || p.contains('\\')
}

/// `PATH` con el que se ejecutan las órdenes.
///
/// Las apps de escritorio de macOS arrancan con un `PATH` mínimo; el de la
/// shell de inicio de sesión del usuario es el que tiene sus herramientas.
pub fn user_path() -> String {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        let current = std::env::var("PATH").unwrap_or_default();
        let mut parts: Vec<String> = Vec::new();
        #[cfg(unix)]
        {
            if let Some(p) = login_shell_path() {
                parts.extend(p.split(':').map(String::from));
            }
            if let Some(home) = std::env::var_os("HOME") {
                let home = PathBuf::from(home);
                for d in [".cargo/bin", ".local/bin", ".bun/bin", ".deno/bin", "go/bin"] {
                    parts.push(home.join(d).display().to_string());
                }
            }
            for d in ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"] {
                parts.push(d.to_string());
            }
        }
        let sep = if cfg!(windows) { ';' } else { ':' };
        parts.extend(current.split(sep).map(String::from));
        let mut seen = std::collections::HashSet::new();
        parts.retain(|p| !p.is_empty() && seen.insert(p.clone()));
        parts.join(&sep.to_string())
    })
    .clone()
}

#[cfg(unix)]
fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    const MARK: &str = "__HFS_PATH__";
    for flags in ["-ilc", "-lc"] {
        let child = std::process::Command::new(&shell)
            .arg(flags)
            .arg(format!("printf '{MARK}%s{MARK}' \"$PATH\""))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let out = wait_with_timeout(child, Duration::from_secs(5));
        if let Some(s) = out {
            if let Some(p) = s.split(MARK).nth(1).filter(|p| !p.is_empty()) {
                return Some(p.to_string());
            }
        }
    }
    None
}

#[cfg(unix)]
fn wait_with_timeout(mut child: std::process::Child, d: Duration) -> Option<String> {
    let deadline = Instant::now() + d;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let mut s = String::new();
                std::io::Read::read_to_string(child.stdout.as_mut()?, &mut s).ok()?;
                return Some(s);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Busca el programa: ruta relativa a `cwd`/`root` o nombre en el `PATH`.
pub fn resolve_program(prog: &str, root: &Path, cwd: &Path, path_var: &str) -> Result<PathBuf, String> {
    if has_separator(prog) {
        let candidate = cwd.join(prog);
        let real = crate::settings::canonical(&candidate).map_err(|_| tr!("No se encuentra «{}» en la carpeta compartida.", prog))?;
        if !real.starts_with(root) {
            return Err(tr!("El programa debe estar dentro de la carpeta compartida."));
        }
        return Ok(real);
    }
    let sep = if cfg!(windows) { ';' } else { ':' };
    let exts: Vec<String> = if cfg!(windows) {
        let has_ext = Path::new(prog).extension().is_some();
        let mut v = if has_ext { vec![String::new()] } else { vec![] };
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        v.extend(pathext.split(';').filter(|e| !e.is_empty()).map(|e| e.to_ascii_lowercase()));
        v
    } else {
        vec![String::new()]
    };
    for dir in path_var.split(sep).filter(|d| !d.is_empty()) {
        for ext in &exts {
            let c = Path::new(dir).join(format!("{prog}{ext}"));
            if is_executable(&c) {
                return Ok(c);
            }
        }
    }
    Err(tr!("No se encuentra el programa «{}» en este equipo.", prog))
}

fn is_executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else { return false };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Lo que produce un proceso en marcha.
pub enum Progress {
    Output(Vec<u8>),
    Exited { code: Option<i32>, reason: Option<String> },
}

/// Lanza el programa y devuelve un canal con su salida (stdout y stderr
/// mezclados en orden de llegada) y el resultado. Detenerlo: `cancel`.
pub fn spawn(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    timeout: Duration,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<mpsc::Receiver<Progress>, String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: sin ventanas de consola
    let mut child = cmd.spawn().map_err(|e| tr!("No se ha podido ejecutar: {}", e))?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel(64);
    if let Some(o) = child.stdout.take() {
        tokio::spawn(pump(o, tx.clone()));
    }
    if let Some(e) = child.stderr.take() {
        tokio::spawn(pump(e, tx.clone()));
    }
    tokio::spawn(async move {
        let started = Instant::now();
        let reason = tokio::select! {
            r = child.wait() => {
                let code = r.ok().and_then(|s| s.code());
                // Da tiempo a que lleguen los últimos datos de las tuberías.
                tokio::time::sleep(Duration::from_millis(50)).await;
                let _ = tx.send(Progress::Exited { code, reason: (code.is_none()).then(|| tr!("terminado por una señal")) }).await;
                return;
            }
            _ = tokio::time::sleep(timeout) => tr!("tiempo límite agotado ({} s)", timeout.as_secs()),
            _ = async { while cancel.changed().await.is_ok() { if *cancel.borrow() { return; } } std::future::pending::<()>().await } => tr!("detenido"),
        };
        kill_tree(pid, &mut child).await;
        tracing::info!("Orden detenida tras {:?}: {reason}", started.elapsed());
        let _ = tx.send(Progress::Exited { code: None, reason: Some(reason) }).await;
    });
    Ok(rx)
}

async fn pump(mut r: impl tokio::io::AsyncRead + Unpin, tx: mpsc::Sender<Progress>) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send(Progress::Output(buf[..n].to_vec())).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// Termina el proceso y todo lo que haya lanzado.
async fn kill_tree(pid: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // Grupo de procesos propio (process_group(0)): primero con aviso.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        if tokio::time::timeout(Duration::from_secs(3), child.wait()).await.is_err() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    #[cfg(windows)]
    if let Some(pid) = pid {
        let mut k = tokio::process::Command::new("taskkill");
        k.args(["/PID", &pid.to_string(), "/T", "/F"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        k.creation_flags(0x0800_0000);
        let _ = tokio::time::timeout(Duration::from_secs(10), k.status()).await;
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Órdenes habituales según los archivos del proyecto (para sugerirlas).
pub fn suggest_commands(root: &Path) -> Vec<String> {
    let has = |f: &str| root.join(f).exists();
    let any_ext = |ext: &str| {
        std::fs::read_dir(root)
            .map(|d| d.flatten().any(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case(ext))))
            .unwrap_or(false)
    };
    let mut v: Vec<&str> = Vec::new();
    if has("Cargo.toml") {
        v.extend(["cargo test", "cargo build", "cargo check", "cargo clippy"]);
    }
    if has("package.json") {
        if has("pnpm-lock.yaml") {
            v.extend(["pnpm test", "pnpm run build", "pnpm install"]);
        } else if has("yarn.lock") {
            v.extend(["yarn test", "yarn build", "yarn install"]);
        } else if has("bun.lockb") || has("bun.lock") {
            v.extend(["bun test", "bun run build", "bun install"]);
        } else {
            v.extend(["npm test", "npm run build", "npm run lint", "npm ci"]);
        }
    }
    if has("pyproject.toml") || has("pytest.ini") || has("setup.py") || has("requirements.txt") {
        v.extend(["pytest", "python -m pytest"]);
    }
    if has("go.mod") {
        v.extend(["go test ./...", "go build ./..."]);
    }
    if any_ext("sln") || any_ext("csproj") || any_ext("fsproj") {
        v.extend(["dotnet test", "dotnet build"]);
    }
    if has("gradlew") || has("gradlew.bat") {
        v.push(if cfg!(windows) { "gradlew.bat test" } else { "./gradlew test" });
    }
    if has("pom.xml") {
        v.push("mvn test");
    }
    if has("Package.swift") {
        v.extend(["swift test", "swift build"]);
    }
    if any_ext("xcodeproj") || any_ext("xcworkspace") {
        v.push("xcodebuild test");
    }
    if has("CMakeLists.txt") {
        v.extend(["cmake --build build", "ctest --test-dir build"]);
    }
    if has("Makefile") {
        v.push("make test");
    }
    if has("Gemfile") {
        v.push("bundle exec rake test");
    }
    if has("composer.json") {
        v.push("composer test");
    }
    if has(".git") {
        v.extend(["git status", "git diff"]);
    }
    v.into_iter().map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn splits_like_a_shell_without_expanding() {
        assert_eq!(split_command("cargo test -p sfs-core").unwrap(), s(&["cargo", "test", "-p", "sfs-core"]));
        assert_eq!(split_command(r#"npm run "build all" 'x y'"#).unwrap(), s(&["npm", "run", "build all", "x y"]));
        assert_eq!(split_command(r#"echo "" $HOME *"#).unwrap(), s(&["echo", "", "$HOME", "*"]));
        assert_eq!(split_command("  a   b  ").unwrap(), s(&["a", "b"]));
        assert_eq!(split_command(r#"a "x\"y""#).unwrap(), s(&["a", "x\"y"]));
        assert!(split_command("a \"b").is_err());
        assert_eq!(split_command("ls; rm -rf /").unwrap(), s(&["ls;", "rm", "-rf", "/"]), "sin shell, ; es solo texto");
    }

    #[test]
    fn allowlist_matches_prefixes_only() {
        let allowed = s(&["cargo test", "npm run build", "./gradlew test"]);
        assert!(is_allowed(&allowed, &s(&["cargo", "test"])));
        assert!(is_allowed(&allowed, &s(&["cargo", "test", "--release", "-p", "x"])));
        assert!(!is_allowed(&allowed, &s(&["cargo", "run"])));
        assert!(!is_allowed(&allowed, &s(&["cargo"])));
        assert!(!is_allowed(&allowed, &s(&["cargo-test"])));
        assert!(is_allowed(&allowed, &s(&["npm", "run", "build"])));
        assert!(!is_allowed(&allowed, &s(&["npm", "run", "deploy"])));
        assert!(is_allowed(&allowed, &s(&["./gradlew", "test"])));
        assert!(!is_allowed(&[], &s(&["cargo", "test"])));
        if cfg!(windows) {
            assert!(is_allowed(&allowed, &s(&["CARGO.EXE", "test"])));
        }
    }

    #[test]
    fn rejects_absolute_and_escaping_programs() {
        assert!(check_request(&s(&["cargo", "test"]), "").is_ok());
        assert!(check_request(&s(&["./gradlew", "test"]), "app").is_ok());
        assert!(check_request(&s(&["/bin/rm", "-rf"]), "").is_err());
        assert!(check_request(&s(&["C:\\Windows\\cmd.exe"]), "").is_err());
        assert!(check_request(&s(&["../outside.sh"]), "").is_err());
        assert!(check_request(&s(&["cargo"]), "../..").is_err());
        assert!(check_request(&[], "").is_err());
    }

    #[test]
    fn joins_for_display() {
        assert_eq!(join_command(&s(&["cargo", "test", "a b"])), "cargo test \"a b\"");
    }
}
