//! Utilidades compartidas por las pruebas de integración.

use std::path::Path;

/// Sistema de archivos de prueba: `LocalFs` de dav-server, pero con una corrección.
///
/// En Windows, dav-server no traduce «la carpeta ya existe» (ERROR_ALREADY_EXISTS)
/// y responde 500 a un MKCOL repetido; un servidor real (Nextcloud, un NAS) responde
/// 405, que el cliente ya acepta. Aquí se devuelve el mismo 405 para que la prueba
/// compruebe el cliente y no los fallos del servidor de prueba.
#[derive(Clone)]
struct ExistsAware(dav_server::localfs::LocalFs);

impl dav_server::fs::DavFileSystem for ExistsAware {
    fn open<'a>(
        &'a self,
        path: &'a dav_server::davpath::DavPath,
        options: dav_server::fs::OpenOptions,
    ) -> dav_server::fs::FsFuture<'a, Box<dyn dav_server::fs::DavFile>> {
        self.0.open(path, options)
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a dav_server::davpath::DavPath,
        meta: dav_server::fs::ReadDirMeta,
    ) -> dav_server::fs::FsFuture<'a, dav_server::fs::FsStream<Box<dyn dav_server::fs::DavDirEntry>>> {
        self.0.read_dir(path, meta)
    }

    fn metadata<'a>(&'a self, path: &'a dav_server::davpath::DavPath) -> dav_server::fs::FsFuture<'a, Box<dyn dav_server::fs::DavMetaData>> {
        self.0.metadata(path)
    }

    fn symlink_metadata<'a>(&'a self, path: &'a dav_server::davpath::DavPath) -> dav_server::fs::FsFuture<'a, Box<dyn dav_server::fs::DavMetaData>> {
        self.0.symlink_metadata(path)
    }

    fn create_dir<'a>(&'a self, path: &'a dav_server::davpath::DavPath) -> dav_server::fs::FsFuture<'a, ()> {
        Box::pin(async move {
            match self.0.create_dir(path).await {
                Err(dav_server::fs::FsError::GeneralFailure) => match self.0.metadata(path).await {
                    Ok(m) if m.is_dir() => Err(dav_server::fs::FsError::Exists),
                    _ => Err(dav_server::fs::FsError::GeneralFailure),
                },
                other => other,
            }
        })
    }

    fn remove_dir<'a>(&'a self, path: &'a dav_server::davpath::DavPath) -> dav_server::fs::FsFuture<'a, ()> {
        self.0.remove_dir(path)
    }

    fn remove_file<'a>(&'a self, path: &'a dav_server::davpath::DavPath) -> dav_server::fs::FsFuture<'a, ()> {
        self.0.remove_file(path)
    }

    fn rename<'a>(&'a self, from: &'a dav_server::davpath::DavPath, to: &'a dav_server::davpath::DavPath) -> dav_server::fs::FsFuture<'a, ()> {
        self.0.rename(from, to)
    }

    fn copy<'a>(&'a self, from: &'a dav_server::davpath::DavPath, to: &'a dav_server::davpath::DavPath) -> dav_server::fs::FsFuture<'a, ()> {
        self.0.copy(from, to)
    }
}

/// Servidor WebDAV de prueba (como un Nextcloud o un NAS) sobre `dir`. Devuelve su URL.
pub async fn webdav_server(dir: &Path) -> String {
    use dav_server::{fakels::FakeLs, localfs::LocalFs, DavHandler};
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    let fs = ExistsAware(*LocalFs::new(dir, false, false, false));
    let dav = DavHandler::builder().filesystem(Box::new(fs)).locksystem(FakeLs::new()).build_handler();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let dav = dav.clone();
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(move |req| {
                            let dav = dav.clone();
                            async move { Ok::<_, std::convert::Infallible>(dav.handle(req).await) }
                        }),
                    )
                    .await;
            });
        }
    });
    url
}
