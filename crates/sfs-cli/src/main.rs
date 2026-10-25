//! `showfilesync-cli`: Show File Sync sin interfaz gráfica.
//!
//! Útil para equipos sin pantalla, para diagnóstico y para probar varias
//! instancias en una misma máquina (cada una con su `--data`).
//!
//! ```text
//! showfilesync-cli --data DIR --folder CARPETA [opciones] <orden>
//!
//! Órdenes:
//!   crear            crea un grupo nuevo y muestra el código
//!   unir CÓDIGO      se une al grupo que muestra ese código
//!   ejecutar         sincroniza con el grupo ya configurado
//!
//! Opciones:
//!   --nombre NOMBRE      nombre del equipo
//!   --puerto N           puerto TCP (por defecto 47800 o uno libre)
//!   --sin-descubrimiento desactiva mDNS y baliza
//!   --conectar IP:PUERTO conecta con esa dirección (repetible)
//!   --json               imprime el estado completo en JSON
//! ```

use sfs_core::{Engine, EngineConfig};
use std::path::PathBuf;
use std::time::Duration;

fn usage() -> ! {
    eprintln!(
        "uso: showfilesync-cli --data DIR --folder CARPETA [--nombre N] [--puerto P] [--sin-descubrimiento] [--conectar IP:PUERTO] [--json] <crear|unir CÓDIGO|ejecutar>"
    );
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();
    let mut args = std::env::args().skip(1);
    let mut data = None;
    let mut folder = None;
    let mut name = None;
    let mut port = None;
    let mut discovery = true;
    let mut connect = Vec::new();
    let mut json = false;
    let mut cmd = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data" => data = args.next().map(PathBuf::from),
            "--folder" => folder = args.next().map(PathBuf::from),
            "--nombre" => name = args.next(),
            "--puerto" => port = args.next().and_then(|p| p.parse().ok()),
            "--sin-descubrimiento" => discovery = false,
            "--conectar" => connect.push(args.next().and_then(|a| a.parse().ok()).unwrap_or_else(|| usage())),
            "--json" => json = true,
            "-h" | "--help" => usage(),
            _ => cmd.push(a),
        }
    }
    let (Some(data), Some(folder)) = (data, folder) else { usage() };
    let mut cfg = EngineConfig::new(data);
    cfg.device_name = name;
    cfg.listen_port = port;
    cfg.discovery = discovery;
    let engine = Engine::start(cfg).await.unwrap_or_else(|e| {
        eprintln!("no se pudo arrancar: {e}");
        std::process::exit(1)
    });
    if let Err(e) = engine.set_folder(&folder) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    for a in connect {
        engine.connect_to(a);
    }
    println!("Equipo {} ({}) escuchando en el puerto {}", engine.snapshot().device_name, &engine.device_id()[..8], engine.port());

    match cmd.first().map(String::as_str) {
        Some("crear") => {
            let code = engine.create_group();
            println!("Grupo creado. Código para añadir equipos (15 min): {}", sfs_core::auth::format_code(&code));
        }
        Some("unir") => {
            let code = cmd.get(1).cloned().unwrap_or_else(|| usage());
            // Damos tiempo al descubrimiento.
            let mut last = String::new();
            let mut joined = false;
            for _ in 0..15 {
                tokio::time::sleep(Duration::from_secs(1)).await;
                match engine.join_group(&code, None).await {
                    Ok(()) => {
                        joined = true;
                        break;
                    }
                    Err(e) => last = e,
                }
            }
            if !joined {
                eprintln!("No se pudo unir: {last}");
                std::process::exit(1);
            }
            println!("Unido al grupo «{}»", engine.snapshot().group.map(|g| g.name).unwrap_or_default());
        }
        Some("ejecutar") | None => {}
        _ => usage(),
    }

    let mut last_line = String::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        let s = engine.snapshot();
        let line = if json {
            serde_json::to_string(&s).unwrap_or_default()
        } else {
            let devices: Vec<String> = s.devices.iter().map(|d| format!("{}: {}", d.name, d.detail)).collect();
            let mut l = format!(
                "[{:?}] {} archivos · pendientes {} · equipos: {}",
                s.overall,
                s.file_count,
                s.pending_files,
                if devices.is_empty() { "ninguno".into() } else { devices.join(" | ") }
            );
            if let Some(w) = &s.network_warning {
                l.push_str(&format!("\n  ⚠ {w}"));
            }
            l
        };
        if line != last_line {
            println!("{line}");
            last_line = line;
        }
    }
    engine.shutdown().await;
}
