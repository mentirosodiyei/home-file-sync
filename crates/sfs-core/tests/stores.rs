//! Mismas comprobaciones para cada servicio externo del relevo.
//!
//! Carpeta y WebDAV se prueban siempre (con un servidor WebDAV local). Google
//! Drive, Dropbox y un NAS real solo si se dan credenciales:
//!   HFS_TEST_GOOGLE_REFRESH=<token>   (y HFS_GOOGLE_CLIENT_ID / _SECRET)
//!   HFS_TEST_DROPBOX_REFRESH=<token>  (y HFS_DROPBOX_APP_KEY)
//!   HFS_TEST_SMB=host|carpeta|usuario|contraseña
//!   cargo test --test stores -- --ignored

use sfs_core::store::{self, RelayTarget, Store};
use std::path::Path;

fn exercise(s: &dyn Store) {
    let base = format!("prueba-{}", std::process::id());
    store::probe(s).expect("escribir y leer");
    s.mkdirs(&format!("{base}/blobs")).unwrap();
    s.mkdirs(&format!("{base}/blobs")).unwrap(); // dos veces: no es error
    s.write(&format!("{base}/idx.hfs"), b"uno").unwrap();
    s.write(&format!("{base}/idx.hfs"), b"dos").unwrap();
    assert_eq!(s.read(&format!("{base}/idx.hfs")).unwrap(), b"dos", "sustituye");
    // Un archivo grande (varias partes en los servicios que suben por partes).
    let big: Vec<u8> = (0..9 * 1024 * 1024 + 123).map(|i: usize| (i % 251) as u8).collect();
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), &big).unwrap();
    s.upload(&format!("{base}/blobs/a.hfs"), tmp.path()).unwrap();
    let mut back = Vec::new();
    std::io::Read::read_to_end(&mut s.reader(&format!("{base}/blobs/a.hfs")).unwrap(), &mut back).unwrap();
    assert!(back == big, "contenido grande íntegro");
    let items = s.list(&format!("{base}/blobs")).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, "a.hfs");
    assert_eq!(items[0].size, big.len() as u64);
    s.mkdirs(&format!("{base}/old/hoy")).unwrap();
    s.rename(&format!("{base}/blobs/a.hfs"), &format!("{base}/old/hoy/a.hfs")).unwrap();
    assert!(s.list(&format!("{base}/blobs")).unwrap().is_empty());
    assert!(s.exists(&format!("{base}/old/hoy/a.hfs")).unwrap());
    s.delete(&format!("{base}/old/hoy/a.hfs")).unwrap();
    s.delete(&format!("{base}/old/hoy/a.hfs")).unwrap(); // ya no existe: no es error
    s.delete(&format!("{base}/idx.hfs")).unwrap();
    assert!(!s.exists(&format!("{base}/idx.hfs")).unwrap());
    assert!(s.list("no-existe").unwrap().is_empty());
    assert_eq!(s.reader("no-existe/x.hfs").err().map(|e| e.kind()), Some(std::io::ErrorKind::NotFound));
}

fn run(target: RelayTarget, secret: Option<&str>) {
    let s = store::open(&target, secret).expect("abrir el servicio");
    exercise(s.as_ref());
}

#[test]
fn folder() {
    let d = tempfile::tempdir().unwrap();
    run(RelayTarget::Folder { path: d.path().to_path_buf() }, None);
    assert!(d.path().join("Home File Sync").is_dir());
}

async fn webdav_server(dir: &Path) -> String {
    use dav_server::{fakels::FakeLs, localfs::LocalFs, DavHandler};
    let dav = DavHandler::builder().filesystem(LocalFs::new(dir, false, false, false)).locksystem(FakeLs::new()).build_handler();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let dav = dav.clone();
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req| {
                    let dav = dav.clone();
                    async move { Ok::<_, std::convert::Infallible>(dav.handle(req).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(hyper_util::rt::TokioIo::new(stream), svc).await;
            });
        }
    });
    url
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webdav() {
    let d = tempfile::tempdir().unwrap();
    let url = webdav_server(d.path()).await;
    tokio::task::spawn_blocking(move || run(RelayTarget::WebDav { url, user: "u".into() }, Some("p"))).await.unwrap();
    assert!(d.path().join("Home File Sync").is_dir());
}

#[test]
fn webdav_refuses_plain_http_outside_home() {
    assert!(store::open(&RelayTarget::WebDav { url: "http://nube.ejemplo.com/dav".into(), user: "u".into() }, Some("p")).is_err());
}

#[test]
#[ignore = "necesita una cuenta de Google y HFS_TEST_GOOGLE_REFRESH"]
fn google_drive() {
    let token = std::env::var("HFS_TEST_GOOGLE_REFRESH").expect("HFS_TEST_GOOGLE_REFRESH");
    run(RelayTarget::GoogleDrive { account: "prueba".into() }, Some(&token));
}

#[test]
#[ignore = "necesita una cuenta de Dropbox y HFS_TEST_DROPBOX_REFRESH"]
fn dropbox() {
    let token = std::env::var("HFS_TEST_DROPBOX_REFRESH").expect("HFS_TEST_DROPBOX_REFRESH");
    run(RelayTarget::Dropbox { account: "prueba".into(), path: "/Home File Sync (pruebas)".into() }, Some(&token));
}

#[test]
#[ignore = "necesita un NAS y HFS_TEST_SMB=host|carpeta|usuario|contraseña"]
fn smb_nas() {
    let v = std::env::var("HFS_TEST_SMB").expect("HFS_TEST_SMB");
    let p: Vec<&str> = v.splitn(4, '|').collect();
    run(RelayTarget::Smb { host: p[0].into(), share: p[1].into(), subpath: String::new(), user: p[2].into() }, Some(p[3]));
}
