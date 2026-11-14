//! Varias carpetas compartidas por equipo (uso doméstico): grupos distintos,
//! modo copia de seguridad, nombres locales y recepción desde el navegador.

use sfs_core::hub::{Hub, HubConfig};
use sfs_core::settings::ShareMode;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Home {
    hub: Hub,
    _data: TempDir,
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
        Home { hub, _data: data, dirs: Vec::new() }
    }

    /// Carpeta local nueva (ya canonicalizada).
    fn folder(&mut self) -> PathBuf {
        let d = tempfile::tempdir().unwrap();
        let p = std::fs::canonicalize(d.path()).unwrap();
        self.dirs.push(d);
        p
    }

    fn addr(&self, share: &str) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.hub.share(share).unwrap().port()))
    }

    fn device_names(&self, share: &str) -> Vec<String> {
        let s = self.hub.snapshot();
        s.shares.iter().find(|x| x.id == share).unwrap().state.devices.iter().map(|d| d.name.clone()).collect()
    }
}

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = walk(dir).iter().map(|p| p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/")).collect();
    v.sort();
    v
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.file_name().unwrap() == ".homefilesync" {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn family_with_separate_shared_folders() {
    let mut padre = Home::new("Portátil Papá").await;
    let mut madre = Home::new("Mac Mamá").await;
    let mut hijo = Home::new("PC Hijo").await;

    // La pareja comparte una carpeta.
    let p_pareja = padre.folder();
    let (pareja, code) = padre.hub.create_share("Pareja", &p_pareja, ShareMode::Sync).await.unwrap();
    let m_pareja = madre.folder();
    let m_id = madre.hub.join_share_at(padre.addr(&pareja), &code, &m_pareja, ShareMode::Sync).await.unwrap();

    // El padre comparte otra con el hijo.
    let p_hijo = padre.folder();
    let (ph, code) = padre.hub.create_share("Papá e hijo", &p_hijo, ShareMode::Sync).await.unwrap();
    let h_dir = hijo.folder();
    let h_id = hijo.hub.join_share_at(padre.addr(&ph), &code, &h_dir, ShareMode::Sync).await.unwrap();

    // Cada carpeta llega solo a quien la comparte.
    std::fs::write(p_pareja.join("vacaciones.jpg"), b"foto").unwrap();
    std::fs::write(h_dir.join("deberes.pdf"), b"pdf").unwrap();
    wait_for("foto en la madre", 30, || m_pareja.join("vacaciones.jpg").exists()).await;
    wait_for("deberes en el padre", 30, || p_hijo.join("deberes.pdf").exists()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(files(&m_pareja), vec!["vacaciones.jpg"], "la madre no recibe la carpeta del hijo");
    assert_eq!(files(&h_dir), vec!["deberes.pdf"], "el hijo no recibe la carpeta de la pareja");
    assert_eq!(files(&p_pareja), vec!["vacaciones.jpg"]);

    // El padre ve dos carpetas; el nombre de cada una es el que se le dio.
    let s = padre.hub.snapshot();
    assert_eq!(s.shares.len(), 2);
    assert_eq!(madre.hub.snapshot().shares[0].state.group.as_ref().unwrap().name, "Pareja");
    assert_eq!(hijo.hub.snapshot().shares[0].id, h_id);

    // Una carpeta no puede usarse (ni anidarse) en dos carpetas compartidas.
    assert!(padre.hub.create_share("Otra", &p_pareja, ShareMode::Sync).await.is_err());
    let nested = p_pareja.join("sub");
    std::fs::create_dir_all(&nested).unwrap();
    assert!(padre.hub.create_share("Otra", &nested, ShareMode::Sync).await.is_err());

    // Nombres locales: el padre renombra el equipo de la madre solo para él.
    let madre_id = madre.hub.device_id();
    padre.hub.set_alias(&madre_id, "Ana");
    wait_for("la madre aparece como Ana para el padre", 10, || padre.device_names(&pareja) == vec!["Ana"]).await;
    assert_eq!(madre.hub.snapshot().device_name, "Mac Mamá", "su propio nombre no cambia");
    let names_for_madre = madre.device_names(&m_id);
    assert_eq!(names_for_madre, vec!["Portátil Papá"], "los demás no ven el alias del padre");

    // Salir de una carpeta no toca sus archivos y no afecta a la otra.
    madre.hub.leave_share(&m_id).await.unwrap();
    assert!(m_pareja.join("vacaciones.jpg").exists());
    assert!(madre.hub.snapshot().shares.is_empty());
    std::fs::write(p_hijo.join("nuevo.txt"), b"1").unwrap();
    wait_for("la carpeta del hijo sigue", 30, || h_dir.join("nuevo.txt").exists()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_folder_only_receives_and_keeps_deleted_files() {
    let mut origen = Home::new("Origen").await;
    let mut copia = Home::new("Disco de copias").await;
    let o = origen.folder();
    std::fs::write(o.join("foto.jpg"), b"v1").unwrap();
    std::fs::write(o.join("borrar.txt"), b"x").unwrap();
    let (id, code) = origen.hub.create_share("Fotos", &o, ShareMode::Sync).await.unwrap();
    let c = copia.folder();
    copia.hub.join_share_at(origen.addr(&id), &code, &c, ShareMode::Backup).await.unwrap();
    wait_for("copia inicial", 30, || c.join("foto.jpg").exists() && c.join("borrar.txt").exists()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Lo borrado en el origen se conserva en la copia.
    std::fs::remove_file(o.join("borrar.txt")).unwrap();
    // Una edición en el origen sí llega (y la anterior queda en old/).
    std::fs::write(o.join("foto.jpg"), b"v2 editada").unwrap();
    wait_for("edición en la copia", 30, || std::fs::read(c.join("foto.jpg")).ok().as_deref() == Some(b"v2 editada")).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(c.join("borrar.txt").exists(), "la copia conserva lo borrado en el origen");
    assert!(files(&c).iter().any(|f| f.starts_with("old/foto (")), "{:?}", files(&c));

    // Lo que se crea en la copia no viaja al origen.
    std::fs::write(c.join("solo en la copia.txt"), b"local").unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(!o.join("solo en la copia.txt").exists());
    // El origen ve la copia como copia de seguridad.
    let s = origen.hub.snapshot();
    assert!(s.shares[0].state.devices.iter().any(|d| d.backup && d.name == "Disco de copias"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_can_send_files_through_the_qr_link() {
    let mut casa = Home::new("Mac Casa").await;
    let mut otro = Home::new("PC Salón").await;
    let f = casa.folder();
    let (id, code) = casa.hub.create_share("Familia", &f, ShareMode::Sync).await.unwrap();
    let g = otro.folder();
    otro.hub.join_share_at(casa.addr(&id), &code, &g, ShareMode::Sync).await.unwrap();

    let view = casa.hub.start_receive(Some(&id)).unwrap();
    assert!(view.qr_svg.starts_with("<?xml") || view.qr_svg.contains("<svg"), "QR en SVG");
    let token = view.url.rsplit('/').next().unwrap().to_string();
    let port = casa.hub.inbox_port().unwrap();

    // La página del enlace se abre (como en el móvil).
    // En el idioma del móvil: castellano, catalán, y si no, inglés.
    let open = |lang: &'static str| {
        let head = format!("GET /r/{token} HTTP/1.1\r\nHost: x\r\nAccept-Language: {lang}\r\n\r\n");
        async move { http(port, &head, b"").await }
    };
    let page = open("es-ES,es;q=0.9").await;
    assert!(page.starts_with("HTTP/1.1 200") && page.contains("Enviar archivos a «Mac Casa»") && page.contains("lang=\"es\""), "{page}");
    assert!(open("ca-ES").await.contains("Envia fitxers a «Mac Casa»"));
    let en = open("fr-FR,de;q=0.8").await;
    assert!(en.contains("Send files to “Mac Casa”") && en.contains("the shared folder “Familia”"), "{en}");

    // Subida de una foto con nombre "peligroso".
    let body = b"contenido de la foto";
    let resp = http(
        port,
        &format!("POST /r/{token}/upload?name=..%2F..%2FFoto%20de%20Ana%C3%B1.jpg HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n", body.len()),
        body,
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 200") && resp.contains("\"ok\":true"), "{resp}");
    assert_eq!(std::fs::read(f.join("Foto de Anañ.jpg")).unwrap(), body, "se guarda dentro de la carpeta, sin rutas");
    // Mismo nombre otra vez: no sobrescribe.
    http(port, &format!("POST /r/{token}/upload?name=Foto%20de%20Ana%C3%B1.jpg HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\n"), b"dos").await;
    assert_eq!(std::fs::read(f.join("Foto de Anañ (2).jpg")).unwrap(), b"dos");
    // Llega al otro equipo de la carpeta compartida.
    wait_for("la foto llega al salón", 30, || g.join("Foto de Anañ.jpg").exists()).await;
    assert_eq!(casa.hub.snapshot().receive.unwrap().received.len(), 2);

    // Sin token válido no se acepta nada; al cerrar, el enlace deja de valer.
    let bad = http(port, "POST /r/00000000000000000000000000000000/upload?name=x HTTP/1.1\r\nContent-Length: 1\r\n\r\n", b"x").await;
    assert!(bad.starts_with("HTTP/1.1 404"), "{bad}");
    casa.hub.stop_receive();
    let closed = http(port, &format!("GET /r/{token} HTTP/1.1\r\n\r\n"), b"").await;
    assert!(closed.starts_with("HTTP/1.1 404"));
    assert!(!f.join("x").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shares_survive_restart() {
    let data = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let mk = |name: &str| {
        let mut cfg = HubConfig::new(data.path());
        cfg.device_name = Some(name.into());
        cfg.discovery = false;
        cfg.inbox_port = Some(0);
        cfg.engine.listen_port = Some(0);
        cfg
    };
    let hub = Hub::start(mk("Equipo")).await.unwrap();
    let (id, _) = hub.create_share("Documentos", folder.path(), ShareMode::Backup).await.unwrap();
    let dev = hub.device_id();
    hub.shutdown().await;
    let hub = Hub::start(mk("Equipo")).await.unwrap();
    let s = hub.snapshot();
    assert_eq!(hub.device_id(), dev, "misma identidad");
    assert_eq!(s.shares.len(), 1);
    assert_eq!(s.shares[0].id, id);
    assert_eq!(s.shares[0].state.mode, ShareMode::Backup);
    assert_eq!(s.shares[0].state.group.as_ref().unwrap().name, "Documentos");
}

async fn http(port: u16, head: &str, body: &[u8]) -> String {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

/// Descubrimiento real: B ve la invitación abierta de A y se une solo con el código.
/// Necesita mDNS/broadcast en la red local: `cargo test -- --ignored`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "necesita mDNS/broadcast en la red local"]
async fn join_by_code_with_real_discovery() {
    let mk = |data: &Path, name: &str| {
        let mut cfg = HubConfig::new(data);
        cfg.device_name = Some(name.into());
        cfg.inbox_port = Some(0);
        cfg.engine.listen_port = Some(0);
        cfg.engine.stable_ms = 300;
        cfg
    };
    let (da, db, fa, fb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = Hub::start(mk(da.path(), "Prueba A")).await.unwrap();
    let b = Hub::start(mk(db.path(), "Prueba B")).await.unwrap();
    let (_, code) = a.create_share("Prueba descubrimiento", fa.path(), ShareMode::Sync).await.unwrap();
    std::fs::write(fa.path().join("hola.txt"), b"hola").unwrap();
    wait_for("B ve la invitación", 20, || b.invites().iter().any(|i| i.group_name == "Prueba descubrimiento")).await;
    let key = b.invites().into_iter().find(|i| i.group_name == "Prueba descubrimiento").unwrap().key;
    b.join_share(Some(&key), &code, fb.path(), ShareMode::Sync).await.unwrap();
    wait_for("archivo en B", 30, || fb.path().join("hola.txt").exists()).await;
    a.shutdown().await;
    b.shutdown().await;
}

/// Conexiones por túnel VPN: rechazadas salvo que se permitan en Ajustes.
/// Usa un túnel real de este equipo (WireGuard, utun…); si no hay, se omite.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vpn_connections_follow_the_setting() {
    let Some(tun) = sfs_core::netpath::local_interfaces().into_iter().find(|i| i.tunnel && i.ip.is_ipv4()) else {
        eprintln!("(sin túnel VPN en este equipo: prueba omitida)");
        return;
    };
    let mut casa = Home::new("Casa").await;
    let mut fuera = Home::new("Portátil fuera").await;
    let f = casa.folder();
    std::fs::write(f.join("por vpn.txt"), b"hola").unwrap();
    let (id, code) = casa.hub.create_share("Casa", &f, ShareMode::Sync).await.unwrap();
    let via_vpn = SocketAddr::new(tun.ip, casa.hub.share(&id).unwrap().port());

    // Desactivado (por defecto): la conexión por el túnel se rechaza.
    casa.hub.set_listen_vpn(false);
    let g = fuera.folder();
    let e = fuera.hub.join_share_at(via_vpn, &code, &g, ShareMode::Sync).await.unwrap_err();
    assert!(e.contains("cifrado") || e.contains("conexión") || e.contains("conectar"), "{e}");
    assert!(casa.hub.network().addresses.iter().any(|a| a.tunnel && a.ip == tun.ip.to_string()));

    // Activado: misma dirección, ahora funciona y sincroniza.
    casa.hub.set_listen_vpn(true);
    let code = casa.hub.open_invite(&id).unwrap();
    fuera.hub.join_share_at(via_vpn, &code, &g, ShareMode::Sync).await.unwrap();
    wait_for("archivo por la VPN", 30, || g.join("por vpn.txt").exists()).await;

    // Al desactivarlo, la sesión que llegó por el túnel se cierra.
    casa.hub.set_listen_vpn(false);
    wait_for("sesión por VPN cerrada", 20, || casa.hub.snapshot().shares[0].state.devices.iter().all(|d| d.detail != "Sincronizado")).await;
    sfs_core::netpath::set_listen_vpn(false);
}
