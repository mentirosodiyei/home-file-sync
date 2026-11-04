//! Conexión automática cuando el descubrimiento (mDNS y baliza) no funciona:
//! los equipos reconectan con las direcciones aprendidas y se encuentran
//! entre sí con las que les comunica el grupo, sin conexión manual.

use sfs_core::{Engine, EngineConfig};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Node {
    engine: Engine,
    data: TempDir,
    root: PathBuf,
    port: u16,
    _folder: TempDir,
}

fn cfg(data: &Path, name: &str, port: u16) -> EngineConfig {
    let mut c = EngineConfig::new(data);
    c.device_name = Some(name.into());
    c.listen_port = Some(port);
    c.discovery = false; // red que bloquea el descubrimiento
    c.stable_ms = 300;
    c.rescan_secs = 1;
    c
}

/// Puerto libre para poder reiniciar el equipo en el mismo puerto.
fn free_port() -> u16 {
    std::net::TcpListener::bind("0.0.0.0:0").unwrap().local_addr().unwrap().port()
}

impl Node {
    async fn new(name: &str) -> Node {
        let data = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let port = free_port();
        let engine = Engine::start(cfg(data.path(), name, port)).await.unwrap();
        let root = engine.set_folder(folder.path()).unwrap();
        Node { engine, data, root, port, _folder: folder }
    }

    async fn restart(&mut self, name: &str) {
        self.engine.shutdown().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.engine = Engine::start(cfg(self.data.path(), name, self.port)).await.unwrap();
    }

    fn addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    fn sees(&self, name: &str) -> bool {
        self.engine.snapshot().devices.iter().any(|d| d.name == name && d.detail == "Sincronizado")
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnects_after_restart_without_discovery_or_manual_address() {
    let mut a = Node::new("A").await;
    let mut b = Node::new("B").await;
    let code = a.engine.create_group();
    b.engine.join_group_at(a.addr(), &code).await.unwrap();
    wait_for("A y B conectados", 20, || a.sees("B") && b.sees("A")).await;
    tokio::time::sleep(Duration::from_secs(3)).await; // se guardan las direcciones aprendidas

    // B se reinicia: nadie le da una dirección y no hay descubrimiento.
    b.restart("B").await;
    wait_for("B reconecta solo", 30, || b.sees("A") && a.sees("B")).await;
    std::fs::write(a.root.join("tras reinicio B.txt"), b"1").unwrap();
    wait_for("sincroniza", 30, || b.root.join("tras reinicio B.txt").exists()).await;

    // Ahora se reinicia A (el que recibió la conexión): también reconecta.
    tokio::time::sleep(Duration::from_secs(3)).await;
    a.restart("A").await;
    wait_for("A reconecta solo", 30, || a.sees("B") && b.sees("A")).await;
    std::fs::write(b.root.join("tras reinicio A.txt"), b"2").unwrap();
    wait_for("sincroniza", 30, || a.root.join("tras reinicio A.txt").exists()).await;

    // Las direcciones aprendidas se han guardado; nunca la escrita a mano.
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(b.data.path().join("settings.json")).unwrap()).unwrap();
    assert!(s["peer_addrs"].as_object().is_some_and(|m| !m.is_empty()), "{s}");
    assert!(s["manual_peers"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_find_each_other_through_the_group() {
    let a = Node::new("Central").await;
    let b = Node::new("Sala 1").await;
    let c = Node::new("Sala 2").await;
    let code = a.engine.create_group();
    b.engine.join_group_at(a.addr(), &code).await.unwrap();
    let code = a.engine.open_pairing().unwrap();
    c.engine.join_group_at(a.addr(), &code).await.unwrap();
    // B y C solo conocen a A, pero A les comunica sus direcciones.
    wait_for("B y C conectados directamente", 40, || b.sees("Sala 2") && c.sees("Sala 1")).await;
    // Sin A, siguen sincronizando entre ellos.
    a.engine.shutdown().await;
    std::fs::write(b.root.join("sin central.pptx"), b"directo").unwrap();
    wait_for("C recibe de B", 30, || std::fs::read(c.root.join("sin central.pptx")).ok().as_deref() == Some(b"directo")).await;
}
