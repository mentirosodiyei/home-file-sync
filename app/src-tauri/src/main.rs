//! Aplicación de escritorio Show File Sync (Tauri 2).
//!
//! La ventana es solo una vista del motor (`sfs-core`). Al cerrarla, la app
//! sigue funcionando en la bandeja del sistema (Windows) o en la barra de
//! menús (macOS). Solo "Salir" detiene la sincronización.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use sfs_core::status::Overall;
use sfs_core::{Engine, EngineConfig, UiState};
use std::path::PathBuf;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

struct App {
    engine: Engine,
}

// ---------------------------------------------------------------------------
// Órdenes que llama la interfaz
// ---------------------------------------------------------------------------

#[tauri::command]
async fn get_state(app: State<'_, App>) -> Result<UiState, String> {
    Ok(app.engine.snapshot())
}

/// Abre el selector de carpetas del sistema y, si se elige una, la activa.
#[tauri::command]
async fn choose_folder(handle: AppHandle, app: State<'_, App>) -> Result<Option<String>, String> {
    let h = handle.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || h.dialog().file().set_title("Elige la carpeta que quieres sincronizar").blocking_pick_folder())
        .await
        .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else { return Ok(None) };
    app.engine.set_folder(&path).map(|p| Some(p.display().to_string()))
}

#[tauri::command]
async fn create_group(app: State<'_, App>) -> Result<String, String> {
    Ok(app.engine.create_group())
}

#[tauri::command]
async fn open_pairing(app: State<'_, App>) -> Result<String, String> {
    app.engine.open_pairing()
}

#[tauri::command]
async fn close_pairing(app: State<'_, App>) -> Result<(), String> {
    app.engine.close_pairing();
    Ok(())
}

#[tauri::command]
async fn join_group(app: State<'_, App>, code: String, device_id: Option<String>) -> Result<(), String> {
    app.engine.join_group(&code, device_id).await
}

#[tauri::command]
async fn leave_group(app: State<'_, App>) -> Result<(), String> {
    app.engine.leave_group();
    Ok(())
}

#[tauri::command]
async fn ack_conflicts(app: State<'_, App>) -> Result<(), String> {
    app.engine.ack_conflicts();
    Ok(())
}

#[tauri::command]
async fn rename_device(app: State<'_, App>, name: String) -> Result<(), String> {
    app.engine.set_device_name(&name);
    Ok(())
}

#[tauri::command]
async fn open_folder(handle: AppHandle, app: State<'_, App>) -> Result<(), String> {
    open_synced_folder(&handle, &app.engine)
}

fn open_synced_folder(handle: &AppHandle, engine: &Engine) -> Result<(), String> {
    let folder = engine.folder().ok_or("No hay carpeta seleccionada")?;
    handle.opener().open_path(folder.display().to_string(), None::<&str>).map_err(|e| e.to_string())
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

fn tooltip(s: &UiState) -> String {
    let status = match s.overall {
        Overall::NoFolder => "Elige una carpeta".to_string(),
        Overall::NoGroup => "Sin grupo".to_string(),
        Overall::Alone => "Esperando a otros equipos".to_string(),
        Overall::Synced => "Sincronizado".to_string(),
        Overall::Syncing => format!("Sincronizando · faltan {}", s.pending_files),
        Overall::Conflict => "Conflicto resuelto (ambas copias conservadas)".to_string(),
        Overall::Error => "Hay errores".to_string(),
    };
    let online = s.devices.iter().filter(|d| !matches!(d.state, sfs_core::status::DeviceState::Offline)).count();
    format!("Show File Sync — {status} · {online} equipo(s)")
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
    app.path().app_data_dir().unwrap_or_else(|_| std::env::temp_dir().join("ShowFileSync"))
}

fn main() {
    tracing_subscriber::fmt().with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into())).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_window(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let cfg = EngineConfig::new(data_dir(&handle));
            let engine = tauri::async_runtime::block_on(Engine::start(cfg))?;
            app.manage(App { engine: engine.clone() });

            let open = MenuItem::with_id(app, "open", "Abrir Show File Sync", true, None::<&str>)?;
            let folder = MenuItem::with_id(app, "folder", "Abrir carpeta sincronizada", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Salir (detiene la sincronización)", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &folder, &PredefinedMenuItem::separator(app)?, &quit])?;
            let tray = TrayIconBuilder::with_id("main")
                .icon(tray_icon())
                .icon_as_template(true)
                .tooltip("Show File Sync")
                .menu(&menu)
                .show_menu_on_left_click(cfg!(target_os = "macos"))
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "open" => show_window(app),
                    "folder" => {
                        if let Some(s) = app.try_state::<App>() {
                            let _ = open_synced_folder(app, &s.engine);
                        }
                    }
                    "quit" => {
                        if let Some(s) = app.try_state::<App>() {
                            tauri::async_runtime::block_on(s.engine.shutdown());
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
                    let _ = tray.set_tooltip(Some(tooltip(&engine.snapshot())));
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
            choose_folder,
            create_group,
            open_pairing,
            close_pairing,
            join_group,
            leave_group,
            ack_conflicts,
            rename_device,
            open_folder
        ])
        .build(tauri::generate_context!())
        .expect("error al iniciar Show File Sync")
        .run(|app, event| {
            // macOS: clic en el icono del Dock con la ventana oculta.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                show_window(app);
            }
            let _ = (app, event);
        });
}
