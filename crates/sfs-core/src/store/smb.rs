//! NAS por SMB con usuario y contraseña, conectado por el propio sistema
//! (como el Finder o el Explorador): en macOS con NetFS y en Windows con WNet.
//! La contraseña se pasa directamente a la función del sistema, nunca por
//! línea de comandos. Tras conectar, el NAS es una carpeta más.

use std::path::PathBuf;

/// Comprueba el nombre del equipo y de la carpeta compartida.
pub fn validate(host: &str, share: &str) -> Result<(), String> {
    let bad_host = host.is_empty() || host.len() > 253 || host.chars().any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '@' | '?' | '#'));
    let bad_share = share.is_empty() || share.len() > 80 || share.chars().any(|c| c.is_control() || matches!(c, '/' | '\\'));
    if bad_host {
        return Err(tr!("Escribe la dirección del NAS: su IP (p. ej. 192.168.1.10) o su nombre."));
    }
    if bad_share {
        return Err(tr!("Escribe el nombre de la carpeta compartida del NAS (p. ej. «Familia»)."));
    }
    Ok(())
}

/// Conecta (si hace falta) y devuelve la ruta local de la carpeta compartida.
pub fn connect(host: &str, share: &str, user: &str, pass: &str) -> Result<PathBuf, String> {
    validate(host, share)?;
    imp::connect(host.trim(), share.trim(), user, pass)
}

#[cfg(target_os = "macos")]
mod imp {
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFMutableDictionary;
    use core_foundation::string::CFString;
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
    use core_foundation_sys::base::{kCFAllocatorDefault, CFRelease};
    use core_foundation_sys::dictionary::CFMutableDictionaryRef;
    use core_foundation_sys::string::CFStringRef;
    use core_foundation_sys::url::{CFURLCreateWithString, CFURLRef};
    use std::path::PathBuf;

    #[link(name = "NetFS", kind = "framework")]
    extern "C" {
        fn NetFSMountURLSync(
            url: CFURLRef,
            mountpath: CFURLRef,
            user: CFStringRef,
            passwd: CFStringRef,
            open_options: CFMutableDictionaryRef,
            mount_options: CFMutableDictionaryRef,
            mountpoints: *mut CFArrayRef,
        ) -> i32;
    }

    /// Punto de montaje de esa carpeta compartida, si ya está conectada.
    fn existing(host: &str, share: &str) -> Option<PathBuf> {
        let mut mnt: *mut libc::statfs = std::ptr::null_mut();
        let n = unsafe { libc::getmntinfo(&mut mnt, libc::MNT_NOWAIT) };
        if n <= 0 || mnt.is_null() {
            return None;
        }
        let list = unsafe { std::slice::from_raw_parts(mnt, n as usize) };
        let want = format!("{}/{}", host.to_lowercase(), share.to_lowercase());
        for m in list {
            let cstr = |a: &[libc::c_char]| unsafe { std::ffi::CStr::from_ptr(a.as_ptr()) }.to_string_lossy().into_owned();
            if cstr(&m.f_fstypename) != "smbfs" {
                continue;
            }
            // «//usuario@host/carpeta» o «//host/carpeta»
            let from = super::super::pct_decode(&cstr(&m.f_mntfromname)).to_lowercase();
            let tail = from.trim_start_matches('/').rsplit_once('@').map(|(_, t)| t.to_string()).unwrap_or_else(|| from.trim_start_matches('/').to_string());
            if tail == want {
                return Some(PathBuf::from(cstr(&m.f_mntonname)));
            }
        }
        None
    }

    pub fn connect(host: &str, share: &str, user: &str, pass: &str) -> Result<PathBuf, String> {
        if let Some(p) = existing(host, share).filter(|p| p.is_dir()) {
            return Ok(p);
        }
        let h = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.to_string() };
        let url_s = CFString::new(&format!("smb://{h}/{}", super::super::pct(share)));
        let url = unsafe { CFURLCreateWithString(kCFAllocatorDefault, url_s.as_concrete_TypeRef(), std::ptr::null()) };
        if url.is_null() {
            return Err(tr!("Escribe la dirección del NAS: su IP (p. ej. 192.168.1.10) o su nombre."));
        }
        let user_s = CFString::new(user);
        let pass_s = CFString::new(pass);
        // Sin ventanas del sistema: si falta algo, se informa en la app.
        let mut open_opts: CFMutableDictionary<CFString, CFString> = CFMutableDictionary::new();
        open_opts.add(&CFString::new("UIOption"), &CFString::new("NoUI"));
        let mut points: CFArrayRef = std::ptr::null();
        let rc = unsafe {
            NetFSMountURLSync(
                url,
                std::ptr::null(),
                if user.is_empty() { std::ptr::null() } else { user_s.as_concrete_TypeRef() },
                if pass.is_empty() { std::ptr::null() } else { pass_s.as_concrete_TypeRef() },
                open_opts.as_concrete_TypeRef(),
                std::ptr::null_mut(),
                &mut points,
            )
        };
        unsafe { CFRelease(url as _) };
        let mut mounted = None;
        if !points.is_null() {
            unsafe {
                if CFArrayGetCount(points) > 0 {
                    let s = CFString::wrap_under_get_rule(CFArrayGetValueAtIndex(points, 0) as CFStringRef);
                    mounted = Some(PathBuf::from(s.to_string()));
                }
                CFRelease(points as _);
            }
        }
        match rc {
            0 => mounted.or_else(|| existing(host, share)).ok_or_else(|| tr!("No se ha podido conectar con el NAS ({}).", rc)),
            17 => existing(host, share).ok_or_else(|| tr!("No se ha podido conectar con el NAS ({}).", rc)), // ya montado
            80 | 1 | 13 => Err(tr!("El NAS no acepta el usuario o la contraseña.")),
            2 => Err(tr!("El NAS no tiene una carpeta compartida llamada «{}».", share)),
            51 | 60 | 61 | 64 | 65 => Err(tr!("No se encuentra el NAS en {}. ¿Está encendido y en la misma red?", host)),
            _ => Err(tr!("No se ha podido conectar con el NAS ({}).", rc)),
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use windows_sys::Win32::NetworkManagement::WNet::{WNetAddConnection2W, NETRESOURCEW, RESOURCETYPE_DISK};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn connect(host: &str, share: &str, user: &str, pass: &str) -> Result<PathBuf, String> {
        let unc = format!(r"\\{host}\{share}");
        let path = PathBuf::from(format!(r"{unc}\"));
        if std::fs::read_dir(&path).is_ok() {
            return Ok(path);
        }
        let mut remote = wide(&unc);
        let nr = NETRESOURCEW {
            dwScope: 0,
            dwType: RESOURCETYPE_DISK,
            dwDisplayType: 0,
            dwUsage: 0,
            lpLocalName: std::ptr::null_mut(),
            lpRemoteName: remote.as_mut_ptr(),
            lpComment: std::ptr::null_mut(),
            lpProvider: std::ptr::null_mut(),
        };
        let (u, p) = (wide(user), wide(pass));
        let rc = unsafe {
            WNetAddConnection2W(&nr, if pass.is_empty() { std::ptr::null() } else { p.as_ptr() }, if user.is_empty() { std::ptr::null() } else { u.as_ptr() }, 0)
        };
        match rc {
            0 => Ok(path),
            // Ya conectado (quizá con otro usuario): si se puede leer, vale.
            1219 | 85 if std::fs::read_dir(&path).is_ok() => Ok(path),
            1219 => Err(tr!("Windows ya está conectado a ese NAS con otro usuario. Desconéctalo en el Explorador o usa el mismo usuario.")),
            86 | 1326 | 1327 | 1330 | 5 => Err(tr!("El NAS no acepta el usuario o la contraseña.")),
            67 => Err(tr!("El NAS no tiene una carpeta compartida llamada «{}».", share)),
            53 | 1231 | 1232 | 1222 => Err(tr!("No se encuentra el NAS en {}. ¿Está encendido y en la misma red?", host)),
            _ => Err(tr!("No se ha podido conectar con el NAS ({}).", rc)),
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use std::path::PathBuf;
    pub fn connect(_host: &str, _share: &str, _user: &str, _pass: &str) -> Result<PathBuf, String> {
        Err(tr!("No se ha podido conectar con el NAS ({}).", "sistema no compatible"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_host_and_share() {
        assert!(validate("192.168.1.10", "Familia").is_ok());
        assert!(validate("nas.local", "Copias de casa").is_ok());
        assert!(validate("", "x").is_err());
        assert!(validate("192.168.1.10/Familia", "x").is_err());
        assert!(validate("ana@nas", "x").is_err());
        assert!(validate("nas", "a/b").is_err());
    }
}
