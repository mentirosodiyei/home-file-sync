//! Contraseñas y tokens de los servicios externos (NAS, WebDAV, Google,
//! Dropbox). Se guardan en el almacén del sistema: el Llavero en macOS y el
//! Administrador de credenciales en Windows. Nunca en `settings.json`, en el
//! registro de diagnóstico ni en la interfaz.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const SERVICE: &str = "Home File Sync";

static MEMORY: AtomicBool = AtomicBool::new(false);
static MEM: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Guarda los secretos solo en memoria (pruebas automáticas: no tocan el Llavero).
pub fn use_memory_store() {
    MEMORY.store(true, Ordering::SeqCst);
}

fn memory() -> bool {
    MEMORY.load(Ordering::SeqCst) || cfg!(not(any(target_os = "macos", target_os = "windows")))
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    if memory() {
        MEM.lock().unwrap().get_or_insert_with(HashMap::new).insert(key.into(), value.into());
        return Ok(());
    }
    keyring::Entry::new(SERVICE, key)
        .and_then(|e| e.set_password(value))
        .map_err(|e| tr!("No se pudo guardar la contraseña en el almacén del sistema: {}", e))
}

pub fn get(key: &str) -> Option<String> {
    if memory() {
        return MEM.lock().unwrap().as_ref().and_then(|m| m.get(key).cloned());
    }
    keyring::Entry::new(SERVICE, key).and_then(|e| e.get_password()).ok()
}

pub fn delete(key: &str) {
    if memory() {
        if let Some(m) = MEM.lock().unwrap().as_mut() {
            m.remove(key);
        }
        return;
    }
    if let Ok(e) = keyring::Entry::new(SERVICE, key) {
        let _ = e.delete_credential();
    }
}
