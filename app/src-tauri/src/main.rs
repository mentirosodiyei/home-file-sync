//! Aplicación de escritorio Home File Sync (Tauri 2).
//!
//! La ventana es solo una vista del hub (`sfs-core`), que gestiona todas las
//! carpetas compartidas de este equipo. Al cerrarla, la app sigue funcionando
//! en la bandeja del sistema (Windows) o en la barra de menús (macOS). Solo
//! "Salir" detiene la sincronización.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use sfs_core::hub::{Hub, HubConfig, HubState};
use sfs_core::tr;
use sfs_core::settings::ShareMode;
use sfs_core::status::Overall;
use std::path::PathBuf;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

struct App {
    hub: Hub,
}

fn mode(m: &str) -> ShareMode {
    if m == "backup" {
        ShareMode::Backup
    } else {
        ShareMode::Sync
    }
}

fn path(p: &str) -> Result<PathBuf, String> {
    if p.trim().is_empty() {
        return Err(tr!("Elige una carpeta."));
    }
    Ok(PathBuf::from(p))
}

// ---------------------------------------------------------------------------
// Órdenes que llama la interfaz
// ---------------------------------------------------------------------------

#[tauri::command]
async fn get_state(app: State<'_, App>) -> Result<HubState, String> {
    Ok(app.hub.snapshot())
}

/// Abre el selector de carpetas del sistema. Devuelve la ruta elegida.
#[tauri::command]
async fn pick_folder(handle: AppHandle) -> Result<Option<String>, String> {
    let h = handle.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || h.dialog().file().set_title(tr!("Elige la carpeta")).blocking_pick_folder())
        .await
        .map_err(|e| e.to_string())?;
    Ok(picked.and_then(|p| p.into_path().ok()).map(|p| p.display().to_string()))
}

#[tauri::command]
async fn create_share(app: State<'_, App>, name: String, folder: String, share_mode: String) -> Result<String, String> {
    let (id, _code) = app.hub.create_share(&name, &path(&folder)?, mode(&share_mode)).await?;
    Ok(id)
}

#[tauri::command]
async fn join_share(app: State<'_, App>, invite: Option<String>, code: String, folder: String, share_mode: String) -> Result<String, String> {
    app.hub.join_share(invite.as_deref(), &code, &path(&folder)?, mode(&share_mode)).await
}

#[tauri::command]
async fn join_manual(app: State<'_, App>, host: String, port: Option<String>, code: String, folder: String, share_mode: String, remember: bool) -> Result<String, String> {
    let port = match port.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => Some(sfs_core::netpath::parse_port(p)?),
        None => None,
    };
    app.hub.join_share_manual(&host, port, &code, &path(&folder)?, mode(&share_mode), remember).await
}

#[tauri::command]
async fn leave_share(app: State<'_, App>, id: String) -> Result<(), String> {
    app.hub.leave_share(&id).await
}

#[tauri::command]
async fn open_invite(app: State<'_, App>, id: String) -> Result<String, String> {
    app.hub.open_invite(&id)
}

#[tauri::command]
async fn close_invite(app: State<'_, App>, id: String) -> Result<(), String> {
    app.hub.close_invite(&id)
}

#[tauri::command]
async fn set_mode(app: State<'_, App>, id: String, share_mode: String) -> Result<(), String> {
    app.hub.set_mode(&id, mode(&share_mode))
}

#[tauri::command]
async fn rename_share(app: State<'_, App>, id: String, name: String) -> Result<(), String> {
    app.hub.rename_share(&id, &name)
}

#[tauri::command]
async fn change_folder(handle: AppHandle, app: State<'_, App>, id: String) -> Result<Option<String>, String> {
    let Some(p) = pick_folder(handle).await? else { return Ok(None) };
    app.hub.change_folder(&id, &PathBuf::from(p)).map(|p| Some(p.display().to_string()))
}

#[tauri::command]
async fn open_folder(handle: AppHandle, app: State<'_, App>, id: String) -> Result<(), String> {
    let folder = app.hub.folder(&id).ok_or_else(|| tr!("Esa carpeta no está disponible"))?;
    handle.opener().open_path(folder.display().to_string(), None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
async fn ack_conflicts(app: State<'_, App>, id: String) -> Result<(), String> {
    app.hub.ack_conflicts(&id)
}

#[tauri::command]
async fn rename_device(app: State<'_, App>, name: String) -> Result<(), String> {
    app.hub.set_device_name(&name);
    Ok(())
}

#[tauri::command]
async fn set_alias(app: State<'_, App>, device_id: String, alias: String) -> Result<(), String> {
    app.hub.set_alias(&device_id, &alias);
    Ok(())
}

#[tauri::command]
async fn get_network(app: State<'_, App>) -> Result<sfs_core::hub::NetworkView, String> {
    Ok(app.hub.network())
}

#[tauri::command]
async fn set_listen_vpn(app: State<'_, App>, on: bool) -> Result<(), String> {
    app.hub.set_listen_vpn(on);
    Ok(())
}

/// Idioma de la interfaz ("ca", "es" o "en"), según el del sistema.
#[tauri::command]
async fn get_lang() -> Result<String, String> {
    Ok(sfs_core::i18n::lang().code().to_string())
}

#[tauri::command]
async fn app_version() -> Result<String, String> {
    Ok(env!("CARGO_PKG_VERSION").to_string())
}

#[tauri::command]
async fn start_receive(app: State<'_, App>, share: Option<String>) -> Result<sfs_core::inbox::ReceiveView, String> {
    app.hub.start_receive(share.as_deref())
}

#[tauri::command]
async fn stop_receive(app: State<'_, App>) -> Result<(), String> {
    app.hub.stop_receive();
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LogView {
    entries: Vec<sfs_core::diag::LogEntry>,
    debug: bool,
    dir: Option<String>,
}

#[tauri::command]
async fn get_logs(level: String, query: String) -> Result<LogView, String> {
    Ok(LogView {
        entries: sfs_core::diag::entries(&level, &query, 1500),
        debug: sfs_core::diag::debug_enabled(),
        dir: sfs_core::diag::dir().map(|d| d.display().to_string()),
    })
}

#[tauri::command]
async fn logs_text(level: String, query: String) -> Result<String, String> {
    Ok(sfs_core::diag::as_text(&sfs_core::diag::entries(&level, &query, sfs_core::diag::RING_CAPACITY)))
}

#[tauri::command]
async fn set_debug_logging(on: bool) -> Result<(), String> {
    sfs_core::diag::set_debug(on);
    Ok(())
}

/// Exporta el registro completo a un archivo elegido por el usuario.
#[tauri::command]
async fn export_logs(handle: AppHandle, app: State<'_, App>) -> Result<Option<String>, String> {
    let h = handle.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        h.dialog().file().set_title(tr!("Exportar registro de diagnóstico")).set_file_name("HomeFileSync-diagnostico.log").blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else { return Ok(None) };
    let s = app.hub.snapshot();
    let ifs: Vec<String> = sfs_core::netpath::local_interfaces().iter().map(|i| format!("{} {}/{}", i.name, i.ip, i.prefix)).collect();
    let shares: Vec<String> = s
        .shares
        .iter()
        .map(|sh| format!("{} ({:?}, {} equipo(s))", sh.state.group.as_ref().map(|g| g.name.as_str()).unwrap_or("?"), sh.state.mode, sh.state.devices.len()))
        .collect();
    let header = format!(
        "Versión: {}\nSistema: {} {}\nEquipo: {} (id {})\nInterfaces: {}\nCarpetas compartidas: {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        s.device_name,
        &s.device_id[..8],
        ifs.join(", "),
        if shares.is_empty() { "ninguna".into() } else { shares.join("; ") }
    );
    sfs_core::diag::export(&path, &header).map_err(|e| e.to_string())?;
    Ok(Some(path.display().to_string()))
}

#[tauri::command]
async fn open_logs_folder(handle: AppHandle) -> Result<(), String> {
    let dir = sfs_core::diag::dir().ok_or_else(|| tr!("El registro en disco no está activo"))?;
    handle.opener().open_path(dir.display().to_string(), None::<&str>).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Ventana y bandeja
// ---------------------------------------------------------------------------

fn show_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn hide_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    // En macOS, sin ventana la app vive solo en la barra de menús (sin icono en el Dock).
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}

fn tooltip(s: &HubState) -> String {
    if s.shares.is_empty() {
        return tr!("Home File Sync — sin carpetas compartidas");
    }
    let syncing: u32 = s.shares.iter().map(|x| x.state.pending_files).sum();
    let errors = s.shares.iter().any(|x| x.state.overall == Overall::Error);
    let status = if errors {
        tr!("hay errores")
    } else if syncing > 0 {
        tr!("sincronizando · faltan {}", syncing)
    } else {
        tr!("todo al día")
    };
    tr!("Home File Sync — {} carpeta(s) · {}", s.shares.len(), status)
}

/// Icono de la bandeja dibujado en código: dos flechas en círculo.
/// En macOS se usa como "template" (el sistema lo colorea según el tema).
fn tray_icon() -> tauri::image::Image<'static> {
    const S: usize = 44;
    let mut rgba = vec![0u8; S * S * 4];
    let c = (S as f32 - 1.0) / 2.0;
    let (r_out, r_in) = (19.0f32, 14.0f32);
    for y in 0..S {
        for x in 0..S {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let d = (dx * dx + dy * dy).sqrt();
            let ang = dy.atan2(dx).to_degrees(); // -180..180
            // Dos arcos con huecos (arriba-derecha y abajo-izquierda).
            let in_gap = (ang > -75.0 && ang < -30.0) || (ang > 105.0 && ang < 150.0);
            let mut a = 0.0f32;
            if !in_gap {
                a = ((d - r_in).min(r_out - d) + 0.5).clamp(0.0, 1.0);
            }
            // Puntas de flecha al final de cada arco.
            for (tx, ty, dirx, diry) in [(c + 13.0, c - 12.0, 1.0f32, 0.9f32), (c - 13.0, c + 12.0, -1.0, -0.9)] {
                let (px, py) = (x as f32 - tx, y as f32 - ty);
                let along = (px * diry - py * dirx) / (dirx * dirx + diry * diry).sqrt();
                let across = (px * dirx + py * diry) / (dirx * dirx + diry * diry).sqrt();
                if along > -7.0 && along < 2.0 && across.abs() < (along + 7.0) * 0.8 {
                    a = 1.0;
                }
            }
            let i = (y * S + x) * 4;
            rgba[i + 3] = (a * 255.0) as u8;
        }
    }
    tauri::image::Image::new_owned(rgba, S as u32, S as u32)
}

fn data_dir(app: &AppHandle) -> PathBuf {
    app.path().app_data_dir().unwrap_or_else(|_| std::env::temp_dir().join("HomeFileSync"))
}

/// Menú de la aplicación en macOS, en el idioma de la interfaz (el que trae
/// Tauri por defecto está en inglés).
#[cfg(target_os = "macos")]
fn app_menu(app: &tauri::App) -> tauri::Result<Menu<tauri::Wry>> {
    use tauri::menu::{AboutMetadata, Submenu};
    let p = |m| PredefinedMenuItem::separator(m);
    let app_sub = Submenu::with_items(
        app,
        "Home File Sync",
        true,
        &[
            &PredefinedMenuItem::about(app, Some(&tr!("Acerca de Home File Sync")), Some(AboutMetadata::default()))?,
            &p(app)?,
            &PredefinedMenuItem::hide(app, Some(&tr!("Ocultar Home File Sync")))?,
            &PredefinedMenuItem::hide_others(app, Some(&tr!("Ocultar otros")))?,
            &PredefinedMenuItem::show_all(app, Some(&tr!("Mostrar todo")))?,
            &p(app)?,
            &PredefinedMenuItem::quit(app, Some(&tr!("Salir de Home File Sync")))?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        tr!("Edición"),
        true,
        &[
            &PredefinedMenuItem::undo(app, Some(&tr!("Deshacer")))?,
            &PredefinedMenuItem::redo(app, Some(&tr!("Rehacer")))?,
            &p(app)?,
            &PredefinedMenuItem::cut(app, Some(&tr!("Cortar")))?,
            &PredefinedMenuItem::copy(app, Some(&tr!("Copiar")))?,
            &PredefinedMenuItem::paste(app, Some(&tr!("Pegar")))?,
            &PredefinedMenuItem::select_all(app, Some(&tr!("Seleccionar todo")))?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        tr!("Ventana"),
        true,
        &[&PredefinedMenuItem::minimize(app, Some(&tr!("Minimizar")))?, &PredefinedMenuItem::close_window(app, Some(&tr!("Cerrar ventana")))?],
    )?;
    Menu::with_items(app, &[&app_sub, &edit, &window])
}

fn main() {
    // Idioma del sistema: català, castellano o inglés.
    sfs_core::i18n::set_lang(sfs_core::i18n::system_lang());
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;
    // Consola (filtrable con RUST_LOG) + registro de diagnóstico persistente.
    let console = tracing_subscriber::fmt::layer()
        .with_filter(tracing_subscriber::EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into())));
    tracing_subscriber::registry().with(console).with(sfs_core::diag::layer()).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_window(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            if let Err(e) = sfs_core::diag::init_file(&data_dir(&handle).join("logs")) {
                eprintln!("No se pudo abrir el registro de diagnóstico: {e}");
            }
            tracing::info!("Home File Sync {} iniciado ({} {})", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH);
            let cfg = HubConfig::new(data_dir(&handle));
            let hub = tauri::async_runtime::block_on(Hub::start(cfg))?;
            app.manage(App { hub: hub.clone() });

            #[cfg(target_os = "macos")]
            app.set_menu(app_menu(app)?)?;
            let open = MenuItem::with_id(app, "open", tr!("Abrir Home File Sync"), true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", tr!("Salir (detiene la sincronización)"), true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &PredefinedMenuItem::separator(app)?, &quit])?;
            let tray = TrayIconBuilder::with_id("main")
                .icon(tray_icon())
                .icon_as_template(true)
                .tooltip("Home File Sync")
                .menu(&menu)
                .show_menu_on_left_click(cfg!(target_os = "macos"))
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "open" => show_window(app),
                    "quit" => {
                        if let Some(s) = app.try_state::<App>() {
                            tauri::async_runtime::block_on(s.hub.shutdown());
                        }
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                        if !cfg!(target_os = "macos") {
                            show_window(tray.app_handle());
                        }
                    }
                })
                .build(app)?;

            // Mantiene el texto de la bandeja al día.
            tauri::async_runtime::spawn(async move {
                loop {
                    let _ = tray.set_tooltip(Some(tooltip(&hub.snapshot())));
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Cerrar la ventana no detiene la sincronización.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                hide_window(window.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            pick_folder,
            create_share,
            join_share,
            join_manual,
            leave_share,
            open_invite,
            close_invite,
            set_mode,
            rename_share,
            change_folder,
            open_folder,
            ack_conflicts,
            rename_device,
            set_alias,
            start_receive,
            stop_receive,
            get_network,
            set_listen_vpn,
            app_version,
            get_lang,
            get_logs,
            logs_text,
            set_debug_logging,
            export_logs,
            open_logs_folder
        ])
        .build(tauri::generate_context!())
        .expect("error al iniciar Home File Sync")
        .run(|app, event| {
            // macOS: clic en el icono del Dock con la ventana oculta.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                show_window(app);
            }
            let _ = (app, event);
        });
}
