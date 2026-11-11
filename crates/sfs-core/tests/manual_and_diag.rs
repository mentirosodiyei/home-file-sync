//! Conexión manual y registro de diagnóstico.
//!
//! Archivo de pruebas propio porque instala el registro global de `tracing`
//! del proceso (como hace la aplicación).

use sfs_core::{diag, Engine, EngineConfig};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tracing_subscriber::layer::SubscriberExt;

static LOG_DIR: OnceLock<TempDir> = OnceLock::new();

fn init_logging() -> PathBuf {
    let dir = LOG_DIR.get_or_init(|| {
        let d = tempfile::tempdir().unwrap();
        diag::init_file(d.path()).unwrap();
        let sub = tracing_subscriber::registry().with(diag::layer());
        let _ = tracing::subscriber::set_global_default(sub);
        d
    });
    dir.path().to_path_buf()
}

struct Node {
    engine: Engine,
    data: TempDir,
    root: PathBuf,
    _folder: TempDir,
}

impl Node {
    async fn new(name: &str) -> Node {
        init_logging();
        let data = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let mut c = EngineConfig::new(data.path());
        c.device_name = Some(name.into());
        c.listen_port = Some(0);
        c.discovery = false;
        c.stable_ms = 300;
        c.rescan_secs = 1;
        let engine = Engine::start(c).await.unwrap();
        let root = engine.set_folder(folder.path()).unwrap();
        Node { engine, data, root, _folder: folder }
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

fn read_settings(data: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(data.join("settings.json")).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_join_by_ip_and_port_syncs_like_automatic() {
    let a = Node::new("Anfitrión").await;
    let b = Node::new("Manual").await;
    std::fs::write(a.root.join("Ponencia.pptx"), b"diapositivas").unwrap();
    let code = a.engine.create_group();
    b.engine.join_group_manual("127.0.0.1", Some(a.engine.port()), &code, false).await.expect("unión manual");
    wait_for("archivo en B", 30, || std::fs::read(b.root.join("Ponencia.pptx")).ok().as_deref() == Some(b"diapositivas")).await;
    // Y en sentido contrario, por la misma sesión.
    std::fs::write(b.root.join("Desde B.pdf"), b"pdf").unwrap();
    wait_for("archivo en A", 30, || a.root.join("Desde B.pdf").exists()).await;
    // Sin «recordar», la dirección no se guarda.
    assert!(read_settings(b.data.path())["manual_peers"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_join_accepts_host_with_port_and_remembers_on_request() {
    let a = Node::new("A").await;
    let b = Node::new("B").await;
    let code = a.engine.create_group();
    let target = format!("127.0.0.1:{}", a.engine.port());
    b.engine.join_group_manual(&target, None, &format!("{} {}", &code[..4], &code[4..]), true).await.unwrap();
    let saved = read_settings(b.data.path())["manual_peers"].clone();
    assert_eq!(saved, serde_json::json!([format!("127.0.0.1|{}", a.engine.port())]));
    assert_eq!(b.engine.remembered_addresses(), vec![target]);
    b.engine.forget_addresses();
    assert!(b.engine.remembered_addresses().is_empty());
}

/// "localhost" resuelve a 127.0.0.1 y ::1: el anfitrión escucha en ambas.
/// Con un código erróneo debe gastarse UN solo intento, no uno por dirección:
/// tras 4 intentos fallidos el código correcto todavía funciona.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_code_uses_a_single_attempt_even_with_several_addresses() {
    let a = Node::new("A").await;
    let b = Node::new("B").await;
    let code = a.engine.create_group();
    let wrong = if code == "00000000" { "11111111" } else { "00000000" };
    for _ in 0..4 {
        let e = b.engine.join_group_manual("localhost", Some(a.engine.port()), wrong, false).await.unwrap_err();
        assert!(e.contains("Código incorrecto"), "{e}");
    }
    b.engine.join_group_manual("localhost", Some(a.engine.port()), &code, false).await.expect("el código correcto sigue valiendo");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_join_reports_understandable_errors() {
    let b = Node::new("B").await;
    // Puerto cerrado.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let e = b.engine.join_group_manual("127.0.0.1", Some(closed), "12345678", false).await.unwrap_err();
    assert!(e.contains("rechaza la conexión") && e.contains(&format!("127.0.0.1:{closed}")) && e.contains("conexión TCP"), "{e}");
    // Entradas no válidas: no se intenta conectar.
    for (host, code) in [("http://192.168.1.66", "12345678"), ("192.168.1.300", "12345678"), ("", "12345678"), ("127.0.0.1", "123")] {
        assert!(b.engine.join_group_manual(host, None, code, false).await.is_err(), "{host:?}");
    }
    // Nombre que no existe.
    let e = b.engine.join_group_manual("no-existe-home-file-sync.invalid", None, "12345678", false).await.unwrap_err();
    assert!(e.contains("No se encuentra"), "{e}");
    // Un servicio que no es Home File Sync en ese puerto.
    let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = other.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = other.accept().await {
            use tokio::io::AsyncWriteExt;
            let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        }
    });
    let t = Instant::now();
    let e = b.engine.join_group_manual("127.0.0.1", Some(port), "12345678", false).await.unwrap_err();
    assert!(e.contains("cifrado"), "{e}");
    assert!(t.elapsed() < Duration::from_secs(15));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diagnostic_log_records_phases_without_secrets() {
    let dir = init_logging();
    let a = Node::new("Registro A").await;
    let b = Node::new("Registro B").await;
    let code = a.engine.create_group();
    b.engine.join_group_manual("127.0.0.1", Some(a.engine.port()), &code, false).await.unwrap();
    wait_for("sesión", 20, || b.engine.snapshot().devices.iter().any(|d| d.detail == "Sincronizado")).await;
    std::fs::write(a.root.join("registro.txt"), b"x").unwrap();
    wait_for("archivo", 20, || b.root.join("registro.txt").exists()).await;

    let all = diag::as_text(&diag::entries("DEBUG", "", 100_000));
    for needle in [
        "Emparejamiento: direcciones candidatas",
        "Conexión TCP establecida con 127.0.0.1",
        "Emparejamiento completado",
        "Autenticado con",
        "Descarga de «registro.txt»",
        "Envío de «registro.txt»",
    ] {
        assert!(all.contains(needle), "falta «{needle}» en el registro");
    }
    // Nunca el código de unión ni la clave del grupo.
    let key = read_settings(a.data.path())["group"]["key"].as_str().unwrap().to_string();
    let on_disk: String = std::fs::read_dir(&dir).unwrap().flatten().map(|f| std::fs::read_to_string(f.path()).unwrap_or_default()).collect();
    assert!(on_disk.contains("Emparejamiento completado"), "también se escribe en disco");
    for text in [&all, &on_disk] {
        assert!(!text.contains(&code), "el código de unión no debe registrarse");
        assert!(!text.contains(&sfs_core::auth::format_code(&code)), "ni con el formato «1234 5678»");
        assert!(!text.contains(&key), "la clave del grupo no debe registrarse");
    }
    // Filtros.
    assert!(diag::entries("ERROR", "", 1000).iter().all(|e| e.level == "ERROR"));
    assert!(diag::entries("INFO", "emparejamiento completado", 1000).len() >= 1);
    // Exportación.
    let out = tempfile::tempdir().unwrap();
    let f = out.path().join("export.log");
    diag::export(&f, "prueba").unwrap();
    let exported = std::fs::read_to_string(&f).unwrap();
    assert!(exported.starts_with("Home File Sync — registro de diagnóstico") && exported.contains("Autenticado con"));
}

/// Cada unión manual deja en el registro por qué no funcionó la automática.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_join_explains_why_automatic_did_not_work() {
    init_logging();
    let a = Node::new("Invisible").await;
    let b = Node::new("Buscador").await;
    let code = a.engine.create_group();
    b.engine.join_group_manual("127.0.0.1", Some(a.engine.port()), &code, false).await.unwrap();
    let log = diag::as_text(&diag::entries("WARN", "conexión automática no lo consiguió", 100));
    assert!(log.contains("Unión manual con «127.0.0.1» correcta"), "{log}");
    assert!(log.contains("no se había descubierto") && log.contains("desactivado"), "{log}");
}
