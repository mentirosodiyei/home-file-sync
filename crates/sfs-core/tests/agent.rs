//! Órdenes de las IA entre equipos y carpetas de proyecto de código.

use serde_json::json;
use sfs_core::agent::{control, AgentPolicy, Approval, RunRequest, RunState};
use sfs_core::hub::{Hub, HubConfig};
use sfs_core::settings::ShareMode;
use sfs_core::Engine;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Home {
    hub: Hub,
    data: TempDir,
    dirs: Vec<TempDir>,
}

impl Home {
    async fn new(name: &str) -> Home {
        let data = tempfile::tempdir().unwrap();
        let mut cfg = HubConfig::new(data.path());
        cfg.device_name = Some(name.into());
        cfg.discovery = false;
        cfg.inbox_port = Some(0);
        cfg.engine.listen_port = Some(0);
        cfg.engine.stable_ms = 300;
        cfg.engine.rescan_secs = 1;
        let hub = Hub::start(cfg).await.unwrap();
        Home { hub, data, dirs: Vec::new() }
    }

    fn folder(&mut self) -> PathBuf {
        let d = tempfile::tempdir().unwrap();
        let p = std::fs::canonicalize(d.path()).unwrap();
        self.dirs.push(d);
        p
    }

    fn addr(&self, share: &str) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.hub.share(share).unwrap().port()))
    }
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("tiempo agotado esperando: {what}");
}

fn argv(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Lee un archivo de la carpeta.
fn read_cmd(file: &str) -> Vec<String> {
    if cfg!(windows) {
        argv(&["cmd", "/c", "type", file])
    } else {
        argv(&["cat", file])
    }
}

/// Crea `out.txt` en la carpeta.
fn write_cmd() -> Vec<String> {
    if cfg!(windows) {
        argv(&["cmd", "/c", "echo hecho> out.txt"])
    } else {
        argv(&["sh", "-c", "echo hecho > out.txt"])
    }
}

fn sleep_cmd() -> Vec<String> {
    if cfg!(windows) {
        argv(&["ping", "-n", "60", "127.0.0.1"])
    } else {
        argv(&["sleep", "60"])
    }
}

fn req(argv: Vec<String>) -> RunRequest {
    RunRequest { argv, subdir: String::new(), timeout_s: 120, client: "prueba".into(), wait_sync: true, sync_back: true }
}

async fn finish(e: &Engine, run: u64) -> sfs_core::agent::RunView {
    e.run_wait(run, Duration::from_secs(60), 10_000).await.unwrap()
}

fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in walkdir(dir) {
        out.push(e.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/"));
    }
    out.sort();
    out
}

fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.file_name().unwrap() == ".homefilesync" {
            continue;
        }
        if p.is_dir() {
            out.extend(walkdir(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn code_project_skips_repository_and_ignored_files() {
    let mut mac = Home::new("Mac").await;
    let mut pc = Home::new("PC").await;
    let m = mac.folder();
    std::fs::create_dir_all(m.join("src")).unwrap();
    std::fs::create_dir_all(m.join("target/debug")).unwrap();
    std::fs::create_dir_all(m.join(".git/objects")).unwrap();
    std::fs::create_dir_all(m.join("logs")).unwrap();
    std::fs::write(m.join(".gitignore"), "target/\n").unwrap();
    std::fs::write(m.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(m.join("target/debug/app"), "binario").unwrap();
    std::fs::write(m.join(".git/HEAD"), "ref: refs/heads/main").unwrap();
    std::fs::write(m.join("logs/a.log"), "uno").unwrap();

    let (share, code) = mac.hub.create_share("Proyecto", &m, ShareMode::Sync).await.unwrap();
    mac.hub.set_code_project(&share, true).unwrap();
    let p = pc.folder();
    let p_id = pc.hub.join_share_at(mac.addr(&share), &code, &p, ShareMode::Sync).await.unwrap();
    pc.hub.set_code_project(&p_id, true).unwrap();

    wait_for("el código llega al PC", 30, || p.join("src/main.rs").exists() && p.join("logs/a.log").exists()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(files(&p), vec![".gitignore", "logs/a.log", "src/main.rs"], "ni el repositorio ni lo excluido viajan");

    // Lo que el PC compila no llega al Mac.
    std::fs::create_dir_all(p.join("target/release")).unwrap();
    std::fs::write(p.join("target/release/app.exe"), "otro binario").unwrap();
    std::fs::write(p.join("src/lib.rs"), "pub fn x() {}").unwrap();
    wait_for("lib.rs llega al Mac", 30, || m.join("src/lib.rs").exists()).await;
    assert!(!m.join("target/release").exists());

    // Pasar a excluir algo nunca lo borra en los demás.
    std::fs::write(m.join(".gitignore"), "target/\nlogs/\n").unwrap();
    wait_for("la regla nueva llega al PC", 30, || std::fs::read_to_string(p.join(".gitignore")).unwrap_or_default().contains("logs/")).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(p.join("logs/a.log").exists(), "excluir no es borrar");
    assert!(m.join("logs/a.log").exists());
    std::fs::write(m.join("logs/b.log"), "dos").unwrap();
    std::fs::write(m.join("src/otro.rs"), "//").unwrap();
    wait_for("otro.rs llega al PC", 30, || p.join("src/otro.rs").exists()).await;
    assert!(!p.join("logs/b.log").exists(), "lo nuevo excluido no viaja");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ai_runs_commands_on_the_other_computer() {
    let mut mac = Home::new("Mac de Ana").await;
    let mut pc = Home::new("PC del salón").await;
    let m = mac.folder();
    let (share, code) = mac.hub.create_share("Proyecto", &m, ShareMode::Sync).await.unwrap();
    let p = pc.folder();
    let p_share = pc.hub.join_share_at(mac.addr(&share), &code, &p, ShareMode::Sync).await.unwrap();
    let a = mac.hub.share(&share).unwrap();
    let b = pc.hub.share(&p_share).unwrap();
    let pc_id = pc.hub.device_id();
    wait_for("conectados", 30, || a.agent_peers().iter().any(|x| x.id == pc_id && x.online && x.supported)).await;

    // Por defecto, el PC no acepta nada.
    let err = a.run_remote(&pc_id, req(read_cmd("nota.txt"))).unwrap_err();
    assert!(err.contains("no acepta"), "{err}");

    pc.hub
        .set_agent_policy(&p_share, AgentPolicy { enabled: true, allowed: vec![sfs_core::agent::exec::join_command(&read_cmd("nota.txt"))], ask: true })
        .unwrap();
    wait_for("el Mac sabe que el PC acepta órdenes", 10, || a.agent_peers().iter().any(|x| x.id == pc_id && x.exec)).await;

    // El archivo recién escrito ya está allí cuando se ejecuta la orden.
    std::fs::write(m.join("nota.txt"), "hola desde el Mac").unwrap();
    let run = a.run_remote(&pc_id, req(read_cmd("nota.txt"))).unwrap();
    let v = finish(&a, run).await;
    assert_eq!(v.state, RunState::Finished, "{v:?}");
    assert_eq!(v.exit_code, Some(0), "{v:?}");
    assert!(v.output.contains("hola desde el Mac"), "{v:?}");

    // Una orden no permitida espera aprobación en el PC; «siempre» la añade.
    let run = a.run_remote(&pc_id, req(write_cmd())).unwrap();
    wait_for("petición pendiente en el PC", 20, || b.agent_view().approvals.len() == 1).await;
    assert_eq!(a.run_view(run, 100).unwrap().state, RunState::Approval);
    let ap = b.agent_view().approvals[0].clone();
    assert_eq!(ap.client, "prueba");
    pc.hub.approve_run(&p_share, &ap.peer_id, &ap.run, Approval::Always).unwrap();
    let v = finish(&a, run).await;
    assert_eq!((v.state, v.exit_code), (RunState::Finished, Some(0)), "{v:?}");
    assert!(m.join("out.txt").exists(), "lo generado ha vuelto antes de dar la orden por terminada");
    assert!(b.agent_policy().allowed.contains(&sfs_core::agent::exec::join_command(&write_cmd())));
    let run = a.run_remote(&pc_id, req(write_cmd())).unwrap();
    assert_eq!(finish(&a, run).await.exit_code, Some(0), "ya no pregunta");

    // Rechazada.
    let run = a.run_remote(&pc_id, req(sleep_cmd())).unwrap();
    wait_for("petición pendiente en el PC", 20, || b.agent_view().approvals.len() == 1).await;
    let ap = b.agent_view().approvals[0].clone();
    pc.hub.approve_run(&p_share, &ap.peer_id, &ap.run, Approval::Deny).unwrap();
    let v = finish(&a, run).await;
    assert_eq!(v.state, RunState::Refused, "{v:?}");

    // Cancelar una orden larga la detiene en el otro equipo.
    let run = a.run_remote(&pc_id, req(sleep_cmd())).unwrap();
    wait_for("petición pendiente en el PC", 20, || b.agent_view().approvals.len() == 1).await;
    let ap = b.agent_view().approvals[0].clone();
    pc.hub.approve_run(&p_share, &ap.peer_id, &ap.run, Approval::Once).unwrap();
    wait_for("en marcha", 20, || a.run_view(run, 100).unwrap().state == RunState::Running).await;
    let t = Instant::now();
    a.run_cancel(run).unwrap();
    let v = finish(&a, run).await;
    assert!(v.state.is_final() && v.exit_code.is_none(), "{v:?}");
    assert!(t.elapsed() < Duration::from_secs(20));
    wait_for("nada en marcha en el PC", 10, || b.agent_view().active.is_empty()).await;
    assert!(b.agent_view().history.len() >= 4);

    // Programas fuera de la carpeta: nunca.
    assert!(a.run_remote(&pc_id, req(argv(&["/bin/rm", "-rf", "/"]))).is_err());

    // De extremo a extremo, como lo usa una IA: servidor MCP → canal local → app.
    let text = control::call_app(mac.data.path(), "list_machines", json!({}), Some(m.clone()), "Claude Code".into()).await.unwrap();
    assert!(text.contains("PC del salón") && text.contains("accepts commands"), "{text}");
    assert!(text.contains("your working directory is here"), "{text}");
    std::fs::write(m.join("nota.txt"), "segunda versión").unwrap();
    let cmd = sfs_core::agent::exec::join_command(&read_cmd("nota.txt"));
    let text = control::call_app(mac.data.path(), "run_command", json!({ "command": cmd }), Some(m.clone()), "Claude Code".into()).await.unwrap();
    assert!(text.contains("Exit code: 0") && text.contains("segunda versión"), "{text}");
    let err = control::call_app(mac.data.path(), "run_command", json!({ "command": "cargo test && rm -rf /" }), Some(m.clone()), "x".into()).await.unwrap_err();
    assert!(err.contains("without a shell"), "{err}");
    let text = control::call_app(mac.data.path(), "sync_now", json!({ "wait_seconds": 20 }), Some(m.clone()), "x".into()).await.unwrap();
    assert!(text.contains("in sync"), "{text}");
    let bad = tempfile::tempdir().unwrap();
    let err = control::call_app(bad.path(), "list_machines", json!({}), None, "x".into()).await.unwrap_err();
    assert!(err.contains("not open"), "{err}");
}
