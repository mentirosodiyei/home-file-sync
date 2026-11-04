//! Pruebas de integración: varios equipos reales (motores completos) en la
//! misma máquina, conectados por TCP en 127.0.0.1, sin descubrimiento.
//!
//! Cubren emparejamiento, sincronización inicial, duplicados, conflictos,
//! actualizaciones, borrados, desconexiones a mitad de transferencia,
//! corrupción de datos, archivos en escritura y equipos maliciosos.

use sfs_core::{Engine, EngineConfig};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Node {
    engine: Engine,
    data: TempDir,
    root: PathBuf,
    _folder: TempDir,
}

fn cfg(data: &Path, name: &str) -> EngineConfig {
    let mut c = EngineConfig::new(data);
    c.device_name = Some(name.into());
    c.listen_port = Some(0);
    c.discovery = false;
    c.stable_ms = 300;
    c.rescan_secs = 1;
    c.block_size = 64 * 1024;
    c
}

impl Node {
    async fn new(name: &str) -> Node {
        Self::with(name, |_| {}).await
    }

    async fn with(name: &str, f: impl FnOnce(&mut EngineConfig)) -> Node {
        let _ = tracing_subscriber::fmt().with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into())).try_init();
        let data = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let mut c = cfg(data.path(), name);
        f(&mut c);
        let engine = Engine::start(c).await.unwrap();
        let root = engine.set_folder(folder.path()).unwrap();
        Node { engine, data, root, _folder: folder }
    }

    /// Reinicia el motor conservando datos y carpeta (equipo que se apaga y vuelve).
    async fn restart(&mut self, name: &str, f: impl FnOnce(&mut EngineConfig)) {
        self.engine.shutdown().await;
        let mut c = cfg(self.data.path(), name);
        f(&mut c);
        self.engine = Engine::start(c).await.unwrap();
    }

    fn addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.engine.port()))
    }

    fn root(&self) -> PathBuf {
        self.root.clone()
    }

    fn write(&self, rel: &str, data: &[u8]) {
        let p = self.root().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }

    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root().join(rel)).ok()
    }

    /// Archivos visibles (sin la carpeta interna), con su contenido.
    fn files(&self) -> Vec<(String, Vec<u8>)> {
        let root = self.root();
        let mut v: Vec<(String, Vec<u8>)> = walk(&root)
            .into_iter()
            .map(|p| (p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"), std::fs::read(&p).unwrap()))
            .collect();
        v.sort();
        v
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.file_name().unwrap() == ".showfilesync" {
            continue;
        }
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
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

/// Crea un grupo en `a` y une al resto mediante el código real.
async fn group(a: &Node, others: &[&Node]) {
    let code = a.engine.create_group();
    for n in others {
        n.engine.join_group_at(a.addr(), &code).await.expect("emparejamiento");
    }
}

fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_and_basic_sync_with_special_names() {
    let a = Node::new("Mac Escenario").await;
    let b = Node::new("PC Presentación 1").await;
    let c = Node::new("PC Presentación 2").await;
    a.write("Día 1/Sala Ñ/Presentación Final (v2) #3.pptx", b"diapositivas");
    a.write("vídeo 😀 con espacios.mp4", &pseudo_random(3_000_000, 1));
    group(&a, &[&b, &c]).await;

    wait_for("B y C reciben los archivos", 30, || b.files() == a.files() && c.files() == a.files() && a.files().len() == 2).await;
    // Ahora un archivo nuevo en un equipo cualquiera llega a los demás.
    c.write("Nuevo desde C.pdf", b"%PDF-1.7");
    wait_for("archivo de C llega a A y B", 30, || a.read("Nuevo desde C.pdf").is_some() && b.read("Nuevo desde C.pdf").is_some()).await;
    let s = a.engine.snapshot();
    assert_eq!(s.devices.len(), 2, "A ve a B y C: {:?}", s.devices);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_code_is_rejected_and_pairing_locks() {
    let a = Node::new("A").await;
    let b = Node::new("B").await;
    let code = a.engine.create_group();
    let wrong = if code == "00000000" { "11111111" } else { "00000000" };
    for _ in 0..5 {
        let e = b.engine.join_group_at(a.addr(), wrong).await.unwrap_err();
        assert!(e.contains("ncorrecto"), "{e}");
    }
    let e = b.engine.join_group_at(a.addr(), &code).await.unwrap_err();
    assert!(e.contains("intentos"), "tras 5 fallos el código se bloquea: {e}");
    assert!(b.engine.snapshot().group.is_none());
    // Un código nuevo sí funciona.
    let code = a.engine.open_pairing().unwrap();
    b.engine.join_group_at(a.addr(), &code).await.unwrap();
    assert_eq!(b.engine.snapshot().group.unwrap().id, a.engine.snapshot().group.unwrap().id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identical_files_are_not_transferred() {
    let a = Node::new("A").await;
    let b = Node::new("B").await;
    let data = pseudo_random(5_000_000, 7);
    a.write("Ponencia.pptx", &data);
    b.write("Ponencia.pptx", &data);
    a.write("solo-en-a.txt", b"a");
    group(&a, &[&b]).await;
    wait_for("sincronizado", 30, || b.read("solo-en-a.txt").is_some()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(a.files(), b.files());
    assert_eq!(a.files().len(), 2, "sin copias de conflicto");
    assert!(b.engine.stats().bytes_down < 1000, "no debe volver a transferir el archivo idéntico");
    assert_eq!(a.engine.stats().bytes_down, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_edits_keep_both_copies() {
    let a = Node::new("Mac A").await;
    let b = Node::new("PC B").await;
    a.write("Final.pptx", b"version de A");
    b.write("Final.pptx", b"version de B, mas reciente");
    let old = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    let new = filetime::FileTime::from_unix_time(1_700_000_100, 0);
    filetime::set_file_mtime(a.root().join("Final.pptx"), old).unwrap();
    filetime::set_file_mtime(b.root().join("Final.pptx"), new).unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    group(&a, &[&b]).await;
    wait_for("ambos con las dos versiones", 30, || a.files().len() == 2 && a.files() == b.files()).await;
    let files = a.files();
    assert_eq!(a.read("Final.pptx").unwrap(), b"version de B, mas reciente");
    let copy = files.iter().find(|(p, _)| p.contains("conflicto")).expect("copia de conflicto");
    assert_eq!(copy.1, b"version de A");
    assert!(copy.0.starts_with("old/Final (conflicto - Mac A - "), "la perdedora va a old/: {}", copy.0);
    assert!(a.engine.snapshot().conflicts >= 1);
    // Estable: no se generan más copias.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(a.files().len(), 2);
    assert_eq!(b.files().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_propagate_and_old_version_is_kept() {
    let a = Node::new("A").await;
    let b = Node::new("B").await;
    a.write("guion.pdf", b"v1");
    group(&a, &[&b]).await;
    wait_for("v1 en B", 30, || b.read("guion.pdf").as_deref() == Some(b"v1")).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    a.write("guion.pdf", b"v2 corregida");
    wait_for("v2 en B", 30, || b.read("guion.pdf").as_deref() == Some(b"v2 corregida")).await;
    // La versión sustituida va a la carpeta visible old/ con su fecha…
    let old_v1 = |n: &Node| n.files().into_iter().any(|(p, d)| p.starts_with("old/guion (") && p.ends_with(").pdf") && d == b"v1");
    wait_for("v1 en old/ de B", 10, || old_v1(&b)).await;
    // …y como old/ se sincroniza, también llega al equipo donde se editó.
    wait_for("v1 en old/ de A", 30, || old_v1(&a)).await;
    // Un cambio en B vuelve a A, y la v2 queda en old/ en ambos.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    b.write("guion.pdf", b"v3 desde B");
    wait_for("v3 en A", 30, || a.read("guion.pdf").as_deref() == Some(b"v3 desde B")).await;
    let old_v2 = |n: &Node| n.files().into_iter().any(|(p, d)| p.starts_with("old/guion (") && d == b"v2 corregida");
    wait_for("v2 en old/ de A y B", 30, || old_v2(&a) && old_v2(&b)).await;
    assert!(!a.files().iter().any(|(p, _)| p.contains("conflicto")));
    // Sin duplicados: cada versión antigua una sola vez, igual en ambos.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(a.files(), b.files());
    assert_eq!(a.files().len(), 3, "{:?}", a.files().iter().map(|f| &f.0).collect::<Vec<_>>());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deletions_propagate_to_everyone_and_go_to_trash() {
    let a = Node::new("A").await;
    let b = Node::new("B").await;
    let c = Node::new("C").await;
    a.write("borrame.pptx", b"importante");
    a.write("Dia 1/Sala A/ponencia.key", b"key");
    a.write("Dia 1/Sala A/notas.pdf", b"pdf");
    a.write("se queda.txt", b"ok");
    group(&a, &[&b, &c]).await;
    wait_for("todos completos", 30, || b.files().len() == 4 && c.files().len() == 4).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Borrar un archivo en A lo borra en B y C.
    std::fs::remove_file(a.root().join("borrame.pptx")).unwrap();
    wait_for("borrado en B y C", 30, || b.read("borrame.pptx").is_none() && c.read("borrame.pptx").is_none()).await;
    // Se conserva en la papelera interna de quien lo recibe.
    let trash = b.root().join(".showfilesync/papelera");
    assert!(walk(&trash).iter().any(|p| std::fs::read(p).ok().as_deref() == Some(b"importante")), "copia en la papelera");

    // Borrar una carpeta entera en B la borra en A y C, sin dejar carpetas vacías.
    std::fs::remove_dir_all(b.root().join("Dia 1")).unwrap();
    wait_for("carpeta borrada en A y C", 30, || !a.root().join("Dia 1").exists() && !c.root().join("Dia 1").exists()).await;

    // Estable: nada reaparece y lo demás sigue.
    tokio::time::sleep(Duration::from_secs(3)).await;
    for n in [&a, &b, &c] {
        assert_eq!(n.files(), vec![("se queda.txt".to_string(), b"ok".to_vec())]);
    }
    // Volver a crear un archivo borrado lo vuelve a sincronizar.
    c.write("borrame.pptx", b"nueva");
    wait_for("vuelve a A y B", 30, || a.read("borrame.pptx").as_deref() == Some(b"nueva") && b.read("borrame.pptx").as_deref() == Some(b"nueva")).await;
}

/// Si un equipo edita un archivo mientras otro lo borra, gana la edición.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edit_wins_over_concurrent_deletion() {
    let mut a = Node::new("A").await;
    let mut b = Node::new("B").await;
    a.write("guion.pptx", b"v1");
    group(&a, &[&b]).await;
    wait_for("en B", 30, || b.read("guion.pptx").is_some()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // Sin conexión entre ellos: A borra, B edita.
    b.engine.shutdown().await;
    std::fs::remove_file(a.root().join("guion.pptx")).unwrap();
    wait_for("A registra el borrado", 20, || a.engine.snapshot().file_count == 0).await;
    a.engine.shutdown().await;
    b.restart("B", |_| {}).await;
    b.write("guion.pptx", b"v2 editada en B");
    wait_for("B registra la edición", 20, || b.engine.snapshot().file_count == 1 && b.engine.snapshot().total_bytes == 15).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    a.restart("A", |_| {}).await;
    b.engine.connect_to(a.addr());
    wait_for("la edición vuelve a A", 30, || a.read("guion.pptx").as_deref() == Some(b"v2 editada en B")).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(b.read("guion.pptx").as_deref(), Some(&b"v2 editada en B"[..]), "B no la pierde");
}

/// Renombrar o mover: el archivo aparece con el nombre nuevo sin volver a
/// transferirse y desaparece con el antiguo.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn renames_are_mirrored_without_retransfer() {
    let a = Node::new("A").await;
    let b = Node::new("B").await;
    let data = pseudo_random(3_000_000, 21);
    a.write("Video ensayo.mov", &data);
    group(&a, &[&b]).await;
    wait_for("en B", 30, || b.read("Video ensayo.mov").is_some()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let before = b.engine.stats().bytes_down;
    std::fs::create_dir_all(a.root().join("Final")).unwrap();
    std::fs::rename(a.root().join("Video ensayo.mov"), a.root().join("Final/Video definitivo.mov")).unwrap();
    wait_for("renombrado en B", 30, || b.read("Final/Video definitivo.mov").as_deref() == Some(&data[..]) && b.read("Video ensayo.mov").is_none()).await;
    assert_eq!(b.engine.stats().bytes_down, before, "se copia en local, no por la red");
}

/// Disco desmontado o carpeta sustituida (desaparece todo, incluida la
/// carpeta interna): no se borra nada en los demás y se vuelve a llenar.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn emptied_folder_without_meta_is_not_propagated_as_deletion() {
    let a = Node::new("A").await;
    let b = Node::new("B").await;
    a.write("presentacion.pptx", b"importante");
    a.write("video.mp4", b"video");
    group(&a, &[&b]).await;
    wait_for("en B", 30, || b.files().len() == 2).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    for e in std::fs::read_dir(a.root()).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() { std::fs::remove_dir_all(p).unwrap() } else { std::fs::remove_file(p).unwrap() }
    }
    wait_for("A se vuelve a llenar", 30, || a.files().len() == 2).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(b.files().len(), 2, "B no borra nada");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupted_transfer_resumes() {
    let a = Node::new("A").await;
    let mut b = Node::with("B", |c| c.throttle_bytes_per_sec = Some(4_000_000)).await;
    let size = 12_000_000;
    let data = pseudo_random(size, 3);
    a.write("video grande.mov", &data);
    group(&a, &[&b]).await;
    let hash = blake3::hash(&data).to_hex().to_string();
    let part = b.root().join(".showfilesync/tmp").join(format!("{hash}.sfspart"));
    wait_for("descarga a medias", 30, || std::fs::metadata(&part).map(|m| m.len() > 3_000_000).unwrap_or(false)).await;
    let first = b.engine.stats().bytes_down;
    // "Se desconecta el cable": el equipo B desaparece.
    b.engine.shutdown().await;
    let partial = std::fs::metadata(&part).unwrap().len();
    assert!(partial > 0 && (partial as usize) < size, "queda un temporal parcial: {partial}");
    assert!(b.read("video grande.mov").is_none(), "nunca aparece un archivo a medias");
    // Vuelve la red.
    b.restart("B", |_| {}).await;
    b.engine.connect_to(a.addr());
    wait_for("archivo completo en B", 60, || b.read("video grande.mov").map(|d| d.len()) == Some(size)).await;
    assert_eq!(b.read("video grande.mov").unwrap(), data);
    let second = b.engine.stats().bytes_down;
    assert!(
        (first + second) < size as u64 + size as u64 / 2,
        "debe reanudar, no empezar de cero (primera {first}, segunda {second})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrupted_partial_is_detected_and_redownloaded() {
    let a = Node::new("A").await;
    let mut b = Node::with("B", |c| c.throttle_bytes_per_sec = Some(4_000_000)).await;
    let data = pseudo_random(8_000_000, 9);
    a.write("clip.mp4", &data);
    group(&a, &[&b]).await;
    let hash = blake3::hash(&data).to_hex().to_string();
    let part = b.root().join(".showfilesync/tmp").join(format!("{hash}.sfspart"));
    wait_for("descarga a medias", 30, || std::fs::metadata(&part).map(|m| m.len() > 1_000_000).unwrap_or(false)).await;
    b.engine.shutdown().await;
    // Corrompemos el temporal (fallo de disco, apagado brusco…).
    let mut p = std::fs::read(&part).unwrap();
    for x in p.iter_mut().take(1000) {
        *x ^= 0xff;
    }
    std::fs::write(&part, p).unwrap();
    b.restart("B", |_| {}).await;
    b.engine.connect_to(a.addr());
    wait_for("archivo correcto en B", 60, || b.read("clip.mp4").as_deref() == Some(&data[..])).await;
    assert!(b.engine.stats().hash_failures >= 1, "la corrupción debe detectarse");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_joiner_receives_everything_and_rejoin_catches_up() {
    let a = Node::new("A").await;
    let b = Node::new("B").await;
    for i in 0..20 {
        a.write(&format!("carpeta {}/archivo {i}.txt", i % 3), format!("contenido {i}").as_bytes());
    }
    group(&a, &[&b]).await;
    wait_for("B completo", 30, || b.files().len() == 20).await;
    // Un equipo nuevo se une más tarde.
    let c = Node::new("C").await;
    let code = a.engine.open_pairing().unwrap();
    c.engine.join_group_at(a.addr(), &code).await.unwrap();
    wait_for("C completo", 30, || c.files() == a.files()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_being_written_is_not_copied_until_stable() {
    let a = Node::new("A").await;
    let b = Node::new("B").await;
    group(&a, &[&b]).await;
    wait_for("conectados", 20, || b.engine.snapshot().devices.iter().any(|d| d.detail == "Sincronizado")).await;
    let path = a.root().join("grabando.mov");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..20u8 {
            f.write_all(&vec![i; 50_000]).unwrap();
            f.flush().unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(b.read("grabando.mov").is_none(), "no se copia mientras se escribe");
        }
    }
    let full = std::fs::read(&path).unwrap();
    wait_for("archivo completo en B", 30, || b.read("grabando.mov").as_deref() == Some(&full[..])).await;
    assert_eq!(b.engine.stats().files_received, 1, "una sola transferencia, sin versiones parciales");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_peers_get_one_copy_each() {
    let a = Node::new("A").await;
    let mut others = Vec::new();
    for i in 0..5 {
        others.push(Node::new(&format!("PC {i}")).await);
    }
    let data = pseudo_random(4_000_000, 11);
    a.write("Keynote.key", &data);
    group(&a, &others.iter().collect::<Vec<_>>()).await;
    wait_for("todos lo tienen", 60, || others.iter().all(|n| n.read("Keynote.key").as_deref() == Some(&data[..]))).await;
    for n in &others {
        let s = n.engine.stats();
        assert_eq!(s.files_received, 1);
        assert!(s.bytes_down <= data.len() as u64, "sin descargas duplicadas: {}", s.bytes_down);
    }
}

/// Un equipo con la clave del grupo pero comportamiento malicioso no puede
/// escribir ni leer fuera de la carpeta.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malicious_peer_cannot_escape_folder() {
    use sfs_core::auth::group_proof;
    use sfs_core::protocol::{Msg, PROTOCOL_VERSION};
    use sfs_core::transport::{generate_keypair, handshake};

    let a = Node::new("A").await;
    let code = a.engine.create_group();
    // El atacante se empareja legítimamente para obtener la clave (peor caso).
    let evil = Node::new("Evil").await;
    evil.engine.join_group_at(a.addr(), &code).await.unwrap();
    let s = sfs_core::settings::Settings::load_or_create(evil.data.path(), None).unwrap();
    let group = s.group.unwrap();
    evil.engine.shutdown().await;

    let secret_dir = tempfile::tempdir().unwrap();
    std::fs::write(secret_dir.path().join("secreto.txt"), b"no deberias leer esto").unwrap();
    let (key, _) = generate_keypair();
    let stream = tokio::net::TcpStream::connect(a.addr()).await.unwrap();
    let mut h = handshake(stream, &key, true).await.unwrap();
    h.writer
        .send(&Msg::Hello {
            proto: PROTOCOL_VERSION,
            name: "Evil".into(),
            group_id: group.id.clone(),
            proof: group_proof(&group.key_bytes(), "client", &h.handshake_hash),
        })
        .await
        .unwrap();
    assert!(matches!(h.reader.recv().await.unwrap(), Msg::Welcome { .. }));

    // Anuncia archivos con rutas peligrosas e intenta leer fuera.
    let evil_entry = |path: &str| sfs_core::index::FileEntry {
        path: path.into(),
        size: 4,
        mtime_ns: 0,
        hash: "a".repeat(64),
        vv: Default::default(),
        deleted: false,
        modified_by: "Evil".into(),
    };
    h.writer
        .send(&Msg::IndexPage {
            reset: true,
            last: true,
            entries: vec![evil_entry("../fuera.txt"), evil_entry("/etc/evil"), evil_entry(".showfilesync/tmp/x")],
        })
        .await
        .unwrap();
    let target = format!("../../{}", secret_dir.path().join("secreto.txt").display());
    h.writer.send(&Msg::DlOpen { id: 1, path: target, hash: "0".repeat(64) }).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got_err = false;
    while Instant::now() < deadline && !got_err {
        match tokio::time::timeout(Duration::from_secs(5), h.reader.recv()).await {
            Ok(Ok(Msg::DlErr { id: 1, .. })) => got_err = true,
            Ok(Ok(Msg::Block { id: 1, .. })) => panic!("¡ha enviado datos del archivo pedido!"),
            Ok(Ok(Msg::DlOpenOk { .. })) => panic!("¡ha abierto un archivo fuera de la carpeta!"),
            Ok(Ok(_)) => {}
            _ => break,
        }
    }
    assert!(got_err, "debe rechazar la lectura");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!a.root().parent().unwrap().join("fuera.txt").exists());
    assert!(a.files().is_empty(), "no se crea nada: {:?}", a.files());
}

/// Un equipo de otro grupo (o sin clave) no puede conectarse.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outsiders_are_rejected() {
    let a = Node::new("A").await;
    let b = Node::new("B").await;
    let outsider = Node::new("Cliente").await;
    a.write("confidencial.pptx", b"secreto");
    group(&a, &[&b]).await;
    outsider.engine.create_group(); // otro grupo distinto
    outsider.engine.connect_to(a.addr());
    wait_for("B sincronizado", 30, || b.read("confidencial.pptx").is_some()).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(outsider.files().is_empty());
    assert!(a.engine.snapshot().devices.iter().all(|d| d.name != "Cliente"));
}
